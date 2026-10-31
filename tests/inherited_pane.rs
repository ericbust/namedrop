#[path = "support/cleanup.rs"]
mod cleanup;

#[path = "support/codex_app_server.rs"]
mod codex_app_server;
use namedrop::{HerdrCommandExecutor, HerdrCommandOutput, verify_inherited_pane};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

/// Herdr as seen through the executor port: the pane's shell is whatever pid
/// the test says it is.
struct Shell(Option<u64>);
impl HerdrCommandExecutor for Shell {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        assert_eq!(args, ["pane", "process-info", "--pane", "wH:p1"]);
        let shell_pid = self.0.ok_or_else(|| "pane not found".to_owned())?;
        Ok(HerdrCommandOutput {
            stdout: format!(
                r#"{{"result":{{"process_info":{{"pane_id":"wH:p1","shell_pid":{shell_pid},"foreground_process_group_id":{shell_pid},"foreground_processes":[]}}}}}}"#
            ),
        })
    }
}

/// A live process that is certainly not an ancestor of this one.
fn unrelated_process() -> std::process::Child {
    Command::new("sleep").arg("30").spawn().unwrap()
}

#[test]
fn an_inherited_pane_whose_shell_is_an_ancestor_is_accepted() {
    let parent = u64::from(std::os::unix::process::parent_id());
    assert_eq!(
        verify_inherited_pane(&Shell(Some(parent)), "wH:p1", &[]),
        Ok(())
    );
}

#[test]
fn an_inherited_pane_whose_shell_is_not_an_ancestor_is_refused_with_the_remedy() {
    let mut other = unrelated_process();
    let error =
        verify_inherited_pane(&Shell(Some(u64::from(other.id()))), "wH:p1", &[]).unwrap_err();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(error.contains("HERDR_PANE_ID=wH:p1"), "{error}");
    assert!(error.contains("not running inside pane wH:p1"), "{error}");
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn an_inherited_pane_herdr_cannot_describe_is_refused_with_herdr_s_own_reason() {
    let error = verify_inherited_pane(&Shell(None), "wH:p1", &[]).unwrap_err();
    assert!(error.contains("HERDR_PANE_ID=wH:p1"), "{error}");
    assert!(error.contains("pane not found"), "{error}");
    assert!(error.contains("--herdr-pane"), "{error}");
}

/// The binary with a fake `herdr` on `HERDR_BIN_PATH` and a temporary home.
/// `pane process-info` reports the shell pid written to `shell-pid`.
struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-inherited-pane-{}-{label}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        let host = root.join("herdr");
        fs::write(
            &host,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{root}/herdr-calls'
case "$1 $2" in
  'worktree list') printf '%s\n' '{{"result":{{"worktrees":[{{"path":"/repo","open_workspace_id":"w1"}}]}}}}';;
  'session list') printf '%s\n' '{{"sessions":[]}}';;
  'pane list') printf '%s\n' '{{"result":{{"panes":[{{"pane_id":"p1","terminal_id":"t1","tab_id":"tab1","workspace_id":"w1","agent_status":"unknown","agent_session":{{"value":"session-p1"}}}},{{"pane_id":"p2","terminal_id":"t2","tab_id":"tab1","workspace_id":"w1","agent_status":"unknown","agent_session":{{"value":"session-p2"}}}}]}}}}';;
  'pane process-info') printf '{{"result":{{"process_info":{{"pane_id":"p1","shell_pid":%s,"foreground_process_group_id":42,"foreground_processes":[]}}}}}}\n' "$(cat '{root}/shell-pid')";;
  'server reload-config'|'pane report-metadata'|'tab rename') printf '%s\n' '{{}}';;
  'tab get') printf '%s\n' '{{"result":{{"label":"Main"}}}}';;
  *) printf 'unexpected Herdr call: %s\n' "$*" >&2; exit 1;;
