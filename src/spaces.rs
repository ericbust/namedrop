use crate::message::SendError;
use crate::{RecipientRun, RoutingAddressSnapshot, SpaceId, StateNamespace};
use serde::Serialize;
use std::{collections::BTreeSet, fmt};

/// How long a workspace label may be once folded. Herdr labels are free text, and
/// a label reaches prompts and errors, so it is bounded on the way in.
const MAX_LABEL_CHARS: usize = 64;

/// A Herdr workspace label in the form people type and match it. Folding
/// lowercases ASCII and turns each run of characters a `#` token cannot hold
/// into one `-`, so "My Space" and `#my-space` compare equal, and the folded
/// form is always safe to show inside a prompt.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WorkspaceLabel(String);

impl WorkspaceLabel {
    pub fn from_token(token: &str) -> Option<Self> {
        if token.is_empty() || !token.bytes().all(is_label_token_byte) {
            return None;
        }
        let label = Self::from_herdr(token);
        (label.0 != "all").then_some(label)
    }

    pub fn from_herdr(label: &str) -> Self {
        let mut folded = String::new();
        let mut separator = false;
        for character in label.trim().chars() {
            if character.is_ascii() && is_label_token_byte(character as u8) {
                if separator && !folded.is_empty() {
                    folded.push('-');
                }
                separator = false;
                folded.push(character.to_ascii_lowercase());
            } else {
                separator = true;
            }
        }
        Self(folded.chars().take(MAX_LABEL_CHARS).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for WorkspaceLabel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl std::fmt::Display for WorkspaceLabel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "#{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpaceScope {
    Unqualified,
    Named(Vec<WorkspaceLabel>),
    All,
}

fn is_label_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

pub(crate) struct HerdrWorkspaces<'a, E> {
    herdr: &'a E,
    listed: std::cell::OnceCell<Vec<(String, String)>>,
}

impl<'a, E: crate::HerdrCommandExecutor> HerdrWorkspaces<'a, E> {
    pub(crate) fn new(herdr: &'a E) -> Self {
        Self {
            herdr,
            listed: std::cell::OnceCell::new(),
        }
    }

    fn list(&self) -> Result<&[(String, String)], String> {
        if let Some(listed) = self.listed.get() {
            return Ok(listed);
        }
        let listed = crate::lifecycle::workspace_labels(self.herdr)?;
        Ok(self.listed.get_or_init(|| listed))
    }

