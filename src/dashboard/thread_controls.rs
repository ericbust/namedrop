use super::snapshot::SpaceKey;
pub(super) use super::snapshot::ThreadAction as Action;
use crate::{
    ActorKind, HerdrCommandExecutor, MutationAttribution, StateStore, StoreError, ThreadId,
};
use serde_json::{Value, json};
use std::{error::Error, path::Path};

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

pub(super) fn apply(
    state: &Path,
    key: &SpaceKey,
    thread_id: &str,
    action: Action,
) -> Result<(crate::StateNamespace, Value), ControlError> {
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
    drop(store);
    let herdr =
        crate::ProcessHerdr::default().with_socket(Some(namespace.session_socket_id.clone()));
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: state,
        home_directory: None,
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: Path::new("/"),
        workspace_id: Some(namespace.space_id.as_str()),
        current_pane_id: None,
        session_socket_id: &namespace.session_socket_id,
        herdr: &herdr,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    let mut receipt = json!({"state": control});
    if let Some(stopped) = stopped {
        let mut interrupted = Vec::new();
        let mut warnings = Vec::new();
        if !stopped.active_responses.is_empty() {
            let live = crate::discover_worktree_for_workspace(&herdr, namespace.space_id.as_str());
            for response in &stopped.active_responses {
                let proven_running = response.binding_active
                    && live.as_ref().is_ok_and(|discovered| {
                        discovered.workspace_id == namespace.space_id.as_str()
                            && discovered.panes.iter().any(|pane| {
                                pane.pane_id == response.pane_id
                                    && pane.agent_session_id.as_ref()
                                        == Some(&response.agent_session_id)
                                    && pane.agent_status.as_deref() == Some("working")
                            })
                    });
                if !proven_running {
                    warnings.push(format!(
                        "Did not interrupt {} because the stopped thread could not be proven to own its current response.",
                        response.pane_id
                    ));
                    continue;
                }
                match herdr.execute(&["agent", "send-keys", &response.pane_id, "ctrl+c"]) {
                    Ok(_) => interrupted.push(response.pane_id.clone()),
                    Err(error) => warnings.push(format!(
                        "Could not interrupt {} after stopping the thread: {error}",
                        response.pane_id
                    )),
                }
            }
        }
        receipt["cancelled_delivery_ids"] = json!(stopped.cancelled_delivery_ids);
        receipt["interrupted_pane_ids"] = json!(interrupted);
        receipt["warnings"] = json!(warnings);
    }
    receipt["drain"] = crate::drain_receipt(&runtime);
    Ok((namespace, receipt))
}
