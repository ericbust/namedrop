use crate::{
    ActorKind, Clock, DeliveryAttemptId, DeliveryState, DiscoveredPane, DispatchCandidate,
    HerdrCommandExecutor, IdGenerator, IntegrationToolInvocation, LeaseDraft, LifecycleRuntime,
    MutationAttribution, PromptFailureKind, StateNamespace, StateStore, StateTransaction,
    StoreError, collaboration_executable, discover_worktree, discover_worktree_for_workspace,
    tool_environment,
};
use serde::Serialize;
use serde_json::{Value, json};

const LEASE_DURATION_MS: u64 = 30_000;
const ACKNOWLEDGEMENT_GRACE_MS: u64 = 30_000;
const QUEUED_HOLD_AFTER_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DrainReport {
    pub terminal_waiting: Vec<crate::PaneAddressId>,
    pub terminal_running: Vec<DeliveryAttemptId>,
    pub delivery_notice: &'static str,
    pub submitted: Vec<DeliveryAttemptId>,
    pub requeued: Vec<DeliveryAttemptId>,
    pub failed: Vec<DeliveryAttemptId>,
    pub settled: Vec<DeliveryAttemptId>,
    pub held: Vec<DeliveryAttemptId>,
    pub expired: Vec<crate::DeliveryId>,
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
        }
    }
}

pub(crate) fn drain_receipt<E, C, I>(runtime: &LifecycleRuntime<'_, E, C, I>) -> Value
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
        let same_session = attempt.binding_active
            && pane.and_then(|pane| pane.agent_session_id.as_ref())
                == Some(&attempt.agent_session_id);
        let session_is_settled = pane
            .and_then(|pane| pane.agent_status.as_deref())
            .is_some_and(|status| matches!(status, "idle" | "done"));
        let acknowledgement_grace_elapsed = attribution
            .occurred_at_ms
            .saturating_sub(attempt.created_at_ms)
            >= ACKNOWLEDGEMENT_GRACE_MS;
        let must_hold = (restart
            && matches!(
                attempt.state,
                DeliveryState::Dispatching | DeliveryState::Submitted
            ))
            || !same_session
            || (attempt.state == DeliveryState::Dispatching
                && attempt
                    .lease_expires_at_ms
                    .is_none_or(|expires_at_ms| expires_at_ms <= attribution.occurred_at_ms))
            || (attempt.state == DeliveryState::Submitted
                && session_is_settled
                && acknowledgement_grace_elapsed);
        let next = if must_hold {
            Some(DeliveryState::Held)
        } else if attempt.state == DeliveryState::Acknowledged
            && session_is_settled
            && acknowledgement_grace_elapsed
        {
            Some(DeliveryState::Settled)
        } else {
            None
        };
        match next {
            Some(DeliveryState::Held) => {
                transaction.transition_delivery_attempt(
                    namespace,
                    &attempt.attempt_id,
                    DeliveryState::Held,
                    attribution,
                )?;
                report.held.push(attempt.attempt_id);
            }
            Some(DeliveryState::Settled) => {
                transaction.transition_delivery_attempt(
                    namespace,
                    &attempt.attempt_id,
                    DeliveryState::Settled,
                    attribution,
                )?;
                report.settled.push(attempt.attempt_id);
            }
            Some(_) | None => {}
        }
    }
    Ok(report)
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
    let awaiting_token = format!("namedrop-awaiting={waiting}");
    let attention_token = format!("namedrop-attention={attention}");
    for binding in bindings {
        if let Some(pane) = discovered
            .panes
            .iter()
            .find(|pane| pane.pane_id == binding.pane_id)
        {
            // Show the connect hint while the session is missing, and clear it
            // once Herdr reports one; otherwise leave the labels as named.
            if let Some(address) = addresses
                .iter()
                .find(|address| !address.retired && address.id == binding.address_id)
            {
                let awaiting_session = crate::lifecycle::awaits_agent_session(pane);
                let showing_hint = pane.idle_state_label.as_deref()
                    == Some(crate::lifecycle::connect_hint_label(&address.alias).as_str());
                if awaiting_session != showing_hint {
                    crate::lifecycle::publish_pane_address_metadata(
                        runtime,
                        &binding.pane_id,
                        &address.alias,
                        awaiting_session,
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
    store
        .transaction(|transaction| {
            transaction.complete_ready_result_sets(
                &namespace,
                &MutationAttribution::new(ActorKind::Plugin, "namedrop-dispatch", now),
            )?;
            transaction.reconcile_coordinations(
                &namespace,
                &first_discovery,
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
                && !crate::terminal::shell_is_available(runtime.herdr, &first.pane_id))
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
                    || !crate::terminal::shell_is_available(runtime.herdr, &second.pane_id)))
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
                    &second_discovery,
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
                            transaction.prepare_terminal_execution(
                                &namespace,
                                &leased.attempt_id,
                                &pane.terminal_id,
                            )?;
                            let command = crate::terminal::runner_command(
                                runtime,
                                &second_discovery.workspace_id,
                                &pane.pane_id,
                                &leased.attempt_id,
                            )
                            .map_err(StoreError::invalid_input)?;
                            return Ok((leased, command));
                        }
                        transaction
                            .delivery_envelope(&namespace, &leased.attempt_id, tool)
                            .and_then(|envelope| {
                                serde_json::to_string(&envelope)
                                    .map(|prompt| (leased, prompt))
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
        let prompt_result = if second.pane_type == crate::PaneType::Terminal {
            runtime
                .herdr
                .execute(&["pane", "run", &second.pane_id, &prompt])
        } else {
            runtime
                .herdr
                .execute(&["agent", "prompt", &second.pane_id, &prompt])
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
            Err(error)
                if runtime.herdr.classify_prompt_failure(&error)
                    == PromptFailureKind::TemporaryPreInvocation =>
            {
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
            Err(error)
                if runtime.herdr.classify_prompt_failure(&error)
                    == PromptFailureKind::DefinitiveNonRetryable =>
            {
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

fn discover<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<crate::DiscoveredWorktree, String>
where
    E: HerdrCommandExecutor,
{
    match runtime.workspace_id {
        Some(workspace_id) => discover_worktree_for_workspace(runtime.herdr, workspace_id),
        None => discover_worktree(runtime.herdr, runtime.cwd),
    }
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
