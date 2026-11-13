use super::snapshot::SpaceKey;
pub(super) use super::snapshot::ThreadAction as Action;
use crate::{
    ActorKind, MutationAttribution, StateStore, StoreError, ThreadId,
    mux::{Budget, MuxConnector},
    pane_effects::{InterruptReport, interrupt_if_current},
};
use serde_json::{Value, json};
use std::{error::Error, path::Path, sync::Arc};

pub(super) enum ControlError {
    NotFound,
    Unavailable,
}

fn store_error(error: StoreError) -> ControlError {
    if error
        .source()
        .and_then(|source| source.downcast_ref::<rusqlite::Error>())
        .is_some_and(|error| matches!(error, rusqlite::Error::QueryReturnedNoRows))
    {
        ControlError::NotFound
    } else {
        ControlError::Unavailable
    }
}

pub(super) fn commit(
    state: &Path,
    key: &SpaceKey,
    thread_id: &str,
    action: Action,
) -> Result<CommittedControl, ControlError> {
    let namespace = StateStore::open_read_only(state)
        .and_then(|store| store.map(|mut store| store.dashboard_spaces()).transpose())
        .map_err(store_error)?
        .and_then(|spaces| {
            spaces
                .into_iter()
                .find(|space| SpaceKey::of(&space.namespace) == *key)
                .map(|space| space.namespace)
        })
        .ok_or(ControlError::NotFound)?;
    let thread = ThreadId::new(thread_id);
    let now = crate::Clock::now_unix_ms(&crate::SystemClock);
    let attribution = MutationAttribution::new(ActorKind::Human, "local-human", now);
    let mut store = StateStore::open(state).map_err(store_error)?;
    if store
        .transaction(|tx| tx.direct_message(&namespace, &thread))
        .map_err(store_error)?
        .is_some()
    {
        return Err(ControlError::NotFound);
    }
    let (control, stopped) = store
        .transaction(|tx| {
            let current = tx.thread_control_state(&namespace, &thread)?;
            if current.stopped {
                return Ok((current, None));
            }
            match action {
                Action::Continue if current.reply_limit_paused => {
                    Ok((tx.continue_thread(&namespace, &thread, &attribution)?, None))
                }
                Action::Continue => Ok((current, None)),
                Action::Stop => {
                    let stopped = tx.stop_thread(&namespace, &thread, &attribution)?;
                    Ok((stopped.state.clone(), Some(stopped)))
                }
            }
        })
        .map_err(store_error)?;
    Ok(CommittedControl {
        namespace,
        control,
        stopped,
    })
}

pub(super) struct CommittedControl {
    namespace: crate::StateNamespace,
    control: crate::ThreadControlState,
    stopped: Option<crate::ThreadStopResult>,
}

impl CommittedControl {
    pub(super) fn respond<T>(
        self,
        state: &Path,
        mux: &dyn MuxConnector,
        completion: CompletionPermit,
        respond: impl FnOnce(&Value) -> T,
        drain: impl FnOnce(&crate::StateNamespace),
    ) -> T {
        let (namespace, receipt) = self.finish(state, mux);
        drop(completion);
        let result = respond(&receipt);
        drain(&namespace);
        result
    }

    fn finish(self, state: &Path, mux: &dyn MuxConnector) -> (crate::StateNamespace, Value) {
        let Self {
            namespace,
            control,
            stopped,
        } = self;
        let mut receipt = json!({"state": control});
        if let Some(stopped) = stopped {
            let responses = &stopped.active_responses;
            let report = match mux.connect(
                &namespace.session_socket_id,
                Budget::ADAPTER_DEFAULT,
                Arc::default(),
            ) {
                Ok(herdr) => interrupt_if_current(state, &namespace, responses, &herdr, None),
                Err(error) => InterruptReport::unreached(responses, &error.to_string()),
            };
            receipt["cancelled_delivery_ids"] = json!(stopped.cancelled_delivery_ids);
            receipt["interrupted_pane_ids"] = json!(report.interrupted_pane_ids);
            receipt["warnings"] = json!(report.warnings);
        }
        (namespace, receipt)
    }
}

#[derive(Clone, Default)]
pub(super) struct ControlEffects(std::sync::Arc<Completion>);

