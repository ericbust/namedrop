use clap::{Parser, error::ErrorKind};
mod agent_status_ui;
mod naming_ui;
mod popup_input;
use namedrop::{
    Clock, CommandResult, HerdrCommandExecutor, LifecycleRuntime, ProcessHerdr, ProcessIds,
    SystemClock,
    cli::{Cli, RuntimeOptions},
    describe_operations, diagnose_collaboration, execute_context_operation,
    execute_control_operation, execute_group_operation, execute_handoff_operation,
    execute_integration_operation, execute_lifecycle_operation, execute_message_operation,
    execute_session_start_hook, execute_user_prompt_submit_hook, is_context_operation,
    is_control_operation, is_integration_operation, is_lifecycle_operation, is_message_operation,
    render_command_result_to_text, validate_message_input,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn main() -> ExitCode {
    if env::args_os().skip(1).eq(["--runtime-info"]) {
        println!("{}", include_str!("../runtime.json").trim());
        return ExitCode::SUCCESS;
    }
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    // Activation invokes this only behind the exclusive runtime command gate.
    // It performs the existing atomic migration without host or package actions.
    if arguments.len() == 2 && arguments[0] == "--migrate-runtime-state" {
        use std::os::unix::fs::MetadataExt;
        let state = Path::new(&arguments[1]);
        let inherited_gate = env::var("NAMEDROP_MIGRATION_GATE_FD")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|fd| *fd >= 3)
            .and_then(|fd| fs::File::open(format!("/dev/fd/{fd}")).ok())
            .and_then(|file| file.metadata().ok());
        let gate_matches = inherited_gate
            .zip(fs::metadata(state.join("runtime/commands.lock")).ok())
            .is_some_and(|(inherited, expected)| {
                inherited.is_file()
                    && inherited.dev() == expected.dev()
                    && inherited.ino() == expected.ino()
            });
        if !state.is_absolute()
            || !gate_matches
            || !state.join("runtime/migration.json").is_file()
            || fs::canonicalize(state.join("runtime/current/namedrop")).ok()
                != env::current_exe().and_then(fs::canonicalize).ok()
        {
            eprintln!(
                "State migration requires the selected runtime, inherited command gate, and a pending activation."
            );
            return ExitCode::from(65);
        }
        return match namedrop::StateStore::open(state).and_then(|store| store.schema_version()) {
            Ok(schema) => {
                println!("{}", serde_json::json!({"state_schema": schema}));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("Runtime state migration failed: {error}");
                ExitCode::from(65)
            }
        };
    }
    if arguments.len() == 2 && arguments[0] == "--validate-update-manifest" {
        return match namedrop::validate_update_manifest(Path::new(&arguments[1])) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(65)
            }
        };
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return if error.print().is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(74)
            };
        }
        Err(error) => {
            let args: Vec<_> = env::args_os().skip(1).collect();
            let human = !args.iter().any(|arg| arg == "--json")
                && (args.iter().any(|arg| arg == "--human") || io::stdout().is_terminal());
            return write_result(
                &CommandResult::invalid_request(error.to_string()),
                64,
                human,
            );
        }
    };
    let selected_state = cli
        .runtime
        .plugin_state
        .clone()
        .or_else(|| {
            cli.user_prompt_submit_hook_state_directory()
                .map(Path::to_path_buf)
        })
        .or_else(|| {
            cli.mailbox_wake_hook_state_directory()
                .map(Path::to_path_buf)
        })
        .or_else(|| {
            cli.session_start_hook_state_directory()
                .map(Path::to_path_buf)
        })
        .or_else(|| env::var_os("HERDR_PLUGIN_STATE_DIR").map(PathBuf::from));
    let _command_guard = match selected_state
        .as_deref()
        .map(namedrop::runtime_command_guard)
        .transpose()
    {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(69);
        }
    };
    // Recheck selection after acquiring the gate: a queued command may have
    // started before activation completed or before a failed switch rolled back.
    if let Some(state) = selected_state.as_deref() {
        let launcher = state.join("runtime/namedrop");
        if launcher.exists() {
            let selected = fs::canonicalize(state.join("runtime/current/namedrop"));
            let running = env::current_exe().and_then(fs::canonicalize);
            if selected.as_ref().ok() != running.as_ref().ok() || selected.is_err() {
                use std::os::unix::process::CommandExt;
                let error = Command::new(launcher).args(env::args_os().skip(1)).exec();
                eprintln!("Could not run the selected Namedrop runtime: {error}");
                return ExitCode::from(69);
            }
        }
    }
    let human = cli.human_output(io::stdout().is_terminal());
    let input_file = cli.input_is_file();
    let omitted = "Input file rejected; file contents are omitted from diagnostics.";
    let write_result = |result: &CommandResult, code| write_result(result, code, human);
    let input_error = |error: String| {
        if input_file && error.starts_with("invalid operation input: ") {
            omitted.to_owned()
        } else {
            error
        }
    };
    let finish = |operation, result: Result<serde_json::Value, String>| {
        finish(operation, result.map_err(input_error), human)
    };

    if let Some(state_directory) = cli.user_prompt_submit_hook_state_directory() {
        return run_context_hook(state_directory, execute_user_prompt_submit_hook);
    }
    if let Some(state_directory) = cli.session_start_hook_state_directory() {
        return run_context_hook(state_directory, execute_session_start_hook);
    }
    if let Some(state_directory) = cli.mailbox_wake_hook_state_directory() {
        return run_mailbox_wake_hook(state_directory);
    }
    if cli.is_agent_status_changed_hook() {
        return run_agent_status_changed_hook();
    }
    if cli.is_agent_status_worker_hook() {
        return run_agent_status_worker();
    }
    if matches!(cli.operation(), "reconcile" | "event.pane-lifecycle")
        && let Some(state) = selected_state.as_deref()
    {
        namedrop::update_check::start(state, SystemClock.now_unix_ms());
    }
    if let Some(attempt) = cli.terminal_attempt() {
        let result = with_runtime_options(&cli.runtime, |runtime| {
            namedrop::execute_terminal(attempt, runtime)
        });
        if result.is_ok()
            && let Err(error) = wake_mailboxes(&cli.runtime)
        {
            eprintln!("Result saved; could not wake delivery: {error}");
        }
        return match result {
            Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(65)
            }
        };
    }
    if let Some(config) = cli.shortcuts_config() {
        return finish("ui.install-shortcuts", naming_ui::install_shortcuts(config));
    }
    if cli.is_reset_tour() {
        return finish("ui.reset-tour", naming_ui::reset_tour(&cli.runtime));
    }
    if let Some((launch, mode)) = cli.naming_ui() {
        return if launch {
            finish("ui.launch", naming_ui::launch(&cli.runtime, mode))
        } else {
            naming_ui::run(&cli.runtime, mode)
        };
    }
    if let Some(alias) = cli.agent_name() {
        let alias = alias.to_owned();
        // Absent stays null, which preserves the saved persona; `--generic`
        // sends the blank description that clears it.
        let persona = cli.name_persona().map(str::to_owned);
        let kind = match choose_name_type(&cli) {
            Ok(kind) => kind,
            Err(error) => return finish("name", Err(error)),
        };
        return finish(
            "name",
            run_lifecycle(
                "name",
                serde_json::json!({"alias": alias,"pane_type":kind,"persona":persona}),
                &cli.runtime,
            ),
        );
    }

    let operation = cli.operation();
    let input = match cli.input() {
        Ok(input) if input.is_object() => input,
        Ok(_) => {
            return write_result(
                &CommandResult::invalid_request("input JSON must be an object"),
                64,
            );
        }
        Err(error) => {
            return write_result(&CommandResult::invalid_request(input_error(error)), 64);
        }
    };

    if operation == "describe" {
        if input.as_object().is_some_and(|object| !object.is_empty()) {
            return write_result(
                &CommandResult::rejected(
                    operation,
                    "invalid_request",
                    "describe takes its operation token as a positional argument",
                ),
                64,
            );
        }
        return finish(operation, describe_operations(cli.described_operation()));
    }

    if operation == "doctor" {
        if input.as_object().is_some_and(|object| !object.is_empty()) {
            return write_result(
                &CommandResult::rejected(
                    operation,
                    "invalid_request",
                    "doctor does not accept an input payload",
                ),
                64,
            );
        }
        let report =
            match with_runtime_options(&cli.runtime, |runtime| Ok(diagnose_collaboration(runtime)))
            {
                Ok(report) => report,
                Err(error) => {
                    return write_result(
                        &CommandResult::rejected(operation, "operation_failed", error),
                        2,
                    );
                }
            };
        let exit_code = match report.status {
            namedrop::HealthStatus::Healthy => 0,
            namedrop::HealthStatus::NeedsAttention => 1,
            namedrop::HealthStatus::Unavailable => 2,
        };
        return match serde_json::to_value(report) {
            Ok(data) => write_result(&CommandResult::success(operation, data), exit_code),
            Err(error) => write_result(
                &CommandResult::rejected(operation, "operation_failed", error.to_string()),
                2,
            ),
        };
    }

    if is_message_operation(operation)
        && let Err(error) = validate_message_input(operation, input.clone())
    {
        return write_result(&CommandResult::invalid_request(input_error(error)), 64);
    }

    let result = if matches!(
        operation,
        "handoff.begin"
            | "handoff.resolve"
            | "handoff.adopt"
            | "handoff.status"
            | "handoff.results"
            | "handoff.preview"
    ) {
        run_operation(operation, input, OperationKind::Handoff, &cli.runtime)
    } else if matches!(operation, "group.list" | "group.set" | "group.remove") {
        run_operation(operation, input, OperationKind::Group, &cli.runtime)
    } else if is_control_operation(operation) {
        run_operation(operation, input, OperationKind::Control, &cli.runtime)
    } else if is_context_operation(operation) {
        run_operation(operation, input, OperationKind::Context, &cli.runtime)
    } else if is_integration_operation(operation) {
        run_operation(operation, input, OperationKind::Integration, &cli.runtime)
    } else if is_message_operation(operation) {
        run_operation(operation, input, OperationKind::Message, &cli.runtime)
    } else if is_lifecycle_operation(operation) {
        run_lifecycle(operation, input, &cli.runtime)
    } else {
        Err(format!("unsupported operation: {operation}"))
    };
    if result.as_ref().is_ok_and(needs_mailbox_watcher)
        && let Err(error) = wake_mailboxes(&cli.runtime)
    {
        eprintln!("Command queued; could not start shell watcher: {error}");
    }
    finish(operation, result)
}

