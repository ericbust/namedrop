use clap::{Parser, error::ErrorKind};
mod agent_status_ui;
mod naming_ui;
mod popup_input;
mod prompt_hook;
mod stop_hook;
use namedrop::{
    Clock, CommandResult, HerdrCommandExecutor, LifecycleRuntime, ProcessHerdr, ProcessIds,
    SystemClock, agent_result_view,
    cli::{Cli, ResultFormat, RuntimeOptions},
    describe_operations, diagnose_collaboration, execute_context_operation,
    execute_control_operation, execute_group_operation, execute_handoff_operation,
    execute_integration_operation, execute_lifecycle_operation, execute_message_operation,
    execute_session_start_hook, execute_user_prompt_submit_hook, is_context_operation,
    is_control_operation, is_integration_operation, is_lifecycle_operation, is_message_operation,
    render_command_result_to_text, validate_message_input,
};
use serde::Deserialize;
use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn main() -> ExitCode {
    if env::args_os().skip(1).eq(["--runtime-info"]) {
        println!("{}", include_str!("../runtime.json").trim());
        return ExitCode::SUCCESS;
    }
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments.first().is_some_and(|arg| arg == "reminder-clock") {
        return match arguments.as_slice() {
            [_, state, socket] => {
                match namedrop::reminder::run_clock(Path::new(state), &socket.to_string_lossy()) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(_) => ExitCode::from(65),
                }
            }
            _ => ExitCode::from(64),
        };
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == "--deferred-worker" || arg == "--deferred-execute")
    {
        let result = match arguments.as_slice() {
            [mode, state, socket] if mode == "--deferred-worker" => {
                namedrop::deferred_commands::run_worker(Path::new(state), &socket.to_string_lossy())
            }
            [mode, state, socket, key] if mode == "--deferred-execute" => {
                namedrop::deferred_commands::execute_queued(
                    Path::new(state),
                    &socket.to_string_lossy(),
                    &key.to_string_lossy(),
                    schedule_dispatch,
                )
            }
            _ => Err("invalid deferred command arguments".into()),
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(65)
            }
        };
    }
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
            if arguments.first().is_some_and(|arg| arg == "hook")
                && arguments.get(1).is_some_and(|arg| arg == "subagent")
            {
                return ExitCode::SUCCESS;
            }
            let args: Vec<_> = env::args_os().skip(1).collect();
            let format = if args.iter().any(|arg| arg == "--json") {
                ResultFormat::Full
            } else if args.iter().any(|arg| arg == "--human") || io::stdout().is_terminal() {
                ResultFormat::Human
            } else {
                ResultFormat::Agent
            };
            return write_result(
                &CommandResult::invalid_request(error.to_string()),
                64,
                format,
            );
        }
    };
    if let Some((state, host)) = cli.subagent_hook() {
        let _ = namedrop::reminder::ensure_clock(state, None);
        return run_subagent_hook(state, host);
    }
    if let Some(state_directory) = cli.stop_hook_state_directory() {
        let _ = namedrop::reminder::ensure_clock(state_directory, None);
        stop_hook::run(state_directory);
        return ExitCode::SUCCESS;
    }
    if let Some(state_directory) = cli.stop_worker_state_directory() {
        let _ = namedrop::reminder::ensure_clock(state_directory, None);
        stop_hook::run_worker(state_directory);
        return ExitCode::SUCCESS;
    }
    if let Some((terminal, rows, cols)) = cli.dashboard_observer() {
        return namedrop::dashboard::run_observer_guard(terminal, rows, cols);
    }
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
    // The status worker outlives the hook that starts it and takes the
    // command gate itself, around each event it reconciles.
    if cli.is_agent_status_worker_hook() {
        if let Some(state) = selected_state.as_deref() {
            let socket = env::var("HERDR_SOCKET_PATH").ok();
            let _ = namedrop::reminder::ensure_clock(state, socket.as_deref());
        }
        return run_agent_status_worker();
    }
    // A dashboard successor started mid-update waits a bounded time rather
    // than parking forever behind a stuck migration.
    let gate_wait = cli.is_dashboard_server().then_some(Duration::from_secs(60));
    let mut command_guard = match selected_state
        .as_deref()
        .map(|state| namedrop::runtime_command_guard(state, gate_wait))
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
    if let Some(state) = selected_state.as_deref()
        && !cli.dashboard_inherit_claim()
    {
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
    if cli.dashboard_tailscale() == Some(false) {
        let result = selected_state
            .as_deref()
            .ok_or_else(|| "HERDR_PLUGIN_STATE_DIR or --plugin-state is required".to_owned())
            .and_then(namedrop::dashboard::disable_tailscale);
        return finish(
            "dashboard",
            result,
            cli.result_format(io::stdout().is_terminal()),
        );
    }
    if let Some(state) = selected_state.as_deref() {
        let socket = cli
            .runtime
            .herdr_socket
            .clone()
            .or_else(|| env::var("HERDR_SOCKET_PATH").ok());
        let _ = namedrop::reminder::ensure_clock(state, socket.as_deref());
    }
    if cli.is_dashboard_server() {
        let Some(state) = selected_state else {
            eprintln!("dashboard-server requires --plugin-state");
            return ExitCode::from(64);
        };
        return namedrop::dashboard::serve(
            &state,
            command_guard.take().flatten(),
            cli.dashboard_ui(),
            cli.dashboard_inherit_claim(),
        );
    }
    let format = cli.result_format(io::stdout().is_terminal());
    let input_file = cli.input_is_file();
    let omitted = "Input file rejected; file contents are omitted from diagnostics.";
    let write_result = |result: &CommandResult, code| write_result(result, code, format);
    let input_error = |error: String| {
        if input_file && error.starts_with("invalid operation input: ") {
            omitted.to_owned()
        } else {
            error
        }
    };
    let finish = |operation, result: Result<serde_json::Value, String>| {
        finish(operation, result.map_err(input_error), format)
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
    if let Some(open_browser) = cli.dashboard_open_browser() {
        return finish(
            "dashboard",
            with_runtime_options(&cli.runtime, |runtime| {
                namedrop::dashboard::open(
                    runtime,
                    open_browser,
                    cli.dashboard_ui(),
                    cli.dashboard_tailscale(),
                )
            }),
        );
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

    match run_bound_operation(operation, input, &cli.runtime) {
        Ran::Refused(error) => {
            write_result(&CommandResult::invalid_request(input_error(error)), 64)
        }
        Ran::Finished(result) => finish(operation, result),
    }
}

/// An input the CLI refuses before running anything, or the operation's own
/// result.
enum Ran {
    Refused(String),
    Finished(Result<serde_json::Value, String>),
}

/// One operation the way the CLI runs it once its input is parsed: a compact
/// reply is completed from its attempt, a message is validated, a sandboxed
/// pane queues the command outside, and send, reply and complete wake the
/// mailboxes. The Stop hook settles a final reply through this same path, so
/// every guard of the agent's own bound line applies to it.
fn run_bound_operation(
    operation: &'static str,
    input: serde_json::Value,
    options: &RuntimeOptions,
) -> Ran {
    let input = if operation == "reply" {
        let state = options
            .plugin_state
            .clone()
            .or_else(|| env::var_os("HERDR_PLUGIN_STATE_DIR").map(PathBuf::from));
        match namedrop::expand_attempt_reply(input, state.as_deref()) {
            Ok(input) => input,
            Err(error) => return Ran::Refused(error),
        }
    } else {
        input
    };
    if is_message_operation(operation)
        && let Err(error) = validate_message_input(operation, input.clone())
    {
        return Ran::Refused(error);
    }

    if let Some(result) = namedrop::deferred_commands::try_defer(operation, &input, options) {
        return Ran::Finished(result);
    }

    let result = if matches!(
        operation,
        "handoff.begin"
            | "handoff.resolve"
            | "handoff.adopt"
            | "handoff.acknowledge-granted"
            | "handoff.status"
            | "handoff.results"
            | "handoff.preview"
    ) {
        run_operation(operation, input, OperationKind::Handoff, options)
    } else if operation.starts_with("reminder.") {
        run_operation(operation, input, OperationKind::Reminder, options)
    } else if matches!(operation, "group.list" | "group.set" | "group.remove") {
        run_operation(operation, input, OperationKind::Group, options)
    } else if is_control_operation(operation) {
        run_operation(operation, input, OperationKind::Control, options)
    } else if is_context_operation(operation) {
        run_operation(operation, input, OperationKind::Context, options)
    } else if is_integration_operation(operation) {
        run_operation(operation, input, OperationKind::Integration, options)
    } else if is_message_operation(operation) {
        run_operation(operation, input, OperationKind::Message, options)
    } else if is_lifecycle_operation(operation) {
        run_lifecycle(operation, input, options)
    } else {
        Err(format!("unsupported operation: {operation}"))
    };
    Ran::Finished(result.map(|mut data| {
        schedule_dispatch(operation, &mut data, options);
        data
    }))
}

fn run_subagent_hook(state: &Path, host: namedrop::AgentRuntime) -> ExitCode {
    if thread::Builder::new()
        .spawn(|| {
            thread::sleep(Duration::from_secs(5));
            std::process::exit(0);
        })
        .is_err()
    {
        return ExitCode::SUCCESS;
    }
    let mut input = String::new();
    let cap = namedrop::subagent::SUBAGENT_STDIN_CAP;
    if io::stdin()
        .take(cap as u64 + 1)
        .read_to_string(&mut input)
        .is_err()
        || input.len() > cap
    {
        return ExitCode::SUCCESS;
    }
    let Some((session, event)) =
        namedrop::subagent::Event::parse(host, &input, &namedrop::subagent::DiskFiles)
    else {
        return ExitCode::SUCCESS;
    };
    let identity = env::var("HERDR_SOCKET_PATH")
        .ok()
        .zip(env::var("HERDR_WORKSPACE_ID").ok())
        .zip(env::var("HERDR_PANE_ID").ok())
        .and_then(|((socket, workspace), pane)| {
            namedrop::subagent::ParentIdentity::new(&socket, &workspace, &pane, &session)
        });
    if let Some(parent) = identity {
        let _ = namedrop::subagent::record(
            state,
            &parent,
            event,
            SystemClock.now_unix_ms().min(i64::MAX as u64) as i64,
        );
    }
    ExitCode::SUCCESS
}

/// Send, reply and complete only commit. The space's mailbox worker, a detached
/// process, dispatches what they queued after the command has answered, so the
/// caller never waits on the recipient's settle delay or prompt. When no worker
/// can start, the command drains before it answers, so delivery still happens.
fn schedule_dispatch(operation: &str, data: &mut serde_json::Value, options: &RuntimeOptions) {
    let hands_off = matches!(
        operation,
        "send" | "reply" | "complete" | namedrop::final_reply::STOP_NO_REPLY
    );
    if !hands_off && !needs_mailbox_watcher(data) {
        return;
    }
    let woken = wake_mailboxes(options);
    if let Err(error) = &woken {
        eprintln!("Command queued; could not start shell watcher: {error}");
    }
    if !hands_off {
        return;
    }
    let Err(error) = woken else {
        data["dispatch"] = serde_json::json!("scheduled");
        return;
    };
    data["dispatch"] = serde_json::json!("waiting");
    data["dispatch_error"] = serde_json::json!(error);
    if let Err(error) = with_runtime_options(options, |runtime| drain_inline(data, runtime)) {
        data["drain"] = serde_json::json!({"error": error});
    }
}

/// Drains every space a mailbox worker would have: this one into `drain`, the
/// others into `drains` by space id.
fn drain_inline(
    data: &mut serde_json::Value,
    runtime: &LifecycleRuntime<'_, ProcessHerdr, SystemClock, ProcessIds>,
) -> Result<(), String> {
    let here = current_space(runtime)?;
    for space in namedrop::spaces_to_wake(runtime.state_directory, runtime.session_socket_id, &here)
    {
        let receipt = namedrop::drain_receipt(&LifecycleRuntime {
            workspace_id: Some(&space),
            ..*runtime
        });
        if space == here {
            data["drain"] = receipt;
        } else {
            data["drains"][&space] = receipt;
        }
    }
    Ok(())
}

fn wake_mailboxes(options: &RuntimeOptions) -> Result<(), String> {
    with_runtime_options(options, wake_runtime_mailboxes)
}

fn drain_report(result: &serde_json::Value) -> Result<namedrop::DrainReport, String> {
    serde_json::from_value(result.get("drain").unwrap_or(result).clone())
        .map_err(|error| error.to_string())
}

fn needs_mailbox_watcher(result: &serde_json::Value) -> bool {
    namedrop::drain_reports(result).any(|(_, report)| report.needs_mailbox_worker())
}

#[cfg(test)]
mod drain_report_tests {
    use super::{drain_report, needs_mailbox_watcher};
    use namedrop::{DeliveryAttemptId, DrainReport, GraceDeadline};
    use serde_json::json;

    #[test]
    fn deferred_pane_locks_start_a_worker_but_settled_reports_do_not() {
        let deferred = DrainReport {
            pane_lock_deferred: vec![namedrop::PaneAddressId::new("address-1")],
            ..DrainReport::default()
        };
        assert!(deferred.awaits_retry());
        assert!(needs_mailbox_watcher(&json!({"drain": deferred})));
        assert!(!needs_mailbox_watcher(&json!(DrainReport::default())));
    }

    #[test]
    fn nested_operation_receipts_start_a_worker() {
        let deferred = DrainReport {
            pane_lock_deferred: vec![namedrop::PaneAddressId::new("address-1")],
            ..DrainReport::default()
        };
        assert!(needs_mailbox_watcher(&json!({"delivery": deferred})));
        assert!(needs_mailbox_watcher(&json!({
            "drain": DrainReport::default(),
            "delivery": deferred,
        })));
        assert!(needs_mailbox_watcher(
            &json!({"drain": DrainReport::default(), "drains": {
                "a-settled-space": DrainReport::default(),
                "z-deferred-space": deferred,
            }})
        ));
    }

    #[test]
    fn retry_without_event_round_trips_and_old_reports_need_no_worker() {
        let old = json!(DrainReport::default());
        assert!(old.get("retry_without_event").is_none());
        assert!(!drain_report(&old).unwrap().retry_without_event);
        assert!(!needs_mailbox_watcher(&old));
        let retry = DrainReport {
            requeued: vec![DeliveryAttemptId::new("attempt-1")],
            retry_without_event: true,
            ..DrainReport::default()
        };
        let restored = drain_report(&json!(retry)).unwrap();
        assert!(restored.retry_without_event);
        assert!(restored.needs_mailbox_worker());
        assert!(restored.awaits_retry());
    }

    #[test]
    fn a_draft_hold_starts_a_worker_without_a_failure_retry() {
        let held = DrainReport {
            draft_held: vec![namedrop::DraftHold {
                address_id: namedrop::PaneAddressId::new("address-1"),
                pane_id: "pane-1".to_owned(),
                agent: namedrop::AgentRuntime::Codex,
            }],
            ..DrainReport::default()
        };
        let result = json!({"drain": held});
        assert_eq!(drain_report(&result).unwrap().draft_held, held.draft_held);
        assert!(needs_mailbox_watcher(&result));
        assert!(!held.awaits_retry());
    }

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
        assert_eq!(drain_report(&result).unwrap().next_due_ms(), Some(31_000));
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
    schedule_mailbox_workers(
        runtime.state_directory,
        runtime.session_socket_id,
        &current_space(runtime)?,
        false,
    )
}

fn current_space(
    runtime: &LifecycleRuntime<'_, ProcessHerdr, SystemClock, ProcessIds>,
) -> Result<String, String> {
    match runtime.workspace_id {
        Some(id) => Ok(id.to_owned()),
        None => Ok(namedrop::discover_worktree(runtime.herdr, runtime.cwd)?.workspace_id),
    }
}

fn schedule_mailbox_workers(
    state_directory: &Path,
    socket_id: &str,
    here: &str,
    wait_for_workers: bool,
) -> Result<(), String> {
    let mut failures = Vec::new();
    let mut workers = Vec::new();
    for space in namedrop::spaces_to_wake(state_directory, socket_id, here) {
        let scheduled = (|| -> Result<(), String> {
            let directory = reconciliation_directory(state_directory, socket_id, &space);
            create_reconciliation_directory(&directory)?;
            queue_agent_status_reconciliation(
                &directory,
                &serde_json::json!({"operation":"mailbox-wake"}),
            )
            .map_err(|error| error.to_string())?;
            let worker = status_worker_command()?
                .env("HERDR_PLUGIN_STATE_DIR", state_directory)
                .env("HERDR_SOCKET_PATH", socket_id)
                .env("HERDR_WORKSPACE_ID", &space)
                .spawn()
                .map_err(|error| error.to_string())?;
            workers.push((space.clone(), worker));
            Ok(())
        })();
        if let Err(error) = scheduled {
            failures.push(format!("{space}: {error}"));
        }
    }
    if wait_for_workers {
        for (space, mut worker) in workers {
            if let Err(error) = worker.wait() {
                failures.push(format!("{space}: {error}"));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
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
    format: ResultFormat,
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
            write_result(&CommandResult::success(operation, data), code, format)
        }
        Err(error) => write_result(
            &CommandResult::rejected(operation, "operation_failed", error),
            65,
            format,
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
    Reminder,
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
    with_runtime_options(options, |runtime| {
        let result = match kind {
            OperationKind::Reminder => {
                namedrop::execute_reminder_operation(operation, input, runtime)
            }
            OperationKind::Handoff => execute_handoff_operation(operation, input, runtime),
            OperationKind::Group => execute_group_operation(operation, input, runtime),
            OperationKind::Context => execute_context_operation(operation, input, runtime),
            OperationKind::Control => execute_control_operation(operation, input, runtime),
            OperationKind::Lifecycle => execute_lifecycle_operation(operation, input, runtime),
            OperationKind::Message => execute_message_operation(operation, input, runtime),
            OperationKind::Integration => execute_integration_operation(operation, input, runtime),
        };
        if matches!(
            operation,
            "activate"
                | "name"
                | "register"
                | "reconcile"
                | "event.pane-lifecycle"
                | "event.agent-status-changed"
        ) {
            let _ = namedrop::deferred_commands::ensure_worker(
                runtime.state_directory,
                runtime.session_socket_id,
            );
        }
        result
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
    let directories = namedrop::InheritedDirectories::from_environment();
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
        home_directory: directories.home(),
        agent_config: directories.agent_config(),
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
    let prompted = prompt_hook::record(state_directory, &input);
    namedrop::forget_shown_roster(&input, state_directory);
    namedrop::record_hook_run(
        &input,
        state_directory,
        env::var_os("PLUGIN_ROOT").as_deref().map(Path::new),
    );
    let mut output = namedrop::execute_codex_app_server_hook(&input).or_else(|| {
        with_runtime_at(state_directory, |runtime| {
            let _ = namedrop::deferred_commands::ensure_worker(
                runtime.state_directory,
                runtime.session_socket_id,
            );
            Ok(execute(&input, runtime))
        })
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
    if let (Some(prompted), Some(output)) = (&prompted, &output)
        && output["decision"] == "block"
    {
        prompted.blocked(state_directory);
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
        schedule_mailbox_workers(state_directory, &socket_id, &space_id, true)
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
    if let Err(error) = create_reconciliation_directory(&directory).and_then(|()| {
        queue_agent_status_reconciliation(&directory, &input).map_err(|error| error.to_string())
    }) {
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
    Ok(reconciliation_directory(
        &state_directory,
        &socket_id,
        &space_id,
    ))
}

fn reconciliation_directory(state_directory: &Path, socket_id: &str, space_id: &str) -> PathBuf {
    namedrop::status_queue::directory(state_directory, socket_id, space_id)
}

fn create_reconciliation_directory(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())
}

fn queue_agent_status_reconciliation(
    directory: &Path,
    input: &serde_json::Value,
) -> io::Result<()> {
    let occurred_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let marker_id = format!("{}-{occurred_at}", std::process::id());
    let queued = directory.join(format!("queued-{marker_id}"));
    let pending = directory.join(format!("pending-{marker_id}"));
    let bytes = serde_json::to_vec(input)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&queued)?;
    if let Err(error) = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&queued, pending))
        .and_then(|()| fs::File::open(directory).and_then(|file| file.sync_all()))
    {
        let _ = fs::remove_file(queued);
        return Err(error);
    }
    Ok(())
}

fn pending_agent_status_reconciliations(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut markers = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    markers.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("pending-"))
    });
    markers.sort();
    Ok(markers)
}

fn pending_agent_status_reconciliations_with_retry(
    directory: &Path,
    failures: &mut usize,
    scan: &mut impl FnMut(&Path) -> io::Result<Vec<PathBuf>>,
) -> io::Result<Vec<PathBuf>> {
    loop {
        match scan(directory) {
            Ok(pending) => return Ok(pending),
            Err(error) if !retryable_queue_scan(&error) => return Err(error),
            Err(_) => wait_for_reconciliation_retry(failures, false),
        }
    }
}

fn retryable_queue_scan(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::EINTR | libc::EAGAIN | libc::ENOMEM)
    )
}

fn run_agent_status_worker() -> ExitCode {
    // The hook that starts the worker runs in a pane's directory, which can be
    // removed while the worker lives; every reconcile reads the current one.
    if let Some(state) = env::var_os("HERDR_PLUGIN_STATE_DIR").map(PathBuf::from)
        && state.is_absolute()
    {
        let _ = env::set_current_dir(state);
    }
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
    run_agent_status_worker_queue(&directory, lock, None, pending_agent_status_reconciliations)
}

enum ReconciliationWork {
    Marker(PathBuf),
    Wake,
    DraftRecheck(DraftRecheck),
}

impl ReconciliationWork {
    fn remove_marker(&self) -> io::Result<()> {
        match self {
            Self::Marker(marker) => fs::remove_file(marker),
            Self::Wake | Self::DraftRecheck(_) => Ok(()),
        }
    }
}

/// Prompt boxes whose drafts hold mail, and when the worker reads them again.
/// Herdr sends no event when a prompt box changes, so this is how mail goes in
/// soon after the person sends or clears a draft.
struct DraftRecheck {
    held: Vec<namedrop::DraftHold>,
    due: Instant,
    /// How many reports in a row found a draft in the way.
    rechecks: usize,
}

/// The wait before the next read of a draft-held box, by `DraftRecheck::rechecks`.
/// A draft that stays a while costs one pane read every 10 seconds.
const DRAFT_RECHECK_DELAYS_MS: [u64; 5] = [2_000, 3_000, 5_000, 8_000, 10_000];

impl DraftRecheck {
    /// The recheck after a report that found `held`, following `previous`
    /// rechecks in a row, or `None` once no draft holds mail.
    fn after(
        held: Vec<namedrop::DraftHold>,
        previous: Option<usize>,
        now: Instant,
    ) -> Option<Self> {
        if held.is_empty() {
            return None;
        }
        let rechecks = previous.map_or(0, |count| count + 1);
        let delay = DRAFT_RECHECK_DELAYS_MS[rechecks.min(DRAFT_RECHECK_DELAYS_MS.len() - 1)];
        Some(Self {
            held,
            due: now + Duration::from_millis(delay),
            rechecks,
        })
    }
}

/// The monotonic instant at a Unix time a drain report names. The worker
/// waits on the monotonic clock, which the wall clock drifts from. A time
/// more than a day away, which no report sets, is drained again in a day
/// rather than overflow the clock.
fn instant_at_unix_ms(due_ms: u64) -> Instant {
    let wait = Duration::from_millis(due_ms.saturating_sub(SystemClock.now_unix_ms()));
    Instant::now() + wait.min(Duration::from_secs(24 * 60 * 60))
}

fn permanently_unreadable_marker(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidData | io::ErrorKind::IsADirectory
    ) || matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP))
}