#[derive(Default)]
struct Completion {
    count: std::sync::Mutex<usize>,
    finished: std::sync::Condvar,
}

pub(super) struct CompletionPermit(std::sync::Arc<Completion>);

impl ControlEffects {
    pub(super) fn track(&self) -> CompletionPermit {
        *self
            .0
            .count
            .lock()
            .unwrap_or_else(|error| error.into_inner()) += 1;
        CompletionPermit(self.0.clone())
    }

    pub(super) fn wait(&self) {
        drop(
            self.0
                .finished
                .wait_while(
                    self.0
                        .count
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()),
                    |count| *count != 0,
                )
                .unwrap_or_else(|error| error.into_inner()),
        );
    }
}

impl Drop for CompletionPermit {
    fn drop(&mut self) {
        let mut count = self
            .0
            .count
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *count -= 1;
        self.0.finished.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn a_stop_that_cannot_reach_the_multiplexer_warns_for_each_response() {
        struct Unreachable;
        impl MuxConnector for Unreachable {
            fn connect(
                &self,
                _: &str,
                _: Budget,
                _: Arc<std::sync::atomic::AtomicBool>,
            ) -> Result<Arc<dyn crate::mux::Mux>, crate::mux::MuxError> {
                Err(crate::mux::MuxError::Unavailable(
                    "Herdr is not running".into(),
                ))
            }
        }
        let control = crate::ThreadControlState {
            stopped: true,
            paused: false,
            reply_allowance: 10,
            reply_limit_paused: false,
        };
        let response = |pane: &str| crate::ActiveThreadResponse {
            attempt_id: crate::DeliveryAttemptId::new("attempt"),
            binding_id: crate::PaneBindingId::new("binding"),
            pane_id: pane.into(),
            agent_session_id: crate::AgentSessionId::new("session"),
            binding_active: true,
        };
        let committed = CommittedControl {
            namespace: crate::StateNamespace::for_space("socket", "space"),
            control: control.clone(),
            stopped: Some(crate::ThreadStopResult {
                state: control,
                cancelled_delivery_ids: Vec::new(),
                active_responses: vec![response("w1:p1"), response("w1:p2")],
            }),
        };
        let (_, receipt) = committed.finish(
            Path::new("/nonexistent-namedrop-control-test"),
            &Unreachable,
        );
        assert_eq!(receipt["interrupted_pane_ids"], json!([]));
        assert_eq!(
            receipt["warnings"],
            json!([
                "Could not confirm interruption of w1:p1 after stopping the thread: Herdr is not running.",
                "Could not confirm interruption of w1:p2 after stopping the thread: Herdr is not running.",
            ])
        );
    }

    #[test]
    fn handoff_waits_for_effects_and_slow_response_releases_completion() {
        let effects = ControlEffects::default();
        let permit = effects.track();
        let (waiting, entered) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let (responding, response_entered) = mpsc::channel();
        let (release, response_release) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                waiting.send(()).unwrap();
                effects.wait();
                finished.send(()).unwrap();
            });
            entered.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(
                done.recv_timeout(Duration::from_millis(50)).is_err(),
                "handoff passed committed effects"
            );
            scope.spawn(move || {
                let committed = CommittedControl {
                    namespace: crate::StateNamespace::for_space("socket", "space"),
                    control: crate::ThreadControlState {
                        stopped: false,
                        paused: false,
                        reply_allowance: 10,
                        reply_limit_paused: false,
                    },
                    stopped: None,
                };
                let result = committed.respond(
                    Path::new("/nonexistent-namedrop-control-test"),
                    &crate::mux::Connector::new(std::path::Path::new("")),
                    permit,
                    |_| {
                        responding.send(()).unwrap();
                        response_release.recv().unwrap();
                        Err::<(), _>("disconnected response")
                    },
                    |namespace| assert_eq!(namespace.space_id.as_str(), "space"),
                );
                assert_eq!(result, Err("disconnected response"));
            });
            response_entered
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            let completed = done.recv_timeout(Duration::from_secs(1));
            release.send(()).unwrap();
            completed.expect("slow response retained control effects completion");
        });
    }
}
