use crate::{
    CommandResult, HealthReport, lifecycle::terminal_safe_text, render_health_report_to_text,
};
use serde_json::Value;

/// Presentation only: the original response and operation outcome remain intact.
pub fn render_command_result_to_text(result: &CommandResult) -> String {
    if let Some(error) = &result.error {
        // Preserve useful line breaks from Clap, but never emit terminal controls.
        return format!(
            "Error: {}",
            safe_lines(
                error
                    .message
                    .strip_prefix("error: ")
                    .unwrap_or(&error.message)
            )
        );
    }
    let data = result.data.as_ref().unwrap_or(&Value::Null);
    let mut lines = Vec::new();
    match result.operation.unwrap_or_default() {
        "name" => {
            lines.push(format!(
                "Named this {} {}.",
                text(&data["pane_type"]),
                alias(&data["alias"])
            ));
            lines.push(
                if data["ready"] == true {
                    if data["pane_type"] == "terminal" {
                        "Ready to receive commands."
                    } else {
                        "Agent registered. Verify its hooks with a round trip."
                    }
                } else if data["pane_type"] == "terminal" {
                    "Terminal is not available yet. Return this pane to an idle shell before sending commands."
                } else {
                    "Not ready yet. Complete the agent setup below, then name this pane again."
                }
                .into(),
            );
            app_server_advice(&data["codex_app_server"], &mut lines);
            package_errors(&data["integration_packages"], &mut lines);
            if data["pane_type"] == "agent" && data["ready"] != true {
                fields(&data["native_session_setup"], 0, &mut lines);
                hook_steps(&data["hook_activation"], &mut lines);
            }
            if data["tab_label"]["status"] != "applied" {
                field("Tab label", &data["tab_label"], 0, &mut lines);
            }
        }
        "activate" => {
            lines.push("Status".into());
            lines.push(format!(
                "  Namedrop activated in space {}.",
                text(&data["space_id"])
            ));
            package_errors(&data["integration_packages"], &mut lines);
            if let Some(packages) = data["integration_packages"].as_object() {
                for (runtime, package) in packages {
                    if package["installed"] == true {
                        lines.push(format!("  {}: installed", terminal_safe_text(runtime)));
                    }
                }
            }
            let shortcuts = &data["shortcuts"];
            if let Some(error) = shortcuts["error"].as_str() {
                lines.push(format!(
                    "  Shortcuts need attention: {}",
                    terminal_safe_text(error)
                ));
            }
            for binding in shortcuts["bindings"].as_array().into_iter().flatten() {
                let label = text(&binding["label"]);
                if let Some(error) = binding["error"].as_str() {
                    lines.push(format!(
                        "  {label} shortcut needs attention: {}",
                        terminal_safe_text(error)
                    ));
                } else if let Some(key) = binding["key"].as_str() {
                    lines.push(format!("  {label} shortcut: {}", terminal_safe_text(key)));
                }
            }
            lines.push("\nNext steps — for the runtimes you use".into());
            hook_steps(&data["agent_integration_hook_activation"], &mut lines);
            app_server_advice(
                &data["agent_integration_hook_activation"]["codex"]["app_server"],
                &mut lines,
            );
            lines.push("  Check native hooks: herdr integration status\n  If setup is needed, choose your runtime:\n    herdr integration install codex\n    herdr integration install claude".into());
            lines.push("  Start fresh agent sessions and confirm a request and reply to verify their hooks.".into());
            field("Shell", &data["shell_command"]["next_step"], 1, &mut lines);
            field(
                "For this terminal",
                &data["shell_command"]["current_shell"],
                1,
                &mut lines,
            );
            lines.push("\nDetails".into());
            field("State", &data["state_directory"], 1, &mut lines);
            field("Command", &data["shell_command"]["path"], 1, &mut lines);
        }
        "deactivate" => lines.push(
            "Namedrop deactivated in this space. Names and message history are preserved.".into(),
        ),
        "doctor" => {
            if let Ok(report) = serde_json::from_value::<HealthReport>(data.clone()) {
                return render_health_report_to_text(&report);
            }
            fields(data, 0, &mut lines);
        }
        "address.list" => {
            if let Some(addresses) = data.as_array() {
                if addresses.is_empty() {
                    lines.push(
                        "No names in this space. Use namedrop name @name to name this pane.".into(),
                    );
                } else {
                    lines.push(format!(
                        "{:<18} {:<9} {:<12} {:<12} {:<29} ADDRESS ID",
                        "NAME", "TYPE", "STATE", "PANE", "PERSONA"
                    ));
                    for address in addresses {
                        let state = if address["collaboration_disabled"] == true {
                            "disabled".into()
                        } else if address["pane_type"] == "terminal"
                            && address["pane_id"].is_string()
                            && address["lifecycle"] == "empty"
                        {
                            // Empty describes the agent session, not the bound shell.
                            "bound".into()
                        } else {
                            text(&address["lifecycle"])
                        };
                        lines.push(format!(
                            "{:<18} {:<9} {:<12} {:<12} {:<29} {}",
                            alias(&address["alias"]),
                            text(&address["pane_type"]),
                            state,
                            text(&address["pane_id"]),
                            // Terminals cannot hold one, so the column stays
                            // blank for them rather than claiming Generic.
                            if address["pane_type"] == "terminal" {
                                String::new()
                            } else {
                                crate::persona::displayed(
                                    address["persona"].as_str(),
                                    crate::persona::PERSONA_COLUMN_CHARS,
                                )
                            },
                            text(&address["id"])
                        ));
                    }
                }
            }
        }
        "group.list" => {
            if let Some(groups) = data["groups"].as_array() {
                if groups.is_empty() {
                    lines.push("No saved groups in this space.".into());
                }
                for group in groups {
                    group_line(group, &mut lines);
                }
            }
        }
        "address.create" | "address.bind" | "address.unbind" | "address.rename"
        | "address.enable" | "address.disable" | "address.persona" => {
            let action = match result.operation.unwrap_or_default() {
                "address.create" => "Created",
                "address.bind" => "Bound",
                "address.unbind" => "Unbound",
                "address.rename" => "Renamed to",
                "address.enable" => "Enabled",
                "address.persona" => {
                    if data["persona"].is_string() {
                        "Set the persona for"
                    } else {
                        "Cleared the persona for"
                    }
                }
                _ => "Disabled",
            };
            lines.push(format!("{action} {}.", alias(&data["alias"])));
            field("Pane", &data["pane_id"], 0, &mut lines);
            field("Type", &data["pane_type"], 0, &mut lines);
            field("State", &data["lifecycle"], 0, &mut lines);
            if result.operation == Some("address.persona")
                && let Some(persona) = data["persona"].as_str()
            {
                lines.push(format!(
                    "Persona: {}",
                    crate::persona::displayed(Some(persona), crate::MAX_PERSONA_CHARS)
                ));
            }
        }
        "group.set" => {
            lines.push("Group saved.".into());
            group_line(data, &mut lines);
        }
        "group.remove" => lines.push(if data["removed"] == true {
            format!("Removed group {}.", alias(&data["alias"]))
        } else {
            format!(
                "Group {} did not exist; nothing removed.",
                alias(&data["alias"])
            )
        }),
        "worktree.pause" => {
            lines.push("Namedrop paused in this space. Messages will wait until you resume.".into())
        }
        "worktree.resume" => lines.push("Namedrop resumed in this space.".into()),
        "reconcile" => lines.push("Namedrop reconciliation completed.".into()),
        operation => {
            // Less common operations retain their complete data as labeled text,
            // including confirmation requirements, targets and partial outcomes.
            lines.push(format!("{}:", label(operation)));
            fields(data, 0, &mut lines);
            return lines.join("\n");
        }
    }
    drain_summary(&data["drain"], &mut lines);
    lines.join("\n")
}

