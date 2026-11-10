use crate::lifecycle::PaneLabel;
use crate::store::LostSettle;
use crate::{
    ActorKind, Clock, DeliveryAttemptId, DeliveryState, DiscoveredPane, DispatchCandidate,
    HerdrCommandExecutor, IdGenerator, InflightDeliveryAttempt, IntegrationToolInvocation,
    LeaseDraft, LifecycleRuntime, MutationAttribution, PromptFailureKind, StateNamespace,
    StateStore, StateTransaction, StoreError, collaboration_executable, discover_worktree,
    store::WakeReadiness, tool_environment,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const LEASE_DURATION_MS: u64 = 30_000;
const ACKNOWLEDGEMENT_GRACE_MS: u64 = 30_000;
const QUEUED_HOLD_AFTER_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DrainReport {
    pub terminal_waiting: Vec<crate::PaneAddressId>,
    pub terminal_running: Vec<DeliveryAttemptId>,
    #[serde(skip_deserializing)]
    pub delivery_notice: &'static str,
    pub submitted: Vec<DeliveryAttemptId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pane_lock_deferred: Vec<crate::PaneAddressId>,
    pub requeued: Vec<DeliveryAttemptId>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retry_without_event: bool,
    pub failed: Vec<DeliveryAttemptId>,
    pub settled: Vec<DeliveryAttemptId>,
    pub held: Vec<DeliveryAttemptId>,
    pub expired: Vec<crate::DeliveryId>,
    pub pending_grace: Vec<GraceDeadline>,
    /// The earliest time a coordination check-in held for a quiet worker
    /// becomes due. The drain runs again then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_in_due_ms: Option<u64>,
}

/// An idle submitted or acknowledged attempt still inside the acknowledgement
/// grace. The drain observes it again at `due_at_ms`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraceDeadline {
    pub attempt_id: DeliveryAttemptId,
    pub due_at_ms: u64,
}

impl Default for DrainReport {
    fn default() -> Self {
        Self {
            terminal_waiting: Vec::new(),
            terminal_running: Vec::new(),
            delivery_notice: "Best-effort after a Settled Observation; the router records receipt and one terminal disposition is required.",
            submitted: Vec::new(),
            pane_lock_deferred: Vec::new(),
            requeued: Vec::new(),
            retry_without_event: false,
            failed: Vec::new(),
            settled: Vec::new(),
            held: Vec::new(),
            expired: Vec::new(),
            pending_grace: Vec::new(),
            check_in_due_ms: None,
        }
    }
}

impl DrainReport {
    pub fn awaits_retry(&self) -> bool {
        !self.pane_lock_deferred.is_empty() || !self.requeued.is_empty() || self.awaits_terminal()
    }

    pub fn needs_mailbox_worker(&self) -> bool {
        !self.pane_lock_deferred.is_empty()
            || self.retry_without_event
            || self.awaits_terminal()
            || self.next_due_ms().is_some()
    }

    pub fn awaits_terminal(&self) -> bool {
        !self.terminal_waiting.is_empty() || !self.terminal_running.is_empty()
    }

    /// When the drain must run again with no other event: the end of an
    /// acknowledgement grace or of a held check-in.
    pub fn next_due_ms(&self) -> Option<u64> {
        self.pending_grace
            .iter()
            .map(|deadline| deadline.due_at_ms)
            .chain(self.check_in_due_ms)
            .min()
    }

    /// Whether a queued message may be dispatched now. A held check-in keeps
    /// its deadline so the drain comes back for it.
    fn dispatchable(&mut self, readiness: WakeReadiness) -> bool {
        if let WakeReadiness::QuietUntil(due) = readiness {
            self.check_in_due_ms = Some(self.check_in_due_ms.map_or(due, |at| at.min(due)));
        }
        readiness == WakeReadiness::Ready
    }
}

/// Read drain reports from a bare report or an operation receipt. A report with
/// no workspace key belongs to the current space; forwarded reports retain the
/// workspace key used to find their saved copy.
pub fn drain_reports(result: &Value) -> impl Iterator<Item = (Option<&str>, DrainReport)> {
    std::iter::once((None, result))
        .chain(result.get("delivery").map(|value| (None, value)))
        .chain(
            result
                .get("drains")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|drains| {
                    drains
                        .iter()
                        .map(|(workspace, value)| (Some(workspace.as_str()), value))
                }),
        )
        .filter_map(|(workspace, value)| {
            serde_json::from_value(value.get("drain").unwrap_or(value).clone())
                .ok()
                .map(|report| (workspace, report))
        })
}