const REJECTED_MARKER_RETENTION: usize = 8;

fn prune_rejected_markers(directory: &Path) -> io::Result<()> {
    let mut rejected = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("rejected-") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        rejected.push((metadata.modified()?, entry.path(), metadata.is_dir()));
    }
    rejected.sort_by(|left, right| right.cmp(left));
    for (_, path, is_directory) in rejected.into_iter().skip(REJECTED_MARKER_RETENTION) {
        let _ = if is_directory {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
    }
    Ok(())
}

fn run_agent_status_worker_queue(
    directory: &Path,
    lock: fs::File,
    deadline: Option<Instant>,
    scan: impl FnMut(&Path) -> io::Result<Vec<PathBuf>>,
) -> ExitCode {
    run_agent_status_worker_queue_with_io(
        directory,
        lock,
        deadline,
        scan,
        |path| fs::read(path),
        |input| {
            // The worker can wait out retries for days, so it holds the
            // command gate only while it reconciles, and an update between
            // units hands the queue to the runtime it selected.
            let _gate = match env::var_os("HERDR_PLUGIN_STATE_DIR").map(PathBuf::from) {
                Some(state) => {
                    let gate = namedrop::runtime_command_guard(&state, None)?;
                    namedrop::runtime_reexec_if_replaced(&state)?;
                    gate
                }
                None => None,
            };
            if input == serde_json::json!({"operation":"mailbox-wake"}) {
                with_runtime_options(&RuntimeOptions::default(), |runtime| {
                    namedrop::drain_mailboxes(runtime).and_then(|report| {
                        serde_json::to_value(report).map_err(|error| error.to_string())
                    })
                })
            } else if input["operation"] == "draft-recheck" {
                let held = serde_json::from_value(input["held"].clone())
                    .map_err(|error| format!("invalid operation input: {error}"))?;
                with_runtime_options(&RuntimeOptions::default(), |runtime| {
                    namedrop::recheck_drafts(runtime, held).and_then(|report| {
                        serde_json::to_value(report).map_err(|error| error.to_string())
                    })
                })
            } else {
                run_lifecycle(
                    "event.agent-status-changed",
                    input,
                    &RuntimeOptions::default(),
                )
            }
        },
    )
}

