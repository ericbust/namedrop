use crate::{
    ActorKind, LifecycleRuntime, MutationAttribution, PaneAddressId, StateStore, ThreadId,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressInput {
    address_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadInput {
    thread_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelHeldInput {
    attempt_id: Option<String>,
    delivery_id: Option<String>,
    #[serde(default)]
    confirmed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeHandoffInput {
    coordination_id: String,
    #[serde(default)]
    confirmed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeDeliveryInput {
    attempt_id: String,
    #[serde(default)]
    confirmed: bool,
}

pub fn execute_control_operation<H, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, H, C, I>,
) -> Result<Value, String>
where
    H: crate::Host,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let discovered = runtime.host.space(runtime.space_locator(), None)?;
    let namespace = discovered.namespace(runtime.session_socket_id);
    let attribution =
        MutationAttribution::new(ActorKind::Human, "local-human", runtime.clock.now_unix_ms());
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    let mut result = match operation {
        "handoff.acknowledge" => {
            let input: AcknowledgeHandoffInput = parse_input(input)?;
            if !input.confirmed {
                return Err("acknowledging a handoff warning requires explicit human confirmation; set confirmed:true after reviewing its saved report".into());
            }
            let id =
                crate::CoordinationId::new(required(input.coordination_id, "coordination_id")?);
            let changed = store
                .transaction(|transaction| {
                    transaction.acknowledge_coordination_attention(&namespace, &id, &attribution)
                })
                .map_err(|error| error.to_string())?;
            json!({"coordination_id":id, "acknowledged":true, "changed":changed,
                "notice":"Warning acknowledged. The original outcome and report are unchanged; no work was resumed or delivery cancelled."})
        }
        "delivery.acknowledge" => {
            let input: AcknowledgeDeliveryInput = parse_input(input)?;
            if !input.confirmed {
                return Err("acknowledging a missing disposition requires explicit human confirmation; set confirmed:true after confirming the recipient is retired or its session was replaced".into());
            }
            let id = crate::DeliveryAttemptId::new(required(input.attempt_id, "attempt_id")?);
            let changed = store
                .transaction(|transaction| {
                    transaction.acknowledge_disposition_attention(&namespace, &id, &attribution)
                })
                .map_err(|error| error.to_string())?;
            json!({"attempt_id":id, "acknowledged":true, "changed":changed,
                "notice":"Warning acknowledged. The delivery and its history are unchanged; no disposition was recorded and nothing was resent."})
        }
        "delivery.cancel-held" => {
            let input: CancelHeldInput = parse_input(input)?;
            if !input.confirmed {
                return Err("cancelling a held delivery requires explicit human confirmation; set confirmed:true after reviewing its cause and target".to_owned());
            }
            let (state, mut result) = match (input.attempt_id, input.delivery_id) {
                (Some(attempt_id), None) => {
                    let attempt_id =
                        crate::DeliveryAttemptId::new(required(attempt_id, "attempt_id")?);
                    let state = store
                        .transaction(|transaction| {
                            transaction.cancel_held_attempt(&namespace, &attempt_id, &attribution)
                        })
                        .map_err(|error| error.to_string())?;
                    (state, json!({"attempt_id":attempt_id}))
                }
                (None, Some(delivery_id)) => {
                    let delivery_id = crate::DeliveryId::new(required(delivery_id, "delivery_id")?);
                    let state = store
                        .transaction(|transaction| {
                            transaction.cancel_unsent_held_delivery(
                                &namespace,
                                &delivery_id,
                                &attribution,
                            )
                        })
                        .map_err(|error| error.to_string())?;
                    (state, json!({"delivery_id":delivery_id}))
                }
                _ => {
                    return Err(
                        "cancel-held requires exactly one attempt_id or unsent delivery_id"
                            .to_owned(),
                    );
                }
            };
            result["state"] = json!(state);
            result["notice"] = json!(
                "Cancellation is recorded; sent work is not retracted and the agent is not stopped. No retry was requested."
            );
            result
        }
        "address.enable" | "address.disable" => {
            let input: AddressInput = parse_input(input)?;
            let address_id = PaneAddressId::new(required(input.address_id, "address_id")?);
            let record = store
                .transaction(|transaction| {
                    transaction.set_address_collaboration_disabled(
                        &namespace,
                        &address_id,
                        operation == "address.disable",
                        &attribution,
                    )
                })
                .map_err(|error| error.to_string())?;
            serde_json::to_value(record).map_err(|error| error.to_string())?
        }
        "address.allow-remote" => {
            let input: AddressInput = parse_input(input)?;
            let address_id = PaneAddressId::new(required(input.address_id, "address_id")?);
            let invoker = runtime
                .current_pane_id
                .and_then(|id| discovered.panes.iter().find(|pane| pane.pane_id == id))
                .ok_or("allow-remote must run from a Herdr pane in this workspace")?;
            let record = store
                .transaction(|transaction| {
                    transaction.allow_remote_commands(
                        &namespace,
                        &address_id,
                        invoker,
                        &discovered.panes,
                        &attribution,
                    )
                })
                .map_err(|error| error.to_string())?;
            serde_json::to_value(record).map_err(|error| error.to_string())?
        }
        "address.deny-remote" => {
            let input: AddressInput = parse_input(input)?;
            let address_id = PaneAddressId::new(required(input.address_id, "address_id")?);
            let record = store
                .transaction(|transaction| {
                    transaction.deny_remote_commands(&namespace, &address_id, &attribution)
                })
                .map_err(|error| error.to_string())?;
            serde_json::to_value(record).map_err(|error| error.to_string())?
        }
        "thread.pause" | "thread.resume" | "thread.continue" => {
            let input: ThreadInput = parse_input(input)?;
            let thread_id = ThreadId::new(required(input.thread_id, "thread_id")?);
            let state = store
                .transaction(|transaction| match operation {
                    "thread.pause" => {
                        transaction.pause_thread(&namespace, &thread_id, &attribution)
                    }
                    "thread.resume" => {
                        transaction.resume_thread(&namespace, &thread_id, &attribution)
                    }
                    "thread.continue" => {
                        transaction.continue_thread(&namespace, &thread_id, &attribution)
                    }
                    _ => unreachable!("operation was matched"),
                })
                .map_err(|error| error.to_string())?;
            json!({"state": state})
        }
        "thread.stop" => {
            let input: ThreadInput = parse_input(input)?;
            let thread_id = ThreadId::new(required(input.thread_id, "thread_id")?);
            let stopped = store
                .transaction(|transaction| {
                    transaction.stop_thread(&namespace, &thread_id, &attribution)
                })
                .map_err(|error| error.to_string())?;
            let mut interrupted_pane_ids = Vec::new();
            let mut warnings = Vec::new();
            let live_discovered = runtime.host.space(runtime.space_locator(), None);
            let live_discovered = match live_discovered {
                Ok(live_discovered) => Some(live_discovered),
                Err(error) => {
                    warnings.push(format!(
                        "The thread was stopped, but active responses were not interrupted because live pane state could not be refreshed: {error}"
                    ));
                    None
                }
            };
            for response in &stopped.active_responses {
                let live = live_discovered
                    .as_ref()
                    .and_then(|discovered| {
                        (discovered.workspace_id == namespace.space_id.as_str())
                            .then_some(discovered)
                    })
                    .and_then(|discovered| {
                        discovered
                            .panes
                            .iter()
                            .find(|pane| pane.pane_id == response.pane_id)
                    });
                let proven_running = owns_running_response(response, live);
                if !proven_running {
                    warnings.push(format!(
                        "Did not interrupt {} because the stopped thread could not be proven to own its current response.",
                        response.pane_id
                    ));
                    continue;
                }
                match runtime.host.interrupt(&response.pane_id) {
                    Ok(_) => interrupted_pane_ids.push(response.pane_id.clone()),
                    Err(error) => warnings.push(format!(
                        "Could not interrupt {} after stopping the thread: {error}",
                        response.pane_id
                    )),
                }
            }
            let mut value = serde_json::to_value(stopped).map_err(|error| error.to_string())?;
            value["interrupted_pane_ids"] = json!(interrupted_pane_ids);
            value["warnings"] = json!(warnings);
            value
        }
        "worktree.pause" | "worktree.resume" => {
            parse_input::<EmptyInput>(input)?;
            let state = store
                .transaction(|transaction| {
                    transaction.set_worktree_paused(
                        &namespace,
                        operation == "worktree.pause",
                        &attribution,
                    )
                })
                .map_err(|error| error.to_string())?;
            json!({"state": state})
        }
        _ => return Err(format!("{operation} is not a control operation")),
    };
    if matches!(
        operation,
        "address.enable"
            | "thread.resume"
            | "thread.continue"
            | "thread.stop"
            | "worktree.resume"
            | "delivery.cancel-held"
    ) {
        result["drain"] = crate::drain_receipt(runtime);
    }
    Ok(result)
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T, String> {
    serde_json::from_value(input).map_err(crate::input_schema_error)
}

fn required(value: String, field: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(value)
    }
}

fn owns_running_response(
    response: &crate::ActiveThreadResponse,
    pane: Option<&crate::DiscoveredPane>,
) -> bool {
    response.binding_active
        && pane.is_some_and(|pane| {
            pane.agent_session_id.as_ref() == Some(&response.agent_session_id)
                && pane.agent_status.as_deref() == Some("working")
        })
}

#[cfg(test)]
mod hc_status_tests {
    #[test]
    fn hc_status_words_allow_interrupt_only_for_the_owned_working_session() {
        for word in crate::host::hc::testing::WORDS {
            let pane = crate::host::hc::testing::pane(word);
            let mut response = crate::ActiveThreadResponse {
                attempt_id: crate::DeliveryAttemptId::new("attempt"),
                pane_id: pane.pane_id.clone(),
                agent_session_id: pane.agent_session_id.clone().unwrap(),
                binding_active: true,
            };
            assert_eq!(
                super::owns_running_response(&response, Some(&pane)),
                word == Some("working"),
                "{word:?}"
            );
            assert!(!super::owns_running_response(&response, None));
            response.binding_active = false;
            assert!(!super::owns_running_response(&response, Some(&pane)));
            response.binding_active = true;
            response.agent_session_id = crate::AgentSessionId::new("replaced-session");
            assert!(!super::owns_running_response(&response, Some(&pane)));
        }
    }
}
