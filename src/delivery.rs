use crate::lifecycle::PaneLabel;
use crate::{
    ActorKind, Clock, DeliveryAttemptId, DeliveryState, DiscoveredPane, DispatchCandidate, Host,
    IdGenerator, InflightDeliveryAttempt, IntegrationToolInvocation, LeaseDraft, LifecycleRuntime,
    MutationAttribution, PromptFailureKind, StateNamespace, StateStore, StateTransaction,
    StoreError, collaboration_executable, tool_environment,
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
    pub requeued: Vec<DeliveryAttemptId>,
    pub failed: Vec<DeliveryAttemptId>,
    pub settled: Vec<DeliveryAttemptId>,
    pub held: Vec<DeliveryAttemptId>,
    pub expired: Vec<crate::DeliveryId>,
    pub pending_grace: Vec<GraceDeadline>,
    pub next_lease_due_ms: Option<u64>,
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
            requeued: Vec::new(),
            failed: Vec::new(),
            settled: Vec::new(),
            held: Vec::new(),
            expired: Vec::new(),
            pending_grace: Vec::new(),
            next_lease_due_ms: None,
        }
    }
}

impl DrainReport {
    pub fn awaits_retry(&self) -> bool {
        !self.requeued.is_empty() || self.awaits_terminal()
    }

    pub fn awaits_terminal(&self) -> bool {
        !self.terminal_waiting.is_empty() || !self.terminal_running.is_empty()
    }

    pub fn next_grace_due_ms(&self) -> Option<u64> {
        self.pending_grace
            .iter()
            .map(|deadline| deadline.due_at_ms)
            .min()
    }
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

pub(crate) fn drain_receipt<H, C, I>(runtime: &LifecycleRuntime<'_, H, C, I>) -> Value
where
    H: Host,
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
    pub next_lease_due_ms: Option<u64>,
}

pub(crate) fn reconcile_delivery_state(
    transaction: &mut StateTransaction<'_>,
    namespace: &StateNamespace,
    panes: &[DiscoveredPane],
    restart: bool,
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
                report.settled.push(attempt.attempt_id);
            }
            Observation::PendingGrace { due_at_ms } => report.pending_grace.push(GraceDeadline {
                attempt_id: attempt.attempt_id,
                due_at_ms,
            }),
            Observation::PendingLease { due_at_ms } => {
                report.next_lease_due_ms = Some(
                    report
                        .next_lease_due_ms
                        .map_or(due_at_ms, |previous| previous.min(due_at_ms)),
                );
            }
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
    PendingLease { due_at_ms: u64 },
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
        DeliveryState::Dispatching => Observation::PendingLease {
            due_at_ms: attempt
                .lease_expires_at_ms
                .expect("unexpired lease checked"),
        },
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

pub(crate) fn publish_status_metadata<H, C, I>(
    runtime: &LifecycleRuntime<'_, H, C, I>,
    discovered: &crate::DiscoveredWorktree,
) -> Result<(), String>
where
    H: Host,
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
                if !runtime.is_hc()
                    && label.replaces(&address.alias, pane.idle_state_label.as_deref())
                {
                    crate::lifecycle::publish_pane_address_metadata(
                        runtime,
                        &binding.pane_id,
                        &address.alias,
                        label,
                    )?;
                }
            }
            runtime.host.report_metadata(
                &binding.pane_id,
                &crate::PaneMetadata::Counters {
                    awaiting: waiting,
                    attention,
                },
            )?;
        }
    }
    Ok(())
}

