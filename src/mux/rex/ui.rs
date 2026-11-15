use super::{Fault, Narrower, Wire, local};
use crate::mux::{Gone, MuxError};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn report(wire: &impl Wire, socket: &Path, record: &Value) -> Result<Value, MuxError> {
    let (mut control, peer) = wire.open(Narrower::none()).map_err(fault)?;
    local::prove_local(socket, &mut *control, peer).map_err(fault)?;
    let keymap = control
        .request("client.keymap", Some(json!({})))
        .map_err(fault)?
        .payload;
    // The reply wraps the action's return value: {"client_actions": [], "result": …}.
    let about = control
        .request("client.action", Some(json!({"action":"namedrop_about"})))
        .map(|reply| reply.payload["result"].clone());
    Ok(readback(record, &keymap, about))
}

pub(super) fn configuration(
    wire: &impl Wire,
    socket: &Path,
    method: &str,
    record: Option<&Value>,
    deadline: std::time::Instant,
) -> Result<Value, MuxError> {
    let narrower = Narrower {
        budget: Box::new(move || {
            let timeout = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or("Rex configuration budget expired")?;
            Ok(crate::mux::Budget {
                timeout,
                max_output_bytes: super::control::MAX_CALL_BYTES,
            })
        }),
        cancelled: None,
    };
    let (mut control, peer) = wire.open(narrower).map_err(fault)?;
    local::prove_local(socket, &mut *control, peer).map_err(fault)?;
    let result = control
        .request(method, Some(json!({})))
        .map_err(fault)?
        .payload;
    let Some(record) = record else {
        return Ok(result);
    };
    if method == "server.config_reload" && has_errors(&result) {
        return Err(MuxError::Protocol(format!(
            "Rex rejected its configuration: {result}"
        )));
    }
    let keymap = if method == "client.keymap" {
        result
    } else {
        control
            .request("client.keymap", Some(json!({})))
            .map_err(fault)?
            .payload
    };
    let about = control
        .request("client.action", Some(json!({"action":"namedrop_about"})))
        .map(|reply| reply.payload["result"].clone());
    Ok(readback(record, &keymap, about))
}

pub(crate) fn has_errors(reply: &Value) -> bool {
    reply["diagnostics"]
        .as_array()
        .is_some_and(|items| items.iter().any(|d| d["severity"] == "error"))
}

fn fault(fault: Fault) -> MuxError {
    super::wire::mux_error(fault, Gone::Pane)
}

fn readback(record: &Value, keymap: &Value, about: Result<Value, Fault>) -> Value {
    let error = about.as_ref().err().map(|e| fault(e.clone()).to_string());
    let about = about.unwrap_or(Value::Null);
    let actions = keymap["actions"].as_array().cloned().unwrap_or_default();
    let present = |name: &str| actions.iter().any(|a| a["name"] == name);
    let actions_complete = crate::rex_ui::ACTIONS
        .iter()
        .all(|a| present(&format!("namedrop_{}", a.name)))
        && present("namedrop_about");
    let module = about["module"]
        .as_str()
        .map(|p| p.strip_prefix('@').unwrap_or(p));
    let confirmed = module
        .is_some_and(|p| Path::new(p).is_absolute() && Some(p) == record["module"].as_str())
        && about["launcher"]
            .as_str()
            .is_some_and(|s| Some(s) == record["launcher"].as_str())
        && about["state"]
            .as_str()
            .is_some_and(|s| Some(s) == record["state"].as_str())
        && actions_complete;
    let bindings = keymap["bindings"].as_array().cloned().unwrap_or_default();
    let keys: Vec<_> = crate::rex_ui::ACTIONS
        .iter()
        .filter_map(|a| {
            let key = a.key?;
            let action = format!("namedrop_{}", a.name);
            let on_key: Vec<_> = bindings
                .iter()
                .filter(|b| {
                    b["keys"] == json!([key]) && b["mode"].as_str().is_none_or(str::is_empty)
                })
                .collect();
            let bound = on_key.iter().any(|b| b["action"] == action);
            let top = on_key.into_iter().find(|b| {
                b["action"]
                    .as_str()
                    .is_some_and(|name| name.contains('.') || present(name))
            });
            let status = if !bound {
                "skipped"
            } else if top.is_some_and(|b| b["action"] == action) {
                "in_effect"
            } else {
                "shadowed"
            };
            Some(json!({
                "key": key,
                "action": action,
                "status": status,
                "source": top.and_then(|b| b.get("source")),
                "effective_action": top.and_then(|b| b.get("action")),
            }))
        })
        .collect();
    // About runs in the server; never substitute the command's environment.
    let env: serde_json::Map<_, _> = [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "REX_SERVER",
    ]
    .into_iter()
    .map(|k| (k.to_owned(), about["env"][k].clone()))
    .collect();
    json!({"confirmed":confirmed, "module":record["module"], "launcher":record["launcher"], "state":record["state"], "line":record["line"], "config":keymap["config"], "about":about, "env":env, "keys":keys, "actions_complete":actions_complete, "diagnostics":keymap["diagnostics"], "error":error})
}