fn wake_mailboxes(options: &RuntimeOptions) -> Result<(), String> {
    with_runtime_options(options, wake_runtime_mailboxes)
}

fn drain_report(result: &serde_json::Value) -> Result<namedrop::DrainReport, String> {
    serde_json::from_value(result.get("drain").unwrap_or(result).clone())
        .map_err(|error| error.to_string())
}

fn needs_mailbox_watcher(result: &serde_json::Value) -> bool {
    drain_report(result)
        .is_ok_and(|report| report.awaits_terminal() || report.next_grace_due_ms().is_some())
}

#[cfg(test)]
mod drain_report_tests {
    use super::{drain_report, needs_mailbox_watcher};
    use namedrop::{DeliveryAttemptId, DrainReport, GraceDeadline};
    use serde_json::json;

    #[test]
    fn an_operation_result_carries_the_drain_report_it_serialized() {
        let grace = DrainReport {
            pending_grace: vec![GraceDeadline {
                attempt_id: DeliveryAttemptId::new("attempt-1"),
                due_at_ms: 31_000,
            }],
            ..DrainReport::default()
        };
        let result = json!({"attempt_id": "attempt-1", "drain": grace});
        assert_eq!(
            drain_report(&result).unwrap().next_grace_due_ms(),
            Some(31_000)
        );
        assert!(needs_mailbox_watcher(&result));

        let requeued = DrainReport {
            requeued: vec![DeliveryAttemptId::new("attempt-1")],
            ..DrainReport::default()
        };
        let result = json!({"drain": requeued});
        assert!(drain_report(&result).unwrap().awaits_retry());
        assert!(!needs_mailbox_watcher(&result));
    }
}

