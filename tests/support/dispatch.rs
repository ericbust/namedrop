//! Send, reply and complete only commit. The CLI then wakes the mailbox worker
//! of every space `spaces_to_wake` names; these wrappers run that worker's
//! drain in-process, so a test observes what an agent sees after the command.
#![allow(dead_code)]

use namedrop::{
    Clock, IdGenerator, LifecycleRuntime, drain_mailboxes, execute_integration_operation,
    execute_message_operation, spaces_to_wake,
};
use serde_json::Value;

pub fn wake<M, C, I>(runtime: &LifecycleRuntime<'_, M, C, I>)
where
    M: namedrop::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let Some(here) = runtime.workspace_id else {
        let _ = drain_mailboxes(runtime);
        return;
    };
    for space in spaces_to_wake(runtime.state_directory, runtime.session_socket_id, here) {
        let _ = drain_mailboxes(&LifecycleRuntime {
            workspace_id: Some(&space),
            ..*runtime
        });
    }
}

pub fn message<M, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, M, C, I>,
) -> Result<Value, String>
where
    M: namedrop::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let result = execute_message_operation(operation, input, runtime);
    if result.is_ok() {
        wake(runtime);
    }
    result
}

pub fn integration<M, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, M, C, I>,
) -> Result<Value, String>
where
    M: namedrop::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let result = execute_integration_operation(operation, input, runtime);
    if matches!(operation, "complete" | namedrop::final_reply::STOP_NO_REPLY) && result.is_ok() {
        wake(runtime);
    }
    result
}