/// The spaces one mailbox wake should drain: this one first, then every other
/// space of the Herdr session holding a queued forwarded copy, and every space
/// whose open coordination has an assignee here, so a worker going idle
/// reaches its lead's nudge. State that cannot be read still wakes this space.
pub fn spaces_to_wake(
    state_directory: &std::path::Path,
    session_socket_id: &str,
    here: &str,
) -> Vec<String> {
    let others = StateStore::open_read_only(state_directory)
        .ok()
        .flatten()
        .and_then(|mut store| {
            store
                .transaction(|transaction| {
                    let mut spaces = transaction.spaces_awaiting_dispatch(session_socket_id)?;
                    spaces.extend(transaction.reminder_spaces(session_socket_id)?);
                    spaces.extend(transaction.coordination_homes_seated_in(
                        session_socket_id,
                        &crate::SpaceId::new(here),
                    )?);
                    Ok(spaces)
                })
                .ok()
        })
        .unwrap_or_default();
    let mut spaces = vec![here.to_owned()];
    for space in others {
        if !spaces.iter().any(|known| known == space.as_str()) {
            spaces.push(space.as_str().to_owned());
        }
    }
    spaces
}

/// Whether mail queued in `space` waits for the Agent Session `session`.
pub fn mail_waits_for_session(
    state: &std::path::Path,
    socket: &str,
    space: &str,
    session: &str,
) -> bool {
    let namespace = StateNamespace::for_space(socket, space);
    StateStore::open_read_only(state)
        .ok()
        .flatten()
        .and_then(|mut store| {
            store
                .transaction(|transaction| {
                    let queued = transaction.queued_mailbox_addresses(&namespace)?;
                    Ok(transaction
                        .list_pane_addresses(&namespace)?
                        .into_iter()
                        .any(|address| {
                            queued.contains(&address.id)
                                && address
                                    .agent_session_id
                                    .is_some_and(|id| id.as_str() == session)
                        }))
                })
                .ok()
        })
        .unwrap_or(false)
}