fn wake_runtime_mailboxes(
    runtime: &LifecycleRuntime<'_, ProcessHerdr, SystemClock, ProcessIds>,
) -> Result<(), String> {
    let space = match runtime.workspace_id {
        Some(id) => id.to_owned(),
        None => namedrop::discover_worktree(runtime.herdr, runtime.cwd)?.workspace_id,
    };
    let directory =
        reconciliation_directory(runtime.state_directory, runtime.session_socket_id, &space)?;
    queue_agent_status_reconciliation(
        &directory,
        &serde_json::json!({"operation":"mailbox-wake"}),
    )?;
    status_worker_command()?
        .env("HERDR_PLUGIN_STATE_DIR", runtime.state_directory)
        .env("HERDR_SOCKET_PATH", runtime.session_socket_id)
        .env("HERDR_WORKSPACE_ID", space)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn choose_name_type(cli: &Cli) -> Result<Option<namedrop::PaneType>, String> {
    if cli.name_pane_type().is_some() || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Ok(cli.name_pane_type());
    }
    let default = with_runtime_options(&cli.runtime, namedrop::default_pane_type)?;
    loop {
        eprint!(
            "Pane type: terminal / agent{}: ",
            default
                .map(|k| format!(" [{}]", k.as_str()))
                .unwrap_or_default()
        );
        io::stderr().flush().map_err(|e| e.to_string())?;
        let mut answer = String::new();
        if io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Err("naming cancelled before choosing pane type".to_owned());
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" if default.is_some() => return Ok(default),
            "t" | "terminal" => return Ok(Some(namedrop::PaneType::Terminal)),
            "a" | "agent" => return Ok(Some(namedrop::PaneType::Agent)),
            _ => eprintln!("Choose terminal or agent."),
        }
    }
}