fn run_agent_status_worker_queue_with_io(
    directory: &Path,
    lock: fs::File,
    deadline: Option<Instant>,
    scan: impl FnMut(&Path) -> io::Result<Vec<PathBuf>>,
    read: impl FnMut(&Path) -> io::Result<Vec<u8>>,
    reconcile: impl FnMut(serde_json::Value) -> Result<serde_json::Value, String>,
) -> ExitCode {
    run_agent_status_worker_queue_with_policy(
        directory,
        lock,
        deadline,
        scan,
        read,
        reconcile,
        namedrop::status_queue::GIVE_UP_AFTER,
    )
}

fn run_agent_status_worker_queue_with_policy(
    directory: &Path,
    lock: fs::File,
    mut deadline: Option<Instant>,
    mut scan: impl FnMut(&Path) -> io::Result<Vec<PathBuf>>,
    mut read: impl FnMut(&Path) -> io::Result<Vec<u8>>,
    mut reconcile: impl FnMut(serde_json::Value) -> Result<serde_json::Value, String>,
    give_up_after: Duration,
) -> ExitCode {
    let mut failures = 0_usize;
    let mut blocked = std::collections::HashSet::new();
    let mut read_failures = std::collections::HashMap::<PathBuf, usize>::new();
    let mut drafts: Option<DraftRecheck> = None;
    let _ = prune_rejected_markers(directory);
    loop {
        let mut scan_available = |directory: &Path| {
            scan(directory).map(|mut pending| {
                pending.retain(|marker| !blocked.contains(marker));
                pending
            })
        };
        let Ok(pending) = pending_agent_status_reconciliations_with_retry(
            directory,
            &mut failures,
            &mut scan_available,
        ) else {
            return ExitCode::SUCCESS;
        };
        let work = if let Some(marker) = pending.into_iter().next() {
            ReconciliationWork::Marker(marker)
        } else if let Some(due) = deadline
            .into_iter()
            .chain(drafts.as_ref().map(|recheck| recheck.due))
            .min()
        {
            let now = Instant::now();
            if now < due {
                wait_for_marker_or_deadline(directory, due, &mut scan_available);
                continue;
            }
            if deadline.is_some_and(|deadline| deadline <= now) {
                deadline = None;
                ReconciliationWork::Wake
            } else {
                ReconciliationWork::DraftRecheck(drafts.take().expect("the recheck is due"))
            }
        } else {
            let _ = lock.unlock();
            // Recheck after unlocking so a producer that lost try_lock cannot strand work.
            if pending_agent_status_reconciliations_with_retry(
                directory,
                &mut failures,
                &mut scan_available,
            )
            .is_ok_and(|pending| !pending.is_empty())
                && lock.try_lock().is_ok()
            {
                continue;
            }
            return ExitCode::SUCCESS;
        };
        let input = match &work {
            ReconciliationWork::Wake => serde_json::json!({"operation":"mailbox-wake"}),
            ReconciliationWork::DraftRecheck(recheck) => {
                serde_json::json!({"operation": "draft-recheck", "held": recheck.held})
            }
            ReconciliationWork::Marker(marker) => {
                let bytes = match read(marker) {
                    Ok(bytes) => {
                        read_failures.remove(marker);
                        bytes
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        namedrop::status_queue::clear_failure(directory, marker);
                        continue;
                    }
                    Err(error) if !permanently_unreadable_marker(&error) => {
                        let count = read_failures.entry(marker.clone()).or_default();
                        *count += 1;
                        if *count >= 5 {
                            blocked.insert(marker.clone());
                        } else {
                            wait_for_reconciliation_retry(&mut failures, false);
                        }
                        continue;
                    }
                    Err(_) => {
                        let rejected = marker.with_file_name(format!(
                            "rejected-{}",
                            marker.file_name().unwrap().to_string_lossy()
                        ));
                        match fs::rename(marker, rejected) {
                            Ok(()) => {
                                let _ = prune_rejected_markers(directory);
                            }
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(_) => {
                                blocked.insert(marker.clone());
                            }
                        }
                        continue;
                    }
                };
                let Ok(input) = serde_json::from_slice(&bytes) else {
                    let _ = work.remove_marker();
                    continue;
                };
                input
            }
        };
        let event = input.clone();
        let outcome = reconcile(input);
        if let ReconciliationWork::Marker(marker) = &work
            && !matches!(&outcome, Err(error) if retryable_agent_status_reconciliation(error))
        {
            namedrop::status_queue::clear_failure(directory, marker);
        }
        match outcome {
            Ok(result) => match drain_report(&result) {
                Ok(report) => {
                    if !report.awaits_retry()
                        || !report.submitted.is_empty()
                        || !report.settled.is_empty()
                        || !report.failed.is_empty()
                        || !report.held.is_empty()
                        || !report.expired.is_empty()
                    {
                        failures = 0;
                    }
                    let retry_due = report.awaits_retry().then(|| {
                        let delay = reconciliation_report_retry_delay(failures, &report);
                        failures = failures.saturating_add(1);
                        Instant::now() + delay
                    });
                    deadline = deadline
                        .into_iter()
                        .chain(retry_due)
                        .chain(report.next_due_ms().map(instant_at_unix_ms))
                        .min();
                    let previous = match &work {
                        ReconciliationWork::DraftRecheck(recheck) => Some(recheck.rechecks),
                        _ => drafts.as_ref().map(|recheck| recheck.rechecks),
                    };
                    drafts = DraftRecheck::after(report.draft_held, previous, Instant::now());
                    if work.remove_marker().is_err() {
                        wait_for_reconciliation_retry(&mut failures, false);
                    }
                }
                Err(_) => {
                    let _ = work.remove_marker();
                    failures = 0;
                }
            },
            Err(error) if !retryable_agent_status_reconciliation(&error) => {
                let _ = work.remove_marker();
                failures = 0;
            }
            Err(error) => {
                if let ReconciliationWork::Marker(marker) = &work {
                    let now = SystemClock.now_unix_ms();
                    let failure = namedrop::status_queue::record_failure(
                        directory, marker, &event, &error, now,
                    );
                    // A later status event inspects the same current state, so
                    // one that keeps failing must not hold every later one back.
                    // A queued wake carries a delivery's retry, so it waits out
                    // any error instead.
                    if event != serde_json::json!({"operation":"mailbox-wake"})
                        && namedrop::status_queue::should_set_aside(&failure, give_up_after)
                    {
                        match namedrop::status_queue::set_aside(directory, marker, &failure, now) {
                            Ok(()) => {
                                let _ = prune_rejected_markers(directory);
                                failures = 0;
                                // A delivery queued behind the event still drains
                                // once the cause clears.
                                deadline = Some(Instant::now());
                                continue;
                            }
                            Err(_) if !marker.exists() => {
                                namedrop::status_queue::clear_failure(directory, marker);
                                continue;
                            }
                            Err(_) => {}
                        }
                    }
                }
                match work {
                    ReconciliationWork::Wake => deadline = Some(Instant::now()),
                    ReconciliationWork::DraftRecheck(recheck) => {
                        drafts = Some(DraftRecheck {
                            due: Instant::now(),
                            ..recheck
                        });
                    }
                    ReconciliationWork::Marker(_) => {}
                }
                wait_for_reconciliation_retry(&mut failures, false);
            }
        }
    }
}

