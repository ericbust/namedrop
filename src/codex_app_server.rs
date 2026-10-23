//! Whether Codex's shared app-server is in play for a Codex home, which Herdr
//! panes it serves, and the one message Namedrop gives about it. Everything
//! here only reads: config.toml, the server's pid file and the process table.

use crate::integration::pane_agent_runtime;
use crate::{
    AgentConfigDirectories, AgentRuntime, DiscoveredPane, HerdrCommandExecutor, LifecycleRuntime,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use toml_edit::{DocumentMut, Item};

pub(crate) const SUMMARY: &str = "Codex runs its sessions through its shared app-server, so Herdr cannot tell which pane each Codex session is in.";

pub(crate) const EXPLANATION: &str = "Since version 0.162, Codex starts one background app-server for each Codex home (CODEX_HOME) and runs every session's hooks inside it. That server keeps the environment of wherever it was first started, not of the pane you type in. Herdr then files a Codex session under the wrong pane or under none: mentions to that agent stay queued, a second Codex pane's session can be filed under the first, and Namedrop's own Codex hooks go silent. Submitting another prompt or reinstalling the Herdr integration does not help. The server also updates and restarts itself, which can close an open Codex window the next time it saves a setting.";

/// Follows EXPLANATION when the setting is already off but a server still runs.
pub(crate) const LINGERING: &str = "daemon_auto_start is already off; the server started earlier keeps serving new Codex sessions until it is stopped.";

pub(crate) const FIX: &str = "The fix turns the shared app-server off, so each Codex runs inside its own pane, where Herdr can see it. While it is off, the `codex agents` session browser and Codex remote control are unavailable. Namedrop never changes Codex settings: run the steps yourself, or let the agent in the pane run them once you approve.";

pub(crate) const DISABLE: &str = "codex features disable daemon_auto_start";
pub(crate) const STOP: &str = "codex app-server daemon stop";
pub(crate) const PER_SESSION: &str = "codex --no-daemon";
pub(crate) const ENABLE: &str = "codex features enable daemon_auto_start";
pub(crate) const COMMANDS: [&str; 4] = [DISABLE, STOP, PER_SESSION, ENABLE];

/// Each line is exactly one of the commands or prose that does not start with
/// "codex ", so every renderer can print the commands raw and copyable.
pub(crate) const STEPS: [&str; 7] = [
    DISABLE,
    STOP,
    "Then restart Codex in each of its Herdr panes. The stop is needed because a running server keeps taking new sessions even with the setting off. It disconnects every open Codex window that uses it, including the one whose agent ran these steps for you; relaunch those windows.",
    "To fix one session only, leave the setting on and start that Codex with:",
    PER_SESSION,
    "To undo the fix and turn the shared app-server back on:",
    ENABLE,
];

/// The advice every structured consumer carries.
pub(crate) fn advice(lingering: bool) -> Value {
    json!({
        "status": "codex_shared_app_server",
        "summary": SUMMARY,
        "explanation": explanation(lingering),
        "fix": FIX,
        "steps": STEPS,
    })
}

fn explanation(lingering: bool) -> String {
    if lingering {
        format!("{EXPLANATION} {LINGERING}")
    } else {
        EXPLANATION.to_owned()
    }
}

/// The advice as display lines: SUMMARY, EXPLANATION, LINGERING when it
/// applies, FIX, then STEPS.
pub(crate) fn advice_lines(lingering: bool) -> impl Iterator<Item = &'static str> {
    [SUMMARY, EXPLANATION]
        .into_iter()
        .chain(lingering.then_some(LINGERING))
        .chain([FIX])
        .chain(STEPS)
}

/// One line for Codex's warning row, built from the command consts.
pub(crate) fn banner() -> String {
    format!(
        "Namedrop: Codex runs through its shared app-server, so Herdr cannot tell which pane this session is in; mentions to it stay queued or reach the wrong pane. Fix: {DISABLE}, then {STOP}, then restart Codex in its panes. One session only: {PER_SESSION}. Undo: {ENABLE}. Namedrop changes nothing itself; see namedrop doctor."
    )
}