fn finish(
    operation: &'static str,
    result: Result<serde_json::Value, String>,
    human: bool,
) -> ExitCode {
    match result {
        Ok(data) => {
            // Workspace activation can succeed while a shared integration has
            // a conflict, and shortcut repair can add some keys while another
            // is taken. Keep the complete report, but fail the command.
            let code = u8::from(
                (operation == "activate" && !activation_complete(&data))
                    || (operation == "ui.install-shortcuts" && data["status"] == "needs_attention"),
            );
            write_result(&CommandResult::success(operation, data), code, human)
        }
        Err(error) => write_result(
            &CommandResult::rejected(operation, "operation_failed", error),
            65,
            human,
        ),
    }
}

fn activation_complete(data: &serde_json::Value) -> bool {
    data["integration_packages"]
        .as_object()
        .is_some_and(|packages| {
            !packages.is_empty()
                && packages
                    .values()
                    .all(|package| package["installed"] == true && package["error"].is_null())
        })
}

fn run_lifecycle(
    operation: &'static str,
    mut input: serde_json::Value,
    options: &RuntimeOptions,
) -> Result<serde_json::Value, String> {
    if matches!(
        operation,
        "event.pane-lifecycle" | "event.agent-status-changed"
    ) && input.as_object().is_some_and(serde_json::Map::is_empty)
        && let Ok(event) = env::var("HERDR_PLUGIN_EVENT_JSON")
    {
        input = plugin_event_input(operation, &event)?;
    }
    run_operation(operation, input, OperationKind::Lifecycle, options)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginEventEnvelope {
    event: String,
    data: serde_json::Value,
}

fn plugin_event_input(operation: &str, serialized: &str) -> Result<serde_json::Value, String> {
    let hook_event = env::var("HERDR_PLUGIN_EVENT")
        .map_err(|_| "HERDR_PLUGIN_EVENT is required for an event hook".to_owned())?;
    parse_plugin_event_input(operation, serialized, &hook_event)
}

fn parse_plugin_event_input(
    operation: &str,
    serialized: &str,
    hook_event: &str,
) -> Result<serde_json::Value, String> {
    let envelope: PluginEventEnvelope =
        serde_json::from_str(serialized).map_err(|error| error.to_string())?;
    let expected = match envelope.event.as_str() {
        "pane_created" => ("pane.created", "event.pane-lifecycle"),
        "pane_moved" => ("pane.moved", "event.pane-lifecycle"),
        "pane_closed" => ("pane.closed", "event.pane-lifecycle"),
        "pane_exited" => ("pane.exited", "event.pane-lifecycle"),
        "tab_closed" => ("tab.closed", "event.pane-lifecycle"),
        "pane_agent_status_changed" => ("pane.agent_status_changed", "event.agent-status-changed"),
        "pane_agent_detected" => ("pane.agent_detected", "event.agent-status-changed"),
        other => return Err(format!("unsupported Herdr plugin event: {other}")),
    };
    if hook_event != expected.0 || operation != expected.1 {
        return Err(format!(
            "Herdr plugin event {} does not match operation {operation}",
            envelope.event
        ));
    }
    Ok(envelope.data)
}

#[cfg(test)]
mod session_event_tests {
    #[test]
    fn agent_detection_events_reuse_registration_worker() {
        let data = serde_json::json!({"type":"pane_agent_detected", "pane_id":"w1:p1",
            "workspace_id":"w1", "agent":"codex", "released":false});
        let event = serde_json::json!({"event":"pane_agent_detected", "data": data}).to_string();
        let input = super::parse_plugin_event_input(
            "event.agent-status-changed",
            &event,
            "pane.agent_detected",
        )
        .unwrap();
        assert_eq!(input, data);
        assert!(
            super::parse_plugin_event_input("event.agent-status-changed", &event, "pane.created")
                .is_err()
        );
        assert!(
            super::parse_plugin_event_input("event.pane-lifecycle", &event, "pane.agent_detected")
                .is_err()
        );
    }
}

#[derive(Clone, Copy)]
enum OperationKind {
    Handoff,
    Group,
    Context,
    Control,
    Lifecycle,
    Message,
    Integration,
}

fn run_operation(
    operation: &'static str,
    input: serde_json::Value,
    kind: OperationKind,
    options: &RuntimeOptions,
) -> Result<serde_json::Value, String> {
    with_runtime_options(options, |runtime| match kind {
        OperationKind::Handoff => execute_handoff_operation(operation, input, runtime),
        OperationKind::Group => execute_group_operation(operation, input, runtime),
        OperationKind::Context => execute_context_operation(operation, input, runtime),
        OperationKind::Control => execute_control_operation(operation, input, runtime),
        OperationKind::Lifecycle => execute_lifecycle_operation(operation, input, runtime),
        OperationKind::Message => execute_message_operation(operation, input, runtime),
        OperationKind::Integration => execute_integration_operation(operation, input, runtime),
    })
}

fn with_runtime_at<T>(
    state_directory: &Path,
    action: impl FnOnce(
        &LifecycleRuntime<'_, ProcessHerdr, SystemClock, ProcessIds>,
    ) -> Result<T, String>,
) -> Result<T, String> {
    with_runtime_options(
        &RuntimeOptions {
            plugin_state: Some(state_directory.to_path_buf()),
            ..Default::default()
        },
        action,
    )
}

fn with_runtime_options<T>(
    options: &RuntimeOptions,
    action: impl FnOnce(
        &LifecycleRuntime<'_, ProcessHerdr, SystemClock, ProcessIds>,
    ) -> Result<T, String>,
) -> Result<T, String> {
    let state_directory = options
        .plugin_state
        .clone()
        .or_else(|| env::var_os("HERDR_PLUGIN_STATE_DIR").map(PathBuf::from))
        .ok_or_else(|| "HERDR_PLUGIN_STATE_DIR or --plugin-state is required".to_owned())?;
    let home_directory = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let claude_config = env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    let codex_config = env::var_os("CODEX_HOME").map(PathBuf::from);
    let cwd = env::current_dir().map_err(|error| error.to_string())?;
    let herdr = ProcessHerdr::default().with_socket(options.herdr_socket.clone());
    let mut workspace_id = options
        .herdr_workspace
        .clone()
        .or_else(|| env::var("HERDR_WORKSPACE_ID").ok());
    // An explicit --herdr-pane is the caller's assertion. An inherited
    // HERDR_PANE_ID can be stale: a daemon keeps the variables of the pane it
    // started in, so it only counts when this process still runs under that
    // pane's shell.
    let mut pane_id = match options.herdr_pane.clone() {
        Some(pane) => Some(pane),
        None => match env::var("HERDR_PANE_ID") {
            Ok(pane) => {
                let sessions: Vec<String> = ["CODEX_SESSION_ID", "CLAUDE_CODE_SESSION_ID"]
                    .into_iter()
                    .filter_map(|key| env::var(key).ok())
                    .filter(|session| !session.is_empty())
                    .collect();
                let sessions: Vec<&str> = sessions.iter().map(String::as_str).collect();
                namedrop::verify_inherited_pane(&herdr, &pane, &sessions)?;
                Some(pane)
            }
            Err(_) => None,
        },
    };
    if workspace_id.is_none()
        && let Some(pane) = pane_id.as_deref()
    {
        workspace_id = Some(namedrop::pane_workspace(&herdr, pane)?);
    }
    // An agent can run commands without the pane's Herdr variables (a shared
    // Codex app-server keeps the environment it started with). The agent's
    // own session ID still arrives, and Herdr knows which pane runs it.
    if workspace_id.is_none()
        && let Some((space, pane)) = ["CODEX_SESSION_ID", "CLAUDE_CODE_SESSION_ID"]
            .into_iter()
            .find_map(|key| env::var(key).ok().filter(|session| !session.is_empty()))
            .and_then(|session| namedrop::locate_agent_session_pane(&herdr, &session))
    {
        workspace_id = Some(space);
        pane_id = pane_id.or(Some(pane));
    }
    let socket_id = session_socket_id(options.herdr_socket.clone(), &herdr);
    let clock = SystemClock;
    let ids = ProcessIds::default();
    action(&LifecycleRuntime {
        state_directory: &state_directory,
        home_directory: home_directory.as_deref(),
        agent_config: namedrop::AgentConfigDirectories {
            claude: claude_config.as_deref(),
            codex: codex_config.as_deref(),
        },
        cwd: &cwd,
        workspace_id: workspace_id.as_deref(),
        current_pane_id: pane_id.as_deref(),
        session_socket_id: &socket_id,
        herdr: &herdr,
        clock: &clock,
        ids: &ids,
    })
}

/// Runs one hook that reads the host's event payload from stdin and may answer
/// with context for the model. Silence is the only failure mode: a hook that
/// cannot run must not interrupt the person's turn.
fn run_context_hook(
    state_directory: &Path,
    execute: fn(
        &str,
        &LifecycleRuntime<'_, ProcessHerdr, SystemClock, ProcessIds>,
    ) -> Option<serde_json::Value>,
) -> ExitCode {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        return ExitCode::SUCCESS;
    }
    let mut output = namedrop::execute_codex_app_server_hook(&input).or_else(|| {
        with_runtime_at(state_directory, |runtime| Ok(execute(&input, runtime)))
            .ok()
            .flatten()
    });
    if serde_json::from_str::<serde_json::Value>(&input)
        .is_ok_and(|input| input["hook_event_name"] == "SessionStart")
    {
        output = namedrop::update_check::session_notice(
            &input,
            state_directory,
            SystemClock.now_unix_ms(),
            output,
        );
    }
    if let Some(output) = output {
        let _ = serde_json::to_writer(io::stdout().lock(), &output);
    }
    ExitCode::SUCCESS
}