fn wait_for_marker_or_deadline(
    directory: &Path,
    due: Instant,
    scan: &mut impl FnMut(&Path) -> io::Result<Vec<PathBuf>>,
) {
    loop {
        let now = Instant::now();
        if now >= due {
            return;
        }
        match scan(directory) {
            Ok(pending) if pending.is_empty() => {}
            Ok(_) => return,
            Err(error) if !retryable_queue_scan(&error) => return,
            Err(_) => {}
        }
        thread::sleep((due - now).min(Duration::from_millis(100)));
    }
}

#[cfg(test)]
mod worker_shutdown_tests {
    use super::*;

    #[test]
    fn eventless_rejections_keep_terminal_requests_on_the_short_retry() {
        let rejection = namedrop::DrainReport {
            retry_without_event: true,
            requeued: vec![namedrop::DeliveryAttemptId::new("rejected")],
            ..Default::default()
        };
        assert_eq!(
            reconciliation_report_retry_delay(usize::MAX, &rejection),
            Duration::from_secs(60),
            "rejections alone must retain their capped backoff"
        );
        let mut waiting = rejection.clone();
        waiting.terminal_waiting = vec![namedrop::PaneAddressId::new("terminal")];
        let mut running = rejection;
        running.terminal_running = vec![namedrop::DeliveryAttemptId::new("running")];
        for report in [waiting, running] {
            assert_eq!(
                reconciliation_report_retry_delay(usize::MAX, &report),
                Duration::from_secs(5),
                "terminal requests must retain the short retry: {report:?}"
            );
        }
    }