pub fn drain_receipt<E, C, I>(runtime: &LifecycleRuntime<'_, E, C, I>) -> Value
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    match drain_mailboxes(runtime) {
        Ok(report) => serde_json::to_value(report)
            .expect("DrainReport contains only JSON-serializable values"),
        Err(error) => json!({
            "delivery_notice": DrainReport::default().delivery_notice,
            "error": error,
        }),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeliveryReconciliationReport {
    pub settled: Vec<DeliveryAttemptId>,
    pub held: Vec<DeliveryAttemptId>,
    pub pending_grace: Vec<GraceDeadline>,
}

pub(crate) fn reconcile_delivery_state(
    transaction: &mut StateTransaction<'_>,
    namespace: &StateNamespace,
    panes: &[DiscoveredPane],
    restart: bool,
    state_directory: &std::path::Path,
    attribution: &MutationAttribution,
) -> Result<DeliveryReconciliationReport, StoreError> {
    let mut report = DeliveryReconciliationReport::default();
    for attempt in transaction.inflight_delivery_attempts(namespace)? {
        let pane = panes.iter().find(|pane| pane.pane_id == attempt.pane_id);
        if let Some((terminal_id, started)) =
            transaction.terminal_execution(namespace, &attempt.attempt_id)?
        {
            if let Some((code, output)) =
                transaction.terminal_result(namespace, &attempt.attempt_id)?
            {
                transaction.finish_terminal_execution(
                    namespace,
                    &attempt.attempt_id,
                    code,
                    &output,
                    attribution,
                )?;
                report.settled.push(attempt.attempt_id);
                continue;
            }
            let same = attempt.binding_active
                && pane.is_some_and(|p| {
                    p.terminal_id == terminal_id
                        && p.agent.is_none()
                        && p.agent_session_id.is_none()
                });
            if !same
                || (started.is_none()
                    && attribution
                        .occurred_at_ms
                        .saturating_sub(attempt.created_at_ms)
                        >= ACKNOWLEDGEMENT_GRACE_MS)
            {
                transaction.transition_delivery_attempt(
                    namespace,
                    &attempt.attempt_id,
                    DeliveryState::Held,
                    attribution,
                )?;
                report.held.push(attempt.attempt_id);
            }
            // Shell status alone never means command completion. Only the
            // claimed runner can persist an exit status and settle this work.
            continue;
        }
        match observe_agent_attempt(&attempt, pane, restart, attribution.occurred_at_ms) {
            Observation::Hold => {
                transaction.transition_delivery_attempt(
                    namespace,
                    &attempt.attempt_id,
                    DeliveryState::Held,
                    attribution,
                )?;
                report.held.push(attempt.attempt_id);
            }
            Observation::Settle => {
                transaction.settle_observed_attempt(namespace, &attempt.attempt_id, attribution)?;
                if pane.and_then(crate::integration::pane_agent_runtime)
                    == Some(crate::AgentRuntime::Codex)
                    && crate::hook_runs::refused(state_directory, attempt.agent_session_id.as_str())
                {
                    transaction.flag_failed_settle(
                        namespace,
                        &attempt.attempt_id,
                        LostSettle::FinalMessage,
                        crate::hook_runs::REFUSED_REASON,
                        attribution,
                    )?;
                }
                report.settled.push(attempt.attempt_id);
            }
            Observation::PendingGrace { due_at_ms } => report.pending_grace.push(GraceDeadline {
                attempt_id: attempt.attempt_id,
                due_at_ms,
            }),
            Observation::Unchanged => {}
        }
    }
    Ok(report)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Observation {
    Hold,
    Settle,
    PendingGrace { due_at_ms: u64 },
    Unchanged,
}

fn observe_agent_attempt(
    attempt: &InflightDeliveryAttempt,
    pane: Option<&DiscoveredPane>,
    restart: bool,
    now_ms: u64,
) -> Observation {
    let same_session = attempt.binding_active
        && pane.and_then(|pane| pane.agent_session_id.as_ref()) == Some(&attempt.agent_session_id);
    let session_is_settled = pane
        .and_then(|pane| pane.agent_status.as_deref())
        .is_some_and(|status| matches!(status, "idle" | "done"));
    let grace_due_at_ms = attempt
        .created_at_ms
        .saturating_add(ACKNOWLEDGEMENT_GRACE_MS);
    let idle_past_grace = session_is_settled && now_ms >= grace_due_at_ms;
    let lease_expired = attempt
        .lease_expires_at_ms
        .is_none_or(|expires_at_ms| expires_at_ms <= now_ms);
    match attempt.state {
        _ if !same_session => Observation::Hold,
        DeliveryState::Dispatching if restart || lease_expired => Observation::Hold,
        DeliveryState::Submitted if restart || idle_past_grace => Observation::Hold,
        DeliveryState::Acknowledged if idle_past_grace => Observation::Settle,
        DeliveryState::Submitted | DeliveryState::Acknowledged if session_is_settled => {
            Observation::PendingGrace {
                due_at_ms: grace_due_at_ms,
            }
        }
        _ => Observation::Unchanged,
    }
}

pub(crate) fn publish_status_metadata<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    discovered: &crate::DiscoveredWorktree,
) -> Result<(), String>
where
    E: HerdrCommandExecutor,
{
    let namespace = discovered.namespace(runtime.session_socket_id);
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    let (waiting, attention, bindings, addresses) = store
        .transaction(|transaction| {
            let (waiting, _) = transaction.delivery_health(&namespace)?;
            Ok((
                waiting,
                transaction.unresolved_attention_items(&namespace)?.len() as u64,
                transaction.active_pane_bindings(&namespace)?,
                transaction.list_pane_addresses(&namespace)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let served = crate::codex_app_server::served_codex_panes(runtime, &discovered.panes);
    let awaiting_token = format!("namedrop-awaiting={waiting}");
    let attention_token = format!("namedrop-attention={attention}");
    for binding in bindings {
        if let Some(pane) = discovered
            .panes
            .iter()
            .find(|pane| pane.pane_id == binding.pane_id)
        {
            if let Some(address) = addresses
                .iter()
                .find(|address| !address.retired && address.id == binding.address_id)
            {
                let label = PaneLabel::for_pane(pane, served.as_ref());
                if label.replaces(&address.alias, pane.idle_state_label.as_deref()) {
                    crate::lifecycle::publish_pane_address_metadata(
                        runtime,
                        &binding.pane_id,
                        &address.alias,
                        label,
                    )?;
                }
            }
            runtime.herdr.execute(&[
                "pane",
                "report-metadata",
                &binding.pane_id,
                "--source",
                "namedrop",
                "--token",
                &awaiting_token,
                "--token",
                &attention_token,
            ])?;
        }
    }
    Ok(())
}

pub fn drain_mailboxes<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<DrainReport, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    #[cfg(test)]
    crate::work_counters::drain();
    let first_discovery = discover(runtime)?;
    let namespace = first_discovery.namespace(runtime.session_socket_id);
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    let reconciliation_completed = store
        .transaction(|transaction| {
            let completed = transaction.reconciliation_completed(&namespace)?;
            if !completed {
                transaction.maintain_reminder_owners(&namespace, &first_discovery)?;
            }
            Ok(completed)
        })
        .map_err(|error| error.to_string())?;
    if !reconciliation_completed {
        return Ok(DrainReport::default());
    }
    let now = runtime.clock.now_unix_ms();
    // An interrupted helper has no exit result. Once its shell is available
    // again, retain the ambiguity instead of treating shell idleness as success.
    let inflight = store
        .transaction(|tx| tx.inflight_delivery_attempts(&namespace))
        .map_err(|e| e.to_string())?;
    for attempt in inflight {
        let terminal = store
            .transaction(|tx| tx.terminal_execution(&namespace, &attempt.attempt_id))
            .map_err(|e| e.to_string())?;
        if terminal.is_some_and(|(_, started)| started.is_some())
            && now.saturating_sub(attempt.created_at_ms) >= ACKNOWLEDGEMENT_GRACE_MS
            && store
                .transaction(|tx| tx.terminal_result(&namespace, &attempt.attempt_id))
                .map_err(|e| e.to_string())?
                .is_none()
            && crate::terminal::shell_is_available(runtime.herdr, &attempt.pane_id)
        {
            store
                .transaction(|tx| {
                    tx.hold_delivery_attempt(
                        &namespace,
                        &attempt.attempt_id,
                        &MutationAttribution::new(ActorKind::Plugin, "namedrop-terminal", now),
                    )
                })
                .map_err(|e| e.to_string())?;
        }
    }
    let reconciled = store
        .transaction(|transaction| {
            let report = reconcile_delivery_state(
                transaction,
                &namespace,
                &first_discovery.panes,
                false,
                runtime.state_directory,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now),
            )?;
            transaction.maintain_reminder_owners(&namespace, &first_discovery)?;
            Ok(report)
        })
        .map_err(|error| error.to_string())?;
    let cutoff = now.saturating_sub(QUEUED_HOLD_AFTER_MS);
    let expired = store
        .transaction(|transaction| {
            transaction.hold_expired_queued_deliveries(
                &namespace,
                cutoff,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now),
            )
        })
        .map_err(|error| error.to_string())?;
    let remote_seats = store
        .transaction(|transaction| transaction.remote_seat_spaces(&namespace))
        .map_err(|error| error.to_string())?;
    let live =
        crate::spaces::LiveSpaces::load(runtime.herdr, first_discovery.clone(), &remote_seats);
    let check_in_due_ms = store
        .transaction(|transaction| {
            transaction.complete_ready_result_sets(
                &namespace,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now),
            )?;
            transaction.reconcile_coordinations(
                &namespace,
                &live,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-coordination", now),
            )
        })
        .map_err(|error| error.to_string())?;
    let addresses = store
        .transaction(|transaction| transaction.queued_mailbox_addresses(&namespace))
        .map_err(|error| error.to_string())?;
    let mut report = DrainReport {
        settled: reconciled.settled,
        held: reconciled.held,
        expired,
        pending_grace: reconciled.pending_grace,
        check_in_due_ms,
        ..DrainReport::default()
    };

    for address_id in addresses {
        let first = store
            .transaction(|transaction| transaction.dispatch_candidate(&namespace, &address_id))
            .map_err(|error| error.to_string())?;
        let Some(first) = first else {
            continue;
        };
        if !live_candidate_matches(runtime, &first, &first_discovery)
            || (first.pane_type == crate::PaneType::Terminal
                && !crate::terminal::shell_is_available(runtime.herdr, &first.pane_id))
        {
            continue;
        }

        runtime.clock.wait_ms(first.settled_delay_ms);
        let effects = match crate::pane_effects::PaneEffects::acquire(
            runtime.state_directory,
            runtime.session_socket_id,
            &first.pane_id,
        )
        .map_err(|error| error.with_alias(Some(&first.alias)))
        {
            Ok(effects) => Ok(effects),
            Err(crate::pane_effects::AcquireError::Fault(error)) => Err(error),
            Err(crate::pane_effects::AcquireError::Contended) => {
                report.pane_lock_deferred.push(address_id);
                continue;
            }
        };
        let second_discovery = discover(runtime)?;
        if second_discovery.workspace_id != namespace.space_id.as_str() {
            continue;
        }
        let second = store
            .transaction(|transaction| transaction.dispatch_candidate(&namespace, &address_id))
            .map_err(|error| error.to_string())?;
        let Some(second) = second else {
            continue;
        };
        if first != second
            || !live_candidate_matches(runtime, &second, &second_discovery)
            || (second.pane_type == crate::PaneType::Terminal
                && (first_discovery
                    .panes
                    .iter()
                    .find(|p| p.pane_id == first.pane_id)
                    .map(|p| &p.terminal_id)
                    != second_discovery
                        .panes
                        .iter()
                        .find(|p| p.pane_id == second.pane_id)
                        .map(|p| &p.terminal_id)
                    || !crate::terminal::shell_is_available(runtime.herdr, &second.pane_id)))
        {
            continue;
        }

        let now = runtime.clock.now_unix_ms();
        let attribution = MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now);
        let attempt_id = DeliveryAttemptId::new(runtime.ids.next_id("delivery-attempt"));
        let lease = LeaseDraft {
            attempt_id: attempt_id.clone(),
            binding_id: second.binding_id.clone(),
            agent_session_id: second.agent_session_id.clone(),
            retry_of_attempt_id: second.retry_of_attempt_id.clone(),
            owner: format!("dispatcher:{}", runtime.session_socket_id),
            acquired_at_ms: now,
            expires_at_ms: now.saturating_add(LEASE_DURATION_MS),
        };
        let effects = match effects {
            Ok(effects) => effects,
            Err(error) => {
                let summary = format!(
                    "Could not send to @{}. {error}. Check the state directory permissions and available file descriptors, then review the conversation and send it again, or ask the agent that sent it to send it again.",
                    second.alias,
                );
                let failed = store
                    .transaction(|transaction| {
                        if transaction
                            .dispatch_candidate(&namespace, &address_id)?
                            .as_ref()
                            != Some(&second)
                            || !report.dispatchable(
                                transaction.coordination_wake_message_readiness(
                                    &namespace,
                                    &second.message_id,
                                    &live.with_here(second_discovery.clone()),
                                    now,
                                )?,
                            )
                        {
                            return Ok(None);
                        }
                        let Some(leased) = transaction.lease_mailbox_head(
                            &namespace,
                            &address_id,
                            &lease,
                            &attribution,
                        )?
                        else {
                            return Ok(None);
                        };
                        transaction.transition_delivery_attempt(
                            &namespace,
                            &leased.attempt_id,
                            DeliveryState::Failed,
                            &attribution,
                        )?;
                        transaction.open_delivery_attempt_attention(
                            &namespace,
                            &leased.attempt_id,
                            &summary,
                            &attribution,
                        )?;
                        Ok(Some(leased.attempt_id))
                    })
                    .map_err(|error| error.to_string())?;
                report.failed.extend(failed);
                continue;
            }
        };
        let tool = IntegrationToolInvocation::new(
            collaboration_executable()?,
            tool_environment(
                runtime.state_directory,
                runtime.session_socket_id,
                &second_discovery.workspace_id,
                &second.pane_id,
            ),
        );
        let leased = store
            .transaction(|transaction| {
                if !report.dispatchable(transaction.coordination_wake_message_readiness(
                    &namespace,
                    &second.message_id,
                    &live.with_here(second_discovery.clone()),
                    now,
                )?) {
                    return Ok(None);
                }
                let leased = transaction.lease_mailbox_head(
                    &namespace,
                    &address_id,
                    &lease,
                    &attribution,
                )?;
                leased
                    .map(|leased| {
                        if second.pane_type == crate::PaneType::Terminal {
                            let pane = second_discovery
                                .panes
                                .iter()
                                .find(|p| p.pane_id == second.pane_id)
                                .expect("live candidate checked");
                            if transaction
                                .prepare_terminal_execution(
                                    &namespace,
                                    &leased.attempt_id,
                                    &pane.terminal_id,
                                    &attribution,
                                )?
                                .is_err()
                            {
                                return Ok((leased, None));
                            }
                            let command = crate::terminal::runner_command(
                                runtime,
                                &second_discovery.workspace_id,
                                &pane.pane_id,
                                &leased.attempt_id,
                            )
                            .map_err(StoreError::invalid_input)?;
                            return Ok((leased, Some(command)));
                        }
                        transaction
                            .delivery_envelope(&namespace, &leased.attempt_id, tool)
                            .and_then(|envelope| {
                                let pointer =
                                    transaction.delivery_pointer(&namespace, &leased.attempt_id)?;
                                crate::pointer::typed_prompt(&envelope, pointer.as_ref())
                                    .map(|prompt| (leased, Some(prompt)))
                                    .map_err(|error| {
                                        crate::StoreError::invalid_input(error.to_string())
                                    })
                            })
                    })
                    .transpose()
            })
            .map_err(|error| error.to_string())?;
        let Some((leased, prompt)) = leased else {
            continue;
        };
        let Some(prompt) = prompt else {
            report.failed.push(leased.attempt_id);
            continue;
        };
        let owner = crate::pane_effects::InputOwner::delivery(
            &namespace,
            &leased.attempt_id,
            &second.binding_id,
            &leased.agent_session_id,
        );
        let prompt_result = if second.pane_type == crate::PaneType::Terminal {
            effects.run_terminal(runtime.herdr, &owner, &prompt)
        } else {
            effects.prompt(runtime.herdr, &owner, &prompt)
        };
        let attribution = MutationAttribution::new(
            ActorKind::Plugin,
            "namedrop-dispatch",
            runtime.clock.now_unix_ms(),
        );
        if second.pane_type == crate::PaneType::Terminal
            && prompt_result.is_err()
            && !matches!(
                store
                    .transaction(|tx| tx.delivery_attempt_state(&namespace, &leased.attempt_id))
                    .map_err(|e| e.to_string())?,
                DeliveryState::Dispatching | DeliveryState::Submitted
            )
        {
            // A runner's durable claim/result outranks a late transport error.
            continue;
        }
        let prompt_result =
            prompt_result.map_err(|error| runtime.herdr.classify_prompt_failure(&error));
        match prompt_result {
            Ok(_) => {
                store
                    .transaction(|transaction| {
                        if second.pane_type == crate::PaneType::Terminal {
                            if transaction.delivery_attempt_state(&namespace, &leased.attempt_id)?
                                == DeliveryState::Dispatching
                            {
                                transaction.transition_delivery_attempt(
                                    &namespace,
                                    &leased.attempt_id,
                                    DeliveryState::Submitted,
                                    &attribution,
                                )?;
                            }
                            return Ok(());
                        }
                        transaction.transition_delivery_attempt(
                            &namespace,
                            &leased.attempt_id,
                            DeliveryState::Submitted,
                            &attribution,
                        )?;
                        transaction.ensure_delivery_attempt_acknowledged(
                            &namespace,
                            &leased.attempt_id,
                            &leased.agent_session_id,
                            &attribution,
                        )
                    })
                    .map_err(|error| error.to_string())?;
                report.submitted.push(leased.attempt_id);
            }
            Err(kind) if kind.is_temporary_pre_invocation() => {
                store
                    .transaction(|transaction| {
                        transaction.return_delivery_attempt_to_queue(
                            &namespace,
                            &leased.attempt_id,
                            &attribution,
                        )
                    })
                    .map_err(|error| error.to_string())?;
                report.requeued.push(leased.attempt_id);
                report.retry_without_event |=
                    kind == PromptFailureKind::TemporaryPreInvocationWithoutEvent;
            }
            Err(PromptFailureKind::DefinitiveNonRetryable) => {
                store
                    .transaction(|transaction| {
                        transaction.transition_delivery_attempt(
                            &namespace,
                            &leased.attempt_id,
                            DeliveryState::Failed,
                            &attribution,
                        )?;
                        transaction.maintain_reminder_owners(&namespace, &second_discovery)
                    })
                    .map_err(|error| error.to_string())?;
                report.failed.push(leased.attempt_id);
            }
            Err(_) => {
                store
                    .transaction(|transaction| {
                        transaction.transition_delivery_attempt(
                            &namespace,
                            &leased.attempt_id,
                            DeliveryState::Held,
                            &attribution,
                        )
                    })
                    .map_err(|error| error.to_string())?;
                report.held.push(leased.attempt_id);
            }
        }
    }

    (report.terminal_running, report.terminal_waiting) = store
        .transaction(|tx| {
            let running = tx
                .inflight_delivery_attempts(&namespace)?
                .into_iter()
                .filter_map(|attempt| {
                    match tx.terminal_execution(&namespace, &attempt.attempt_id) {
                        Ok(Some(_)) => Some(Ok(attempt.attempt_id)),
                        Ok(None) => None,
                        Err(e) => Some(Err(e)),
                    }
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            let mut waiting = Vec::new();
            for address in tx.queued_mailbox_addresses(&namespace)? {
                let Some(candidate) = tx.dispatch_candidate(&namespace, &address)? else {
                    continue;
                };
                if candidate.pane_type == crate::PaneType::Terminal
                    && live_candidate_matches(runtime, &candidate, &first_discovery)
                    && tx.coordination_wake_message_readiness(
                        &namespace,
                        &candidate.message_id,
                        &live,
                        runtime.clock.now_unix_ms(),
                    )? == WakeReadiness::Ready
                {
                    waiting.push(address);
                }
            }
            Ok((running, waiting))
        })
        .map_err(|e| e.to_string())?;
    publish_status_metadata(runtime, &first_discovery)?;
    Ok(report)
}

fn discover<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<crate::DiscoveredWorktree, String>
where
    E: HerdrCommandExecutor,
{
    match runtime.workspace_id {
        Some(workspace_id) => crate::lifecycle::discover_reminder_workspace(
            runtime.state_directory,
            &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
            runtime.herdr,
        ),
        None => discover_worktree(runtime.herdr, runtime.cwd),
    }
}

/// Whether a typed prompt reaches the candidate's session now: a terminal at
/// its shell, or an agent session open for a prompt with nothing in its way.
fn live_candidate_matches<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    candidate: &DispatchCandidate,
    discovered: &crate::DiscoveredWorktree,
) -> bool
where
    E: HerdrCommandExecutor,
{
    let Some(pane) = discovered
        .panes
        .iter()
        .find(|pane| pane.pane_id == candidate.pane_id)
    else {
        return false;
    };
    if candidate.pane_type == crate::PaneType::Terminal {
        return pane.agent.is_none() && pane.agent_session_id.is_none();
    }
    pane.agent_session_id.as_ref() == Some(&candidate.agent_session_id)
        && agent_wait(runtime, pane).is_none()
}

/// How an agent pane takes a prompt, from Herdr's status and, for Claude Code,
/// the turn edges its hooks recorded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Opening {
    /// Herdr reports the session idle or done.
    Settled,
    /// Herdr reports a Claude Code session working, but its last turn ended
    /// and no prompt came since: background agents it started run on. A prompt
    /// typed now starts the next turn, unless the person's draft is in the box.
    BetweenTurns,
    /// A turn runs, a question waits, or the status is unknown.
    Closed,
}

fn opening(
    pane: &DiscoveredPane,
    phase: impl FnOnce() -> Option<crate::turn_phase::TurnPhase>,
) -> Opening {
    match pane.agent_status.as_deref() {
        Some("idle" | "done") => Opening::Settled,
        Some("working")
            if crate::integration::pane_agent_runtime(pane)
                == Some(crate::AgentRuntime::ClaudeCode)
                && matches!(phase(), Some(crate::turn_phase::TurnPhase::Stopped { .. })) =>
        {
            Opening::BetweenTurns
        }
        _ => Opening::Closed,
    }
}

/// Why an agent pane's mail waits, or `None` when a prompt may be typed now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AgentWait {
    /// Herdr's status for the pane, which is not idle or done.
    Status(Option<String>),
    /// The session is between turns, and its prompt box holds text Namedrop
    /// would type after, or could not be read.
    PromptBox(PromptBox),
}