/// The socket that scopes this session's state: the caller's, the pane's, or
/// the one the running Herdr server reports.
fn session_socket_id(explicit: Option<String>, herdr: &ProcessHerdr) -> String {
    explicit
        .or_else(|| env::var("HERDR_SOCKET_PATH").ok())
        .unwrap_or_else(|| {
            herdr
                .execute(&["status", "server"])
                .ok()
                .and_then(|output| {
                    output
                        .stdout
                        .lines()
                        .find_map(|line| line.strip_prefix("socket: ").map(str::to_owned))
                })
                .unwrap_or_else(|| "default".to_owned())
        })
}

/// The wake the user-prompt-submit wrapper backgrounds once a mention was
/// sent. Its hook shell has exited by the time it runs, so it is nobody's
/// pane: it reconciles the whole space and takes no identity from the
/// HERDR_PANE_ID it inherited, which no session ID vouches for under Codex.
fn run_mailbox_wake_hook(state_directory: &Path) -> ExitCode {
    let wake = || -> Result<(), String> {
        let herdr = ProcessHerdr::default();
        let socket_id = session_socket_id(None, &herdr);
        let space_id = match env::var("HERDR_WORKSPACE_ID") {
            Ok(space_id) => space_id,
            Err(_) => {
                let cwd = env::current_dir().map_err(|error| error.to_string())?;
                namedrop::discover_worktree(&herdr, &cwd)?.workspace_id
            }
        };
        let directory = reconciliation_directory(state_directory, &socket_id, &space_id)?;
        queue_agent_status_reconciliation(
            &directory,
            &serde_json::json!({"operation":"mailbox-wake"}),
        )?;
        status_worker_command()?
            .env("HERDR_PLUGIN_STATE_DIR", state_directory)
            .env("HERDR_SOCKET_PATH", &socket_id)
            .env("HERDR_WORKSPACE_ID", space_id)
            .status()
            .map_err(|error| error.to_string())?;
        Ok(())
    };
    match wake() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("could not wake Namedrop delivery: {error}");
            ExitCode::from(65)
        }
    }
}