pub fn drain_mailboxes<H, C, I>(
    runtime: &LifecycleRuntime<'_, H, C, I>,
) -> Result<DrainReport, String>
where
    H: Host,
    C: Clock,
    I: IdGenerator,
{
    let first_discovery = discover(runtime)?;
    let namespace = first_discovery.namespace(runtime.session_socket_id);
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    let reconciliation_completed = store
        .transaction(|transaction| transaction.reconciliation_completed(&namespace))
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
            && crate::terminal::shell_is_available(&runtime.host, &attempt.pane_id)
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
            reconcile_delivery_state(
                transaction,
                &namespace,
                &first_discovery.panes,
                false,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now),
            )
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
        crate::spaces::LiveSpaces::load(&runtime.host, first_discovery.clone(), &remote_seats);
    store
        .transaction(|transaction| {
            transaction.complete_ready_result_sets(
                &namespace,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now),
            )?;
            transaction.reconcile_coordinations(
                &namespace,
                &live,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-coordination", now),
            )?;
            Ok(())
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
        next_lease_due_ms: reconciled.next_lease_due_ms,
        ..DrainReport::default()
    };

    for address_id in addresses {
        let first = store
            .transaction(|transaction| transaction.dispatch_candidate(&namespace, &address_id))
            .map_err(|error| error.to_string())?;
        let Some(first) = first else {
            continue;
        };
        if !live_candidate_matches(&first, &first_discovery)
            || (first.pane_type == crate::PaneType::Terminal
                && !crate::terminal::shell_is_available(&runtime.host, &first.pane_id))
        {
            if first.pane_type == crate::PaneType::Terminal {
                report.terminal_waiting.push(address_id);
            }
            continue;
        }

        runtime.clock.wait_ms(first.settled_delay_ms);
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
            || !live_candidate_matches(&second, &second_discovery)
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
                    || !crate::terminal::shell_is_available(&runtime.host, &second.pane_id)))
        {
            if second.pane_type == crate::PaneType::Terminal {
                report.terminal_waiting.push(address_id);
            }
            continue;
        }

        let now = runtime.clock.now_unix_ms();
        let attribution = MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now);
        let attempt_id = DeliveryAttemptId::new(runtime.ids.next_id("delivery-attempt"));
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
                if !transaction.coordination_wake_message_eligible(
                    &namespace,
                    &second.message_id,
                    &live.with_here(second_discovery.clone()),
                )? {
                    return Ok(None);
                }
                let leased = transaction.lease_mailbox_head(
                    &namespace,
                    &address_id,
                    &LeaseDraft {
                        attempt_id: attempt_id.clone(),
                        binding_id: second.binding_id.clone(),
                        agent_session_id: second.agent_session_id.clone(),
                        retry_of_attempt_id: second.retry_of_attempt_id.clone(),
                        owner: format!("dispatcher:{}", runtime.session_socket_id),
                        acquired_at_ms: now,
                        expires_at_ms: now.saturating_add(LEASE_DURATION_MS),
                    },
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
                                serde_json::to_string(&envelope)
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
        let prompt_result = if second.pane_type == crate::PaneType::Terminal {
            runtime.host.run_in_pane(&second.pane_id, &prompt)
        } else {
            runtime.host.prompt(&second.pane_id, &prompt)
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
            Err(error) if error.kind == PromptFailureKind::TemporaryPreInvocation => {
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
            }
            Err(error) if error.kind == PromptFailureKind::DefinitiveNonRetryable => {
                store
                    .transaction(|transaction| {
                        transaction.transition_delivery_attempt(
                            &namespace,
                            &leased.attempt_id,
                            DeliveryState::Failed,
                            &attribution,
                        )
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

    report.terminal_running = store
        .transaction(|tx| {
            tx.inflight_delivery_attempts(&namespace)?
                .into_iter()
                .filter_map(|attempt| {
                    match tx.terminal_execution(&namespace, &attempt.attempt_id) {
                        Ok(Some(_)) => Some(Ok(attempt.attempt_id)),
                        Ok(None) => None,
                        Err(e) => Some(Err(e)),
                    }
                })
                .collect::<Result<Vec<_>, StoreError>>()
        })
        .map_err(|e| e.to_string())?;
    publish_status_metadata(runtime, &first_discovery)?;
    Ok(report)
}

fn discover<H, C, I>(
    runtime: &LifecycleRuntime<'_, H, C, I>,
) -> Result<crate::DiscoveredWorktree, String>
where
    H: Host,
{
    runtime
        .host
        .space(runtime.space_locator(), None)
        .map_err(String::from)
}

fn live_candidate_matches(
    candidate: &DispatchCandidate,
    discovered: &crate::DiscoveredWorktree,
) -> bool {
    discovered.panes.iter().any(|pane| {
        pane.pane_id == candidate.pane_id
            && if candidate.pane_type == crate::PaneType::Terminal {
                pane.agent.is_none() && pane.agent_session_id.is_none()
            } else {
                pane.agent_session_id.as_ref() == Some(&candidate.agent_session_id)
                    && pane
                        .agent_status
                        .as_deref()
                        .is_some_and(|status| matches!(status, "idle" | "done"))
            }
    })
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
            status_reported: None,
            idle_state_label: None,
        }
    }

    #[test]
    fn hc_status_words_gate_observation_and_dispatch() {
        for word in crate::host::hc::testing::WORDS {
            let pane = crate::host::hc::testing::pane(word);
            let settled = matches!(word, Some("idle" | "done"));
            let mut attempt = attempt(DeliveryState::Acknowledged);
            attempt.pane_id = pane.pane_id.clone();
            attempt.agent_session_id = pane.agent_session_id.clone().unwrap();
            assert_eq!(
                observe_agent_attempt(&attempt, Some(&pane), false, 40_000),
                if settled {
                    Observation::Settle
                } else {
                    Observation::Unchanged
                },
                "{word:?}"
            );
            let candidate = DispatchCandidate {
                pane_type: crate::PaneType::Agent,
                address_id: attempt.address_id.clone(),
                binding_id: attempt.binding_id.clone(),
                pane_id: pane.pane_id.clone(),
                agent_session_id: attempt.agent_session_id.clone(),
                registration_id: "registration".into(),
                message_id: crate::MessageId::new("message"),
                retry_of_attempt_id: None,
                settled_delay_ms: 500,
            };
            let discovered = crate::DiscoveredWorktree {
                worktree_id: crate::WorktreeId::new("hc-space:fixture-space"),
                workspace_id: "fixture-space".into(),
                worktree_attached: false,
                label: None,
                branch: None,
                panes: vec![pane],
            };
            assert_eq!(
                live_candidate_matches(&candidate, &discovered),
                settled,
                "{word:?}"
            );
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