pub(crate) fn agent_wait<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    pane: &DiscoveredPane,
) -> Option<AgentWait>
where
    E: HerdrCommandExecutor,
{
    let phase = || {
        pane.agent_session_id.as_ref().and_then(|session| {
            crate::turn_phase::read(
                runtime.state_directory,
                runtime.session_socket_id,
                session.as_str(),
            )
        })
    };
    match opening(pane, phase) {
        Opening::Settled => None,
        Opening::BetweenTurns => match read_prompt_box(runtime.herdr, &pane.pane_id) {
            PromptBox::Empty => None,
            held => Some(AgentWait::PromptBox(held)),
        },
        Opening::Closed => Some(AgentWait::Status(pane.agent_status.clone())),
    }
}

/// What a Claude Code pane's prompt box holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptBox {
    Empty,
    /// Text the person typed and has not sent.
    Draft,
    /// The screen shows no prompt box Namedrop recognizes.
    Unseen,
}

/// Enough of the screen's end for the box and whatever Claude Code draws under it.
const PROMPT_BOX_LINES: &str = "60";

fn read_prompt_box(herdr: &impl HerdrCommandExecutor, pane_id: &str) -> PromptBox {
    herdr
        .execute(&[
            "pane",
            "read",
            pane_id,
            "--source",
            "recent",
            "--lines",
            PROMPT_BOX_LINES,
            "--format",
            "ansi",
        ])
        .map_or(PromptBox::Unseen, |output| prompt_box(&output.stdout))
}