fn run_agent_status_changed_hook() -> ExitCode {
    let input = match env::var("HERDR_PLUGIN_EVENT_JSON")
        .map_err(|_| "HERDR_PLUGIN_EVENT_JSON is required for an event hook".to_owned())
        .and_then(|event| plugin_event_input("event.agent-status-changed", &event))
    {
        Ok(input) => input,
        Err(error) => {
            eprintln!("could not queue Namedrop reconciliation: {error}");
            return ExitCode::from(65);
        }
    };
    let directory = match agent_status_reconciliation_directory() {
        Ok(directory) => directory,
        Err(error) => {
            eprintln!("could not queue Namedrop reconciliation: {error}");
            return ExitCode::from(65);
        }
    };
    if let Err(error) = queue_agent_status_reconciliation(&directory, &input) {
        eprintln!("could not queue Namedrop reconciliation: {error}");
        return ExitCode::from(65);
    }
    match status_worker_command()
        .and_then(|mut command| command.spawn().map_err(|error| error.to_string()))
    {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("could not start Namedrop reconciliation: {error}");
            ExitCode::from(65)
        }
    }
}

fn status_worker_command() -> Result<Command, String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(namedrop::runtime_executable()?);
    command
        .args(["hook", "agent-status-worker"])
        .process_group(0)
        // The worker reconciles a whole space and outlives the hook that
        // started it, so it is nobody's pane; the hook's pane must not become
        // its identity.
        .env_remove("HERDR_PANE_ID")
        .env("NAMEDROP_HERDR_TIMEOUT_MS", "10000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

fn agent_status_reconciliation_directory() -> Result<PathBuf, String> {
    let state_directory = env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "HERDR_PLUGIN_STATE_DIR is required".to_owned())?;
    let socket_id =
        env::var("HERDR_SOCKET_PATH").map_err(|_| "HERDR_SOCKET_PATH is required".to_owned())?;
    let space_id =
        env::var("HERDR_WORKSPACE_ID").map_err(|_| "HERDR_WORKSPACE_ID is required".to_owned())?;
    reconciliation_directory(&state_directory, &socket_id, &space_id)
}