/// Model-facing text that lets the agent in the pane relay the notice and
/// offer the fix with the person's approval.
pub(crate) fn agent_guidance() -> String {
    format!(
        "Namedrop shared app-server notice. Relay the notice below to the person once, in plain words, in your next reply. Run none of its commands without the person's approval. If they approve, run `{DISABLE}`, then tell them that `{STOP}` disconnects this session too before you run it, and remind them to restart Codex in each of its Herdr panes. To fix one session only, they can start it with `{PER_SESSION}`."
    )
}

/// What a plain `codex` started now under one Codex home does with its session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CodexAppServer {
    /// Sessions run in their own `codex` process. Also the answer for a Codex
    /// older than 0.162 and for an unusable Codex home.
    InProcess,
    /// No server runs now, but the next plain `codex` starts one: the setting
    /// is not off and this Codex home has used a server before.
    StartsOnNextSession,
    /// A server runs now. New plain `codex` sessions join it even when the
    /// setting is off.
    Running(RunningServer),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunningServer {
    /// A live pid that passes `is_server_process`, so a stale pid file never
    /// produces this value.
    pub(crate) pid: u32,
    pub(crate) pid_file: PathBuf,
    /// The persisted `features.daemon_auto_start`; an absent key means true.
    pub(crate) auto_start: bool,
}

impl CodexAppServer {
    /// Whether a plain `codex` started now would run through a shared server.
    pub(crate) fn affects_new_sessions(&self) -> bool {
        !matches!(self, Self::InProcess)
    }

    /// The advice for new sessions, with the lingering-server sentence when
    /// the setting is already off.
    pub(crate) fn advice(&self) -> Option<Value> {
        match self {
            Self::InProcess => None,
            Self::StartsOnNextSession => Some(advice(false)),
            Self::Running(server) => Some(advice(!server.auto_start)),
        }
    }
}

pub(crate) fn observe(home: Option<&Path>, config: AgentConfigDirectories<'_>) -> CodexAppServer {
    let Some(home) = home else {
        return CodexAppServer::InProcess;
    };
    let codex_home = config.codex(home);
    if !codex_home.is_absolute() {
        return CodexAppServer::InProcess;
    }
    let auto_start = fs::read_to_string(codex_home.join("config.toml"))
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|document| {
            document
                .get("features")
                .and_then(|features| features.get("daemon_auto_start"))
                .and_then(Item::as_bool)
        })
        .unwrap_or(true);
    let pid_file = codex_home.join("app-server-daemon/daemon.pid");
    let pid = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|record| record.get("pid")?.as_u64())
        .and_then(|pid| u32::try_from(pid).ok());
    if let Some(pid) = pid
        && is_server_process(pid)
    {
        return CodexAppServer::Running(RunningServer {
            pid,
            pid_file,
            auto_start,
        });
    }
    let used_a_server = ["app-server-daemon", "packages/app-server-daemon"]
        .iter()
        .any(|directory| fs::metadata(codex_home.join(directory)).is_ok_and(|m| m.is_dir()));
    if auto_start && used_a_server {
        CodexAppServer::StartsOnNextSession
    } else {
        CodexAppServer::InProcess
    }
}

/// The Codex panes, from one list, whose sessions go through a running server.
/// Never empty: `served_codex_panes` returns `None` instead.
#[derive(Clone, Debug)]
pub(crate) struct ServedPanes {
    pub(crate) server: RunningServer,
    pane_ids: Vec<String>,
}

impl ServedPanes {
    pub(crate) fn serves(&self, pane_id: &str) -> bool {
        self.pane_ids.iter().any(|id| id == pane_id)
    }

    pub(crate) fn pane_ids(&self) -> &[String] {
        &self.pane_ids
    }
}

/// A Codex pane counts as served unless its foreground processes prove it runs
/// in-process. When Herdr does not answer, it counts as served, because a
/// server is running. With no Codex pane, nothing is read and Herdr is not asked.
pub(crate) fn served_codex_panes<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    panes: &[DiscoveredPane],
) -> Option<ServedPanes> {
    let codex = panes
        .iter()
        .filter(|pane| pane_agent_runtime(pane) == Some(AgentRuntime::Codex))
        .collect::<Vec<_>>();
    if codex.is_empty() {
        return None;
    }
    let CodexAppServer::Running(server) = observe(runtime.home_directory, runtime.agent_config)
    else {
        return None;
    };
    let pane_ids = codex
        .into_iter()
        .filter(|pane| !pane_runs_in_process(runtime.herdr, &pane.pane_id))
        .map(|pane| pane.pane_id.clone())
        .collect::<Vec<_>>();
    (!pane_ids.is_empty()).then_some(ServedPanes { server, pane_ids })
}