/// Claude Code draws its prompt box as a `❯` line under a rule, with any
/// further lines of the draft down to the rule that closes it. Earlier
/// prompts in the transcript also start with `❯`, so the box is the last one.
/// It draws its own placeholder and the suggested next prompt faint, so only
/// text drawn at normal intensity is the person's. The cursor is drawn in
/// reverse video, over the first letter of a faint suggestion.
fn prompt_box(screen: &str) -> PromptBox {
    let lines = cells(screen);
    let starts_with = |line: &[Cell], character: char| {
        line.first().is_some_and(|cell| cell.character == character)
    };
    let rule = |line: &Vec<Cell>| starts_with(line, '\u{2500}');
    let Some(open) = (1..lines.len())
        .rev()
        .find(|&index| starts_with(&lines[index], '\u{276f}') && rule(&lines[index - 1]))
    else {
        return PromptBox::Unseen;
    };
    let Some(close) = lines[open + 1..].iter().position(rule) else {
        return PromptBox::Unseen;
    };
    let held: Vec<&Cell> = lines[open..=open + close]
        .iter()
        .flatten()
        .skip(1)
        .collect();
    let hinted = held.iter().any(|cell| cell.faint);
    let typed = held
        .iter()
        .any(|cell| !cell.character.is_whitespace() && !cell.faint && !(cell.cursor && hinted));
    if typed {
        PromptBox::Draft
    } else {
        PromptBox::Empty
    }
}

