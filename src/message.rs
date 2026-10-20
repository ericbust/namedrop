use crate::{
    ActorKind, ContextGrantDraft, ContextGrantId, ContextReferenceDraft, ContextSnapshotId,
    DeliveryId, LifecycleRuntime, MessageDraft, MessageRecipientDraft, MutationAttribution,
    PaneAddressLifecycle, PaneAddressRecord, StateNamespace, StateStore, ThreadId,
    context::{MessageContextInput, prepare_message_context, record_snapshot_activation_failure},
    discover_worktree, discover_worktree_for_workspace, validate_alias,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MISSING_RECIPIENTS: &str = "message must begin with one or more recipient Mentions";

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
    /// The token after `#`, when it is label-shaped. `all` is reserved: it is
    /// [`SpaceScope::All`], never a label.
    pub fn from_token(token: &str) -> Option<Self> {
        if token.is_empty() || !token.bytes().all(is_label_token_byte) {
            return None;
        }
        let label = Self::fold(token);
        (label.0 != "all").then_some(label)
    }

    /// A label as Herdr reports it, which may hold spaces, capitals, or anything else.
    pub fn from_herdr(label: &str) -> Self {
        Self::fold(label)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn fold(label: &str) -> Self {
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
}

impl std::fmt::Display for WorkspaceLabel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "#{}", self.0)
    }
}

/// Which workspaces a leading run of Mentions addresses. One scope applies to
/// the whole run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpaceScope {
    /// No `#` tokens: each name resolves here first.
    Unqualified,
    /// `#a #b ...`, deduplicated, in written order.
    Named(Vec<WorkspaceLabel>),
    /// `#all`, which cannot be combined with named labels.
    All,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipientRun {
    pub scope: SpaceScope,
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMessage {
    pub recipients: RecipientRun,
    pub body: String,
    pub context_reference_aliases: Vec<String>,
}

/// Grammar: `[ws] ("#" label ws)* ("@" alias ws)+ body`. A `#` run counts only
/// when at least one valid Mention follows it; otherwise the source addresses
/// nobody and stays literal. A `#` token after the first Mention is body text.
pub fn parse_mentions(source: &str) -> Result<ParsedMessage, String> {
    let mut cursor = skip_whitespace(source, 0);
    let mut labels: Vec<WorkspaceLabel> = Vec::new();
    let mut all = false;
    loop {
        let token_end = token_end(source, cursor);
        let Some(token) = source[cursor..token_end].strip_prefix('#') else {
            break;
        };
        if token.eq_ignore_ascii_case("all") {
            all = true;
        } else if let Some(label) = WorkspaceLabel::from_token(token) {
            if !labels.contains(&label) {
                labels.push(label);
            }
        } else {
            break;
        }
        cursor = skip_whitespace(source, token_end);
    }

    let mut recipients = Vec::new();
    loop {
        cursor = skip_whitespace(source, cursor);
        let token_end = token_end(source, cursor);
        let token = &source[cursor..token_end];
        let Some(alias) = token.strip_prefix('@') else {
            break;
        };
        if alias.is_empty() || validate_alias(alias).is_err() {
            break;
        }
        if !recipients.iter().any(|existing| existing == alias) {
            recipients.push(alias.to_owned());
        }
        cursor = token_end;
    }

    if recipients.is_empty() {
        return Err(MISSING_RECIPIENTS.to_owned());
    }
    let scope = match (all, labels.is_empty()) {
        (false, true) => SpaceScope::Unqualified,
        (false, false) => SpaceScope::Named(labels),
        (true, true) => SpaceScope::All,
        (true, false) => {
            return Err(
                "#all already addresses every workspace, so it cannot be combined with \
                 workspace labels"
                    .to_owned(),
            );
        }
    };
    cursor = skip_whitespace(source, cursor);
    let body = &source[cursor..];
    if body.trim().is_empty() {
        return Err("collaboration message body is required".to_owned());
    }

    Ok(ParsedMessage {
        recipients: RecipientRun {
            scope,
            aliases: recipients,
        },
        body: body.to_owned(),
        context_reference_aliases: context_mentions(body),
    })
}

/// True when `source` opens with at least one column-zero recipient Mention,
/// whether or not the rest of it is a valid message. Only this grammar knows
/// the difference between "addresses nobody" and "addresses someone but is
/// otherwise malformed", and the prompt hook needs it: text that addresses
/// nobody is literal and must be left alone, while a Mention whose message is
/// malformed is a real handoff whose failure the person should hear about.
pub fn carries_leading_mentions(source: &str) -> bool {
    starts_at_column_zero(source)
        && parse_mentions(source).map_or_else(
            |error| error != MISSING_RECIPIENTS,
            |parsed| !parsed.recipients.aliases.is_empty(),
        )
}

/// Parses the native-prompt shorthand. Only a column-zero run of leading
/// Mentions supplies recipients; the complete prompt remains the
/// collaborator-authored body so the direct relay is verbatim. Resolution still
/// happens atomically in the routing transaction, so an unknown alias makes the
/// hook fail open without a partial send.
pub fn parse_prompt_mentions(source: &str) -> Result<ParsedMessage, String> {
    if !starts_at_column_zero(source) {
        return Err(MISSING_RECIPIENTS.to_owned());
    }
    let recipients = parse_mentions(source)?.recipients;
    Ok(ParsedMessage {
        recipients,
        body: source.to_owned(),
        context_reference_aliases: Vec::new(),
    })
}

fn starts_at_column_zero(source: &str) -> bool {
    source.starts_with('@') || source.starts_with('#')
}

/// What sending will do for one recipient. "Send" is not "deliver": a delivery
/// waits until the recipient's Agent Session is observed settled, so the wait
/// equals however long that session stays busy.
///
/// This is presentation over the live address projection. It changes no delivery
/// state transition, no dispatch ordering, no settled-observation rule, and no
/// lease; it reports what those rules already imply for each recipient.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecipientOutlook {
    pub alias: String,
    /// The Agent Session status the projection reports, verbatim. Absent when no
    /// session has been observed for the address since its pane was bound.
    pub agent_status: Option<String>,
    /// The address situation the projection reports.
    pub lifecycle: PaneAddressLifecycle,
    /// The consequence of sending, worded as the person sending should read it.
    pub statement: String,
}