    #[test]
    fn persistent_eventless_rejections_limit_drains_and_keep_retrying() {
        let mut elapsed = Duration::ZERO;
        let mut drains = 0;
        while elapsed < Duration::from_secs(600) {
            let delay = reconciliation_retry_delay(drains, true);
            assert!(delay <= Duration::from_secs(60));
            assert!(!delay.is_zero());
            elapsed += delay;
            drains += 1;
        }
        assert!(
            drains <= 20,
            "persistent rejection caused {drains} drains in ten minutes"
        );
        assert_eq!(
            reconciliation_retry_delay(usize::MAX, true),
            Duration::from_secs(60)
        );
        assert_eq!(
            reconciliation_retry_delay(usize::MAX, false),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn an_outer_queue_scan_retries_transient_errors_until_pending_work_can_be_read() {
        for errno in [
            libc::EMFILE,
            libc::ENFILE,
            libc::EINTR,
            libc::EAGAIN,
            libc::ENOMEM,
        ] {
            let queue = QueueFixture::new(&format!("outer-transient-{errno}"));
            let marker = queue.0.join("pending-test");
            fs::write(&marker, b"invalid marker").unwrap();
            let mut scans = 0;
            let started = std::time::Instant::now();
            let recovered = pending_agent_status_reconciliations_with_retry(
                &queue.0,
                &mut 0,
                &mut |directory| {
                    scans += 1;
                    if scans == 1 {
                        Err(io::Error::from_raw_os_error(errno))
                    } else {
                        pending_agent_status_reconciliations(directory)
                    }
                },
            );
            assert_eq!(recovered.unwrap(), vec![marker], "errno={errno}");
            assert!(started.elapsed() >= Duration::from_millis(100));
        }
    }

    #[test]
    fn an_outer_queue_scan_stops_on_permanent_errors_without_retrying() {
        for errno in [
            libc::EACCES,
            libc::ENOTDIR,
            libc::ENOENT,
            libc::EIO,
            libc::ENOSPC,
            libc::EDQUOT,
        ] {
            let mut scans = 0;
            let result = pending_agent_status_reconciliations_with_retry(
                Path::new("unused"),
                &mut 0,
                &mut |_| {
                    scans += 1;
                    if scans == 1 {
                        Err(io::Error::from_raw_os_error(errno))
                    } else {
                        Ok(Vec::new())
                    }
                },
            );
            assert_eq!(result.unwrap_err().raw_os_error(), Some(errno));
            assert_eq!(scans, 1, "permanent errno={errno} was retried");
        }
    }

    struct QueueFixture(PathBuf);

    impl QueueFixture {
        fn new(name: &str) -> Self {
            let directory =
                env::temp_dir().join(format!("namedrop-queue-{name}-{}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }

        fn lock(&self) -> fs::File {
            let lock = fs::File::create(self.0.join("worker.lock")).unwrap();
            lock.try_lock().unwrap();
            lock
        }
    }

    impl Drop for QueueFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn deadline_drains_without_marker_writes_and_keeps_report_deadlines() {
        for mode in ["complete", "retry", "grace", "check-in", "error"] {
            let queue = QueueFixture::new(&format!("deadline-{mode}"));
            let lock = queue.lock();
            fs::set_permissions(&queue.0, fs::Permissions::from_mode(0o500)).unwrap();
            let mut drains = 0;
            let mut scans = 0;
            let result = run_agent_status_worker_queue_with_io(
                &queue.0,
                lock,
                Some(Instant::now()),
                |directory| {
                    scans += 1;
                    assert!(scans < 30, "deadline drain did not finish");
                    assert_eq!(
                        fs::read_dir(directory)?.count(),
                        1,
                        "deadline wrote a marker"
                    );
                    pending_agent_status_reconciliations(directory)
                },
                |_| panic!("deadline must not read a marker"),
                |input| {
                    assert_eq!(input, serde_json::json!({"operation":"mailbox-wake"}));
                    drains += 1;
                    assert!(drains <= 2);
                    let mut report = namedrop::DrainReport::default();
                    if drains == 1 {
                        match mode {
                            "retry" => report
                                .requeued
                                .push(namedrop::DeliveryAttemptId::new("retry")),
                            "grace" => report.pending_grace.push(namedrop::GraceDeadline {
                                attempt_id: namedrop::DeliveryAttemptId::new("grace"),
                                due_at_ms: SystemClock.now_unix_ms() + 10,
                            }),
                            "check-in" => {
                                report.check_in_due_ms = Some(SystemClock.now_unix_ms() + 10)
                            }
                            "error" => return Err("temporary storage failure".to_owned()),
                            _ => {}
                        }
                    }
                    Ok(serde_json::to_value(report).unwrap())
                },
            );
            fs::set_permissions(&queue.0, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(result, ExitCode::SUCCESS);
            assert_eq!(drains, if mode == "complete" { 1 } else { 2 });
            queue.lock();
        }
    }

    fn draft_hold() -> namedrop::DraftHold {
        namedrop::DraftHold {
            address_id: namedrop::PaneAddressId::new("address-1"),
            pane_id: "pane-1".to_owned(),
            agent: namedrop::AgentRuntime::ClaudeCode,
        }
    }

    #[test]
    fn a_draft_is_read_again_sooner_at_first_and_every_10_seconds_while_it_stays() {
        let now = Instant::now();
        let mut previous = None;
        let mut delays = Vec::new();
        for _ in 0..7 {
            let recheck = DraftRecheck::after(vec![draft_hold()], previous, now).unwrap();
            delays.push((recheck.due - now).as_millis());
            previous = Some(recheck.rechecks);
        }
        assert_eq!(delays, [2_000, 3_000, 5_000, 8_000, 10_000, 10_000, 10_000]);
        assert!(DraftRecheck::after(Vec::new(), previous, now).is_none());
    }

    #[test]
    fn a_draft_recheck_is_never_due_before_its_delay_has_passed() {
        // Timed on the monotonic clock, which the wall clock may run ahead of.
        let started = Instant::now();
        let recheck = DraftRecheck::after(vec![draft_hold()], None, Instant::now()).unwrap();
        wait_for_marker_or_deadline(Path::new("/nonexistent"), recheck.due, &mut |_| {
            Ok(Vec::new())
        });
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(2_000), "{waited:?}");
    }

    #[test]
    fn the_worker_reads_a_held_draft_again_and_stops_once_nothing_holds() {
        let queue = QueueFixture::new("draft-recheck");
        let lock = queue.lock();
        let mut inputs = Vec::new();
        let started = std::time::Instant::now();
        let result = run_agent_status_worker_queue_with_io(
            &queue.0,
            lock,
            Some(Instant::now()),
            pending_agent_status_reconciliations,
            |_| panic!("no marker was queued"),
            |input| {
                inputs.push((started.elapsed(), input));
                assert!(inputs.len() <= 2, "the worker kept rechecking");
                let report = namedrop::DrainReport {
                    draft_held: if inputs.len() == 1 {
                        vec![draft_hold()]
                    } else {
                        Vec::new()
                    },
                    ..Default::default()
                };
                Ok(serde_json::to_value(report).unwrap())
            },
        );
        assert_eq!(result, ExitCode::SUCCESS);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].1, serde_json::json!({"operation":"mailbox-wake"}));
        assert_eq!(
            inputs[1].1,
            serde_json::json!({"operation":"draft-recheck","held":[draft_hold()]})
        );
        let waited = inputs[1].0 - inputs[0].0;
        assert!(
            (Duration::from_millis(2_000)..Duration::from_millis(3_000)).contains(&waited),
            "{waited:?}"
        );
        queue.lock();
    }

    #[test]
    fn transient_marker_reads_retry_without_quarantining_work() {
        for errno in [libc::EIO, libc::ESTALE, libc::ETIMEDOUT, libc::EMFILE] {
            let queue = QueueFixture::new(&format!("marker-transient-{errno}"));
            let marker = queue.0.join("pending-work");
            fs::write(&marker, b"{}").unwrap();
            let mut reads = 0;
            let mut reconciled = 0;
            let started = std::time::Instant::now();
            assert_eq!(
                run_agent_status_worker_queue_with_io(
                    &queue.0,
                    queue.lock(),
                    None,
                    pending_agent_status_reconciliations,
                    |path| {
                        reads += 1;
                        if reads == 1 {
                            Err(io::Error::from_raw_os_error(errno))
                        } else {
                            fs::read(path)
                        }
                    },
                    |_| {
                        reconciled += 1;
                        Ok(serde_json::to_value(namedrop::DrainReport::default()).unwrap())
                    },
                ),
                ExitCode::SUCCESS
            );
            assert_eq!(reconciled, 1, "errno={errno} lost work");
            assert_eq!(reads, 2);
            assert!(started.elapsed() >= Duration::from_millis(100));
            assert!(!queue.0.join("rejected-pending-work").exists());
            queue.lock();
        }
    }

    #[test]
    fn persistent_marker_read_errors_preserve_work_and_release_later_markers() {
        let queue = QueueFixture::new("persistent-marker-read");
        let marker = queue.0.join("pending-0-stuck");
        let later = queue.0.join("pending-1-later");
        fs::write(&marker, br#"{"marker":"stuck"}"#).unwrap();
        fs::write(&later, br#"{"marker":"later"}"#).unwrap();
        let reads = std::cell::Cell::new(0);
        let mut reconciled = Vec::new();
        assert_eq!(
            run_agent_status_worker_queue_with_io(
                &queue.0,
                queue.lock(),
                None,
                pending_agent_status_reconciliations,
                |path| {
                    if path == marker {
                        reads.set(reads.get() + 1);
                        assert!(reads.get() <= 5, "persistent EIO starved the later marker");
                        Err(io::Error::from_raw_os_error(libc::EIO))
                    } else {
                        fs::read(path)
                    }
                },
                |input| {
                    assert!(reads.get() <= 5);
                    reconciled.push(input);
                    Ok(serde_json::to_value(namedrop::DrainReport::default()).unwrap())
                },
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(reconciled, vec![serde_json::json!({"marker":"later"})]);
        assert_eq!(reads.get(), 5);
        assert_eq!(fs::read(&marker).unwrap(), br#"{"marker":"stuck"}"#);
        assert!(!later.exists());
        assert_eq!(
            run_agent_status_worker_queue_with_io(
                &queue.0,
                queue.lock(),
                None,
                pending_agent_status_reconciliations,
                |path| fs::read(path),
                |input| {
                    reconciled.push(input);
                    Ok(serde_json::to_value(namedrop::DrainReport::default()).unwrap())
                },
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(
            reconciled,
            vec![
                serde_json::json!({"marker":"later"}),
                serde_json::json!({"marker":"stuck"})
            ]
        );
        assert!(!marker.exists());
        queue.lock();
    }

    #[test]
    fn marker_read_retry_budgets_are_independent() {
        let queue = QueueFixture::new("independent-marker-reads");
        for name in ["pending-0", "pending-1"] {
            fs::write(queue.0.join(name), b"{}").unwrap();
        }
        let mut reads = std::collections::HashMap::new();
        let mut reconciled = 0;
        assert_eq!(
            run_agent_status_worker_queue_with_io(
                &queue.0,
                queue.lock(),
                None,
                pending_agent_status_reconciliations,
                |path| {
                    let count = reads.entry(path.to_path_buf()).or_insert(0);
                    *count += 1;
                    if *count <= 3 {
                        Err(io::Error::from_raw_os_error(libc::EIO))
                    } else {
                        fs::read(path)
                    }
                },
                |_| {
                    reconciled += 1;
                    Ok(serde_json::to_value(namedrop::DrainReport::default()).unwrap())
                },
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(reconciled, 2);
        assert_eq!(reads.len(), 2);
        assert!(reads.values().all(|count| *count == 4));
        assert!(
            pending_agent_status_reconciliations(&queue.0)
                .unwrap()
                .is_empty()
        );
        queue.lock();
    }

    #[test]
    fn failed_quarantine_skips_marker_serves_later_work_and_releases_lock() {
        let queue = QueueFixture::new("blocked-quarantine");
        let marker = queue.0.join("pending-0-poisoned");
        fs::create_dir(&marker).unwrap();
        let rejected = queue.0.join("rejected-pending-0-poisoned");
        fs::create_dir(&rejected).unwrap();
        fs::write(rejected.join("evidence"), b"keep").unwrap();
        let later = queue.0.join("pending-1-later");
        fs::write(&later, b"{}").unwrap();
        let mut scans = 0;
        let mut reconciled = 0;
        assert_eq!(
            run_agent_status_worker_queue_with_io(
                &queue.0,
                queue.lock(),
                None,
                |directory| {
                    scans += 1;
                    assert!(scans < 10, "blocked marker caused a busy loop");
                    pending_agent_status_reconciliations(directory)
                },
                |path| fs::read(path),
                |_| {
                    reconciled += 1;
                    Ok(serde_json::to_value(namedrop::DrainReport::default()).unwrap())
                },
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(reconciled, 1, "failed quarantine stranded later work");
        assert!(marker.is_dir());
        assert!(!later.exists());
        assert_eq!(fs::read(rejected.join("evidence")).unwrap(), b"keep");
        queue.lock();
    }

    #[test]
    fn idle_worker_retains_only_newest_rejections_without_following_symlinks() {
        let queue = QueueFixture::new("rejected-retention");
        let evidence = queue.0.join("external-evidence");
        fs::create_dir(&evidence).unwrap();
        fs::write(evidence.join("keep"), b"keep").unwrap();
        let old = queue.0.join("rejected-old-directory");
        fs::create_dir(&old).unwrap();
        std::os::unix::fs::symlink(&evidence, old.join("linked-evidence")).unwrap();
        let old_link = queue.0.join("rejected-old-link");
        std::os::unix::fs::symlink(&evidence, &old_link).unwrap();
        for index in 0..REJECTED_MARKER_RETENTION {
            let path = queue.0.join(format!("rejected-new-{index}"));
            let file = fs::File::create(path).unwrap();
            file.set_modified(SystemTime::now() + Duration::from_secs(10 + index as u64))
                .unwrap();
        }
        let queued = queue.0.join("queued-untouched");
        fs::write(&queued, b"keep").unwrap();
        assert_eq!(
            run_agent_status_worker_queue_with_io(
                &queue.0,
                queue.lock(),
                None,
                pending_agent_status_reconciliations,
                |_| panic!("idle queue has no markers"),
                |_| panic!("idle queue has no work"),
            ),
            ExitCode::SUCCESS
        );
        assert!(!old.exists());
        assert!(fs::symlink_metadata(&old_link).is_err());
        assert_eq!(fs::read(evidence.join("keep")).unwrap(), b"keep");
        assert!(queued.exists());
        for index in 0..REJECTED_MARKER_RETENTION {
            assert!(queue.0.join(format!("rejected-new-{index}")).exists());
        }
        queue.lock();
    }

    #[test]
    fn unreadable_markers_do_not_block_later_queue_work() {
        for name in ["mode-zero", "directory", "removed"] {
            let queue = QueueFixture::new(&format!("marker-read-{name}"));
            let marker = queue.0.join("pending-0-poisoned");
            let later = queue.0.join("pending-1-later");
            if name == "directory" {
                fs::create_dir(&marker).unwrap();
                fs::write(marker.join("evidence"), b"preserve me").unwrap();
            } else {
                fs::write(&marker, b"unreadable").unwrap();
                fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
            }
            fs::write(&later, b"invalid marker").unwrap();
            let mut scans = 0;
            assert_eq!(
                run_agent_status_worker_queue(&queue.0, queue.lock(), None, |directory| {
                    scans += 1;
                    assert!(scans < 10, "poisoned marker caused a rescan loop");
                    let pending = pending_agent_status_reconciliations(directory)?;
                    if scans == 1 && name == "removed" {
                        fs::remove_file(&marker).unwrap();
                    }
                    Ok(pending)
                }),
                ExitCode::SUCCESS
            );
            assert!(!later.exists(), "{name} marker stranded later queue work");
            assert!(!marker.exists(), "{name} marker remained pending");
            if name != "removed" {
                let quarantined = queue.0.join("rejected-pending-0-poisoned");
                assert!(quarantined.exists(), "{name} marker was not quarantined");
                if name == "directory" {
                    assert_eq!(
                        fs::read(quarantined.join("evidence")).unwrap(),
                        b"preserve me"
                    );
                }
            }
            queue.lock();
        }
    }

    /// A status event that fails the same way forever, as with a worker whose
    /// starting directory is gone, used to keep every later event waiting. It
    /// is set aside after the give-up time, the queue moves on, and the error
    /// stays where Doctor reads it.
    #[test]
    fn an_event_that_keeps_failing_is_set_aside_and_later_events_run() {
        let queue = QueueFixture::new("keeps-failing");
        let stuck = queue.0.join("pending-0-stuck");
        let later = queue.0.join("pending-1-later");
        fs::write(&stuck, br#"{"type":"pane_agent_detected","pane_id":"w5F:p1","workspace_id":"w5F","agent":"claude"}"#).unwrap();
        fs::write(
            &later,
            br#"{"type":"pane_agent_status_changed","pane_id":"w5F:p2","workspace_id":"w5F"}"#,
        )
        .unwrap();
        let mut handled = Vec::new();
        assert_eq!(
            run_agent_status_worker_queue_with_policy(
                &queue.0,
                queue.lock(),
                None,
                pending_agent_status_reconciliations,
                |path| fs::read(path),
                |input| {
                    if input["pane_id"] == "w5F:p1" {
                        Err("No such file or directory (os error 2)".to_owned())
                    } else {
                        handled.push(input["pane_id"].clone());
                        Ok(serde_json::json!({"drain": namedrop::DrainReport::default()}))
                    }
                },
                Duration::ZERO,
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(
            handled,
            vec![serde_json::json!("w5F:p2"), serde_json::Value::Null],
            "after a set-aside the mailboxes drain once, as a wake"
        );
        assert!(!stuck.exists() && queue.0.join("rejected-pending-0-stuck").exists());
        let (failing, set_aside) = namedrop::status_queue::health(&queue.0);
        assert!(failing.is_none(), "{failing:?}");
        let set_aside = set_aside.expect("the set-aside event is recorded");
        assert_eq!(
            set_aside.failure.error,
            "No such file or directory (os error 2)"
        );
        assert_eq!(
            set_aside.failure.event.as_deref(),
            Some("pane_agent_detected")
        );
        assert_eq!(set_aside.failure.pane_id.as_deref(), Some("w5F:p1"));
        assert!(set_aside.failure.attempts >= 5);
        queue.lock();
    }

    /// A queued wake, such as the one activation writes for a restarted
    /// worker, carries a delivery's retry. It waits out any error.
    #[test]
    fn a_queued_wake_is_never_set_aside() {
        let queue = QueueFixture::new("queued-wake");
        let wake = queue.0.join("pending-activation-1");
        fs::write(&wake, br#"{"operation":"mailbox-wake"}"#).unwrap();
        let mut attempts = 0;
        assert_eq!(
            run_agent_status_worker_queue_with_policy(
                &queue.0,
                queue.lock(),
                None,
                pending_agent_status_reconciliations,
                |path| fs::read(path),
                |_| {
                    attempts += 1;
                    if attempts <= 6 {
                        Err("Herdr did not answer".to_owned())
                    } else {
                        Ok(serde_json::json!({"drain": namedrop::DrainReport::default()}))
                    }
                },
                Duration::ZERO,
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(attempts, 7);
        assert!(!wake.exists() && !queue.0.join("rejected-pending-activation-1").exists());
        assert!(namedrop::status_queue::health(&queue.0).1.is_none());
        queue.lock();
    }

    /// A marker that vanishes while it fails ends its failure, so Doctor
    /// doesn't keep reporting it.
    #[test]
    fn a_vanished_failing_marker_clears_its_failure() {
        let queue = QueueFixture::new("vanished-failing");
        let marker = queue.0.join("pending-0-gone");
        let event = serde_json::json!({"type":"pane_agent_detected","pane_id":"w5F:p1"});
        fs::write(&marker, event.to_string()).unwrap();
        namedrop::status_queue::record_failure(&queue.0, &marker, &event, "No such file", 1);
        assert!(namedrop::status_queue::health(&queue.0).0.is_some());
        assert_eq!(
            run_agent_status_worker_queue_with_policy(
                &queue.0,
                queue.lock(),
                None,
                |directory| {
                    let pending = pending_agent_status_reconciliations(directory)?;
                    Ok(pending)
                },
                |path| {
                    let _ = fs::remove_file(path);
                    Err(io::Error::from(io::ErrorKind::NotFound))
                },
                |_| Ok(serde_json::json!({"drain": namedrop::DrainReport::default()})),
                Duration::ZERO,
            ),
            ExitCode::SUCCESS
        );
        assert!(namedrop::status_queue::health(&queue.0).0.is_none());
        queue.lock();
    }

    #[test]
    fn a_permanent_outer_scan_stops_the_worker_and_releases_ownership() {
        for errno in [
            libc::EACCES,
            libc::ENOTDIR,
            libc::ENOENT,
            libc::EIO,
            libc::ENOSPC,
            libc::EDQUOT,
        ] {
            let queue = QueueFixture::new(&format!("worker-permanent-{errno}"));
            let lock = queue.lock();
            let mut scans = 0;
            assert_eq!(
                run_agent_status_worker_queue(&queue.0, lock, None, |_| {
                    scans += 1;
                    if scans == 1 {
                        Err(io::Error::from_raw_os_error(errno))
                    } else {
                        Ok(Vec::new())
                    }
                }),
                ExitCode::SUCCESS
            );
            assert_eq!(scans, 1, "permanent errno={errno} kept the worker scanning");
            queue.lock();
        }
    }

    #[test]
    fn an_unlock_recheck_retries_transient_errors_and_reclaims_queued_work() {
        for errno in [
            libc::EMFILE,
            libc::ENFILE,
            libc::EINTR,
            libc::EAGAIN,
            libc::ENOMEM,
        ] {
            let queue = QueueFixture::new(&format!("unlock-transient-{errno}"));
            let marker = queue.0.join("pending-handoff");
            let lock = queue.lock();
            let observer = fs::File::open(queue.0.join("worker.lock")).unwrap();
            let mut scans = 0;
            assert_eq!(
                run_agent_status_worker_queue(&queue.0, lock, None, |directory| {
                    scans += 1;
                    if scans == 2 {
                        observer
                            .try_lock()
                            .expect("handoff scan must release worker ownership");
                        observer.unlock().unwrap();
                        fs::write(&marker, b"invalid marker").unwrap();
                        Err(io::Error::from_raw_os_error(errno))
                    } else {
                        pending_agent_status_reconciliations(directory)
                    }
                }),
                ExitCode::SUCCESS
            );
            assert!(
                !marker.exists(),
                "errno={errno} stranded the handoff marker"
            );
            observer
                .try_lock()
                .expect("worker must release its lock when done");
        }
    }

    #[test]
    fn a_retried_unlock_recheck_preserves_a_successors_lock_ownership() {
        let queue = QueueFixture::new("unlock-successor");
        let marker = queue.0.join("pending-handoff");
        let lock = queue.lock();
        let successor = fs::File::open(queue.0.join("worker.lock")).unwrap();
        let mut scans = 0;
        assert_eq!(
            run_agent_status_worker_queue(&queue.0, lock, None, |directory| {
                scans += 1;
                if scans == 2 {
                    successor
                        .try_lock()
                        .expect("successor must acquire released worker lock");
                    fs::write(&marker, b"invalid marker").unwrap();
                    Err(io::Error::from_raw_os_error(libc::EMFILE))
                } else {
                    pending_agent_status_reconciliations(directory)
                }
            }),
            ExitCode::SUCCESS
        );
        assert_eq!(
            scans, 3,
            "handoff scan must recover before checking ownership"
        );
        assert!(
            marker.exists(),
            "original worker consumed its successor's marker"
        );
        assert!(
            fs::File::open(queue.0.join("worker.lock"))
                .unwrap()
                .try_lock()
                .is_err()
        );
    }

    #[test]
    fn an_unlock_recheck_stops_on_permanent_errors_without_retrying() {
        for errno in [
            libc::EACCES,
            libc::ENOTDIR,
            libc::ENOENT,
            libc::EIO,
            libc::ENOSPC,
            libc::EDQUOT,
        ] {
            let queue = QueueFixture::new(&format!("unlock-permanent-{errno}"));
            let lock = queue.lock();
            let mut scans = 0;
            assert_eq!(
                run_agent_status_worker_queue(&queue.0, lock, None, |_| {
                    scans += 1;
                    if scans == 2 {
                        Err(io::Error::from_raw_os_error(errno))
                    } else {
                        Ok(Vec::new())
                    }
                }),
                ExitCode::SUCCESS
            );
            assert_eq!(scans, 2, "permanent errno={errno} was retried after unlock");
            queue.lock();
        }
    }

    #[test]
    fn a_transient_queue_error_preserves_the_grace_deadline() {
        for errno in [
            libc::EMFILE,
            libc::ENFILE,
            libc::EINTR,
            libc::EAGAIN,
            libc::ENOMEM,
        ] {
            let due = Instant::now() + Duration::from_millis(250);
            wait_for_marker_or_deadline(Path::new("unused"), due, &mut |_| {
                Err(io::Error::from_raw_os_error(errno))
            });
            assert!(
                Instant::now() >= due,
                "errno={errno} ended the grace wait early"
            );
        }
    }

    #[test]
    fn a_permanent_queue_error_stops_a_grace_wait_without_retrying() {
        for errno in [
            libc::EACCES,
            libc::ENOTDIR,
            libc::ENOENT,
            libc::EIO,
            libc::ENOSPC,
            libc::EDQUOT,
        ] {
            let mut scans = 0;
            wait_for_marker_or_deadline(
                Path::new("unused"),
                Instant::now() + Duration::from_secs(10),
                &mut |_| {
                    scans += 1;
                    if scans == 1 {
                        Err(io::Error::from_raw_os_error(errno))
                    } else {
                        Ok(vec![PathBuf::from("pending-test")])
                    }
                },
            );
            assert_eq!(
                scans, 1,
                "permanent errno={errno} kept the deadline wait alive"
            );
        }
    }

    #[test]
    fn a_recovered_queue_wakes_a_grace_wait_when_a_marker_arrives() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let queue = QueueFixture::new("grace-recovered");
        let directory = queue.0.clone();
        let available = Arc::new(AtomicBool::new(false));
        let reader_available = available.clone();
        let (done, finished) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            wait_for_marker_or_deadline(
                &directory,
                Instant::now() + Duration::from_secs(10),
                &mut |directory| {
                    if reader_available.load(Ordering::Acquire) {
                        pending_agent_status_reconciliations(directory)
                    } else {
                        Err(io::Error::from_raw_os_error(libc::EMFILE))
                    }
                },
            );
            done.send(()).unwrap();
        });
        let early = finished.recv_timeout(Duration::from_millis(250));
        fs::write(queue.0.join("pending-test"), b"{}").unwrap();
        available.store(true, Ordering::Release);
        let recovered = finished.recv_timeout(Duration::from_secs(1));
        waiter.join().unwrap();
        assert_eq!(
            early,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "a transient queue read error must preserve the grace wait"
        );
        recovered.expect("a recovered queue's pending marker must wake the worker");
    }

    #[test]
    fn a_removed_queue_stops_a_grace_deadline_wait() {
        let directory =
            env::temp_dir().join(format!("namedrop-removed-queue-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::remove_dir(&directory).unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        thread::spawn(move || {
            wait_for_marker_or_deadline(
                &directory,
                Instant::now() + Duration::from_secs(3_600),
                &mut pending_agent_status_reconciliations,
            );
            done.send(()).unwrap();
        });
        finished
            .recv_timeout(Duration::from_secs(1))
            .expect("a removed queue must stop the worker before its grace deadline");
    }
}

fn reconciliation_report_retry_delay(failures: usize, report: &namedrop::DrainReport) -> Duration {
    let rejection_only = report.retry_without_event
        && report.pane_lock_deferred.is_empty()
        && !report.awaits_terminal();
    reconciliation_retry_delay(failures, rejection_only)
}

fn reconciliation_retry_delay(failures: usize, retry_without_event: bool) -> Duration {
    let delays: &[u64] = if retry_without_event {
        &[100, 250, 500, 1_000, 5_000, 10_000, 30_000, 60_000]
    } else {
        &[100, 250, 500, 1_000, 5_000]
    };
    Duration::from_millis(delays[failures.min(delays.len() - 1)])
}

fn wait_for_reconciliation_retry(failures: &mut usize, retry_without_event: bool) {
    let delay = reconciliation_retry_delay(*failures, retry_without_event);
    *failures = failures.saturating_add(1);
    thread::sleep(delay);
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

fn write_result(result: &CommandResult, exit_code: u8, format: ResultFormat) -> ExitCode {
    if format == ResultFormat::Human {
        let text = render_command_result_to_text(result);
        let written = if exit_code == 0 {
            writeln!(io::stdout().lock(), "{text}")
        } else {
            writeln!(io::stderr().lock(), "{text}")
        };
        return ExitCode::from(if written.is_ok() { exit_code } else { 74 });
    }
    let mut output = io::stdout().lock();
    let written = if format == ResultFormat::Full {
        serde_json::to_writer(&mut output, result)
    } else {
        serde_json::to_value(result)
            .and_then(|full| serde_json::to_writer(&mut output, &agent_result_view(&full)))
    };
    if written.is_err() || writeln!(output).is_err() {
        return ExitCode::from(74);
    }
    ExitCode::from(exit_code)
}