esac
"#,
                root = root.display()
            ),
        )
        .unwrap();
        fs::set_permissions(host, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    /// The test process is the parent of every binary it runs, so its pid is
    /// an ancestor of all of them.
    fn shell_is_an_ancestor(&self) {
        fs::write(self.0.join("shell-pid"), std::process::id().to_string()).unwrap();
    }
    fn shell_is(&self, pid: u32) {
        fs::write(self.0.join("shell-pid"), pid.to_string()).unwrap();
    }
    fn namedrop(&self, args: &[&str]) -> std::process::Output {
        self.namedrop_command(args).output().unwrap()
    }
    fn namedrop_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        self.host(&mut command)
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env("PATH", "/usr/bin:/bin")
            .arg("--plugin-state")
            .arg(self.0.join("state"))
            .args(args);
        command
    }
    /// Herdr's `activate` action: `scripts/run.sh activate` from a checkout
    /// with the built binary staged, and `herdr` on PATH for the scripts.
    fn activate_action(&self) -> std::process::Output {
        let checkout = self.0.join("checkout");
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        fs::create_dir_all(checkout.join("scripts")).unwrap();
        fs::create_dir_all(checkout.join("bin")).unwrap();
        for script in ["run.sh", "runtime.py", "update.py"] {
            fs::copy(
                repo.join("scripts").join(script),
                checkout.join("scripts").join(script),
            )
            .unwrap();
        }
        fs::copy(repo.join("runtime.json"), checkout.join("runtime.json")).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_namedrop"),
            checkout.join("bin/namedrop"),
        )
        .unwrap();
        fs::create_dir_all(self.0.join("path")).unwrap();
        std::os::unix::fs::symlink(self.0.join("herdr"), self.0.join("path/herdr")).unwrap();
        fs::create_dir_all(self.0.join("state")).unwrap();
        self.host(Command::new("sh").arg(checkout.join("scripts/run.sh")))
            .env("HERDR_PLUGIN_STATE_DIR", self.0.join("state"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("path").display()),
            )
            .args(["activate", "--input-json", "{\"confirmed\":true}"])
            .output()
            .unwrap()
    }
    fn host<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command
            .env("HOME", self.0.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("home").join(".claude"))
            .env("CODEX_HOME", self.0.join("home").join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("CODEX_HOME", self.0.join("home/.codex"))
            .env_remove("ZDOTDIR")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("HERDR_CONFIG_PATH")
            .env("SHELL", "/bin/zsh")
            .env("HERDR_BIN_PATH", self.0.join("herdr"))
            .env("HERDR_SOCKET_PATH", "isolated-socket")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_PANE_ID", "p1")
    }
    fn activate(&self) {
        self.shell_is_an_ancestor();
        let output = self.namedrop(&["activate", "--input-json", "{\"confirmed\":true}"]);
        assert!(output.status.success(), "{output:?}");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.0).unwrap();
    }
}

fn text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn the_binary_refuses_an_inherited_pane_it_does_not_run_inside() {
    let f = Fixture::new("refused");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f.namedrop(&["address", "list"]);
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!output.status.success(), "{output:?}");
    let text = text(&output);
    assert!(text.contains("HERDR_PANE_ID=p1"), "{text}");
    assert!(text.contains("--herdr-pane"), "{text}");
    assert!(
        !text.contains("\"addresses\""),
        "no roster for a pane this process is not in: {text}"
    );
}