/// State what sending will do for each recipient, in the order the Mentions were
/// written. An alias the projection does not list is left out: it does not
/// resolve, so the send rejects it and hands the draft back with the problem
/// named.
pub fn recipient_outlooks(
    aliases: &[String],
    addresses: &[PaneAddressRecord],
) -> Vec<RecipientOutlook> {
    aliases
        .iter()
        .filter_map(|alias| addresses.iter().find(|address| &address.alias == alias))
        .map(recipient_outlook)
        .collect()
}

fn stored_recipient_outlooks(
    transaction: &crate::StateTransaction<'_>,
    namespace: &StateNamespace,
    aliases: &[String],
) -> Result<Vec<RecipientOutlook>, crate::StoreError> {
    let addresses = transaction.list_pane_addresses(namespace)?;
    let held = transaction.held_deliveries(namespace)?;
    let mut outlooks = recipient_outlooks(aliases, &addresses);
    for outlook in &mut outlooks {
        if let Some(delivery) = addresses
            .iter()
            .find(|address| address.alias == outlook.alias)
            .and_then(|address| {
                held.iter()
                    .find(|delivery| delivery.address_id == address.id)
            })
        {
            outlook.statement = format!(
                "@{}: an earlier held delivery ({}) blocks this mailbox. Queued deliveries wait until that hold is explicitly resolved; run namedrop doctor for recovery.",
                outlook.alias,
                delivery.delivery_id.as_str(),
            );
        }
    }
    Ok(outlooks)
}

