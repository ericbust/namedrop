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
    ) -> Result<Self, SendError> {
        let index = store.transaction(|transaction| transaction.alias_index(socket))?;
        let Some(here_names) = index
            .iter()
            .find(|space| &space.space == here && space.activated)
        else {
            return Err(SendError::Input(
                "Namedrop is not activated for this space".to_owned(),
            ));
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
        let looks_elsewhere = run.scope != SpaceScope::Unqualified
            || run.aliases.iter().any(|alias| {
                !here_entry(None, None).answers_to(alias)
                    && index.iter().any(|space| {
                        &space.space != here && space.activated && space.names.contains(alias)
                    })
            });
        if !looks_elsewhere {
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

/// Live panes of the workspaces one decision reads, discovered before its
/// transaction. A workspace Herdr could not read is unknown, never empty.
#[derive(Clone, Debug)]
pub(crate) struct LiveSpaces {
    here: crate::DiscoveredWorktree,
    others: std::collections::BTreeMap<SpaceId, Option<crate::DiscoveredWorktree>>,
}

impl LiveSpaces {
    pub(crate) fn here(here: crate::DiscoveredWorktree) -> Self {
        Self {
            here,
            others: std::collections::BTreeMap::new(),
        }
    }

    /// Reads each listed workspace once. An empty list issues no Herdr
    /// command, so work that stays in one workspace reads exactly what it did.
    pub(crate) fn load<E: crate::HerdrCommandExecutor>(
        herdr: &E,
        here: crate::DiscoveredWorktree,
        spaces: &[SpaceId],
    ) -> Self {
        let others = spaces
            .iter()
            .filter(|space| space.as_str() != here.workspace_id)
            .map(|space| {
                let discovered = crate::discover_worktree_for_workspace(herdr, space.as_str()).ok();
                (space.clone(), discovered)
            })
            .collect();
        Self { here, others }
    }

    pub(crate) fn with_here(&self, here: crate::DiscoveredWorktree) -> Self {
        Self {
            here,
            others: self.others.clone(),
        }
    }

    pub(crate) fn local(&self) -> &crate::DiscoveredWorktree {
        &self.here
    }

    pub(crate) fn of(&self, space: &SpaceId) -> Option<&crate::DiscoveredWorktree> {
        if space.as_str() == self.here.workspace_id {
            return Some(&self.here);
        }
        self.others.get(space).and_then(Option::as_ref)
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
    /// Remote participants a reply names, pinned to the envoys that stand for
    /// them.
    Participants(Vec<RoutingAddressSnapshot>),
    /// Local participants a reply names by the alias each had in the answered
    /// message, checked again when the reply is written.
    Local(Vec<RoutingAddressSnapshot>),
}

impl RoutePlan {
    pub(crate) fn crosses(&self, here: &SpaceId) -> bool {
        self.legs.iter().any(|leg| match leg {
            RouteLeg::Names { space, .. } => space != here,
            RouteLeg::Participants(participants) => !participants.is_empty(),
            RouteLeg::Local(_) => false,
        })
    }

    pub(crate) fn remote_spaces(
        &self,
        transaction: &crate::StateTransaction<'_>,
        namespace: &StateNamespace,
    ) -> Result<Vec<SpaceId>, crate::StoreError> {
        let mut spaces = Vec::new();
        for leg in &self.legs {
            match leg {
                RouteLeg::Names { space, .. } => spaces.push(space.clone()),
                RouteLeg::Participants(participants) => {
                    for participant in participants {
                        if let Some(origin) =
                            transaction.envoy_origin(namespace, &participant.address_id)?
                        {
                            spaces.push(origin.space);
                        }
                    }
                }
                RouteLeg::Local(_) => {}
            }
        }
        spaces.retain(|space| space != &namespace.space_id);
        spaces.sort();
        spaces.dedup();
        Ok(spaces)
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
                RouteLeg::Local(participants) => {
                    for participant in participants {
                        recipients.push(local_participant(transaction, namespace, participant)?);
                    }
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
    let (pins, fallback) = store.transaction(|transaction| {
        let Some(message_id) = in_reply_to else {
            return Ok((Vec::new(), None));
        };
        let route = match transaction.message_route(namespace, message_id) {
            Ok(route) => route,
            Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => {
                return Ok((Vec::new(), None));
            }
            Err(error) => return Err(error),
        };
        let fallback = if route.from_namedrop {
            ReplyRetry::Nothing
        } else {
            ReplyRetry::ToSender(route.participants[0].alias.clone())
        };
        let pins = match run.scope {
            SpaceScope::Unqualified => reply_participants(
                transaction,
                namespace,
                author,
                route.participants,
                &run.aliases,
            )?,
            _ => Vec::new(),
        };
        Ok((pins, Some(fallback)))
    })?;
    let remaining = RecipientRun {
        scope: run.scope.clone(),
        aliases: run
            .aliases
            .iter()
            .filter(|alias| !pins.iter().any(|(pinned, _)| pinned == *alias))
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
        let plan = plan_route(&remaining, &directory).map_err(|refusal| {
            SendError::Refused(match (refusal, fallback) {
                (
                    RouteRefusal::Ambiguous { choices, .. }
                    | RouteRefusal::NotHere { names: choices },
                    Some(fallback),
                ) => RouteRefusal::UnplacedReply {
                    retry: if run
                        .aliases
                        .iter()
                        .all(|alias| choices.iter().any(|choice| &choice.alias == alias))
                    {
                        fallback
                    } else {
                        ReplyRetry::WithoutThem
                    },
                    choices,
                },
                (refusal, _) => refusal,
            })
        })?;
        refuse_remote_terminals(store, namespace, &remaining.scope, &plan)?;
        plan
    };
    let (mut remote, mut local) = (Vec::new(), Vec::new());
    for (_, pin) in pins {
        match pin {
            Pin::Envoy(participant) => remote.push(participant),
            Pin::Local(participant) => local.push(participant),
            Pin::Author => {}
        }
    }
    if !local.is_empty() {
        plan.legs.insert(0, RouteLeg::Local(local));
    }
    if !remote.is_empty() {
        plan.legs.insert(0, RouteLeg::Participants(remote));
    }
    Ok(plan)
}

/// Says before any write why a remote terminal will not run the command, so
/// the author can fix every member at once. Forwarding enforces the same rule.
fn refuse_remote_terminals(
    store: &mut crate::StateStore,
    namespace: &StateNamespace,
    scope: &SpaceScope,
    plan: &RoutePlan,
) -> Result<(), SendError> {
    let mut unshared = Vec::new();
    for leg in &plan.legs {
        let RouteLeg::Names {
            space,
            label,
            aliases,
        } = leg
        else {
            continue;
        };
        if space == &namespace.space_id {
            continue;
        }
        let there = StateNamespace::new(namespace.session_socket_id.clone(), space.clone());
        let terminals = store.transaction(|transaction| {
            // A name that does not resolve fails later with its own error.
            let members = match transaction.resolve_recipient_aliases(&there, aliases) {
                Ok(members) => members,
                Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => {
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            };
            let mut terminals = Vec::new();
            for member in members {
                if transaction.pane_type(&there, &member.address_id)? == crate::PaneType::Terminal {
                    let grant = transaction.remote_command_grant(&there, &member.address_id)?;
                    terminals.push((member.alias, grant.is_some()));
                }
            }
            Ok(terminals)
        })?;
        if !matches!(scope, SpaceScope::Named(_))
            && let Some((alias, _)) = terminals.first()
        {
            return Err(SendError::Refused(RouteRefusal::TerminalNeedsWorkspace {
                alias: alias.clone(),
                label: label.clone(),
            }));
        }
        unshared.extend(
            terminals
                .into_iter()
                .filter(|(_, shared)| !shared)
                .map(|(alias, _)| (alias, label.clone())),
        );
    }
    if unshared.is_empty() {
        Ok(())
    } else {
        Err(SendError::Refused(RouteRefusal::TerminalNotShared {
            terminals: unshared,
        }))
    }
}

/// Where a written Mention that names a conversation participant goes.
enum Pin {
    Envoy(RoutingAddressSnapshot),
    Local(RoutingAddressSnapshot),
    /// The replying agent itself, which a message never addresses.
    Author,
}

/// The participants a reply names. A written Mention matches the name each
/// participant had in the message being answered and reaches only that
/// participant, so a later rename on either side cannot turn a reply toward
/// someone else, and such a name never enters the cross-workspace search.
fn reply_participants(
    transaction: &crate::StateTransaction<'_>,
    namespace: &StateNamespace,
    author: &crate::PaneAddressId,
    participants: Vec<RoutingAddressSnapshot>,
    aliases: &[String],
) -> Result<Vec<(String, Pin)>, crate::StoreError> {
    let mut pins = Vec::new();
    for alias in aliases {
        let mut named: Vec<&RoutingAddressSnapshot> = participants
            .iter()
            .filter(|participant| &participant.alias == alias)
            .collect();
        if named.len() > 1 {
            named.retain(|participant| &participant.address_id != author);
        }
        let pin = match named.as_slice() {
            [] => continue,
            [participant] if &participant.address_id == author => Pin::Author,
            [participant] if transaction.is_envoy(namespace, &participant.address_id)? => {
                Pin::Envoy(RoutingAddressSnapshot {
                    pane_binding_id: None,
                    agent_session_id: None,
                    ..(*participant).clone()
                })
            }
            [participant] => {
                local_participant(transaction, namespace, participant)?;
                Pin::Local((*participant).clone())
            }
            _ => {
                return Err(crate::StoreError::invalid_input(format!(
                    "@{alias} names more than one participant of this conversation; prefix the \
                     reply with the workspace you mean"
                )));
            }
        };
        pins.push((alias.clone(), pin));
    }
    Ok(pins)
}

fn local_participant(
    transaction: &crate::StateTransaction<'_>,
    namespace: &StateNamespace,
    participant: &RoutingAddressSnapshot,
) -> Result<RoutingAddressSnapshot, crate::StoreError> {
    let alias = &participant.alias;
    if transaction.is_operator_address(namespace, &participant.address_id)? {
        return transaction.routing_address_by_id(namespace, &participant.address_id);
    }
    if transaction.alias_holder(namespace, alias)?.as_ref() == Some(&participant.address_id) {
        return transaction.resolve_routing_alias(namespace, alias);
    }
    let now =
        match transaction.optional_routing_address_by_id(namespace, &participant.address_id)? {
            Some(current) => format!("that participant is now @{}", current.alias),
            None => "that participant has no name now".to_owned(),
        };
    Err(crate::StoreError::invalid_input(format!(
        "@{alias} in this conversation no longer reaches the participant it named; {now}; \
         nothing was sent and the reply's delivery attempt stays open"
    )))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RouteRefusal {
    UnknownMention {
        alias: String,
    },
    /// Bare names this workspace lacks but others have. A bare name never
    /// leaves its workspace, so each comes back with the `#label` tokens that
    /// reach it and nothing is sent.
    NotHere {
        names: Vec<UnplacedName>,
    },
    /// Some `#repo` names have two or more worktree homes. `settled` holds
    /// every name that resolved, as the token that resends it to the same
    /// workspace, so the resend after the person picks keeps them where they
    /// were.
    Ambiguous {
        settled: Vec<ScopedName>,
        choices: Vec<UnplacedName>,
    },
    /// A reply with names it cannot place, which keeps the thread and settles
    /// the attempt when it is retried as `retry` says.
    UnplacedReply {
        choices: Vec<UnplacedName>,
        retry: ReplyRetry,
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
    /// A `#` scope with no Mention after it. `names` lists everyone a send to
    /// that scope would reach, so the person can say who they meant.
    NoRecipient {
        scope: SpaceScope,
        names: Vec<ScopedName>,
    },
    TerminalNeedsWorkspace {
        alias: String,
        label: WorkspaceLabel,
    },
    TerminalNotShared {
        terminals: Vec<(String, WorkspaceLabel)>,
    },
}

/// How a reply refused for names it cannot place is retried.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReplyRetry {
    /// Other names remain once the unplaced ones are removed.
    WithoutThem,
    /// No name remains, so the reply goes back to the answered message's sender.
    ToSender(String),
    /// No name remains, and Namedrop itself sent the answered message.
    Nothing,
}

/// One name in one workspace, written as the exact tokens that resend to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopedName {
    pub workspace: WorkspaceLabel,
    pub alias: String,
}

impl fmt::Display for ScopedName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} @{}", self.workspace, self.alias)
    }
}

fn write_choices(formatter: &mut fmt::Formatter<'_>, choices: &[UnplacedName]) -> fmt::Result {
    let described: Vec<String> = choices.iter().map(UnplacedName::describe).collect();
    formatter.write_str(&described.join("; "))
}

pub(crate) fn listed(names: &[ScopedName]) -> String {
    names
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for SpaceScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unqualified => formatter.write_str("this workspace"),
            Self::Named(labels) => formatter.write_str(&joined(labels, " ")),
            Self::All => formatter.write_str("#all"),
        }
    }
}

/// A name the route could not place in one workspace, with the workspaces
/// that have it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UnplacedName {
    pub alias: String,
    pub labels: Vec<WorkspaceLabel>,
    /// The `#repo` token typed before the name, when two or more of that
    /// repository's worktrees have it. `None` for a bare name this workspace
    /// lacks.
    pub within: Option<WorkspaceLabel>,
}

impl UnplacedName {
    pub(crate) fn describe(&self) -> String {
        match &self.within {
            None => {
                let alias = &self.alias;
                let tokens: Vec<String> = self
                    .labels
                    .iter()
                    .map(|label| format!("{label} @{alias}"))
                    .collect();
                format!(
                    "@{alias} is not named in this workspace. It is named in: {}. Use {} to \
                     reach it",
                    joined(&self.labels, ", "),
                    tokens.join(" or ")
                )
            }
            Some(repo) => format!(
                "@{} is named in more than one {} worktree: {}",
                self.alias,
                repo.as_str(),
                joined(&self.labels, ", ")
            ),
        }
    }
}

impl fmt::Display for RouteRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMention { alias } => write!(formatter, "unknown Mention: @{alias}"),
            Self::NotHere { names } => {
                for name in names {
                    write!(formatter, "{}. ", name.describe())?;
                }
                formatter.write_str("Nothing was sent.")
            }
            Self::Ambiguous { settled, choices } => {
                write_choices(formatter, choices)?;
                if !settled.is_empty() {
                    write!(formatter, "; already resolved: {}", listed(settled))?;
                }
                formatter.write_str(
                    "; ask which workspace is meant, then send once per workspace, never with \
                     #all, each source being that workspace's #label, every @name bound there \
                     (the resolved names that are there and the names chosen there), a space, \
                     and the message text",
                )
            }
            Self::UnplacedReply { choices, retry } => {
                write_choices(formatter, choices)?;
                let dropped = choices
                    .iter()
                    .map(|choice| format!("@{}", choice.alias))
                    .collect::<Vec<_>>()
                    .join(" ");
                formatter.write_str(
                    "; nothing was sent and the reply's delivery attempt stays open; ",
                )?;
                let rerun = "run reply again with the same in_reply_to, attempt_id and \
                             work_outcome and every other input unchanged except the source, ";
                match retry {
                    ReplyRetry::WithoutThem => write!(
                        formatter,
                        "{rerun}which is this source with {dropped} removed and everything \
                         else, #scope tokens and the other names, kept as written; then "
                    )?,
                    ReplyRetry::ToSender(sender) => write!(
                        formatter,
                        "{rerun}which is @{sender}, a space, and the message text, because \
                         removing {dropped} leaves no one to address; then "
                    )?,
                    ReplyRetry::Nothing => write!(
                        formatter,
                        "this delivery takes no reply to its sender, so send to {dropped} \
                         separately instead; "
                    )?,
                }
                write!(
                    formatter,
                    "tell the person the reply left out {dropped}, ask which workspace is \
                     meant, and only after they pick send once per chosen workspace, never with \
                     #all, each source being that workspace's #label, the names chosen there, a \
                     space, and the message text"
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
            Self::NoRecipient { scope, names } if names.is_empty() => {
                write!(formatter, "{scope} has no named agents; nothing was sent")
            }
            Self::NoRecipient { scope, names } => write!(
                formatter,
                "message must begin with one or more recipient Mentions; {scope} has these named \
                 agents: {}; start the source with the #label @name you mean",
                listed(names)
            ),
            Self::TerminalNeedsWorkspace { alias, label } => write!(
                formatter,
                "@{alias} in {label} is a named terminal; put {label} before the mention to run \
                 a command there, as in {label} @{alias} <command>"
            ),
            Self::TerminalNotShared { terminals } => write!(
                formatter,
                "{} {} a named terminal that takes commands only from its own workspace; a \
                 person there can allow others with `namedrop address allow-remote`",
                terminals
                    .iter()
                    .map(|(alias, label)| format!("@{alias} in {label}"))
                    .collect::<Vec<_>>()
                    .join(" and "),
                if terminals.len() == 1 {
                    "is"
                } else {
                    "are each"
                }
            ),
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

/// Every name a send to `scope` would accept, by workspace, each as the token
/// that reaches it alone. The same scope rule as [`plan_route`] decides which
/// workspaces count: a label's exact match and its repository's worktrees.
pub(crate) fn names_in_scope(
    scope: &SpaceScope,
    directory: &SpaceDirectory,
) -> Result<Vec<ScopedName>, RouteRefusal> {
    let mut reached: Vec<&SpaceEntry> = Vec::new();
    match scope {
        SpaceScope::Unqualified => reached.push(directory.here()),
        SpaceScope::Named(labels) => {
            for label in labels {
                let scope = directory.scope(label)?;
                if scope.exact.is_none() && scope.family.is_empty() {
                    return Err(RouteRefusal::NotActivated {
                        label: label.clone(),
                    });
                }
                for entry in scope.exact.into_iter().chain(scope.family) {
                    if !reached.iter().any(|seen| seen.space == entry.space) {
                        reached.push(entry);
                    }
                }
            }
        }
        SpaceScope::All => reached.extend(directory.entries.iter().filter(|entry| entry.activated)),
    }
    Ok(reached
        .into_iter()
        .flat_map(|entry| {
            let workspace = directory.token(entry);
            entry.names.iter().map(move |alias| ScopedName {
                workspace: workspace.clone(),
                alias: alias.clone(),
            })
        })
        .collect())
}

pub(crate) fn plan_route(
    run: &RecipientRun,
    directory: &SpaceDirectory,
) -> Result<RoutePlan, RouteRefusal> {
    let mut targets: Vec<(&SpaceEntry, &String)> = Vec::new();
    let mut ambiguous = Vec::new();
    match &run.scope {
        SpaceScope::Unqualified => {
            let here = directory.here();
            let mut elsewhere = Vec::new();
            for alias in &run.aliases {
                if here.answers_to(alias) {
                    targets.push((here, alias));
                    continue;
                }
                let labels: Vec<WorkspaceLabel> = directory
                    .others()
                    .filter(|entry| entry.activated && entry.names.contains(alias))
                    .map(|home| directory.token(home))
                    .collect();
                if labels.is_empty() {
                    return Err(RouteRefusal::UnknownMention {
                        alias: alias.clone(),
                    });
                }
                elsewhere.push(UnplacedName {
                    alias: alias.clone(),
                    labels,
                    within: None,
                });
            }
            if !elsewhere.is_empty() {
                return Err(RouteRefusal::NotHere { names: elsewhere });
            }
        }
        SpaceScope::Named(labels) => {
            let scopes = labels
                .iter()
                .map(|label| directory.scope(label))
                .collect::<Result<Vec<_>, _>>()?;
            let mut reached = vec![false; scopes.len()];
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
                        homes => ambiguous.push(UnplacedName {
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
    if !ambiguous.is_empty() {
        return Err(RouteRefusal::Ambiguous {
            settled: targets
                .iter()
                .map(|(entry, alias)| ScopedName {
                    workspace: directory.token(entry),
                    alias: (*alias).clone(),
                })
                .collect(),
            choices: ambiguous,
        });
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

    struct NoHerdr;
    impl crate::HerdrCommandExecutor for NoHerdr {
        fn execute(&self, args: &[&str]) -> Result<crate::HerdrCommandOutput, String> {
            panic!("no Herdr command was expected: {args:?}")
        }
    }

    #[test]
    fn live_spaces_for_one_workspace_read_nothing_from_herdr() {
        let here = crate::DiscoveredWorktree {
            worktree_id: crate::WorktreeId::new("herdr-space:w1"),
            workspace_id: "w1".to_owned(),
            worktree_attached: false,
            label: None,
            branch: None,
            panes: Vec::new(),
        };
        let live = LiveSpaces::load(&NoHerdr, here.clone(), &[SpaceId::new("w1")]);
        assert_eq!(live.of(&SpaceId::new("w1")), Some(&here));
        assert_eq!(
            live.of(&SpaceId::new("w2")),
            None,
            "an unread workspace is unknown"
        );
        LiveSpaces::load(&NoHerdr, here, &[]);
    }

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
                RouteLeg::Participants(_) | RouteLeg::Local(_) => {
                    unreachable!("planning resolves names only")
                }
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
        let not_here = |names: &[(&str, &[&str])]| {
            Err(RouteRefusal::NotHere {
                names: names
                    .iter()
                    .map(|(alias, labels)| UnplacedName {
                        alias: (*alias).to_owned(),
                        labels: labels.iter().map(|token| label(token)).collect(),
                        within: None,
                    })
                    .collect(),
            })
        };
        assert_eq!(
            plan(&["tester"]),
            not_here(&[("tester", &["my-space"])]),
            "one remote home is named, never reached"
        );
        assert_eq!(
            plan(&["reviewers"]),
            not_here(&[("reviewers", &["my-space"])]),
            "a group alias follows the same table"
        );
        assert_eq!(plan(&["ops"]), not_here(&[("ops", &["my-space", "ws3"])]));
        assert_eq!(
            plan(&["ops", "lead", "tester"]),
            not_here(&[("ops", &["my-space", "ws3"]), ("tester", &["my-space"])]),
            "a local name does not let the others through"
        );
        assert_eq!(
            plan(&["ops", "nobody"]),
            Err(RouteRefusal::UnknownMention {
                alias: "nobody".to_owned()
            }),
            "an unknown name wins over names found elsewhere"
        );
        assert_eq!(
            plan(&["ghost"]),
            Err(RouteRefusal::UnknownMention {
                alias: "ghost".to_owned()
            }),
            "an inactive workspace is not a home"
        );
        assert_eq!(
            plan(&["ops", "nobody"]).unwrap_err(),
            plan(&["nobody", "ops"]).unwrap_err(),
            "an unknown name wins in any order"
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
                settled: vec![
                    ScopedName {
                        workspace: label("catchup-again"),
                        alias: "claude".to_owned(),
                    },
                    ScopedName {
                        workspace: label("other"),
                        alias: "pi".to_owned(),
                    },
                ],
                choices: vec![UnplacedName {
                    alias: "codex".to_owned(),
                    labels: vec![label("catchup-again"), label("other")],
                    within: Some(label("namestack")),
                }],
            }),
            "two homes ask, an inactive worktree is not a home, and the names that resolved come \
             with the question"
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
            &run(
                SpaceScope::Named(vec![label("namestack")]),
                &["codex", "claude"],
            ),
            &repositories(),
        )
        .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "@codex is named in more than one namestack worktree: #catchup-again, #other; already \
             resolved: #catchup-again @claude; ask which workspace is meant, then send once per \
             workspace, never with #all, each source being that workspace's #label, every @name \
             bound there (the resolved names that are there and the names chosen there), a \
             space, and the message text"
        );
    }

    fn scoped(pairs: &[(&str, &str)]) -> Vec<ScopedName> {
        pairs
            .iter()
            .map(|(workspace, alias)| ScopedName {
                workspace: label(workspace),
                alias: (*alias).to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_scope_without_a_mention_offers_every_name_it_reaches() {
        let named = |directory: &SpaceDirectory, labels: &[&str]| {
            names_in_scope(
                &SpaceScope::Named(labels.iter().map(|token| label(token)).collect()),
                directory,
            )
        };
        let workspaces = directory();
        assert_eq!(
            named(&workspaces, &["ws3"]),
            Ok(scoped(&[("ws3", "lead"), ("ws3", "ops")])),
            "an exact workspace offers each of its names"
        );
        let repositories = repositories();
        assert_eq!(
            named(&repositories, &["namestack"]),
            Ok(scoped(&[
                ("catchup-again", "claude"),
                ("catchup-again", "codex"),
                ("other", "codex"),
                ("other", "pi"),
            ])),
            "a repository offers each activated worktree's names under that worktree's token"
        );
        assert_eq!(
            named(&repositories, &["namestack", "catchup-again"]),
            named(&repositories, &["namestack"]),
            "a workspace reached twice is offered once"
        );
        assert_eq!(
            names_in_scope(&SpaceScope::All, &workspaces),
            Ok(scoped(&[
                ("ws1", "lead"),
                ("my-space", "lead"),
                ("my-space", "old"),
                ("my-space", "ops"),
                ("my-space", "reviewers"),
                ("my-space", "tester"),
                ("ws3", "lead"),
                ("ws3", "ops"),
            ])),
            "#all offers every activated workspace's names"
        );
        assert_eq!(named(&repositories, &["solo-feature"]), Ok(Vec::new()));
        assert_eq!(
            RouteRefusal::NoRecipient {
                scope: SpaceScope::Named(vec![label("solo-feature")]),
                names: Vec::new(),
            }
            .to_string(),
            "#solo-feature has no named agents; nothing was sent"
        );
        assert_eq!(
            named(&workspaces, &["dormant"]),
            Err(RouteRefusal::NotActivated {
                label: label("dormant")
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