/// True when an ancestor of this process is a Codex shared app-server, which
/// proves this process runs inside it.
pub(crate) fn runs_inside_server() -> bool {
    crate::terminal::ancestor_pids(std::process::id()).any(is_server_process)
}

const IN_PROCESS_FLAGS: [&str; 2] = ["--no-daemon", "--dangerously-bypass-hook-trust"];

fn pane_runs_in_process(herdr: &impl HerdrCommandExecutor, pane_id: &str) -> bool {
    crate::terminal::foreground_process_ids(herdr, pane_id)
        .into_iter()
        .filter_map(crate::terminal::process_arguments)
        .any(|argv| argv.iter().any(|a| IN_PROCESS_FLAGS.contains(&a.as_str())))
}

/// The server runs from `<CODEX_HOME>/packages/app-server-daemon/releases/<v>/`,
/// which also rejects a reused pid that a stale pid file still names.
fn is_server_process(pid: u32) -> bool {
    crate::terminal::process_arguments(pid)
        .and_then(|argv| argv.into_iter().next())
        .is_some_and(|program| is_server_binary(Path::new(&program)))
        || crate::terminal::process_executable(pid).is_some_and(|path| is_server_binary(&path))
}

fn is_server_binary(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    components
        .windows(3)
        .any(|window| window == ["packages", "app-server-daemon", "releases"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HerdrCommandOutput;
    use std::{
        cell::RefCell,
        collections::BTreeMap,
        os::unix::process::CommandExt,
        process::{Child, Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "namedrop-app-server-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn codex_home(&self) -> PathBuf {
            self.0.join(".codex")
        }
        fn write(&self, relative: &str, contents: &str) {
            let path = self.codex_home().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        fn observe(&self) -> CodexAppServer {
            observe(Some(&self.0), AgentConfigDirectories::default())
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A spawned process group that the test kills on drop. Spawning waits
    /// until the kernel shows the new argv: a vfork-based spawn returns while
    /// exec is still replacing the child's image, which shows no argv or ours.
    struct Process(Child);
    impl Process {
        fn spawn(command: &mut Command) -> Self {
            let child = command
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let ours = crate::terminal::process_arguments(std::process::id());
            while crate::terminal::process_arguments(child.id())
                .is_none_or(|argv| Some(argv) == ours)
            {
                assert!(Instant::now() < deadline, "argv never appeared");
                std::thread::sleep(Duration::from_millis(2));
            }
            Self(child)
        }
        fn server(codex_home: &Path) -> Self {
            let program =
                codex_home.join("packages/app-server-daemon/releases/0.162.1-x/bin/codex");
            Self::spawn(Command::new("sleep").arg0(&program).arg("30"))
        }
        fn pid(&self) -> u32 {
            self.0.id()
        }
    }
    impl Drop for Process {
        fn drop(&mut self) {
            let group = libc::pid_t::try_from(self.0.id()).unwrap();
            // SAFETY: kill only sends a signal, to the group this test created.
            unsafe { libc::kill(-group, libc::SIGKILL) };
            let _ = self.0.wait();
        }
    }

    fn pid_record(pid: u32) -> String {
        json!({"pid": pid, "processStartTime": "Fri Oct 23 07:53:57 2026"}).to_string()
    }

    fn running(root: &Root, pid: u32, auto_start: bool) -> CodexAppServer {
        CodexAppServer::Running(RunningServer {
            pid,
            pid_file: root.codex_home().join("app-server-daemon/daemon.pid"),
            auto_start,
        })
    }

    fn snapshot(directory: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut entries = BTreeMap::new();
        let mut pending = vec![directory.to_path_buf()];
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(&path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    entries.insert(path.clone(), None);
                    pending.push(path);
                } else {
                    entries.insert(path.clone(), Some(fs::read(&path).unwrap()));
                }
            }
        }
        entries
    }

    #[test]
    fn no_home_or_codex_home_means_in_process() {
        assert_eq!(
            observe(None, AgentConfigDirectories::default()),
            CodexAppServer::InProcess
        );
        let root = Root::new();
        assert_eq!(root.observe(), CodexAppServer::InProcess);
    }

    #[test]
    fn a_codex_that_never_used_a_server_runs_in_process() {
        let root = Root::new();
        root.write("config.toml", "model = \"gpt-5\"\n");
        assert_eq!(root.observe(), CodexAppServer::InProcess);
    }

    #[test]
    fn a_codex_home_that_used_a_server_starts_one_on_the_next_session() {
        for directory in ["app-server-daemon", "packages/app-server-daemon"] {
            let root = Root::new();
            fs::create_dir_all(root.codex_home().join(directory)).unwrap();
            assert_eq!(root.observe(), CodexAppServer::StartsOnNextSession);
            root.write("config.toml", "[features]\ndaemon_auto_start = false\n");
            assert_eq!(root.observe(), CodexAppServer::InProcess, "{directory}");
        }
    }

    #[test]
    fn a_live_server_named_by_the_pid_file_is_running() {
        let root = Root::new();
        let server = Process::server(&root.codex_home());
        root.write("app-server-daemon/daemon.pid", &pid_record(server.pid()));
        assert_eq!(root.observe(), running(&root, server.pid(), true));
        assert!(root.observe().affects_new_sessions());
    }

    #[test]
    fn a_server_still_running_after_the_setting_was_turned_off_is_running() {
        let root = Root::new();
        let server = Process::server(&root.codex_home());
        root.write("config.toml", "[features]\ndaemon_auto_start = false\n");
        root.write("app-server-daemon/daemon.pid", &pid_record(server.pid()));
        let observed = root.observe();
        assert_eq!(observed, running(&root, server.pid(), false));
        let advice = observed.advice().unwrap();
        assert_eq!(
            advice["explanation"],
            format!("{EXPLANATION} {LINGERING}"),
            "the lingering server is named"
        );
    }

    #[test]
    fn a_pid_file_naming_a_process_that_is_not_a_server_is_stale() {
        let root = Root::new();
        let other = Process::spawn(Command::new("sleep").arg("30"));
        root.write("app-server-daemon/daemon.pid", &pid_record(other.pid()));
        assert_eq!(root.observe(), CodexAppServer::StartsOnNextSession);
    }

    #[test]
    fn a_pid_file_naming_a_dead_server_is_stale() {
        let root = Root::new();
        let pid = Process::server(&root.codex_home()).pid();
        root.write("config.toml", "[features]\ndaemon_auto_start = false\n");
        root.write("app-server-daemon/daemon.pid", &pid_record(pid));
        assert_eq!(root.observe(), CodexAppServer::InProcess);
    }

    #[test]
    fn malformed_files_never_panic_and_leave_the_setting_on() {
        let root = Root::new();
        root.write("config.toml", "[features\ndaemon_auto_start = ");
        root.write("app-server-daemon/daemon.pid", "{\"pid\": \"many\"");
        assert_eq!(root.observe(), CodexAppServer::StartsOnNextSession);
        root.write("config.toml", "[features]\ndaemon_auto_start = \"no\"\n");
        root.write("app-server-daemon/daemon.pid", "{\"pid\": 99999999999}");
        assert_eq!(root.observe(), CodexAppServer::StartsOnNextSession);
    }

    #[test]
    fn a_relative_codex_home_is_never_read() {
        let root = Root::new();
        let relative = Path::new("relative-codex-home");
        let config = AgentConfigDirectories {
            claude: None,
            codex: Some(relative),
        };
        assert_eq!(observe(Some(&root.0), config), CodexAppServer::InProcess);
        let empty = AgentConfigDirectories {
            claude: None,
            codex: Some(Path::new("")),
        };
        assert_eq!(observe(Some(&root.0), empty), CodexAppServer::InProcess);
    }

    #[test]
    fn observing_changes_nothing_under_the_codex_home() {
        let root = Root::new();
        let server = Process::server(&root.codex_home());
        root.write("config.toml", "[features]\ndaemon_auto_start = true\n");
        root.write("app-server-daemon/daemon.pid", &pid_record(server.pid()));
        fs::create_dir_all(root.codex_home().join("app-server-control")).unwrap();
        let before = snapshot(&root.0);
        assert!(matches!(root.observe(), CodexAppServer::Running(_)));
        assert_eq!(snapshot(&root.0), before);
    }

    #[test]
    fn process_arguments_round_trip_spaces_and_non_ascii() {
        let arguments = ["two words", "naïve café", "", "--no-daemon"];
        let child = Process::spawn(Command::new("sleep").arg0("my sleep").arg("30"));
        let probe = Process::spawn(
            Command::new("/bin/sh")
                .arg0("probe")
                .args(["-c", "sleep 30; :", "sh"])
                .args(arguments),
        );
        let sleep = crate::terminal::process_arguments(child.pid());
        let shell = crate::terminal::process_arguments(probe.pid());
        assert_eq!(sleep.unwrap(), ["my sleep", "30"]);
        let mut expected = vec!["probe", "-c", "sleep 30; :", "sh"];
        expected.extend(arguments);
        assert_eq!(shell.unwrap(), expected);
    }

    #[test]
    fn this_test_process_does_not_run_inside_a_server() {
        assert!(!runs_inside_server());
        assert!(
            crate::terminal::ancestor_pids(std::process::id())
                .any(|pid| pid == std::os::unix::process::parent_id())
        );
    }

    #[test]
    fn the_identity_rule_needs_the_release_directory_components() {
        assert!(is_server_binary(Path::new(
            "/h/.codex/packages/app-server-daemon/releases/0.162.1-x86_64/bin/codex"
        )));
        assert!(!is_server_binary(Path::new("/usr/bin/codex")));
        assert!(!is_server_binary(Path::new(
            "/h/.codex/packages/app-server-daemon/current/bin/codex"
        )));
        assert!(!is_server_binary(Path::new(
            "/h/packages-app-server-daemon-releases"
        )));
    }

    struct Herdr {
        foreground: BTreeMap<String, Vec<u32>>,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl HerdrCommandExecutor for Herdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|a| (*a).to_owned()).collect());
            match args {
                ["pane", "process-info", "--pane", pane] => {
                    let pids = self
                        .foreground
                        .get(*pane)
                        .ok_or_else(|| "herdr is not answering".to_owned())?;
                    let processes = pids
                        .iter()
                        .map(|pid| json!({"pid": pid}))
                        .collect::<Vec<_>>();
                    Ok(HerdrCommandOutput {
                        stdout: json!({"result": {"process_info": {
                            "shell_pid": 1, "foreground_process_group_id": 2,
                            "foreground_processes": processes
                        }}})
                        .to_string(),
                    })
                }
                _ => Err(format!("unexpected herdr call {args:?}")),
            }
        }
    }

    fn pane(pane_id: &str, agent: &str) -> DiscoveredPane {
        DiscoveredPane {
            pane_id: pane_id.to_owned(),
            terminal_id: format!("t-{pane_id}"),
            tab_id: "w1:t1".to_owned(),
            workspace_id: "w1".to_owned(),
            cwd: None,
            pane_label: None,
            agent_session_id: None,
            agent: Some(agent.to_owned()),
            occupant_label: None,
            agent_status: None,
            idle_state_label: None,
        }
    }

    fn served(root: &Root, herdr: &Herdr, panes: &[DiscoveredPane]) -> Option<ServedPanes> {
        let runtime = LifecycleRuntime {
            state_directory: &root.0,
            home_directory: Some(&root.0),
            agent_config: AgentConfigDirectories::default(),
            cwd: &root.0,
            workspace_id: Some("w1"),
            current_pane_id: None,
            session_socket_id: "default",
            herdr,
            clock: &(),
            ids: &(),
        };
        served_codex_panes(&runtime, panes)
    }

    #[test]
    fn served_panes_are_the_codex_panes_not_proven_in_process() {
        let root = Root::new();
        let server = Process::server(&root.codex_home());
        root.write("app-server-daemon/daemon.pid", &pid_record(server.pid()));
        let in_process = Process::spawn(Command::new("/bin/sh").args([
            "-c",
            "sleep 30; :",
            "sh",
            "--no-daemon",
        ]));
        let plain =
            Process::spawn(Command::new("/bin/sh").args(["-c", "sleep 30; :", "sh", "resume"]));
        let herdr = Herdr {
            foreground: BTreeMap::from([
                ("w1:p1".to_owned(), vec![in_process.pid()]),
                ("w1:p2".to_owned(), vec![plain.pid()]),
            ]),
            calls: RefCell::default(),
        };
        let only_in_process = served(&root, &herdr, &[pane("w1:p1", "codex")]);
        let mixed = served(
            &root,
            &herdr,
            &[
                pane("w1:p1", "codex"),
                pane("w1:p2", "codex"),
                pane("w1:p3", "codex"),
                pane("w1:p4", "claude"),
            ],
        );
        assert!(
            only_in_process.is_none(),
            "a --no-daemon pane is not served"
        );
        let mixed = mixed.expect("a plain Codex pane beside a running server is served");
        assert_eq!(
            mixed.pane_ids(),
            ["w1:p2", "w1:p3"],
            "w1:p3 has no process info"
        );
        assert!(mixed.serves("w1:p2") && !mixed.serves("w1:p1") && !mixed.serves("w1:p4"));
        assert_eq!(mixed.server.pid, server.pid());
    }

    #[test]
    fn panes_without_codex_read_nothing_and_ask_herdr_nothing() {
        let root = Root::new();
        let server = Process::server(&root.codex_home());
        root.write("app-server-daemon/daemon.pid", &pid_record(server.pid()));
        let herdr = Herdr {
            foreground: BTreeMap::new(),
            calls: RefCell::default(),
        };
        assert!(served(&root, &herdr, &[pane("w1:p1", "claude")]).is_none());
        assert!(herdr.calls.borrow().is_empty());
        assert!(served(&root, &herdr, &[pane("w1:p1", "codex")]).is_some());
        assert_eq!(herdr.calls.borrow().len(), 1, "the fake can see a call");
    }

    #[test]
    fn without_a_running_server_no_pane_is_served() {
        let root = Root::new();
        fs::create_dir_all(root.codex_home().join("app-server-daemon")).unwrap();
        let herdr = Herdr {
            foreground: BTreeMap::new(),
            calls: RefCell::default(),
        };
        assert!(served(&root, &herdr, &[pane("w1:p1", "codex")]).is_none());
    }

    fn assert_commands_are_exact(text: &str) {
        assert!(
            !text.starts_with("codex ") || COMMANDS.contains(&text),
            "{text:?} starts like a command but is not one"
        );
    }

    #[test]
    fn every_command_in_the_advice_is_one_of_the_four() {
        for line in advice_lines(true) {
            assert_commands_are_exact(line);
        }
        for prose in [SUMMARY, EXPLANATION, LINGERING, FIX] {
            assert!(!prose.starts_with("codex "), "{prose}");
        }
        for command in COMMANDS {
            assert!(STEPS.contains(&command), "{command} is a step of its own");
        }
        for text in [agent_guidance(), banner()] {
            for quoted in text.split('`').skip(1).step_by(2) {
                assert_commands_are_exact(quoted);
            }
            for command in [DISABLE, STOP, PER_SESSION] {
                assert!(text.contains(command), "{command} in {text}");
            }
        }
        assert!(banner().contains(ENABLE) && !banner().contains('\n'));
    }

    #[test]
    fn the_advice_carries_exactly_the_shared_text() {
        let value = advice(false);
        let keys = value.as_object().unwrap().keys().collect::<Vec<_>>();
        assert_eq!(keys, ["explanation", "fix", "status", "steps", "summary"]);
        assert_eq!(value["status"], "codex_shared_app_server");
        assert_eq!(value["summary"], SUMMARY);
        assert_eq!(value["explanation"], EXPLANATION);
        assert_eq!(value["fix"], FIX);
        assert_eq!(value["steps"], json!(STEPS));
        let lines = advice_lines(false).collect::<Vec<_>>();
        assert_eq!(lines[..3], [SUMMARY, EXPLANATION, FIX]);
        assert_eq!(lines[3..], STEPS);
        let lingering = advice_lines(true).collect::<Vec<_>>();
        assert_eq!(lingering[..4], [SUMMARY, EXPLANATION, LINGERING, FIX]);
    }

    #[test]
    fn advice_is_the_only_copy() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut pending = vec![root.join("src"), root.join("scripts")];
        let mut copies = Vec::new();
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(&path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(text) = fs::read_to_string(&path)
                    && (text.contains("daemon_auto_start") || text.contains("app-server daemon"))
                {
                    copies.push(path.strip_prefix(root).unwrap().to_path_buf());
                }
            }
        }
        assert_eq!(copies, [PathBuf::from("src/codex_app_server.rs")]);
    }
}
