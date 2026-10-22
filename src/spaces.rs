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
    listed: std::cell::OnceCell<Vec<crate::lifecycle::ListedWorkspace>>,
}

impl<'a, E: crate::HerdrCommandExecutor> HerdrWorkspaces<'a, E> {
    pub(crate) fn new(herdr: &'a E) -> Self {
        Self {
            herdr,
            listed: std::cell::OnceCell::new(),
        }
    }

    fn list(&self) -> Result<&[crate::lifecycle::ListedWorkspace], String> {
        if let Some(listed) = self.listed.get() {
            return Ok(listed);
        }
        let listed = crate::lifecycle::workspace_labels(self.herdr)?;
        Ok(self.listed.get_or_init(|| listed))
    }

    /// Whether `#label` names a workspace by its label or id, or names the
    /// repository some workspace's checkout belongs to.
    pub(crate) fn names(&self, label: &WorkspaceLabel) -> Result<bool, String> {
        Ok(self.list()?.iter().any(|workspace| {
            [
                Some(&workspace.id),
                Some(&workspace.label),
                workspace.repo.as_ref(),
            ]
            .into_iter()
            .flatten()
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
    /// The repository this workspace's checkout belongs to. A `#repo` token
    /// reaches every workspace that shares it: the main checkout and each
    /// linked worktree alike.
    pub repo: Option<WorkspaceLabel>,
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
        let here_entry = |label: Option<&str>, repo: Option<&str>| SpaceEntry {
            space: here.clone(),
            label: label.map(WorkspaceLabel::from_herdr),
            repo: repo.map(WorkspaceLabel::from_herdr),
            display: label.map(displayed_label).unwrap_or_default(),
            activated: true,
            names: here_names.names.clone(),
            retired: here_names.retired.clone(),
        };
        let crosses = run.scope != SpaceScope::Unqualified
            || run.aliases.iter().any(|alias| {
                !here_entry(None, None).answers_to(alias)
                    && index.iter().any(|space| {
                        &space.space != here && space.activated && space.names.contains(alias)
                    })
            });
        if !crosses {
            return Ok(Self::new(here_entry(None, None), Vec::new()));
        }
        let listed = workspaces.list()?;
        let entry = |workspace: &crate::lifecycle::ListedWorkspace| {
            let space = SpaceId::new(&workspace.id);
            let stored = index.iter().find(|stored| stored.space == space);
            SpaceEntry {
                label: Some(WorkspaceLabel::from_herdr(&workspace.label)),
                repo: workspace.repo.as_deref().map(WorkspaceLabel::from_herdr),
                display: displayed_label(&workspace.label),
                activated: stored.is_some_and(|stored| stored.activated),
                names: stored
                    .map(|stored| stored.names.clone())
                    .unwrap_or_default(),
                retired: BTreeSet::new(),
                space,
            }
        };
        let here_listed = listed
            .iter()
            .find(|workspace| workspace.id == here.as_str());
        let others = listed
            .iter()
            .filter(|workspace| workspace.id != here.as_str())
            .map(entry)
            .collect();
        Ok(Self::new(
            here_entry(
                here_listed.map(|workspace| workspace.label.as_str()),
                here_listed.and_then(|workspace| workspace.repo.as_deref()),
            ),
            others,
        ))
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

    /// What a typed `#label` reaches. A label that names two workspaces is
    /// refused before the repository is consulted: the person typed a token
    /// that already means two things, so a third reading would only guess.
    fn scope<'a>(&'a self, label: &'a WorkspaceLabel) -> Result<LabelScope<'a>, RouteRefusal> {
        let exact: Vec<&SpaceEntry> = self
            .entries
            .iter()
            .filter(|entry| &entry.id() == label || entry.label.as_ref() == Some(label))
            .collect();
        let family: Vec<&SpaceEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.repo.as_ref() == Some(label))
            .filter(|entry| !exact.iter().any(|other| other.space == entry.space))
            .collect();
        let exact = match exact.as_slice() {
            [] if family.is_empty() => {
                return Err(RouteRefusal::UnknownWorkspace {
                    label: label.clone(),
                    known: self
                        .entries
                        .iter()
                        .filter(|entry| entry.activated)
                        .map(|entry| self.token(entry))
                        .collect(),
                });
            }
            [] => None,
            [entry] => Some(*entry),
            many => {
                return Err(RouteRefusal::DuplicateLabel {
                    label: label.clone(),
                    workspaces: many
                        .iter()
                        .map(|entry| (entry.display.clone(), self.token(entry)))
                        .collect(),
                });
            }
        };
        let family: Vec<&SpaceEntry> = family.into_iter().filter(|entry| entry.activated).collect();
        Ok(LabelScope {
            label,
            not_activated: match exact {
                Some(entry) => !entry.activated,
                None => family.is_empty(),
            },
            exact: exact.filter(|entry| entry.activated),
            family,
        })
    }
}

/// One typed `#label`: the activated workspace it names exactly, and the
/// activated workspaces of the repository it names. The exact match wins; the
/// repository is the fallback for a name the exact match lacks.
struct LabelScope<'a> {
    label: &'a WorkspaceLabel,
    exact: Option<&'a SpaceEntry>,
    family: Vec<&'a SpaceEntry>,
    /// A name found nowhere in this scope reports that Namedrop is not
    /// activated, rather than that the name is missing.
    not_activated: bool,
}

impl<'a> LabelScope<'a> {
    fn homes(&self, alias: &str) -> Vec<&'a SpaceEntry> {
        match self.exact {
            Some(exact) if exact.answers_to(alias) => vec![exact],
            _ => self
                .family
                .iter()
                .copied()
                .filter(|entry| entry.answers_to(alias))
                .collect(),
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
    /// The `#repo` token the person typed, when the name is ambiguous among
    /// that repository's workspaces rather than among all of them.
    pub within: Option<WorkspaceLabel>,
}

impl AmbiguousName {
    pub(crate) fn describe(&self, separator: &str) -> String {
        match &self.within {
            None => format!(
                "@{} is not named in this workspace. It is named in these workspaces: {}",
                self.alias,
                joined(&self.labels, separator)
            ),
            Some(repo) => format!(
                "@{} is named in more than one {} worktree: {}",
                self.alias,
                repo.as_str(),
                joined(&self.labels, separator)
            ),
        }
    }
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
                    match &choice.within {
                        None => write!(
                            formatter,
                            "@{} is not named in this workspace but is named in {}",
                            choice.alias,
                            joined(&choice.labels, " and ")
                        )?,
                        Some(_) => formatter.write_str(&choice.describe(", "))?,
                    }
                }
                if choices.iter().any(|choice| choice.within.is_some()) {
                    formatter.write_str(
                        "; name the workspace you mean in place of the repository, or name each \
                         one",
                    )
                } else {
                    formatter.write_str(
                        "; prefix the source with the workspace you mean, or #all for every one",
                    )
                }
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
                        within: None,
                    }),
                }
            }
            if !ambiguous.is_empty() {
                return Err(RouteRefusal::Ambiguous { choices: ambiguous });
            }
        }
        SpaceScope::Named(labels) => {
            let scopes = labels
                .iter()
                .map(|label| directory.scope(label))
                .collect::<Result<Vec<_>, _>>()?;
            let mut reached = vec![false; scopes.len()];
            let mut ambiguous = Vec::new();
            for alias in &run.aliases {
                let mut found = false;
                for (scope, reached) in scopes.iter().zip(&mut reached) {
                    match scope.homes(alias).as_slice() {
                        [] if scope.not_activated => {
                            return Err(RouteRefusal::NotActivated {
                                label: scope.label.clone(),
                            });
                        }
                        [] => continue,
                        [home] => {
                            if !targets.iter().any(|(target, named)| {
                                target.space == home.space && *named == alias
                            }) {
                                targets.push((home, alias));
                            }
                        }
                        homes => ambiguous.push(AmbiguousName {
                            alias: alias.clone(),
                            labels: homes.iter().map(|home| directory.token(home)).collect(),
                            within: Some(scope.label.clone()),
                        }),
                    }
                    *reached = true;
                    found = true;
                }
                if !found {
                    return Err(RouteRefusal::NotInNamedWorkspaces {
                        alias: alias.clone(),
                        labels: labels.clone(),
                    });
                }
            }
            if let Some((scope, _)) = scopes.iter().zip(&reached).find(|(_, reached)| !**reached) {
                return Err(RouteRefusal::NothingForWorkspace {
                    label: scope.label.clone(),
                });
            }
            if !ambiguous.is_empty() {
                return Err(RouteRefusal::Ambiguous { choices: ambiguous });
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
            repo: None,
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
                    within: None,
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

    fn worktree(id: &str, herdr_label: &str, repo: &str, names: &[&str]) -> SpaceEntry {
        SpaceEntry {
            repo: Some(WorkspaceLabel::from_herdr(repo)),
            ..entry(id, herdr_label, names)
        }
    }

    /// `namestack` is a repository with its main checkout in w2 (labelled
    /// after the repo, not activated) and linked worktrees in w3, w4 and w5.
    /// `tools` is a repository whose main checkout is activated.
    fn repositories() -> SpaceDirectory {
        SpaceDirectory::new(
            entry("w1", "ws1", &["lead"]),
            vec![
                SpaceEntry {
                    activated: false,
                    ..worktree("w2", "namestack", "namestack", &["claude"])
                },
                worktree("w3", "catchup-again", "namestack", &["claude", "codex"]),
                worktree("w4", "other", "namestack", &["codex", "pi"]),
                SpaceEntry {
                    activated: false,
                    ..worktree("w5", "parked", "namestack", &["pi", "ghost"])
                },
                worktree("w6", "tools", "tools", &["lead"]),
                worktree("w7", "tools-fix", "tools", &["lead", "fixer"]),
                worktree("w8", "solo-feature", "solo", &[]),
            ],
        )
    }

    #[test]
    fn repository_resolution_table() {
        let directory = repositories();
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
            legs(plan(&["namestack"], &["claude"])),
            [leg("w3", &["claude"])],
            "an inactive main checkout hands the name to its one worktree that has it"
        );
        assert_eq!(
            legs(plan(&["tools"], &["lead"])),
            [leg("w6", &["lead"])],
            "the exact label wins over another worktree with the same name"
        );
        assert_eq!(
            legs(plan(&["tools"], &["fixer"])),
            [leg("w7", &["fixer"])],
            "an activated exact match without the name still looks in its repository"
        );
        assert_eq!(
            plan(&["namestack"], &["codex", "claude", "pi"]),
            Err(RouteRefusal::Ambiguous {
                choices: vec![AmbiguousName {
                    alias: "codex".to_owned(),
                    labels: vec![label("catchup-again"), label("other")],
                    within: Some(label("namestack")),
                }],
            }),
            "two homes ask, and an inactive worktree is not a home"
        );
        assert_eq!(
            plan(&["namestack"], &["ghost"]),
            Err(RouteRefusal::NotActivated {
                label: label("namestack")
            }),
            "an inactive exact match with no home in its repository is still not activated"
        );
        assert_eq!(
            plan(&["namestack"], &["claude", "nobody"]),
            Err(RouteRefusal::NotActivated {
                label: label("namestack")
            }),
            "one missing name keeps the whole send refused"
        );
        assert!(
            matches!(
                plan(&["nowhere"], &["claude"]),
                Err(RouteRefusal::UnknownWorkspace { .. })
            ),
            "a token naming neither a workspace nor a repository is unknown"
        );
        assert_eq!(
            plan(&["solo"], &["lead"]),
            Err(RouteRefusal::NotInNamedWorkspaces {
                alias: "lead".to_owned(),
                labels: vec![label("solo")],
            }),
            "a repository-only token is valid even when no worktree has the name"
        );
        assert_eq!(
            legs(plan(&["namestack", "catchup-again"], &["claude"])),
            [leg("w3", &["claude"])],
            "a workspace reached twice gets the name once"
        );
        assert_eq!(
            plan(&["ws1", "namestack"], &["lead"]),
            Err(RouteRefusal::NotActivated {
                label: label("namestack")
            })
        );
    }

    #[test]
    fn a_repository_name_alone_reaches_its_one_worktree() {
        let directory = SpaceDirectory::new(
            entry("w1", "ws1", &["lead"]),
            vec![
                worktree("w2", "catchup-again", "namestack", &["claude"]),
                worktree("w3", "other", "namestack", &["codex"]),
            ],
        );
        assert_eq!(
            legs(plan_route(
                &run(SpaceScope::Named(vec![label("namestack")]), &["claude"]),
                &directory
            )),
            [leg("w2", &["claude"])]
        );
    }

    #[test]
    fn a_duplicate_label_refuses_before_its_repository_is_consulted() {
        let directory = SpaceDirectory::new(
            entry("w1", "ws1", &[]),
            vec![
                worktree("w2", "namestack", "namestack", &[]),
                worktree("w3", "Namestack", "namestack", &[]),
                worktree("w4", "catchup-again", "namestack", &["claude"]),
            ],
        );
        assert!(matches!(
            plan_route(
                &run(SpaceScope::Named(vec![label("namestack")]), &["claude"]),
                &directory
            ),
            Err(RouteRefusal::DuplicateLabel { .. })
        ));
    }

    #[test]
    fn a_name_in_two_worktrees_says_which_repository_and_offers_each() {
        let refusal = plan_route(
            &run(SpaceScope::Named(vec![label("namestack")]), &["codex"]),
            &repositories(),
        )
        .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "@codex is named in more than one namestack worktree: #catchup-again, #other; name \
             the workspace you mean in place of the repository, or name each one"
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