fn recipient_outlook(address: &PaneAddressRecord) -> RecipientOutlook {
    let alias = &address.alias;
    let statement = if address.pane_type == crate::PaneType::Terminal && address.pane_id.is_some() {
        format!(
            "@{alias}: terminal command; executes when its shell is available and returns output and exit status."
        )
    } else {
        match address.lifecycle {
            PaneAddressLifecycle::Occupied => {
                // Dispatch waits for a Settled Observation, and only "idle" and
                // "done" are settled. Every other status keeps the message queued.
                let status = address.agent_status.as_deref().unwrap_or("unknown");
                if matches!(status, "idle" | "done") {
                    format!(
                        "@{alias}: Agent Session status {status} — settled, so delivery is imminent."
                    )
                } else {
                    format!(
                        "@{alias}: Agent Session status {status} — this message stays queued until that Agent Session settles."
                    )
                }
            }
            PaneAddressLifecycle::Empty => format!(
                "@{alias}: no Agent Session in the bound pane — this message queues normally with nobody there to receive it."
            ),
            // The projection never lists a retired address and a retired Mention
            // never resolves; it shares the wording because neither has a pane.
            PaneAddressLifecycle::Unbound | PaneAddressLifecycle::Retired => format!(
                "@{alias}: no bound pane — this message queues normally with nobody there to receive it."
            ),
        }
    };
    RecipientOutlook {
        alias: alias.clone(),
        agent_status: address.agent_status.clone(),
        lifecycle: address.lifecycle,
        statement,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendInput {
    request_id: String,
    source: String,
    #[serde(default = "human_actor")]
    author_kind: ActorKind,
    context: Option<MessageContextInput>,
    #[serde(flatten)]
    tracking: crate::coordination::TrackingInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyInput {
    request_id: String,
    source: String,
    in_reply_to: String,
    #[serde(default)]
    reply_all: bool,
    #[serde(default = "human_actor")]
    author_kind: ActorKind,
    attempt_id: Option<String>,
    context: Option<MessageContextInput>,
    /// Ask the disposed delivery's sender for its working context: the reply
    /// transaction creates a pending Context Grant whose id is the request.
    #[serde(default)]
    request_context: bool,
    /// Settle a pending context request by refusal, naming its grant id.
    decline_context_request: Option<String>,
    #[serde(flatten)]
    tracking: crate::coordination::TrackingInput,
}

fn human_actor() -> ActorKind {
    ActorKind::Human
}

struct MessageOperationInput {
    request_id: String,
    source: String,
    author_kind: ActorKind,
    in_reply_to: Option<crate::MessageId>,
    disposition_attempt_id: Option<crate::DeliveryAttemptId>,
    context: Option<MessageContextInput>,
    request_context: bool,
    decline_context_request: Option<ContextGrantId>,
    tracking: crate::coordination::TrackingInput,
}

struct PreparedMessageOperation {
    request_id: String,
    authored_source: String,
    parsed: ParsedMessage,
    author_kind: ActorKind,
    in_reply_to: Option<crate::MessageId>,
    disposition_attempt_id: Option<crate::DeliveryAttemptId>,
    context: Option<MessageContextInput>,
    request_context: bool,
    decline_context_request: Option<ContextGrantId>,
    tracking: crate::coordination::TrackingInput,
    grammar_version: &'static str,
}

struct RelayPresentation {
    pane_id: String,
    sender_alias: String,
    body: String,
}

pub fn validate_message_input(operation: &str, input: Value) -> Result<(), String> {
    let input = parse_operation_input(operation, input)?;
    parse_mentions(&input.source)?;
    Ok(())
}

pub fn execute_message_operation<E, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let mut response = persist_message_operation(operation, input, runtime)?;
    response["drain"] = crate::drain_receipt(runtime);
    Ok(response)
}

/// Commits one send or reply without waiting for best-effort delivery. Runtime
/// hooks use this seam to avoid the known post-commit mailbox-drain timeout
/// window before the hook emits its blocking decision. The normal operation
/// path above immediately follows the same commit with a mailbox drain.
pub(crate) fn persist_message_operation<E, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let input = prepare_source_message(parse_operation_input(operation, input)?)?;
    persist_prepared_message_operation(input, runtime)
}

/// Commits the verbatim leading-Mention shorthand used by native prompt hooks.
pub(crate) fn persist_prompt_mention_operation<E, C, I>(
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let input = parse_operation_input("send", input)?;
    if input.context.is_some() {
        // The native @alias hook shorthand keeps its current semantics: context
        // handoff belongs to the explicit agent-authored send and reply only.
        return Err("a prompt Mention cannot carry a context block".to_owned());
    }
    let parsed = parse_prompt_mentions(&input.source)?;
    let prepared = PreparedMessageOperation {
        request_id: input.request_id,
        authored_source: input.source,
        parsed,
        author_kind: input.author_kind,
        in_reply_to: input.in_reply_to,
        disposition_attempt_id: input.disposition_attempt_id,
        context: None,
        request_context: false,
        decline_context_request: None,
        tracking: input.tracking,
        grammar_version: "mention-hook/v1",
    };
    persist_prepared_message_operation(prepared, runtime)
}

fn persist_prepared_message_operation<E, C, I>(
    input: PreparedMessageOperation,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let PreparedMessageOperation {
        request_id,
        authored_source,
        parsed,
        author_kind,
        in_reply_to,
        disposition_attempt_id,
        context,
        request_context,
        decline_context_request,
        grammar_version,
        tracking,
    } = input;
    if context.is_some() && author_kind != ActorKind::AgentSession {
        return Err(
            "a context block requires an agent-authored message (author_kind agent_session)"
                .to_owned(),
        );
    }
    if request_context && context.is_some() {
        return Err(
            "a reply cannot both request context and attach it: request_context asks the \
             sender, the context block answers"
                .to_owned(),
        );
    }
    if request_context && decline_context_request.is_some() {
        return Err("a reply cannot both request context and decline a request".to_owned());
    }
    if (request_context || decline_context_request.is_some())
        && author_kind != ActorKind::AgentSession
    {
        return Err(
            "a context request marker requires an agent-authored reply (author_kind \
             agent_session)"
                .to_owned(),
        );
    }
    if parsed.recipients.scope != SpaceScope::Unqualified {
        return Err("workspace-qualified Mentions are not routable yet".to_owned());
    }
    let pane_id = runtime
        .current_pane_id
        .ok_or_else(|| "the current Herdr pane is required to resolve the sender".to_owned())?;
    let discovered = match runtime.workspace_id {
        Some(workspace_id) => discover_worktree_for_workspace(runtime.herdr, workspace_id)?,
        None => discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    let namespace = discovered.namespace(runtime.session_socket_id);
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    // Validation, redaction, and the optional git pin all happen before the
    // routing transaction: a send that fails from here on delivers nothing and
    // leaves at most one orphaned pin ref for the lazy sweep.
    let fulfillment_grant_ids = context
        .as_ref()
        .map(MessageContextInput::fulfillment_grant_ids)
        .unwrap_or_default();
    if !fulfillment_grant_ids.is_empty() && in_reply_to.is_none() {
        return Err(
            "context grant_ids fulfill a context request, so they ride a reply in the \
             requesting thread, never a send"
                .to_owned(),
        );
    }
    let context = match context
        .map(|context| prepare_message_context(context, &discovered, runtime.ids, &mut store))
        .transpose()
    {
        Ok(context) => context,
        Err(error) => {
            record_snapshot_activation_failure(&fulfillment_grant_ids, &error, runtime);
            return Err(error);
        }
    };
    let routed = store.transaction(|transaction| {
        let state = transaction.worktree_state(&namespace)?;
        if !state.is_some_and(|state| state.activated && !state.deactivated) {
            return Err(crate::StoreError::invalid_input(
                "Namedrop is not activated for this space",
            ));
        }
        let author = transaction.routing_author_for_pane(&namespace, pane_id)?;
        if author_kind == ActorKind::AgentSession && author.agent_session_id.is_none() {
            return Err(crate::StoreError::invalid_input(
                "an agent-authored message requires a current Agent Session",
            ));
        }
        if in_reply_to.is_some()
            && author_kind == ActorKind::AgentSession
            && disposition_attempt_id.is_none()
        {
            return Err(crate::StoreError::invalid_input(
                "an agent-authored reply requires the Delivery Attempt id",
            ));
        }
        if author_kind != ActorKind::AgentSession && disposition_attempt_id.is_some() {
            return Err(crate::StoreError::invalid_input(
                "only an Agent Session may complete a Delivery Attempt with a reply",
            ));
        }
        let attribution = MutationAttribution::new(
            author_kind,
            if author_kind == ActorKind::AgentSession {
                author
                    .agent_session_id
                    .as_ref()
                    .expect("Agent Session was checked")
                    .as_str()
            } else {
                "local-human"
            },
            runtime.clock.now_unix_ms(),
        );
        if let Some(replay) = transaction.replay_message(
            &namespace,
            &author.address_id,
            &request_id,
            &authored_source,
            in_reply_to.as_ref(),
            author_kind,
        )? {
            if !transaction.coordination_links(&namespace,&replay.receipt.message_id)?.is_empty() {
                crate::context::registered_current_address(transaction,&namespace,pane_id,&discovered)?;
            }
            transaction.check_coordination_replay(&namespace, &replay.receipt.message_id, &tracking.fingerprint(), &author)?;
            let outlook = stored_recipient_outlooks(
                transaction,
                &namespace,
                &replay
                    .recipients
                    .iter()
                    .map(|r| r.alias.clone())
                    .collect::<Vec<_>>(),
            )?;
            return Ok((replay, outlook, None, None, None, None));
        }
        let reply_route = in_reply_to
            .as_ref()
            .map(|message_id| transaction.message_route(&namespace, message_id))
            .transpose()?;
        let answers_relay_context_request = transaction.answers_report_to_human_context_request(
            &namespace,
            in_reply_to.as_ref(),
            reply_route.as_ref().map(|route| route.reply_mode),
            &author.address_id,
            &fulfillment_grant_ids,
            decline_context_request.as_ref(),
        )?;
        if disposition_attempt_id.is_some()
            && reply_route
                .as_ref()
                .is_some_and(|route| route.reply_mode == crate::DeliveryReplyMode::ReportToHuman)
            && !answers_relay_context_request
        {
            return Err(crate::StoreError::invalid_input(
                "a report_to_human delivery must finish with complete.no-reply unless its \
                     exact context_request is fulfilled or declined",
            ));
        }
        let thread_id = match &reply_route {
            Some(route) => route.thread_id.clone(),
            None => {
                let thread_id = ThreadId::new(runtime.ids.next_id("thread"));
                transaction.create_thread(
                    &namespace,
                    &thread_id,
                    thread_title(&parsed.body),
                    &attribution,
                )?;
                thread_id
            }
        };
        let recipients = transaction.resolve_message_recipients(
            &namespace,
            &parsed.recipients.aliases,
            &author.address_id,
        )?;
        if (in_reply_to.is_some() || context.is_some()) && recipients.iter().any(|r|
            transaction.pane_type(&namespace,&r.address_id).is_ok_and(|kind|kind==crate::PaneType::Terminal)) {
            return Err(crate::StoreError::invalid_input("terminal recipients accept new shell command requests without agent context attachments"));
        }
        let outlook = stored_recipient_outlooks(
            transaction,
            &namespace,
            &recipients
                .iter()
                .map(|r| r.alias.clone())
                .collect::<Vec<_>>(),
        )?;
        let references =
            resolve_aliases(transaction, &namespace, &parsed.context_reference_aliases)?;
        let grammar_version = if disposition_attempt_id.is_some() && answers_relay_context_request {
            // Resume the suspended one-shot human relay. The requester gets
            // one return_to_source delivery, whose answer becomes the final
            // report_to_human leg and cannot reply again.
            "mention-hook/v1"
        } else if disposition_attempt_id.is_some()
            && reply_route
                .as_ref()
                .is_some_and(|route| route.reply_mode == crate::DeliveryReplyMode::ReturnToSource)
        {
            "mention-return/v1"
        } else if author_kind == ActorKind::AgentSession
            && in_reply_to.is_none()
            && grammar_version == "mention/v1"
        {
            // This durable grammar is the explicit provenance for an
            // intent-shaped agent delegation. It prevents ordinary replies or
            // pre-feature messages from being inferred as delegated work.
            "delegated/v1"
        } else {
            grammar_version
        };
        let draft = MessageDraft {
            thread_id,
            message_id: crate::MessageId::new(runtime.ids.next_id("message")),
            in_reply_to,
            author_address_id: author.address_id,
            author_kind,
            author_alias_snapshot: author.alias,
            author_agent_session_snapshot: author.agent_session_id,
            request_id,
            authored_source,
            body: parsed.body,
            grammar_version: grammar_version.to_owned(),
            recipients: recipients
                .iter()
                .map(|recipient| MessageRecipientDraft {
                    address_id: recipient.address_id.clone(),
                    alias_snapshot: recipient.alias.clone(),
                    agent_session_snapshot: recipient.agent_session_id.clone(),
                    delivery_id: DeliveryId::new(runtime.ids.next_id("delivery")),
                })
                .collect(),
            context_references: references
                .iter()
                .map(|reference| ContextReferenceDraft {
                    address_id: reference.address_id.clone(),
                    alias_snapshot: reference.alias.clone(),
                    pane_binding_snapshot: reference.pane_binding_id.clone(),
                    agent_session_snapshot: reference.agent_session_id.clone(),
                })
                .collect(),
        };
        let reply_message_id = draft.message_id.clone();
        let relay = reply_route
            .as_ref()
            .filter(|route| {
                disposition_attempt_id.is_some()
                    && route.reply_mode == crate::DeliveryReplyMode::ReturnToSource
                    && !request_context
            })
            .and_then(|route| route.participants.first())
            .and_then(|source| {
                transaction
                    .active_pane_bindings(&namespace)
                    .ok()?
                    .into_iter()
                    .find(|binding| binding.address_id == source.address_id)
                    .map(|binding| RelayPresentation {
                        pane_id: binding.pane_id,
                        sender_alias: draft.author_alias_snapshot.clone(),
                        body: draft.body.clone(),
                    })
            });
        if let Some(attempt_id) = &disposition_attempt_id {
            transaction.ensure_delivery_attempt_acknowledged(
                &namespace,
                attempt_id,
                draft
                    .author_agent_session_snapshot
                    .as_ref()
                    .expect("agent-authored reply was checked"),
                &attribution,
            )?;
        }
        let receipt = transaction.append_message(&namespace, &draft, &attribution)?;
        crate::coordination::track_message(transaction, &namespace, &draft, &tracking, disposition_attempt_id.as_ref(), &discovered, runtime)?;
        if let Some(attempt_id) = &disposition_attempt_id {
            transaction.record_reply_disposition(
                &namespace,
                attempt_id,
                &reply_message_id,
                draft
                    .author_agent_session_snapshot
                    .as_ref()
                    .expect("agent-authored reply was checked"),
                &attribution,
            )?;
        }
        // A reply's request marker creates the typed request itself: a
        // pending Context Grant naming the disposed delivery's sender as
        // source, recorded against that sender's copy of this reply so its
        // envelope carries the grant id as context_request.
        let context_request_grant = if request_context {
            let route = reply_route.as_ref().ok_or_else(|| {
                crate::StoreError::invalid_input(
                    "request_context is a reply marker; send cannot carry it",
                )
            })?;
            if !matches!(
                route.reply_mode,
                crate::DeliveryReplyMode::AgentThread | crate::DeliveryReplyMode::ReturnToSource
            ) {
                return Err(crate::StoreError::invalid_input(
                    "a context request rides an agent thread or one-shot human relay reply",
                ));
            }
            let caller = route
                .participants
                .first()
                .expect("a routed message always names its author");
            if !recipients
                .iter()
                .any(|recipient| recipient.address_id == caller.address_id)
            {
                return Err(crate::StoreError::invalid_input(format!(
                    "a context request must be addressed to the delivery's sender @{}",
                    caller.alias
                )));
            }
            let current = transaction.routing_address_by_id(&namespace, &caller.address_id)?;
            let binding_id = current.pane_binding_id.clone().ok_or_else(|| {
                crate::StoreError::invalid_input(
                    "the delivery's sender no longer has an active pane binding to \
                         answer a context request",
                )
            })?;
            let caller_session = current.agent_session_id.clone().ok_or_else(|| {
                crate::StoreError::invalid_input(
                    "the delivery's sender no longer has a current Agent Session to \
                         answer a context request",
                )
            })?;
            let grant_id = ContextGrantId::new(runtime.ids.next_id("context-grant"));
            let grant = transaction.create_context_grant(
                &namespace,
                &ContextGrantDraft {
                    id: grant_id.clone(),
                    thread_id: draft.thread_id.clone(),
                    source_address_id: caller.address_id.clone(),
                    grantee_address_id: draft.author_address_id.clone(),
                    pane_binding_id: binding_id,
                    agent_session_id: caller_session,
                },
                &attribution,
            )?;
            transaction.record_message_context_request(
                &namespace,
                &draft.message_id,
                &caller.address_id,
                &grant_id,
            )?;
            Some(grant)
        } else {
            None
        };
        // Decline settles the request before any context block runs, so a
        // reply can never both refuse a grant and activate it.
        let declined_context_request = decline_context_request
            .as_ref()
            .map(|grant_id| {
                let session = draft
                    .author_agent_session_snapshot
                    .as_ref()
                    .expect("a context decline requires an agent-authored reply");
                transaction.decline_context_grant(
                    &namespace,
                    grant_id,
                    &draft.author_address_id,
                    session,
                    &attribution,
                )
            })
            .transpose()?;
        // A context block runs snapshot.publish's flow against the thread
        // and recipients this same transaction routed. Create-mode grants
        // every recipient; activate-mode fulfills exactly the pending
        // grants a request named — or the whole send fails.
        let context_grants = context
            .as_ref()
            .map(|context| {
                let source_session = draft
                    .author_agent_session_snapshot
                    .as_ref()
                    .expect("a context block requires an agent-authored message");
                if context.grant_ids.is_empty() {
                    let binding_id = author.pane_binding_id.clone().ok_or_else(|| {
                        crate::StoreError::invalid_input(
                            "a context block requires an active pane binding",
                        )
                    })?;
                    recipients
                        .iter()
                        .map(|recipient| {
                            let grant_id =
                                ContextGrantId::new(runtime.ids.next_id("context-grant"));
                            transaction.create_context_grant(
                                &namespace,
                                &ContextGrantDraft {
                                    id: grant_id.clone(),
                                    thread_id: draft.thread_id.clone(),
                                    source_address_id: draft.author_address_id.clone(),
                                    grantee_address_id: recipient.address_id.clone(),
                                    pane_binding_id: binding_id.clone(),
                                    agent_session_id: source_session.clone(),
                                },
                                &attribution,
                            )?;
                            let grant = transaction.activate_context_grant(
                                &namespace,
                                &grant_id,
                                &draft.author_address_id,
                                source_session,
                                &ContextSnapshotId::new(
                                    runtime.ids.next_id("working-context-snapshot"),
                                ),
                                &context.payload,
                                &context.evidence,
                                &attribution,
                            )?;
                            transaction.record_message_context_attachment(
                                &namespace,
                                &draft.message_id,
                                &recipient.address_id,
                                &grant_id,
                            )?;
                            Ok(grant)
                        })
                        .collect::<Result<Vec<_>, crate::StoreError>>()
                } else {
                    context
                        .grant_ids
                        .iter()
                        .map(|grant_id| {
                            // Addressing is validated before activation
                            // mutates anything: a fulfillment answers its
                            // request in the requesting thread, delivered
                            // to the requesting grantee.
                            let pending = transaction.context_grant(&namespace, grant_id)?;
                            if pending.thread_id != draft.thread_id {
                                return Err(crate::StoreError::invalid_input(
                                    "a context fulfillment answers its request in the \
                                         same Collaboration Thread",
                                ));
                            }
                            if !recipients
                                .iter()
                                .any(|recipient| recipient.address_id == pending.grantee_address_id)
                            {
                                return Err(crate::StoreError::invalid_input(
                                    "a context fulfillment must be addressed to the \
                                         pending grant's exact grantee",
                                ));
                            }
                            let grant = transaction.activate_context_grant(
                                &namespace,
                                grant_id,
                                &draft.author_address_id,
                                source_session,
                                &ContextSnapshotId::new(
                                    runtime.ids.next_id("working-context-snapshot"),
                                ),
                                &context.payload,
                                &context.evidence,
                                &attribution,
                            )?;
                            transaction.record_message_context_attachment(
                                &namespace,
                                &draft.message_id,
                                &grant.grantee_address_id,
                                grant_id,
                            )?;
                            Ok(grant)
                        })
                        .collect::<Result<Vec<_>, crate::StoreError>>()
                }
            })
            .transpose()?;
        Ok((
            crate::RoutedMessageReceipt {
                receipt,
                recipients,
                context_references: references,
            },
            outlook,
            relay,
            context_grants,
            context_request_grant,
            declined_context_request,
        ))
    });
    let (routed, outlook, relay, context_grants, context_request_grant, declined_context_request) =
        match routed {
            Ok(routed) => routed,
            Err(error) => {
                let error = error.to_string();
                record_snapshot_activation_failure(&fulfillment_grant_ids, &error, runtime);
                return Err(error);
            }
        };

    let links = store
        .transaction(|tx| tx.coordination_links(&namespace, &routed.receipt.message_id))
        .map_err(|e| e.to_string())?;
    let mut response = receipt_json(routed, outlook);
    if !links.is_empty() {
        response["coordination"] = json!(links);
    }
    if let Some(grants) = context_grants {
        response["context_grants"] = json!(grants);
    }
    if let Some(grant) = context_request_grant {
        response["context_request"] = json!(grant);
    }
    if let Some(grant) = declined_context_request {
        response["declined_context_request"] = json!(grant);
    }
    // A reply that finished a Delivery Attempt keeps its whole message receipt and adds the
    // same terminal fields `complete --no-reply` returns, so an Agent Session reads the
    // recorded disposition instead of inferring it.
    if let Some(attempt_id) = &disposition_attempt_id {
        response["attempt_id"] = json!(attempt_id);
        response["disposition"] = json!(crate::DeliveryDisposition::Reply.as_str());
    }
    if let Some(relay) = relay {
        response["relay"] = present_relay_result(&relay, runtime);
    }
    Ok(response)
}

fn present_relay_result<E, C, I>(
    relay: &RelayPresentation,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Value
where
    E: crate::HerdrCommandExecutor,
{
    let title = format!("@{} replied", relay.sender_alias);
    let summary = relay.body.lines().next().unwrap_or_default().trim();
    let summary = summary.chars().take(160).collect::<String>();
    let token = format!("namedrop-relay=@{}: {summary}", relay.sender_alias);
    let notification = runtime.herdr.execute(&[
        "notification",
        "show",
        &title,
        "--body",
        &relay.body,
        "--position",
        "bottom-right",
        "--sound",
        "done",
    ]);
    let metadata = runtime.herdr.execute(&[
        "pane",
        "report-metadata",
        &relay.pane_id,
        "--source",
        "namedrop-relay",
        "--token",
        &token,
        "--ttl-ms",
        "15000",
    ]);
    json!({
        "mode": crate::DeliveryReplyMode::ReturnToSource.as_str(),
        "notification_shown": notification.is_ok(),
        "notification_error": notification.err(),
        "metadata_error": metadata.err(),
    })
}

fn prepare_source_message(
    input: MessageOperationInput,
) -> Result<PreparedMessageOperation, String> {
    let parsed = parse_mentions(&input.source)?;
    Ok(PreparedMessageOperation {
        request_id: input.request_id,
        authored_source: input.source,
        parsed,
        author_kind: input.author_kind,
        in_reply_to: input.in_reply_to,
        disposition_attempt_id: input.disposition_attempt_id,
        context: input.context,
        request_context: input.request_context,
        decline_context_request: input.decline_context_request,
        tracking: input.tracking,
        grammar_version: "mention/v1",
    })
}

fn parse_operation_input(operation: &str, input: Value) -> Result<MessageOperationInput, String> {
    match operation {
        "send" => {
            let input: SendInput =
                serde_json::from_value(input).map_err(crate::input_schema_error)?;
            input.tracking.validate(false)?;
            Ok(MessageOperationInput {
                request_id: required_nonempty(input.request_id, "request_id")?,
                source: input.source,
                author_kind: message_author_kind(input.author_kind)?,
                in_reply_to: None,
                disposition_attempt_id: None,
                context: input.context,
                request_context: false,
                decline_context_request: None,
                tracking: input.tracking,
            })
        }
        "reply" => {
            let input: ReplyInput =
                serde_json::from_value(input).map_err(crate::input_schema_error)?;
            // Reply All selects an editable prefill; the submitted source is the stored route.
            let _ = input.reply_all;
            input.tracking.validate(true)?;
            Ok(MessageOperationInput {
                request_id: required_nonempty(input.request_id, "request_id")?,
                source: input.source,
                author_kind: message_author_kind(input.author_kind)?,
                in_reply_to: Some(crate::MessageId::new(required_nonempty(
                    input.in_reply_to,
                    "in_reply_to",
                )?)),
                disposition_attempt_id: input
                    .attempt_id
                    .map(|attempt_id| {
                        required_nonempty(attempt_id, "attempt_id")
                            .map(crate::DeliveryAttemptId::new)
                    })
                    .transpose()?,
                context: input.context,
                request_context: input.request_context,
                tracking: input.tracking,
                decline_context_request: input
                    .decline_context_request
                    .map(|grant_id| {
                        required_nonempty(grant_id, "decline_context_request")
                            .map(ContextGrantId::new)
                    })
                    .transpose()?,
            })
        }
        _ => Err(format!("{operation} is not a message operation")),
    }
}

fn message_author_kind(author_kind: ActorKind) -> Result<ActorKind, String> {
    if matches!(author_kind, ActorKind::Human | ActorKind::AgentSession) {
        Ok(author_kind)
    } else {
        Err("message author_kind must be human or agent_session".to_owned())
    }
}

fn resolve_aliases(
    transaction: &crate::StateTransaction<'_>,
    namespace: &StateNamespace,
    aliases: &[String],
) -> Result<Vec<crate::RoutingAddressSnapshot>, crate::StoreError> {
    aliases
        .iter()
        .map(|alias| transaction.resolve_routing_alias(namespace, alias))
        .collect()
}

fn receipt_json(routed: crate::RoutedMessageReceipt, outlook: Vec<RecipientOutlook>) -> Value {
    let crate::RoutedMessageReceipt {
        receipt,
        recipients,
        context_references,
    } = routed;
    json!({
        "message_id": receipt.message_id,
        "thread_id": receipt.thread_id,
        "thread_order": receipt.thread_order,
        "deliveries": receipt.deliveries,
        "idempotent_replay": receipt.idempotent_replay,
        "recipients": address_receipt(&recipients),
        "context_references": address_receipt(&context_references),
        "delivery_outlook": outlook,
    })
}

fn address_receipt(addresses: &[crate::RoutingAddressSnapshot]) -> Vec<Value> {
    addresses
        .iter()
        .map(|address| {
            json!({
                "address_id": address.address_id,
                "alias": address.alias,
            })
        })
        .collect()
}

fn thread_title(body: &str) -> &str {
    let first_line = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or_else(|| body.trim());
    let end = first_line
        .char_indices()
        .nth(80)
        .map_or(first_line.len(), |(index, _)| index);
    &first_line[..end]
}

fn required_nonempty(value: String, field: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(value)
    }
}

fn token_end(source: &str, cursor: usize) -> usize {
    source[cursor..]
        .find(char::is_whitespace)
        .map_or(source.len(), |offset| cursor + offset)
}

fn skip_whitespace(source: &str, mut cursor: usize) -> usize {
    while let Some(character) = source[cursor..].chars().next()
        && character.is_whitespace()
    {
        cursor += character.len_utf8();
    }
    cursor
}

/// Every byte offset in `text` where an at-sign begins a Mention under the
/// canonical grammar. It is the one scanner: a code span is skipped whole, and
/// an at-sign preceded by an alias character, an underscore, or a backslash is
/// embedded text rather than a Mention. Whether the token that follows is
/// alias-shaped is a separate question each caller answers for itself.
fn mention_anchors(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut anchors = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'`' {
            let delimiter_start = cursor;
            while cursor < bytes.len() && bytes[cursor] == b'`' {
                cursor += 1;
            }
            let delimiter = &text[delimiter_start..cursor];
            if let Some(offset) = text[cursor..].find(delimiter) {
                cursor += offset + delimiter.len();
            } else {
                cursor = bytes.len();
            }
            continue;
        }
        if bytes[cursor] != b'@' {
            cursor += 1;
            continue;
        }
        if is_embedded_at_sign(bytes, cursor) {
            cursor += 1;
            continue;
        }
        anchors.push(cursor);
        cursor = mention_token_end(text, cursor);
    }
    anchors
}

fn is_embedded_at_sign(bytes: &[u8], at: usize) -> bool {
    at > 0
        && (bytes[at - 1].is_ascii_alphanumeric()
            || bytes[at - 1] == b'_'
            || bytes[at - 1] == b'\\')
}

fn mention_token_end(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = (at + 1).min(bytes.len());
    while end < bytes.len() && is_mention_token_byte(bytes[end]) {
        end += 1;
    }
    end
}

/// Collects the Context Reference Mentions of a body. An at-sign token is classified whole before
/// any alias is looked up: a token that is not alias-shaped is not Mention-shaped, so it stays
/// literal prose instead of rejecting the send, and no valid alias prefix is taken out of it.
/// A run that opens with a `#label` names someone in another workspace, so it is prose here too.
fn context_mentions(body: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut qualified_run_end = None;
    for anchor in mention_anchors(body) {
        let end = mention_token_end(body, anchor);
        let continues_run = qualified_run_end
            .is_some_and(|run_end| body[run_end..anchor].chars().all(char::is_whitespace));
        if continues_run || follows_workspace_label(body, anchor) {
            qualified_run_end = Some(end);
            continue;
        }
        qualified_run_end = None;
        let token = &body[anchor + 1..end];
        if validate_alias(token).is_ok() && !aliases.iter().any(|existing| existing == token) {
            aliases.push(token.to_owned());
        }
    }
    aliases
}

fn follows_workspace_label(text: &str, anchor: usize) -> bool {
    let before = &text[..anchor];
    if !before.ends_with(char::is_whitespace) {
        return false;
    }
    before
        .split_whitespace()
        .next_back()
        .and_then(|token| token.strip_prefix('#'))
        .is_some_and(|label| !label.is_empty() && label.bytes().all(is_label_token_byte))
}

fn is_label_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn is_mention_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}