    pub(crate) fn names(&self, label: &WorkspaceLabel) -> Result<bool, String> {
        Ok(self.list()?.iter().any(|(id, herdr_label)| {
            [id, herdr_label]
                .into_iter()
                .any(|name| &WorkspaceLabel::from_herdr(name) == label)
        }))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SpaceDirectory {
    entries: Vec<SpaceEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpaceEntry {
    pub space: SpaceId,
    pub label: Option<WorkspaceLabel>,
    pub display: String,
    pub activated: bool,
    pub names: BTreeSet<String>,
    /// Retired aliases, kept for the current space only: a retired local name
    /// keeps its retired error instead of silently reaching another workspace.
    pub retired: BTreeSet<String>,
}

impl SpaceEntry {
    fn answers_to(&self, alias: &str) -> bool {
        self.names.contains(alias) || self.retired.contains(alias)
    }

    fn id(&self) -> WorkspaceLabel {
        WorkspaceLabel::from_herdr(self.space.as_str())
    }
}

impl SpaceDirectory {
    pub(crate) fn load<E: crate::HerdrCommandExecutor>(
        store: &mut crate::StateStore,
        workspaces: &HerdrWorkspaces<'_, E>,
        socket: &str,
        here: &SpaceId,
        run: &RecipientRun,
    ) -> Result<Self, String> {
        let index = store
            .transaction(|transaction| transaction.alias_index(socket))
            .map_err(|error| error.to_string())?;
        let Some(here_names) = index
            .iter()
            .find(|space| &space.space == here && space.activated)
        else {
            return Err("Namedrop is not activated for this space".to_owned());
        };
        let here_entry = |label: Option<&str>| SpaceEntry {
            space: here.clone(),
            label: label.map(WorkspaceLabel::from_herdr),
            display: label.map(displayed_label).unwrap_or_default(),
            activated: true,
            names: here_names.names.clone(),
            retired: here_names.retired.clone(),
        };
        let crosses = run.scope != SpaceScope::Unqualified
            || run.aliases.iter().any(|alias| {
                !here_entry(None).answers_to(alias)
                    && index.iter().any(|space| {
                        &space.space != here && space.activated && space.names.contains(alias)
                    })
            });
        if !crosses {
            return Ok(Self::new(here_entry(None), Vec::new()));
        }
        let labels = workspaces.list()?;
        let entry = |space: SpaceId, label: Option<&str>| {
            let stored = index.iter().find(|stored| stored.space == space);
            SpaceEntry {
                label: label.map(WorkspaceLabel::from_herdr),
                display: label.map(displayed_label).unwrap_or_default(),
                activated: stored.is_some_and(|stored| stored.activated),
                names: stored
                    .map(|stored| stored.names.clone())
                    .unwrap_or_default(),
                retired: BTreeSet::new(),
                space,
            }
        };
        let here_label = labels
            .iter()
            .find(|(id, _)| id == here.as_str())
            .map(|(_, label)| label.as_str());
        let others = labels
            .iter()
            .filter(|(id, _)| id != here.as_str())
            .map(|(id, label)| entry(SpaceId::new(id), Some(label)))
            .collect();
        Ok(Self::new(here_entry(here_label), others))
    }

    pub(crate) fn new(here: SpaceEntry, others: Vec<SpaceEntry>) -> Self {
        let mut entries = vec![here];
        entries.extend(others);
        Self { entries }
    }

    fn here(&self) -> &SpaceEntry {
        &self.entries[0]
    }

    fn others(&self) -> impl Iterator<Item = &SpaceEntry> {
        self.entries[1..].iter()
    }

    pub(crate) fn token(&self, entry: &SpaceEntry) -> WorkspaceLabel {
        workspace_token(
            &entry.space,
            entry.label.as_ref(),
            self.entries
                .iter()
                .map(|other| (&other.space, other.label.as_ref())),
        )
    }

    fn named(&self, label: &WorkspaceLabel) -> Result<&SpaceEntry, RouteRefusal> {
        let matches: Vec<&SpaceEntry> = self
            .entries
            .iter()
            .filter(|entry| &entry.id() == label || entry.label.as_ref() == Some(label))
            .collect();
        match matches.as_slice() {
            [] => Err(RouteRefusal::UnknownWorkspace {
                label: label.clone(),
                known: self
                    .entries
                    .iter()
                    .filter(|entry| entry.activated)
                    .map(|entry| self.token(entry))
                    .collect(),
            }),
            [entry] if !entry.activated => Err(RouteRefusal::NotActivated {
                label: label.clone(),
            }),
            [entry] => Ok(entry),
            many => Err(RouteRefusal::DuplicateLabel {
                label: label.clone(),
                workspaces: many
                    .iter()
                    .map(|entry| (entry.display.clone(), self.token(entry)))
                    .collect(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoutePlan {
    pub sender_label: WorkspaceLabel,
    pub legs: Vec<RouteLeg>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RouteLeg {
    Names {
        space: SpaceId,
        label: WorkspaceLabel,
        aliases: Vec<String>,
    },
    Participants(Vec<RoutingAddressSnapshot>),
}

impl RoutePlan {
    pub(crate) fn crosses(&self, here: &SpaceId) -> bool {
        self.legs.iter().any(|leg| match leg {
            RouteLeg::Names { space, .. } => space != here,
            RouteLeg::Participants(participants) => !participants.is_empty(),
        })
    }

    pub(crate) fn resolve(
        &self,
        transaction: &mut crate::StateTransaction<'_>,
        namespace: &StateNamespace,
        author: &crate::PaneAddressId,
        now_ms: u64,
    ) -> Result<Vec<RoutingAddressSnapshot>, crate::StoreError> {
        let mut recipients = Vec::new();
        for leg in &self.legs {
            match leg {
                RouteLeg::Participants(participants) => {
                    recipients.extend(participants.iter().cloned());
                }
                RouteLeg::Names { space, aliases, .. } if space == &namespace.space_id => {
                    recipients.extend(transaction.resolve_recipient_aliases(namespace, aliases)?);
                }
                RouteLeg::Names {
                    space,
                    label,
                    aliases,
                } => {
                    let there =
                        StateNamespace::new(namespace.session_socket_id.clone(), space.clone());
                    for member in transaction.resolve_recipient_aliases(&there, aliases)? {
                        recipients.push(transaction.envoy_for(
                            namespace,
                            &crate::store::EnvoyOrigin {
                                space: space.clone(),
                                address_id: member.address_id,
                                alias: member.alias,
                                origin_label: label.clone(),
                                host_label: self.sender_label.clone(),
                            },
                            now_ms,
                        )?);
                    }
                }
            }
        }
        crate::store::addressed_recipients(recipients, author)
    }
}

pub(crate) fn route<E: crate::HerdrCommandExecutor>(
    store: &mut crate::StateStore,
    workspaces: &HerdrWorkspaces<'_, E>,
    namespace: &StateNamespace,
    (author, in_reply_to): (&crate::PaneAddressId, Option<&crate::MessageId>),
    run: &RecipientRun,
) -> Result<RoutePlan, SendError> {
    let participants = store
        .transaction(|transaction| {
            let (Some(message_id), SpaceScope::Unqualified) = (in_reply_to, &run.scope) else {
                return Ok(Vec::new());
            };
            let Ok(route) = transaction.message_route(namespace, message_id) else {
                return Ok(Vec::new());
            };
            reply_participants(
                transaction,
                namespace,
                author,
                route.participants,
                &run.aliases,
            )
        })
        .map_err(|error| error.to_string())?;
    let remaining = RecipientRun {
        scope: run.scope.clone(),
        aliases: run
            .aliases
            .iter()
            .filter(|alias| {
                !participants
                    .iter()
                    .any(|participant| &participant.alias == *alias)
            })
            .cloned()
            .collect(),
    };
    let mut plan = if remaining.aliases.is_empty() {
        RoutePlan {
            sender_label: WorkspaceLabel::from_herdr(namespace.space_id.as_str()),
            legs: Vec::new(),
        }
    } else {
        let directory = SpaceDirectory::load(
            store,
            workspaces,
            &namespace.session_socket_id,
            &namespace.space_id,
            &remaining,
        )?;
        plan_route(&remaining, &directory).map_err(SendError::Refused)?
    };
    if !participants.is_empty() {
        plan.legs.insert(0, RouteLeg::Participants(participants));
    }
    Ok(plan)
}

/// The remote participants a reply names. A written Mention matches the name
/// each participant had in the message being answered, so a later rename on
/// either side cannot turn a reply toward someone else.
fn reply_participants(
    transaction: &crate::StateTransaction<'_>,
    namespace: &StateNamespace,
    author: &crate::PaneAddressId,
    participants: Vec<RoutingAddressSnapshot>,
    aliases: &[String],
) -> Result<Vec<RoutingAddressSnapshot>, crate::StoreError> {
    let mut remote = Vec::new();
    for alias in aliases {
        let named: Vec<&RoutingAddressSnapshot> = participants
            .iter()
            .filter(|participant| &participant.alias == alias && &participant.address_id != author)
            .collect();
        let mut envoys = Vec::new();
        for participant in &named {
            if transaction.is_envoy(namespace, &participant.address_id)? {
                envoys.push(*participant);
            }
        }
        match (envoys.as_slice(), named.len()) {
            ([], _) => {}
            ([participant], 1) => remote.push(RoutingAddressSnapshot {
                pane_binding_id: None,
                agent_session_id: None,
                ..(*participant).clone()
            }),
            _ => {
                return Err(crate::StoreError::invalid_input(format!(
                    "@{alias} names more than one participant of this conversation; prefix the \
                     reply with the workspace you mean"
                )));
            }
        }
    }
    Ok(remote)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RouteRefusal {
    UnknownMention {
        alias: String,
    },
    Ambiguous {
        choices: Vec<AmbiguousName>,
    },
    UnknownWorkspace {
        label: WorkspaceLabel,
        known: Vec<WorkspaceLabel>,
    },
    DuplicateLabel {
        label: WorkspaceLabel,
        workspaces: Vec<(String, WorkspaceLabel)>,
    },
    NotActivated {
        label: WorkspaceLabel,
    },
    NotInNamedWorkspaces {
        alias: String,
        labels: Vec<WorkspaceLabel>,
    },
    NothingForWorkspace {
        label: WorkspaceLabel,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AmbiguousName {
    pub alias: String,
    pub labels: Vec<WorkspaceLabel>,
}

impl fmt::Display for RouteRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMention { alias } => write!(formatter, "unknown Mention: @{alias}"),
            Self::Ambiguous { choices } => {
                for (index, choice) in choices.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str("; ")?;
                    }
                    write!(
                        formatter,
                        "@{} is not named in this workspace but is named in {}",
                        choice.alias,
                        joined(&choice.labels, " and ")
                    )?;
                }
                formatter.write_str(
                    "; prefix the source with the workspace you mean, or #all for every one",
                )
            }
            Self::UnknownWorkspace { label, known } if known.is_empty() => {
                write!(formatter, "no workspace is labelled {label}")
            }
            Self::UnknownWorkspace { label, known } => write!(
                formatter,
                "no workspace is labelled {label}; activated workspaces: {}",
                joined(known, ", ")
            ),
            Self::DuplicateLabel { label, workspaces } => write!(
                formatter,
                "{label} matches more than one workspace ({}); use the token shown, or rename one in \
                 Herdr",
                workspaces
                    .iter()
                    .map(|(display, id)| format!(
                        "{} as {id}",
                        serde_json::Value::String(display.clone())
                    ))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            Self::NotActivated { label } => write!(
                formatter,
                "Namedrop is not activated in {label}; activate it there before sending"
            ),
            Self::NotInNamedWorkspaces { alias, labels } => write!(
                formatter,
                "@{alias} is not named in {}",
                joined(labels, " or ")
            ),
            Self::NothingForWorkspace { label } => {
                write!(formatter, "none of the Mentions is named in {label}")
            }
        }
    }
}

pub(crate) fn workspace_token<'a>(
    space: &SpaceId,
    label: Option<&WorkspaceLabel>,
    workspaces: impl Iterator<Item = (&'a SpaceId, Option<&'a WorkspaceLabel>)>,
) -> WorkspaceLabel {
    let id = WorkspaceLabel::from_herdr(space.as_str());
    match label {
        Some(label) if !matches!(label.as_str(), "" | "all") => {
            let shared = workspaces
                .filter(|(other, other_label)| {
                    *other != space
                        && (*other_label == Some(label)
                            || &WorkspaceLabel::from_herdr(other.as_str()) == label)
                })
                .count();
            if shared == 0 { label.clone() } else { id }
        }
        _ => id,
    }
}

fn displayed_label(label: &str) -> String {
    crate::lifecycle::terminal_safe_text(label)
        .chars()
        .take(64)
        .collect()
}

fn joined(labels: &[WorkspaceLabel], separator: &str) -> String {
    labels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

pub(crate) fn plan_route(
    run: &RecipientRun,
    directory: &SpaceDirectory,
) -> Result<RoutePlan, RouteRefusal> {
    let mut targets: Vec<(&SpaceEntry, &String)> = Vec::new();
    match &run.scope {
        SpaceScope::Unqualified => {
            let mut ambiguous = Vec::new();
            for alias in &run.aliases {
                let here = directory.here();
                if here.answers_to(alias) {
                    targets.push((here, alias));
                    continue;
                }
                let homes: Vec<&SpaceEntry> = directory
                    .others()
                    .filter(|entry| entry.activated && entry.names.contains(alias))
                    .collect();
                match homes.as_slice() {
                    [] => {
                        return Err(RouteRefusal::UnknownMention {
                            alias: alias.clone(),
                        });
                    }
                    [home] => targets.push((home, alias)),
                    homes => ambiguous.push(AmbiguousName {
                        alias: alias.clone(),
                        labels: homes.iter().map(|home| directory.token(home)).collect(),
                    }),
                }
            }
            if !ambiguous.is_empty() {
                return Err(RouteRefusal::Ambiguous { choices: ambiguous });
            }
        }
        SpaceScope::Named(labels) => {
            let named = labels
                .iter()
                .map(|label| directory.named(label))
                .collect::<Result<Vec<_>, _>>()?;
            for alias in &run.aliases {
                let homes: Vec<&SpaceEntry> = named
                    .iter()
                    .copied()
                    .filter(|entry| entry.answers_to(alias))
                    .collect();
                if homes.is_empty() {
                    return Err(RouteRefusal::NotInNamedWorkspaces {
                        alias: alias.clone(),
                        labels: labels.clone(),
                    });
                }
                targets.extend(homes.into_iter().map(|home| (home, alias)));
            }
            if let Some((_, label)) = named.iter().zip(labels).find(|(entry, _)| {
                !targets
                    .iter()
                    .any(|(target, _)| target.space == entry.space)
            }) {
                return Err(RouteRefusal::NothingForWorkspace {
                    label: label.clone(),
                });
            }
        }
        SpaceScope::All => {
            for alias in &run.aliases {
                let before = targets.len();
                targets.extend(
                    directory
                        .entries
                        .iter()
                        .filter(|entry| entry.activated && entry.names.contains(alias))
                        .map(|entry| (entry, alias)),
                );
                if targets.len() == before {
                    return Err(RouteRefusal::UnknownMention {
                        alias: alias.clone(),
                    });
                }
            }
        }
    }
    let legs = directory
        .entries
        .iter()
        .filter_map(|entry| {
            let aliases: Vec<String> = targets
                .iter()
                .filter(|(target, _)| target.space == entry.space)
                .map(|(_, alias)| (*alias).clone())
                .collect();
            (!aliases.is_empty()).then(|| RouteLeg::Names {
                space: entry.space.clone(),
                label: directory.token(entry),
                aliases,
            })
        })
        .collect();
    Ok(RoutePlan {
        sender_label: directory.token(directory.here()),
        legs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(token: &str) -> WorkspaceLabel {
        WorkspaceLabel::from_token(token).unwrap()
    }

    fn entry(id: &str, herdr_label: &str, names: &[&str]) -> SpaceEntry {
        SpaceEntry {
            space: SpaceId::new(id),
            label: Some(WorkspaceLabel::from_herdr(herdr_label)),
            display: herdr_label.to_owned(),
            activated: true,
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            retired: BTreeSet::new(),
        }
    }

    fn directory() -> SpaceDirectory {
        SpaceDirectory::new(
            SpaceEntry {
                retired: BTreeSet::from(["old".to_owned()]),
                ..entry("w1", "ws1", &["lead"])
            },
            vec![
                entry(
                    "w2",
                    "My Space",
                    &["lead", "tester", "ops", "reviewers", "old"],
                ),
                entry("w3", "ws3", &["lead", "ops"]),
                SpaceEntry {
                    activated: false,
                    ..entry("w4", "dormant", &["lead", "ghost"])
                },
            ],
        )
    }

    fn run(scope: SpaceScope, aliases: &[&str]) -> RecipientRun {
        RecipientRun {
            scope,
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        }
    }

    fn legs(plan: Result<RoutePlan, RouteRefusal>) -> Vec<(String, Vec<String>)> {
        plan.unwrap()
            .legs
            .into_iter()
            .map(|leg| match leg {
                RouteLeg::Names { space, aliases, .. } => (space.as_str().to_owned(), aliases),
                RouteLeg::Participants(_) => unreachable!("planning resolves names only"),
            })
            .collect()
    }

    fn leg(space: &str, aliases: &[&str]) -> (String, Vec<String>) {
        (
            space.to_owned(),
            aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        )
    }

    #[test]
    fn unqualified_resolution_table() {
        let directory = directory();
        let plan =
            |aliases: &[&str]| plan_route(&run(SpaceScope::Unqualified, aliases), &directory);
        assert_eq!(legs(plan(&["lead"])), [leg("w1", &["lead"])], "local wins");
        assert_eq!(
            legs(plan(&["old"])),
            [leg("w1", &["old"])],
            "a local tombstone stays local and keeps its retired error"
        );
        assert_eq!(
            legs(plan(&["tester"])),
            [leg("w2", &["tester"])],
            "one remote home"
        );
        assert_eq!(
            legs(plan(&["reviewers"])),
            [leg("w2", &["reviewers"])],
            "a group alias follows the same table"
        );
        assert_eq!(
            plan(&["ops"]),
            Err(RouteRefusal::Ambiguous {
                choices: vec![AmbiguousName {
                    alias: "ops".to_owned(),
                    labels: vec![label("my-space"), label("ws3")],
                }],
            })
        );
        assert_eq!(
            plan(&["ops", "nobody"]),
            Err(RouteRefusal::UnknownMention {
                alias: "nobody".to_owned()
            }),
            "an unknown name wins over a question the person cannot answer"
        );
        assert_eq!(
            plan(&["ghost"]),
            Err(RouteRefusal::UnknownMention {
                alias: "ghost".to_owned()
            }),
            "an inactive workspace is not a home"
        );
        assert_eq!(
            legs(plan(&["tester", "lead"])),
            [leg("w1", &["lead"]), leg("w2", &["tester"])],
            "here's leg comes first"
        );
    }

    #[test]
    fn named_resolution_table() {
        let directory = directory();
        let plan = |labels: &[&str], aliases: &[&str]| {
            plan_route(
                &run(
                    SpaceScope::Named(labels.iter().map(|token| label(token)).collect()),
                    aliases,
                ),
                &directory,
            )
        };
        assert_eq!(
            legs(plan(&["my-space"], &["lead"])),
            [leg("w2", &["lead"])],
            "My Space matches #my-space, and a named scope skips the local name"
        );
        assert_eq!(
            legs(plan(&["ws3", "my-space"], &["lead", "ops"])),
            [leg("w2", &["lead", "ops"]), leg("w3", &["lead", "ops"])]
        );
        assert_eq!(legs(plan(&["ws1"], &["lead"])), [leg("w1", &["lead"])]);
        assert!(matches!(
            plan(&["nowhere"], &["lead"]),
            Err(RouteRefusal::UnknownWorkspace { known, .. })
                if known == [label("ws1"), label("my-space"), label("ws3")]
        ));
        assert_eq!(
            plan(&["dormant"], &["lead"]),
            Err(RouteRefusal::NotActivated {
                label: label("dormant")
            })
        );
        assert_eq!(
            plan(&["ws3"], &["tester"]),
            Err(RouteRefusal::NotInNamedWorkspaces {
                alias: "tester".to_owned(),
                labels: vec![label("ws3")],
            })
        );
        assert_eq!(
            plan(&["my-space", "ws3"], &["tester"]),
            Err(RouteRefusal::NothingForWorkspace {
                label: label("ws3")
            })
        );
    }

    #[test]
    fn duplicate_folded_labels_name_both_workspaces() {
        let directory = SpaceDirectory::new(
            entry("w1", "ws1", &[]),
            vec![
                entry("w2", "Dash Board", &["lead"]),
                entry("w3", "dash-board", &["lead"]),
            ],
        );
        let refusal = plan_route(
            &run(SpaceScope::Named(vec![label("dash-board")]), &["lead"]),
            &directory,
        )
        .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "#dash-board matches more than one workspace (\"Dash Board\" as #w2 and \"dash-board\" \
             as #w3); use the token shown, or rename one in Herdr"
        );
    }

    #[test]
    fn a_workspace_is_always_reachable_by_its_id() {
        let directory = SpaceDirectory::new(
            entry("w1", "ws1", &[]),
            vec![entry("w2", "All", &["lead"]), entry("w3", "", &["lead"])],
        );
        assert_eq!(
            directory
                .entries
                .iter()
                .map(|entry| directory.token(entry).to_string())
                .collect::<Vec<_>>(),
            ["#ws1", "#w2", "#w3"]
        );
        for id in ["w2", "w3"] {
            assert_eq!(
                legs(plan_route(
                    &run(SpaceScope::Named(vec![label(id)]), &["lead"]),
                    &directory
                )),
                [leg(id, &["lead"])]
            );
        }
    }

    #[test]
    fn all_reaches_every_activated_workspace_with_the_name() {
        let directory = directory();
        assert_eq!(
            legs(plan_route(&run(SpaceScope::All, &["lead"]), &directory)),
            [
                leg("w1", &["lead"]),
                leg("w2", &["lead"]),
                leg("w3", &["lead"])
            ]
        );
        assert_eq!(
            plan_route(&run(SpaceScope::All, &["ghost"]), &directory),
            Err(RouteRefusal::UnknownMention {
                alias: "ghost".to_owned()
            })
        );
    }

    #[test]
    fn an_unknown_mention_keeps_the_text_the_hook_matches() {
        assert_eq!(
            RouteRefusal::UnknownMention {
                alias: "x".to_owned()
            }
            .to_string(),
            "unknown Mention: @x"
        );
    }
}