/// One character on screen, with the attributes that tell the person's text
/// from what Claude Code draws around it.
#[derive(Clone, Copy, Debug)]
struct Cell {
    character: char,
    faint: bool,
    cursor: bool,
}

/// The screen's lines from Herdr's ANSI rendering. Control sequences other
/// than the faint and reverse-video attributes are dropped.
fn cells(screen: &str) -> Vec<Vec<Cell>> {
    let mut lines = vec![Vec::new()];
    let (mut faint, mut cursor) = (false, false);
    let mut characters = screen.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\n' => lines.push(Vec::new()),
            '\u{1b}' if characters.peek() == Some(&'[') => {
                characters.next();
                let mut parameters = String::new();
                for next in characters.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        if next == 'm' {
                            apply_sgr(&parameters, &mut faint, &mut cursor);
                        }
                        break;
                    }
                    parameters.push(next);
                }
            }
            '\u{1b}' if characters.peek() == Some(&']') => {
                // An operating system command ends at BEL or at the string terminator.
                while let Some(next) = characters.next() {
                    if next == '\u{7}'
                        || (next == '\u{1b}' && characters.next_if_eq(&'\\').is_some())
                    {
                        break;
                    }
                }
            }
            '\u{1b}' => {
                characters.next();
            }
            control if control.is_control() => {}
            character => lines
                .last_mut()
                .expect("lines starts with one line")
                .push(Cell {
                    character,
                    faint,
                    cursor,
                }),
        }
    }
    lines
}