#[test]
fn the_binary_accepts_an_inherited_pane_it_runs_inside() {
    let f = Fixture::new("accepted");
    f.activate();
    let output = f.namedrop(&["address", "list"]);
    assert!(output.status.success(), "{output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["status"], "ok", "{listed}");
}

#[test]
fn an_explicit_pane_is_the_callers_assertion_and_is_not_checked() {
    let f = Fixture::new("explicit");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f.namedrop(&["--herdr-pane", "p1", "address", "list"]);
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(output.status.success(), "{output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["status"], "ok", "{listed}");
}

#[test]
fn herdr_s_activate_action_passes_its_pane_to_activation() {
    // Herdr runs the action as its own child, so the pane's shell is never an
    // ancestor of the runtime that activation starts.
    let f = Fixture::new("action");
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f.activate_action();
    other.kill().unwrap();
    other.wait().unwrap();
    let text = text(&output);
    assert!(output.status.success(), "{text}");
    let activated: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(activated["data"]["activated"], true, "{text}");
}

#[test]
fn an_inherited_pane_is_accepted_when_the_agent_session_runs_in_it() {
    // Claude Code's Bash can run outside the pane shell's process tree, yet
    // its session ID still names the pane Herdr says runs it.
    let f = Fixture::new("session");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CLAUDE_CODE_SESSION_ID", "session-p1")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["status"], "ok", "{listed}");
}

#[test]
fn an_agent_session_running_in_another_pane_does_not_vouch_for_the_inherited_pane() {
    let f = Fixture::new("session-elsewhere");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CLAUDE_CODE_SESSION_ID", "session-p2")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!output.status.success(), "{}", text(&output));
    let text = text(&output);
    assert!(text.contains("HERDR_PANE_ID=p1"), "{text}");
    assert!(text.contains("--herdr-pane"), "{text}");
}

#[test]
fn a_stale_codex_session_does_not_hide_the_claude_session_that_runs_in_the_pane() {
    let f = Fixture::new("two-sessions");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CODEX_SESSION_ID", "session-from-an-outer-codex")
        .env("CLAUDE_CODE_SESSION_ID", "session-p1")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(output.status.success(), "{}", text(&output));
}

/// Herdr's own `pane list` output, captured from a live pane: many more
/// fields than the fixture's, with the session under `agent_session`.
struct LivePaneList;
impl HerdrCommandExecutor for LivePaneList {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["pane", "process-info", ..] => {
                r#"{"id":"cli:pane:process_info","result":{"process_info":{"foreground_process_group_id":1147346,"foreground_processes":[{"argv":["claude"],"cmdline":"claude","cwd":"/repo","name":"claude","pid":1147346}],"pane_id":"wG:p21","shell_pid":4000000000},"type":"pane_process_info"}}"#
            }
            ["pane", "list"] => {
                r#"{"id":"cli:pane:list","result":{"panes":[{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"sess-21"},"agent_status":"working","cwd":"/repo","focused":false,"foreground_cwd":"/repo","pane_id":"wG:p21","revision":20,"scroll":{"max_offset_from_bottom":0,"offset_from_bottom":0,"viewport_rows":40},"state_labels":{"working":"@a working"},"tab_id":"wG:tP","terminal_id":"term_1","terminal_title":"t","title":"@a","tokens":{"x":"0"},"workspace_id":"wG"},{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"sess-1"},"agent_status":"idle","cwd":"/repo","pane_id":"wG:p1","tab_id":"wG:tP","terminal_id":"term_2","workspace_id":"wG"},{"agent_status":"unknown","pane_id":"w9:p1","tab_id":"w9:t1","terminal_id":"term_3","workspace_id":"w9"}],"type":"pane_list"}}"#
            }
            other => return Err(format!("unexpected call {other:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: stdout.to_owned(),
        })
    }
}

#[test]
fn the_live_pane_list_shape_vouches_for_the_session_s_own_pane_only() {
    assert_eq!(
        verify_inherited_pane(&LivePaneList, "wG:p21", &["sess-21"]),
        Ok(())
    );
    let error = verify_inherited_pane(&LivePaneList, "wG:p21", &["sess-1"]).unwrap_err();
    assert!(error.contains("--herdr-pane"), "{error}");
    let error = verify_inherited_pane(&LivePaneList, "wG:p21", &["nobody"]).unwrap_err();
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn a_session_vouches_for_the_pane_when_the_process_is_a_child_of_init() {
    // A double fork (a backgrounded subshell whose parent exits) reparents the
    // command to init, so no ancestor of it is the pane's shell. Unlike
    // `setsid`, it works on macOS.
    let f = Fixture::new("orphan");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let out = f.0.join("orphan.out");
    let mut command = f.namedrop_command(&["address", "list"]);
    command.env("CLAUDE_CODE_SESSION_ID", "session-p1");
    let mut detached = Command::new("sh");
    detached
        .arg("-c")
        .arg(r#"( "$0" "$@" > "$OUT" 2>&1 & )"#)
        .arg(command.get_program())
        .args(command.get_args())
        .env("OUT", &out);
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => detached.env(key, value),
            None => detached.env_remove(key),
        };
    }
    assert!(detached.status().unwrap().success());
    let mut waited = 0;
    while fs::read_to_string(&out).unwrap_or_default().is_empty() && waited < 100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    other.kill().unwrap();
    other.wait().unwrap();
    let printed = fs::read_to_string(&out).unwrap_or_default();
    assert!(printed.contains("\"status\":\"ok\""), "{printed}");
}

#[test]
fn the_mailbox_wake_hook_drains_the_space_after_its_hook_shell_has_exited() {
    // The user-prompt-submit wrapper backgrounds the wake and exits, so the
    // wake runs reparented away from the pane's shell. Codex gives its hooks
    // no session ID that could vouch for the inherited pane, and the wake
    // needs none: it reconciles the whole space, as nobody's pane.
    let f = Fixture::new("wake");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let calls = f.0.join("herdr-calls");
    fs::write(&calls, "").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
    f.host(&mut command)
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .env("PATH", "/usr/bin:/bin")
        .arg("hook")
        .arg("mailbox-wake")
        .arg("--state-directory")
        .arg(f.0.join("state"))
        .stdin(std::process::Stdio::null());
    let mut hook = command.spawn().unwrap();
    let drained = |calls: &Path| {
        fs::read_to_string(calls)
            .unwrap_or_default()
            .lines()
            .any(|call| call.starts_with("pane list"))
    };
    let mut waited = 0;
    while hook.try_wait().unwrap().is_none() && waited < 200 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    if waited == 200 {
        hook.kill().unwrap();
    }
    let status = hook.wait().unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(status.success(), "mailbox wake failed: {status}");
    for scope in fs::read_dir(f.0.join("state/agent-status-reconciliation")).unwrap() {
        for entry in fs::read_dir(scope.unwrap().path()).unwrap() {
            let entry = entry.unwrap();
            assert!(
                !entry.file_name().to_string_lossy().starts_with("pending-"),
                "mailbox wake left a pending reconciliation: {}",
                entry.path().display()
            );
        }
    }
    assert!(
        drained(&calls),
        "the wake never asked Herdr for the space's panes, so nothing was drained; Herdr calls: {:?}",
        fs::read_to_string(&calls).unwrap_or_default()
    );
}

#[test]
fn a_refusal_says_how_many_agent_sessions_it_checked() {
    let none = verify_inherited_pane(&LivePaneList, "wG:p21", &[]).unwrap_err();
    assert!(
        none.contains("There is no CODEX_SESSION_ID or CLAUDE_CODE_SESSION_ID in the environment"),
        "{none}"
    );
    let elsewhere = verify_inherited_pane(&LivePaneList, "wG:p21", &["sess-1"]).unwrap_err();
    assert!(
        elsewhere
            .contains("None of the 1 agent session ID(s) in the environment runs in pane wG:p21"),
        "{elsewhere}"
    );
}

#[test]
fn an_empty_session_variable_is_not_counted_as_a_session() {
    let f = Fixture::new("empty-session");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CLAUDE_CODE_SESSION_ID", "")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!output.status.success(), "{}", text(&output));
    let text = text(&output);
    assert!(text.contains("There is no CODEX_SESSION_ID"), "{text}");
    assert!(!text.contains("None of the 1"), "{text}");
}

#[test]
fn a_session_start_inside_codex_s_shared_app_server_gets_its_notice_without_a_runtime() {
    let f = Fixture::new("app-server");
    f.activate();
    let other = codex_app_server::Process::spawn(Command::new("sleep").arg("60"), "sleep");
    f.shell_is(other.pid());
    let codex_home = f.0.join("home/.codex");
    let hook = |event: &str, input: &str, inside: bool| {
        let program = Path::new(env!("CARGO_BIN_EXE_namedrop"));
        let mut command = if inside {
            codex_app_server::inside_server(&codex_home, program)
        } else {
            Command::new(program)
        };
        f.host(&mut command)
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .args(["hook", event, "--state-directory"])
            .arg(f.0.join("state"));
        let output = codex_app_server::run_with_stdin(&mut command, input);
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let start =
        r#"{"hook_event_name":"SessionStart","source":"startup","session_id":"s1","cwd":"/repo"}"#;

    let notice = hook("session-start", start, true);
    assert!(notice.ends_with('}'), "no trailing newline: {notice:?}");
    let value: serde_json::Value = serde_json::from_str(&notice).expect(&notice);
    assert_eq!(
        value.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["hookSpecificOutput", "systemMessage"]
    );
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let banner = value["systemMessage"].as_str().unwrap();
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(!banner.contains('\n'), "one warning line: {banner}");
    assert!(context.contains(codex_app_server::SUMMARY), "{context}");
    assert!(
        context.contains("without the person's approval"),
        "{context}"
    );
    for command in codex_app_server::COMMANDS {
        assert!(banner.contains(command), "{command}: {banner}");
        assert!(
            context.lines().any(|line| line == command),
            "{command} on its own line: {context}"
        );
    }
    for source in ["resume", "fork"] {
        let input = start.replace("startup", source);
        assert_eq!(hook("session-start", &input, true), notice, "{source}");
    }

    assert_eq!(
        hook("session-start", start, false),
        "",
        "outside the server"
    );
    for input in [
        start.replace("startup", "clear"),
        start.replace("startup", "compact"),
        "not json".to_owned(),
        start.replace("SessionStart", "UserPromptSubmit"),
    ] {
        assert_eq!(hook("session-start", &input, true), "", "{input}");
    }
    let prompt = r#"{"hook_event_name":"UserPromptSubmit","prompt":"hello","session_id":"s1"}"#;
    assert_eq!(hook("user-prompt-submit", prompt, true), "");
}