#[cfg(test)]
mod tests {
    use super::super::tests::{PEER, SOCKET, Script, status};
    use super::*;
    use std::sync::atomic::Ordering;

    fn record() -> Value {
        json!({"module":"/cfg/namedrop.lua", "launcher":"/state/runtime/namedrop", "state":"/state", "config":"/cfg/init.lua", "line":"inserted"})
    }
    fn about() -> Value {
        json!({"module":"@/cfg/namedrop.lua", "launcher":"/state/runtime/namedrop", "state":"/state", "env":{"HOME":"/server/home", "XDG_STATE_HOME":"/server/state", "REX_SERVER":"unix:///tmp/r.sock"}})
    }
    fn keymap() -> Value {
        let mut actions: Vec<_> = crate::rex_ui::ACTIONS
            .iter()
            .map(|a| json!({"name":format!("namedrop_{}", a.name)}))
            .collect();
        actions.extend([json!({"name":"namedrop_about"}), json!({"name":"mine"})]);
        json!({"config":{"path":"/cfg/init.lua", "exists":true}, "actions":actions, "bindings":[
            {"keys":["alt+shift+cmd+i"], "action":"not_available", "source":"init.lua:9"},
            {"keys":["alt+shift+cmd+i"], "action":"namedrop_name", "source":"namedrop.lua:39"},
            {"keys":["alt+shift+cmd+m"], "action":"mine", "source":"init.lua:3"},
            {"keys":["alt+shift+cmd+m"], "action":"namedrop_manage", "source":"namedrop.lua:39"},
            {"keys":["alt+shift+cmd+h"], "mode":"resize", "action":"mine", "source":"init.lua:4"},
            {"keys":["alt+shift+cmd+h"], "action":"namedrop_doctor", "source":"namedrop.lua:39"}
        ], "diagnostics":[]})
    }

    #[test]
    fn ui_query_proves_locality_and_runs_only_about() {
        let script = Script::new(|request, _| match request.method.as_str() {
            "server.status" => Ok(status(PEER)),
            "client.keymap" => {
                assert_eq!(request.payload, json!({}));
                Ok(keymap())
            }
            "client.action" => {
                assert_eq!(request.payload, json!({"action":"namedrop_about"}));
                Ok(json!({"client_actions":[], "result":about()}))
            }
            other => panic!("unexpected {other}"),
        });
        let report = report(&script, Path::new(SOCKET), &record()).unwrap();
        assert_eq!(report["confirmed"], true);
        assert_eq!(report["env"]["HOME"], "/server/home");
        assert!(report["env"]["XDG_CONFIG_HOME"].is_null());
        assert_eq!(
            script.calls(),
            ["server.status", "client.keymap", "client.action"]
        );
        script.forget();
        script.peer.store(PEER + 1, Ordering::Relaxed);
        assert!(super::report(&script, Path::new(SOCKET), &record()).is_err());
        assert_eq!(script.calls(), ["server.status"]);
    }

    #[test]
    fn ui_readback_requires_all_identity_fields_and_eleven_actions() {
        assert_eq!(
            readback(&record(), &keymap(), Ok(about()))["confirmed"],
            true
        );
        for field in ["module", "launcher", "state"] {
            let mut wrong = about();
            wrong[field] = json!("/another/install");
            let report = readback(&record(), &keymap(), Ok(wrong.clone()));
            assert_eq!(report["confirmed"], false, "{field}");
            assert_eq!(report["about"], wrong);
        }
        let mut missing = keymap();
        missing["actions"].as_array_mut().unwrap().remove(0);
        assert_eq!(
            readback(&record(), &missing, Ok(about()))["confirmed"],
            false
        );
        assert_eq!(
            readback(&record(), &keymap(), Err(Fault::Closed("no about".into())))["confirmed"],
            false
        );
    }

    #[test]
    fn ui_keys_distinguish_effective_shadowed_and_skipped_bindings() {
        let report = readback(&record(), &keymap(), Ok(about()));
        let keys = report["keys"].as_array().unwrap();
        assert_eq!(keys.len(), 5);
        assert_eq!(keys[0]["status"], "in_effect");
        assert_eq!(keys[1]["status"], "shadowed");
        assert_eq!(keys[1]["source"], "init.lua:3");
        assert_eq!(keys[1]["effective_action"], "mine");
        assert_eq!(keys[2]["status"], "in_effect");
        assert_eq!(keys[3]["status"], "skipped");
        assert_eq!(keys[4]["status"], "skipped");
        for client_action in ["client.key.forward", "pane.split"] {
            let mut map = keymap();
            map["bindings"].as_array_mut().unwrap().insert(
                0,
                json!({"keys":["alt+shift+cmd+i"],"action":client_action,"source":"init.lua:20"}),
            );
            let report = readback(&record(), &map, Ok(about()));
            assert_eq!(report["keys"][0]["status"], "shadowed");
            assert_eq!(report["keys"][0]["source"], "init.lua:20");
        }
        assert_eq!(
            readback(
                &record(),
                &json!({"bindings":null,"actions":null}),
                Ok(about())
            )["confirmed"],
            false
        );
    }
}