fn apply_sgr(parameters: &str, faint: &mut bool, cursor: &mut bool) {
    let codes: Vec<u16> = parameters
        .split(';')
        .map(|code| code.parse().unwrap_or(0))
        .collect();
    let mut index = 0;
    while let Some(code) = codes.get(index) {
        match code {
            0 => (*faint, *cursor) = (false, false),
            2 => *faint = true,
            22 => *faint = false,
            7 => *cursor = true,
            27 => *cursor = false,
            // An extended color's own numbers are not attributes.
            38 | 48 | 58 => {
                index += match codes.get(index + 1) {
                    Some(5) => 2,
                    Some(2) => 4,
                    _ => 0,
                };
            }
            _ => {}
        }
        index += 1;
    }
}

#[cfg(test)]
mod prompt_box_tests {
    use super::*;

    const WAITING_EMPTY: &str =
        include_str!("../tests/fixtures/between-turns/claude-2.1.322-waiting-empty.ansi");
    const DRAFT: &str =
        include_str!("../tests/fixtures/between-turns/claude-2.1.322-waiting-draft.ansi");

    /// The box line of `WAITING_EMPTY` swapped for `line`, as Claude Code
    /// 2.1.322 drew it in the same box.
    fn with_box_line(line: &str) -> String {
        WAITING_EMPTY
            .lines()
            .map(|drawn| {
                if drawn.starts_with('\u{276f}') {
                    line
                } else {
                    drawn
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn only_text_the_person_typed_counts_as_a_draft() {
        assert_eq!(prompt_box(WAITING_EMPTY), PromptBox::Empty);
        assert_eq!(prompt_box(DRAFT), PromptBox::Draft);
        // The suggested next prompt and the first-run placeholder are faint,
        // with the cursor on their first letter.
        for hint in [
            "\u{276f}\u{a0}\u{1b}[0m\u{1b}[7mm\u{1b}[0m\u{1b}[2my unsent draft\u{1b}[0m\r",
            "\u{276f}\u{a0}\u{1b}[0m\u{1b}[7mT\u{1b}[0m\u{1b}[2mry \"write a test for <filepath>\"\u{1b}[0m\r",
        ] {
            assert_eq!(
                prompt_box(&with_box_line(hint)),
                PromptBox::Empty,
                "{hint:?}"
            );
        }
        for typed in [
            // The cursor moved back into the draft.
            "\u{276f}\u{a0}my unsent dr\u{1b}[0m\u{1b}[7ma\u{1b}[0mft\r",
            "\u{276f}\u{a0}\u{1b}[0m\u{1b}[7mx\u{1b}[0m\r",
            // A gray color's numbers are not the faint attribute.
            "\u{276f}\u{a0}\u{1b}[38;2;2;2;2mtyped\u{1b}[0m",
        ] {
            assert_eq!(
                prompt_box(&with_box_line(typed)),
                PromptBox::Draft,
                "{typed:?}"
            );
        }
        let second_line =
            WAITING_EMPTY.replacen("\u{276f}\u{a0}", "\u{276f}\u{a0}\n  and a second line", 1);
        assert_eq!(prompt_box(&second_line), PromptBox::Draft);
    }

    #[test]
    fn a_screen_without_the_box_is_not_read_as_empty() {
        for screen in [
            "",
            "plain shell output\n$ ",
            "\u{276f} an earlier prompt\n\nanswer\n",
        ] {
            assert_eq!(prompt_box(screen), PromptBox::Unseen, "{screen:?}");
        }
        let unclosed: String = WAITING_EMPTY
            .lines()
            .take_while(|line| !line.starts_with('\u{276f}'))
            .chain(["\u{276f}\u{a0}"])
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(prompt_box(&unclosed), PromptBox::Unseen);
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use crate::{AgentSessionId, PaneAddressId, PaneBindingId};

    const CREATED_AT_MS: u64 = 1_000;

    fn attempt(state: DeliveryState) -> InflightDeliveryAttempt {
        InflightDeliveryAttempt {
            attempt_id: DeliveryAttemptId::new("attempt-1"),
            address_id: PaneAddressId::new("address-1"),
            binding_id: PaneBindingId::new("binding-1"),
            state,
            binding_active: true,
            pane_id: "pane-1".to_owned(),
            agent_session_id: AgentSessionId::new("session-1"),
            created_at_ms: CREATED_AT_MS,
            lease_expires_at_ms: None,
        }
    }

    fn pane(session: &str, status: &str) -> DiscoveredPane {
        DiscoveredPane {
            pane_id: "pane-1".to_owned(),
            terminal_id: "terminal-1".to_owned(),
            tab_id: "tab-1".to_owned(),
            workspace_id: "workspace-1".to_owned(),
            cwd: None,
            pane_label: None,
            agent_session_id: Some(AgentSessionId::new(session)),
            agent: None,
            occupant_label: None,
            agent_status: Some(status.to_owned()),
            idle_state_label: None,
        }
    }

    #[test]
    fn an_inflight_attempt_is_observed_against_its_session_and_the_grace() {
        let pending = Observation::PendingGrace {
            due_at_ms: CREATED_AT_MS + ACKNOWLEDGEMENT_GRACE_MS,
        };
        let cases = [
            (
                DeliveryState::Acknowledged,
                "session-1",
                "idle",
                29_000,
                pending,
            ),
            (
                DeliveryState::Acknowledged,
                "session-1",
                "idle",
                30_000,
                Observation::Settle,
            ),
            (
                DeliveryState::Submitted,
                "session-1",
                "idle",
                10_000,
                pending,
            ),
            (
                DeliveryState::Acknowledged,
                "session-1",
                "working",
                29_000,
                Observation::Unchanged,
            ),
            (
                DeliveryState::Acknowledged,
                "session-2",
                "idle",
                29_000,
                Observation::Hold,
            ),
        ];
        for (state, session, status, elapsed_ms, expected) in cases {
            assert_eq!(
                observe_agent_attempt(
                    &attempt(state),
                    Some(&pane(session, status)),
                    false,
                    CREATED_AT_MS + elapsed_ms,
                ),
                expected,
                "{state:?} on {session} {status} after {elapsed_ms} ms"
            );
        }
    }
}