fn group_line(group: &Value, lines: &mut Vec<String>) {
    let members = group["members"]
        .as_array()
        .map(|members| {
            members
                .iter()
                .map(|member| {
                    if member["alias"].is_string() {
                        alias(&member["alias"])
                    } else {
                        format!("unavailable ({})", text(&member["address_id"]))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    lines.push(format!("{}: {members}", alias(&group["alias"])));
}

fn hook_steps(hooks: &Value, lines: &mut Vec<String>) {
    if let Some(hooks) = hooks.as_object() {
        for (runtime, hook) in hooks {
            if let Some(steps) = hook["steps"].as_array() {
                for step in steps {
                    lines.push(format!("{}: {}", terminal_safe_text(runtime), text(step)));
                }
            }
        }
    }
}

/// Codex's shared app-server advice, with each step on its own indented line
/// so the commands stay copyable.
fn app_server_advice(advice: &Value, lines: &mut Vec<String>) {
    if !advice.is_object() {
        return;
    }
    lines.push(format!("Warning: {}", text(&advice["summary"])));
    lines.push(text(&advice["explanation"]));
    lines.push(text(&advice["fix"]));
    for step in advice["steps"].as_array().into_iter().flatten() {
        lines.push(format!("  {}", text(step)));
    }
}

fn package_errors(packages: &Value, lines: &mut Vec<String>) {
    if let Some(packages) = packages.as_object() {
        for (runtime, package) in packages {
            if package["installed"] == false || !package["error"].is_null() {
                lines.push(format!(
                    "{} integration needs attention: {}",
                    terminal_safe_text(runtime),
                    text(&package["error"])
                ));
            }
        }
    }
}

fn drain_summary(drain: &Value, lines: &mut Vec<String>) {
    if let Some(error) = drain.get("error") {
        field("Delivery error", error, 0, lines);
    }
    for key in [
        "held",
        "failed",
        "expired",
        "requeued",
        "submitted",
        "terminal_running",
        "terminal_waiting",
        "settled",
    ] {
        if let Some(entries) = drain[key].as_array().filter(|entries| !entries.is_empty()) {
            lines.push(format!(
                "Deliveries {}: {}.",
                key.replace('_', " "),
                entries.len()
            ));
            if matches!(key, "held" | "failed" | "expired") {
                lines.push(
                    "Run namedrop doctor to review these deliveries before taking action.".into(),
                );
            }
        }
    }
}

fn fields(value: &Value, depth: usize, lines: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "drain" {
                    drain_summary(value, lines);
                } else {
                    field(&label(key), value, depth, lines);
                }
            }
        }
        Value::Array(array) => {
            for value in array {
                fields(value, depth + 1, lines);
            }
        }
        Value::Null => {}
        _ => lines.push(format!("{}{}", "  ".repeat(depth), text(value))),
    }
}

fn field(name: &str, value: &Value, depth: usize, lines: &mut Vec<String>) {
    if value.is_null()
        || value.as_array().is_some_and(Vec::is_empty)
        || value.as_object().is_some_and(serde_json::Map::is_empty)
    {
        return;
    }
    let indent = "  ".repeat(depth);
    if value.is_object() || value.is_array() {
        lines.push(format!("{indent}{name}:"));
        fields(value, depth + 1, lines);
    } else {
        lines.push(format!("{indent}{name}: {}", text(value)));
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(value) => terminal_safe_text(value),
        Value::Null => "—".into(),
        Value::Bool(value) => if *value { "yes" } else { "no" }.into(),
        _ => terminal_safe_text(&value.to_string()),
    }
}

fn alias(value: &Value) -> String {
    format!("@{}", text(value).trim_start_matches('@'))
}
fn label(value: &str) -> String {
    let value = terminal_safe_text(value);
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        format!(
            "{}{}",
            first.to_uppercase(),
            chars.as_str().replace(['_', '.'], " ")
        )
    })
}
fn safe_lines(value: &str) -> String {
    value
        .lines()
        .map(terminal_safe_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn naming_keeps_setup_failures_and_delivery_attention_without_agent_noise_for_a_terminal() {
        let result = CommandResult::success(
            "name",
            json!({
                "alias":"@build", "pane_type":"terminal", "ready":true,
                "registered":false, "registration_required":false,
                "integration_packages":{"codex":{"installed":false,"error":"unowned rules file"}},
                "hook_activation":{"codex":{"steps":["unrelated agent refresh"]}},
                "tab_label":{"status":"applied"},
                "drain":{"held":["attempt-1"],"expired":[],"submitted":[]}
            }),
        );
        let rendered = render_command_result_to_text(&result);
        assert!(rendered.contains("Ready to receive commands."));
        assert!(rendered.contains("unowned rules file") && rendered.contains("Deliveries held: 1"));
        assert!(rendered.contains("namedrop doctor"));
        assert!(!rendered.contains("registered") && !rendered.contains("unrelated agent refresh"));
        let busy = CommandResult::success(
            "name",
            json!({"alias":"build", "pane_type":"terminal", "ready":false}),
        );
        let rendered = render_command_result_to_text(&busy);
        assert!(rendered.contains("idle shell") && !rendered.contains("agent setup"));
    }

    #[test]
    fn naming_a_served_codex_pane_shows_the_shared_app_server_advice_whatever_its_readiness() {
        let advice = crate::codex_app_server::advice(false);
        for ready in [true, false] {
            let result = CommandResult::success(
                "name",
                json!({"alias":"@builder", "pane_type":"agent", "ready":ready,
                       "codex_app_server":advice, "native_session_setup":null,
                       "tab_label":{"status":"applied"}}),
            );
            let rendered = render_command_result_to_text(&result);
            assert!(
                rendered.contains(&format!("Warning: {}\n", crate::codex_app_server::SUMMARY)),
                "{rendered}"
            );
            assert!(
                rendered.contains(crate::codex_app_server::FIX),
                "{rendered}"
            );
            for command in crate::codex_app_server::COMMANDS {
                assert!(
                    rendered.lines().any(|line| line == format!("  {command}")),
                    "{command} is copyable on its own line: {rendered}"
                );
            }
        }
        let quiet = CommandResult::success(
            "name",
            json!({"alias":"@builder", "pane_type":"agent", "ready":true,
                   "codex_app_server":null, "tab_label":{"status":"applied"}}),
        );
        assert!(!render_command_result_to_text(&quiet).contains("Warning"));
    }

    #[test]
    fn activation_shows_the_shared_app_server_advice_only_when_codex_carries_it() {
        let activated = |codex: Value| {
            render_command_result_to_text(&CommandResult::success(
                "activate",
                json!({"space_id":"w1", "integration_packages":{},
                       "agent_integration_hook_activation":{"codex":codex}}),
            ))
        };
        let rendered = activated(
            json!({"steps":["codex plugin add namedrop@namedrop --json"],
                                        "app_server":crate::codex_app_server::advice(false)}),
        );
        assert!(
            rendered.contains(crate::codex_app_server::SUMMARY),
            "{rendered}"
        );
        for command in crate::codex_app_server::COMMANDS {
            assert!(
                rendered.lines().any(|line| line == format!("  {command}")),
                "{command}: {rendered}"
            );
        }
        let quiet = activated(json!({"steps":["codex plugin add namedrop@namedrop --json"]}));
        assert!(!quiet.contains("Warning"), "{quiet}");
    }

    /// The readable table shows what a person needs to choose a collaborator:
    /// the persona, Generic where there is none, and nothing for a terminal that
    /// cannot hold one. Text saved before the limits is escaped and truncated.
    #[test]
    fn the_readable_name_table_shows_personas_generic_and_nothing_for_a_terminal() {
        let listed = CommandResult::success(
            "address.list",
            json!([
                {"alias":"reviewer","pane_type":"agent","lifecycle":"occupied","pane_id":"p1",
                 "id":"address-1","collaboration_disabled":false,
                 "persona":"Reviews Rust changes for correctness and risk across the tree"},
                {"alias":"planner","pane_type":"agent","lifecycle":"occupied","pane_id":"p2",
                 "id":"address-2","collaboration_disabled":false,"persona":null},
                {"alias":"build","pane_type":"terminal","lifecycle":"empty","pane_id":"p3",
                 "id":"address-3","collaboration_disabled":false,"persona":null}
            ]),
        );
        let rendered = render_command_result_to_text(&listed);
        assert!(rendered.contains("PERSONA"), "{rendered}");
        assert!(
            rendered.contains("Reviews Rust changes for cor…"),
            "{rendered}"
        );
        assert!(rendered.contains("Generic"), "{rendered}");
        assert_eq!(rendered.matches("Generic").count(), 1, "{rendered}");

        let cleared = CommandResult::success(
            "address.persona",
            json!({"alias":"planner","pane_type":"agent","lifecycle":"occupied","persona":null}),
        );
        assert!(
            render_command_result_to_text(&cleared).contains("Cleared the persona for @planner."),
            "{}",
            render_command_result_to_text(&cleared)
        );
        let set = CommandResult::success(
            "address.persona",
            json!({"alias":"planner","pane_type":"agent","lifecycle":"occupied",
                   "persona":"Plans\u{1b}[2J work"}),
        );
        let rendered = render_command_result_to_text(&set);
        assert!(
            rendered.contains("Set the persona for @planner."),
            "{rendered}"
        );
        assert!(
            rendered.contains("Persona: Plans\\u{1b}[2J work") && !rendered.contains('\u{1b}'),
            "{rendered}"
        );
    }

    #[test]
    fn management_output_keeps_missing_members_confirmations_and_partial_outcomes() {
        let groups = CommandResult::success(
            "group.list",
            json!({"groups":[{"alias":"reviewers","members":[
                {"alias":"claude","address_id":"a1"},{"alias":null,"address_id":"a2"}
            ]}]}),
        );
        assert_eq!(
            render_command_result_to_text(&groups),
            "@reviewers: @claude, unavailable (a2)"
        );
        let missing =
            CommandResult::success("group.remove", json!({"alias":"reviewers","removed":false}));
        assert!(render_command_result_to_text(&missing).contains("nothing removed"));
        let purge = CommandResult::success(
            "purge",
            json!({"confirmation_required":true,"purged":false,"space_id":"w1"}),
        );
        let rendered = render_command_result_to_text(&purge);
        assert!(rendered.contains("Confirmation required: yes") && rendered.contains("Purged: no"));
        let result = CommandResult::success(
            "handoff.results",
            json!({"completed":1,"expected":2,"freshness":{"status":"unknown","reason":"missing evidence"}}),
        );
        let rendered = render_command_result_to_text(&result);
        assert!(
            rendered.contains("Completed: 1")
                && rendered.contains("Expected: 2")
                && rendered.contains("unknown")
                && rendered.contains("missing evidence")
        );
    }

    #[test]
    fn terminal_controls_in_values_keys_and_errors_are_printed_as_text() {
        let result = CommandResult::success(
            "handoff.results",
            json!({"\u{1b}[2J":"\rforged status\u{7}"}),
        );
        let rendered = render_command_result_to_text(&result);
        assert!(!rendered.contains(['\u{1b}', '\r', '\u{7}']));
        let error = CommandResult::invalid_request("bad argument\ntry --help\u{1b}[2J");
        let rendered = render_command_result_to_text(&error);
        assert!(rendered.contains("\ntry --help") && !rendered.contains('\u{1b}'));
    }
}