fn reconciliation_directory(
    state_directory: &Path,
    socket_id: &str,
    space_id: &str,
) -> Result<PathBuf, String> {
    let scope = namedrop::lower_hex(&Sha256::digest(format!("{socket_id}\0{space_id}")));
    let directory = state_directory
        .join("agent-status-reconciliation")
        .join(scope);
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    Ok(directory)
}

fn queue_agent_status_reconciliation(
    directory: &Path,
    input: &serde_json::Value,
) -> Result<(), String> {
    let occurred_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let marker_id = format!("{}-{occurred_at}", std::process::id());
    let queued = directory.join(format!("queued-{marker_id}"));
    let pending = directory.join(format!("pending-{marker_id}"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&queued)
        .map_err(|error| error.to_string())?;
    if let Err(error) = serde_json::to_writer(&mut file, input)
        .map_err(|error| error.to_string())
        .and_then(|()| file.sync_all().map_err(|error| error.to_string()))
        .and_then(|()| fs::rename(&queued, pending).map_err(|error| error.to_string()))
        .and_then(|()| {
            fs::File::open(directory)
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())
        })
    {
        let _ = fs::remove_file(queued);
        return Err(error);
    }
    Ok(())
}

fn pending_agent_status_reconciliations(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut markers = fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pending-"))
        })
        .collect::<Vec<_>>();
    markers.sort();
    Ok(markers)
}

