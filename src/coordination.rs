use crate::{
    ActorKind, AssignmentId, CoordinationId, CoordinationLink, CoordinationOutcome,
    LifecycleRuntime, MessageDraft, ReconciledAssignment, RoutingAddressSnapshot, StateNamespace,
    StateStore, StateTransaction, StoreError, WorkOutcome, store::CoordinationRole,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct TrackingInput {
    pub coordination_id: Option<CoordinationId>,
    pub assignment_id: Option<AssignmentId>,
    pub expected_revision: Option<u64>,
    pub work_outcome: Option<WorkOutcome>,
    pub blocked_reason: Option<String>,
    pub waiting_on_assignment_id: Option<AssignmentId>,
}
impl TrackingInput {
    pub(crate) fn fingerprint(&self) -> Value {
        serde_json::to_value(self).expect("tracking serializes")
    }
    pub(crate) fn present(&self) -> bool {
        self.fingerprint()
            .as_object()
            .unwrap()
            .values()
            .any(|v| !v.is_null())
    }
    pub(crate) fn validate(&self, reply: bool) -> Result<(), String> {
        if self
            .coordination_id
            .as_ref()
            .is_some_and(|v| v.as_str().trim().is_empty())
            || self
                .assignment_id
                .as_ref()
                .is_some_and(|v| v.as_str().trim().is_empty())
        {
            return Err("coordination and assignment ids must be nonempty".to_owned());
        }
        if self.assignment_id.is_some() != self.expected_revision.is_some()
            || self.expected_revision == Some(0)
        {
            return Err(
                "assignment continuation requires a positive expected_revision and assignment_id"
                    .to_owned(),
            );
        }
        if self.assignment_id.is_some() && self.coordination_id.is_none() {
            return Err("assignment_id requires coordination_id".to_owned());
        }
        if !reply
            && (self.work_outcome.is_some()
                || self.blocked_reason.is_some()
                || self.waiting_on_assignment_id.is_some())
        {
            return Err("work outcomes belong on tracked replies".to_owned());
        }
        if self.work_outcome == Some(WorkOutcome::Blocked) {
            if self
                .blocked_reason
                .as_ref()
                .is_none_or(|s| s.trim().is_empty())
            {
                return Err("blocked work requires blocked_reason".to_owned());
            }
        } else if self.blocked_reason.is_some() || self.waiting_on_assignment_id.is_some() {
            return Err("blocker fields require work_outcome blocked".to_owned());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginInput {
    request_id: String,
    deliverable: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveInput {
    pub request_id: String,
    pub coordination_id: CoordinationId,
    pub outcome: CoordinationOutcome,
    pub report: String,
    pub reason: Option<String>,
    #[serde(default)]
    pub reconciled_assignments: Vec<ReconciledAssignment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdoptInput {
    pub request_id: String,
    pub coordination_id: CoordinationId,
}

pub(crate) fn execute<E: crate::HerdrCommandExecutor, C: crate::Clock, I: crate::IdGenerator>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String> {
    let discovered = match runtime.workspace_id {
        Some(id) => crate::discover_worktree_for_workspace(runtime.herdr, id)?,
        None => crate::discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    let ns = discovered.namespace(runtime.session_socket_id);
    let pane = runtime
        .current_pane_id
        .ok_or("the current Herdr pane is required")?;
    let mut store = StateStore::open(runtime.state_directory).map_err(|e| e.to_string())?;
    let response = store
        .transaction(|tx| {
            if !tx
                .worktree_state(&ns)?
                .is_some_and(|s| s.activated && !s.deactivated)
            {
                return Err(StoreError::invalid_input(
                    "Namedrop is not activated for this space",
                ));
            }
            let owner = crate::context::registered_current_address(tx, &ns, pane, &discovered)?;
            match operation {
                "handoff.begin" => {
                    let input: BeginInput = serde_json::from_value(input)
                        .map_err(|e| StoreError::invalid_input(crate::input_schema_error(e)))?;
                    if input.request_id.trim().is_empty() || input.deliverable.trim().is_empty() {
                        return Err(StoreError::invalid_input(
                            "request_id and deliverable are required",
                        ));
                    }
                    let id = tx.begin_coordination(
                        &ns,
                        &owner,
                        &input.request_id,
                        &input.deliverable,
                        &CoordinationId::new(runtime.ids.next_id("coordination")),
                        runtime.clock.now_unix_ms(),
                    )?;
                    tx.coordination_report(&ns, &id, &owner)
                }
                "handoff.resolve" => {
                    let input: ResolveInput = serde_json::from_value(input)
                        .map_err(|e| StoreError::invalid_input(crate::input_schema_error(e)))?;
                    if input.request_id.trim().is_empty() || input.report.trim().is_empty() {
                        return Err(StoreError::invalid_input(
                            "request_id and a nonempty report are required",
                        ));
                    }
                    if input.outcome != CoordinationOutcome::Completed
                        && input.reason.as_ref().is_none_or(|s| s.trim().is_empty())
                    {
                        return Err(StoreError::invalid_input(
                            "blocked or cancelled resolution requires a reason",
                        ));
                    }
                    tx.resolve_coordination(&ns, &owner, &input, runtime.clock.now_unix_ms())
                }
                "handoff.adopt" => {
                    let input: AdoptInput = serde_json::from_value(input)
                        .map_err(|e| StoreError::invalid_input(crate::input_schema_error(e)))?;
                    if input.request_id.trim().is_empty()
                        || input.coordination_id.as_str().trim().is_empty()
                    {
                        return Err(StoreError::invalid_input(
                            "request_id and coordination_id are required",
                        ));
                    }
                    tx.adopt_coordination(&ns, &owner, &input, runtime.clock.now_unix_ms())
                }
                _ => Err(StoreError::invalid_input(
                    "unsupported coordination operation",
                )),
            }
        })
        .map_err(|e| e.to_string())?;
    if operation == "handoff.resolve" {
        let mut response = response;
        response["delivery"] = crate::delivery::drain_receipt(runtime);
        return Ok(response);
    }
    Ok(response)
}

/// Validate and persist alongside append_message, in the same transaction. No
/// transport acknowledgement changes work state; only these explicit actions do.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_message<
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
>(
    tx: &mut StateTransaction<'_>,
    ns: &StateNamespace,
    draft: &MessageDraft,
    input: &TrackingInput,
    attempt: Option<&crate::DeliveryAttemptId>,
    discovered: &crate::DiscoveredWorktree,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<(), StoreError> {
    let parents = draft
        .in_reply_to
        .as_ref()
        .map(|id| tx.coordination_links(ns, id))
        .transpose()?
        .unwrap_or_default();
    if parents.is_empty() && !input.present() {
        return Ok(());
    }
    if draft.author_kind != ActorKind::AgentSession {
        return Err(StoreError::invalid_input(
            "tracked work requires an agent-authored message",
        ));
    }
    let author = crate::context::registered_current_address(
        tx,
        ns,
        runtime.current_pane_id.expect("message requires pane"),
        discovered,
    )?;
    tx.require_coordination_capability(ns, &author)?;
    let inherited = parents
        .iter()
        .find(|link| {
            tx.coordination_assignment(ns, &link.assignment_id)
                .is_ok_and(|a| {
                    a["recipient_address_id"].as_str() == Some(author.address_id.as_str())
                })
        })
        .or_else(|| parents.first());
    let coordination = input
        .coordination_id
        .as_ref()
        .or_else(|| inherited.map(|l| &l.coordination_id))
        .ok_or_else(|| StoreError::invalid_input("work_outcome requires a tracked assignment"))?;
    if inherited.is_some_and(|l| &l.coordination_id != coordination) {
        return Err(StoreError::invalid_input(
            "reply cannot change its coordination",
        ));
    }
    let owner = tx.coordination_owner(ns, coordination)?;
    match CoordinationRole::of(&author, &owner.address_id, owner.agent_session_id.as_ref()) {
        CoordinationRole::Owner | CoordinationRole::OwnerAddressOtherSession => {
            tx.require_coordination_owner(ns, coordination, &author, true)?;
            if input.work_outcome.is_some() {
                return Err(StoreError::invalid_input(
                    "the lead continues assignments; worker outcomes belong to recipients",
                ));
            }
            let existing = input
                .assignment_id
                .as_ref()
                .or_else(|| inherited.map(|l| &l.assignment_id));
            if existing.is_some() && draft.recipients.len() != 1 {
                return Err(StoreError::invalid_input(
                    "an assignment continuation requires its one original recipient",
                ));
            }
            for recipient in &draft.recipients {
                let address = tx.routing_address_by_id(ns, &recipient.address_id)?;
                require_live_recipient(tx, ns, &address, discovered)?;
                if address.address_id == author.address_id {
                    return Err(StoreError::invalid_input(
                        "the lead cannot assign tracked work to itself",
                    ));
                }
                let link = if let Some(id) = existing {
                    let a = tx.coordination_assignment(ns, id)?;
                    if a["coordination_id"].as_str() != Some(coordination.as_str())
                        || a["recipient_address_id"].as_str() != Some(recipient.address_id.as_str())
                        || a["recipient_session_id"].as_str()
                            != recipient
                                .agent_session_snapshot
                                .as_ref()
                                .map(|s| s.as_str())
                    {
                        return Err(StoreError::invalid_input(
                            "assignment continuation must retain its coordination and exact recipient session",
                        ));
                    }
                    let revision = a["revision"].as_u64().expect("stored revision");
                    if input.expected_revision.is_some_and(|r| r != revision)
                        || inherited.is_some_and(|l| l.revision != revision)
                    {
                        return Err(StoreError::invalid_input(
                            "cannot continue a stale assignment revision",
                        ));
                    }
                    CoordinationLink {
                        coordination_id: coordination.clone(),
                        assignment_id: id.clone(),
                        revision: revision + 1,
                    }
                } else {
                    CoordinationLink {
                        coordination_id: coordination.clone(),
                        assignment_id: AssignmentId::new(runtime.ids.next_id("assignment")),
                        revision: 1,
                    }
                };
                tx.dispatch_assignment(ns, &link, draft, recipient, &input.fingerprint())?;
            }
            tx.advance_coordination(ns, coordination, runtime.clock.now_unix_ms())?;
        }
        CoordinationRole::Other => {
            if input.coordination_id.is_some() || input.assignment_id.is_some() {
                return Err(StoreError::invalid_input(
                    "worker replies inherit their assignment from the bound message",
                ));
            }
            let link = inherited.ok_or_else(|| {
                StoreError::invalid_input(
                    "only the exact coordination owner can dispatch assignments",
                )
            })?;
            let assignment = tx.coordination_assignment(ns, &link.assignment_id)?;
            if assignment["recipient_address_id"].as_str() != Some(author.address_id.as_str())
                || assignment["recipient_session_id"].as_str()
                    != author.agent_session_id.as_ref().map(|s| s.as_str())
            {
                return Err(StoreError::invalid_input(
                    "work outcome requires the assignment's exact recipient session",
                ));
            }
            let recipient = match draft.recipients.as_slice() {
                [recipient] if recipient.address_id == owner.address_id => recipient,
                recipients => {
                    let extra = recipients
                        .iter()
                        .filter(|r| r.address_id != owner.address_id)
                        .map(|r| format!("@{}", r.alias_snapshot))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(StoreError::invalid_input(format!(
                        "a tracked reply goes only to @{lead}, but this one also addresses {extra}. \
                         Start the source with @{lead} alone and write any other name without the @",
                        lead = owner.alias
                    )));
                }
            };
            if recipient.agent_session_snapshot != owner.agent_session_id {
                return Err(StoreError::invalid_input(
                    "the lead session restarted; it must run handoff adopt before tracked replies are accepted, so retry this reply after it does",
                ));
            }
            tx.require_assignment_attempt(
                ns,
                attempt.ok_or_else(|| {
                    StoreError::invalid_input("tracked replies require their bound attempt")
                })?,
                draft.in_reply_to.as_ref().expect("inherited message"),
                &author,
            )?;
            tx.record_assignment_outcome(ns, link, draft, input, runtime.clock.now_unix_ms())?;
        }
    }
    Ok(())
}

fn require_live_recipient(
    tx: &StateTransaction<'_>,
    ns: &StateNamespace,
    address: &RoutingAddressSnapshot,
    discovered: &crate::DiscoveredWorktree,
) -> Result<(), StoreError> {
    if tx.pane_type(ns, &address.address_id)? != crate::PaneType::Agent {
        return Err(StoreError::invalid_input(
            "tracked assignments require agent recipients",
        ));
    }
    let binding = tx
        .active_pane_bindings(ns)?
        .into_iter()
        .find(|b| b.address_id == address.address_id)
        .ok_or_else(|| StoreError::invalid_input("tracked recipient has no current binding"))?;
    let live = crate::context::registered_current_address(tx, ns, &binding.pane_id, discovered)?;
    tx.require_coordination_capability(ns, &live)
}
