use crate::{LifecycleRuntime, StateStore};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetInput {
    alias: String,
    members: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveInput {
    alias: String,
}

/// Group mutations are user-requested management, like address management.
/// Only the read-only list is part of the registered collaboration profile.
pub fn execute_group_operation<E, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: crate::Host,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let discovered = match runtime.workspace_id {
        Some(workspace) => runtime
            .host
            .space(crate::SpaceLocator::Workspace(workspace), None)?,
        None => runtime
            .host
            .space(crate::SpaceLocator::Checkout(runtime.cwd), None)?,
    };
    let namespace = discovered.namespace(runtime.session_socket_id);
    let mut store = if operation == "group.list" {
        if !input.as_object().is_some_and(|o| o.is_empty()) {
            return Err("group.list takes no input fields".to_owned());
        }
        let Some(store) =
            StateStore::open_read_only(runtime.state_directory).map_err(|e| e.to_string())?
        else {
            return Ok(json!({"groups":[]}));
        };
        store
    } else {
        StateStore::open(runtime.state_directory).map_err(|e| e.to_string())?
    };
    match operation {
        "group.list" => store.transaction(|tx| {
            tx.list_mention_groups(&namespace)
                .map(|groups| json!({"groups":groups}))
        }),
        "group.set" => {
            let input: SetInput =
                serde_json::from_value(input).map_err(crate::input_schema_error)?;
            store.transaction(|tx| tx.set_mention_group(&namespace, &input.alias, &input.members))
        }
        "group.remove" => {
            let input: RemoveInput =
                serde_json::from_value(input).map_err(crate::input_schema_error)?;
            store.transaction(|tx| tx.remove_mention_group(&namespace, &input.alias))
        }
        _ => return Err(format!("unsupported group operation: {operation}")),
    }
    .map_err(|e| e.to_string())
}