fn run_agent_status_worker() -> ExitCode {
    let directory = match agent_status_reconciliation_directory() {
        Ok(directory) => directory,
        Err(_) => return ExitCode::SUCCESS,
    };
    let lock = match fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join("worker.lock"))
    {
        Ok(lock) => lock,
        Err(_) => return ExitCode::SUCCESS,
    };
    if lock.try_lock().is_err() {
        return ExitCode::SUCCESS;
    }
    let mut failures = 0_usize;
    let mut deadline_ms: Option<u64> = None;
    loop {
        let Ok(pending) = pending_agent_status_reconciliations(&directory) else {
            return ExitCode::SUCCESS;
        };
        let Some(marker) = pending.first() else {
            if let Some(due) = deadline_ms {
                if SystemClock.now_unix_ms() < due {
                    wait_for_marker_or_deadline(&directory, due);
                    continue;
                }
                deadline_ms =
                    with_runtime_options(&RuntimeOptions::default(), namedrop::drain_mailboxes)
                        .ok()
                        .and_then(|report| report.next_grace_due_ms());
                continue;
            }
            let _ = lock.unlock();
            // A producer can queue after our empty scan, then lose try_lock
            // while we still own it. Recheck only after releasing ownership:
            // either we reclaim the queued work or its new worker owns it.
            if pending_agent_status_reconciliations(&directory)
                .is_ok_and(|pending| !pending.is_empty())
                && lock.try_lock().is_ok()
            {
                continue;
            }
            return ExitCode::SUCCESS;
        };
        let bytes = match fs::read(marker) {
            Ok(bytes) => bytes,
            Err(_) => {
                wait_for_reconciliation_retry(&mut failures);
                continue;
            }
        };
        let input = serde_json::from_slice(&bytes);
        let Ok(input) = input else {
            let _ = fs::remove_file(marker);
            continue;
        };
        let result = if input == serde_json::json!({"operation":"mailbox-wake"}) {
            with_runtime_options(&RuntimeOptions::default(), |runtime| {
                namedrop::drain_mailboxes(runtime).and_then(|report| {
                    serde_json::to_value(report).map_err(|error| error.to_string())
                })
            })
        } else {
            run_lifecycle(
                "event.agent-status-changed",
                input,
                &RuntimeOptions::default(),
            )
        };
        match result {
            Ok(result) => match drain_report(&result) {
                Ok(report) if report.awaits_retry() => {
                    wait_for_reconciliation_retry(&mut failures);
                }
                Ok(report) => {
                    let _ = fs::remove_file(marker);
                    failures = 0;
                    deadline_ms = deadline_ms
                        .into_iter()
                        .chain(report.next_grace_due_ms())
                        .min();
                }
                Err(_) => {
                    let _ = fs::remove_file(marker);
                    failures = 0;
                }
            },
            Err(error) if !retryable_agent_status_reconciliation(&error) => {
                let _ = fs::remove_file(marker);
                failures = 0;
            }
            Err(_) => wait_for_reconciliation_retry(&mut failures),
        }
    }
}

fn wait_for_marker_or_deadline(directory: &Path, due_ms: u64) {
    loop {
        let now = SystemClock.now_unix_ms();
        if now >= due_ms
            || pending_agent_status_reconciliations(directory)
                .is_ok_and(|pending| !pending.is_empty())
        {
            return;
        }
        thread::sleep(Duration::from_millis((due_ms - now).min(100)));
    }
}

fn wait_for_reconciliation_retry(failures: &mut usize) {
    let delays = [100, 250, 500, 1_000, 5_000];
    let delay = delays[(*failures).min(delays.len() - 1)];
    *failures = failures.saturating_add(1);
    thread::sleep(Duration::from_millis(delay));
}

fn retryable_agent_status_reconciliation(error: &str) -> bool {
    if error == "Namedrop is not activated for this space"
        || error == "status event pane is not live in its Worktree"
        || error.starts_with("invalid operation input: ")
    {
        return false;
    }
    let code = serde_json::from_str::<serde_json::Value>(error)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    // A status event is a durable request to inspect current state. Only a
    // definitive obsolete/invalid request may be discarded. Unknown host or
    // storage failures retain the event and retry with capped backoff; prompt
    // delivery still independently holds any ambiguous invocation.
    !matches!(
        code.as_deref(),
        Some(
            "invalid_request"
                | "invalid_args"
                | "invalid_input"
                | "unsupported"
                | "unknown_command"
                | "pane_not_found"
                | "workspace_not_found"
                | "worktree_not_found"
        )
    )
}

fn write_result(result: &CommandResult, exit_code: u8, human: bool) -> ExitCode {
    if human {
        let text = render_command_result_to_text(result);
        let written = if exit_code == 0 {
            writeln!(io::stdout().lock(), "{text}")
        } else {
            writeln!(io::stderr().lock(), "{text}")
        };
        return ExitCode::from(if written.is_ok() { exit_code } else { 74 });
    }
    let mut output = io::stdout().lock();
    if serde_json::to_writer(&mut output, result).is_err() || writeln!(output).is_err() {
        return ExitCode::from(74);
    }
    ExitCode::from(exit_code)
}
