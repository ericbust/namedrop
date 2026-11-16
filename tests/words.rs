//! The Words test of experimental/rex/UI.md: every surface a Rex home shows,
//! swept for Herdr's words, and every literal in `src/` and `scripts/` that
//! holds one, classified.
#![cfg(feature = "test-support")]
/// The status panel lives in the binary, so it is compiled in here, as
/// tests/agent_status_ui.rs does, and driven through its data plane.
#[allow(dead_code)]
#[path = "../src/agent_status_ui.rs"]
mod agent_status_ui;
#[path = "support/binary.rs"]
mod binary;
#[path = "support/cleanup.rs"]
mod cleanup;
#[allow(dead_code)]
#[path = "../src/popup_input.rs"]
mod popup_input;
#[path = "support/update_profile.rs"]
mod update_profile;

/// The binary's helper of this name, which the panel's entry point calls.
#[allow(dead_code)]
fn with_runtime_options<T>(
    _options: &namedrop::cli::RuntimeOptions,
    _inherited: &dyn namedrop::mux::home::Inherited,
    _action: impl FnOnce(
        &namedrop::LifecycleRuntime<
            '_,
            std::sync::Arc<dyn namedrop::mux::Mux>,
            namedrop::SystemClock,
            namedrop::ProcessIds,
        >,
    ) -> Result<T, String>,
) -> Result<T, String> {
    Err("the panel's entry point is exercised through the binary".to_owned())
}

use namedrop::{
    mux::{
        AgentState, MuxError, PopupScreen,
        fake::{FakeMux, Op},
        *,
    },
    *,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const ENDPOINT: &str = "rex:/tmp/words.sock";

/// What a Rex home must not say. Each check is case-insensitive.
const FORBIDDEN: [&str; 9] = [
    "herdr",
    "popup",
    "press prefix",
    "rex space",
    "rex workspace",
    "rex block",
    "plugin action",
    "plugin restart",
    "plugin-owned",
];

/// The words of an agent plugin, Codex's or Claude Code's, which a Rex home
/// installs as Herdr's does. Any other "plugin" is Herdr's.
const AGENT_PLUGIN: [&str; 5] = [
    "--plugin-state",
    "codex plugin",
    "/reload-plugins",
    "plugin setup",
    "plugins/",
];

#[derive(serde::Deserialize)]
struct Allowlist {
    printed: Vec<Printed>,
    unprinted: Vec<Unprinted>,
    unstaged: Vec<Unstaged>,
}

/// A Rex text the sweep cannot stage, by file, line and exact text.
#[derive(serde::Deserialize)]
struct Unstaged {
    file: String,
    line: usize,
    text: String,
    #[allow(dead_code)]
    reason: String,
}

/// A text a Rex home prints on purpose, by exact text with `<path>` for a
/// path, on the surfaces named.
#[derive(serde::Deserialize)]
struct Printed {
    kind: String,
    text: String,
    surfaces: Vec<String>,
    sources: Vec<Source>,
    #[allow(dead_code)]
    reason: String,
}

#[derive(serde::Deserialize)]
struct Source {
    file: String,
    line: usize,
    source: String,
}

/// Literals a Rex home never prints, by file and line.
#[derive(serde::Deserialize)]
struct Unprinted {
    kind: String,
    file: String,
    #[allow(dead_code)]
    reason: String,
    literals: Vec<(usize, String)>,
}

#[derive(serde::Deserialize)]
struct Literal {
    file: String,
    line: usize,
    text: String,
    role: String,
    #[serde(default)]
    fragments: Option<Vec<String>>,
}

/// The sweep's state: the allowlist, which printed entries some surface
/// matched, and every text checked, for the reach check at the end.
struct Words {
    allowlist: Allowlist,
    matched: Vec<bool>,
    corpus: Vec<String>,
    violations: Vec<String>,
    /// `NAMEDROP_WORDS_DUMP`: a file that receives every surface checked.
    dump: Option<fs::File>,
}

/// Every place `pattern` occurs in `text`, where `<path>` in the pattern
/// stands for one absolute path: a `/` and the shortest run of characters
/// other than line breaks and quotes that the rest of the pattern follows.
fn occurrences(text: &str, pattern: &str) -> Vec<(usize, usize)> {
    fn rest(text: &str, at: usize, parts: &[&str]) -> Option<usize> {
        let Some((part, later)) = parts.split_first() else {
            return Some(at);
        };
        if !text[at..].starts_with('/') {
            return None;
        }
        let path = text[at..]
            .find(['\n', '"', '\''])
            .map_or(text.len(), |end| at + end);
        if part.is_empty() && later.is_empty() {
            return Some(path);
        }
        (at + 1..=path)
            .filter(|end| text.is_char_boundary(*end) && text[*end..].starts_with(part))
            .find_map(|end| rest(text, end + part.len(), later))
    }
    let parts: Vec<&str> = pattern.split("<path>").collect();
    let lead = parts[0];
    let mut found = Vec::new();
    let mut from = 0;
    // A pattern that starts with `<path>` starts where a path does, at a `/`.
    while let Some(offset) = if lead.is_empty() {
        text[from..].find('/')
    } else {
        text[from..].find(lead)
    } {
        let start = from + offset;
        match rest(text, start + lead.len(), &parts[1..]) {
            Some(end) => {
                found.push((start, end));
                from = end.max(start + 1);
            }
            None => from = start + 1,
        }
        while from < text.len() && !text.is_char_boundary(from) {
            from += 1;
        }
    }
    found
}

fn violation(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    if let Some(word) = FORBIDDEN.iter().find(|word| lower.contains(*word)) {
        return Some(word);
    }
    let mut rest = lower.clone();
    for allowed in AGENT_PLUGIN {
        rest = rest.replace(allowed, "");
    }
    if rest.contains("plugin") {
        return Some("plugin");
    }
    let mut prose = text.replace("--space", "");
    prose = prose
        .replace("`workspaces`", "")
        .replace(r#"\"workspaces\""#, "")
        .replace(r#""workspaces""#, "")
        .replace("routing_scope:space", "");
    for identifier in ["{space}", "{workspace}", "{spaces}", "{workspaces}"] {
        prose = prose.replace(identifier, "<identifier>");
    }
    for pattern in ["'<path>'", "\"<path>\""] {
        for (start, end) in occurrences(&prose, pattern).into_iter().rev() {
            prose.replace_range(start..end, "<path>");
        }
    }
    if matches!(prose.trim(), "space" | "workspaces") {
        return None;
    }
    for line in prose.lines() {
        let line = if let Some(rest) = line.strip_prefix("Usage: space ") {
            rest
        } else if line.trim_start().starts_with("space       ") {
            &line.trim_start()["space".len()..]
        } else {
            line
        };
        if line
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .any(|word| {
                matches!(
                    word,
                    "space"
                        | "spaces"
                        | "workspace"
                        | "workspaces"
                        | "Space"
                        | "Spaces"
                        | "Workspace"
                        | "Workspaces"
                )
            })
        {
            return Some("bare session noun");
        }
    }
    None
}

impl Words {
    fn new() -> Self {
        let mut allowlist: Allowlist =
            serde_json::from_str(include_str!("words_allowlist.json")).unwrap();
        // A longer text that holds a shorter one is removed first.
        allowlist
            .printed
            .sort_by_key(|entry| std::cmp::Reverse(entry.text.len()));
        Self {
            matched: vec![false; allowlist.printed.len()],
            allowlist,
            corpus: Vec::new(),
            violations: Vec::new(),
            dump: None,
        }
    }

    /// Fails on a host word `text` holds outside the printed entries of its
    /// surface.
    fn check(&mut self, surface: &str, text: impl AsRef<str>) {
        let original = text.as_ref();
        let mut text = original.to_owned();
        for (index, entry) in self.allowlist.printed.iter().enumerate() {
            if !entry
                .surfaces
                .iter()
                .any(|scope| surface.starts_with(scope.as_str()))
            {
                continue;
            }
            let found = occurrences(&text, &entry.text);
            if !found.is_empty() {
                self.matched[index] = true;
                for (start, end) in found.into_iter().rev() {
                    text.replace_range(start..end, &format!("<{}>", entry.kind));
                }
            }
        }
        if let Some(word) = violation(&text) {
            self.violations
                .push(format!("Words violation on {surface} ({word:?}): {text}"));
        }
        if let Some(dump) = &mut self.dump {
            use std::io::Write;
            writeln!(dump, "{}", json!({"surface": surface, "text": original})).unwrap();
        }
        self.corpus.push(original.to_owned());
    }

    fn json(&mut self, surface: &str, value: &Value) {
        match value {
            Value::String(text) => self.check(surface, text),
            Value::Array(values) => values.iter().for_each(|value| self.json(surface, value)),
            // Keys are the JSON interface, which stays one for both hosts.
            Value::Object(fields) => fields.values().for_each(|value| self.json(surface, value)),
            _ => {}
        }
    }

    fn outcome(&mut self, surface: &str, result: &Result<Value, String>) {
        match result {
            Ok(value) => self.json(surface, value),
            Err(error) => self.check(surface, error),
        }
    }

    fn reached(&self, literal: &Literal) -> bool {
        match &literal.fragments {
            Some(fragments) if !fragments.is_empty() => self.corpus.iter().any(|text| {
                let mut at = 0;
                fragments.iter().all(|fragment| {
                    text[at..]
                        .find(fragment.as_str())
                        .map(|found| at += found + fragment.len())
                        .is_some()
                })
            }),
            _ => self
                .corpus
                .iter()
                .any(|text| text.contains(literal.text.as_str())),
        }
    }

    /// The end of the sweep: every printed entry matched, every literal with
    /// a host word classified, and every Rex text reached.
    fn finish(self) {
        let mut failures = self.violations.clone();
        for allowance in AGENT_PLUGIN {
            if !self
                .corpus
                .iter()
                .any(|text| text.to_lowercase().contains(allowance))
            {
                failures.push(format!("unused agent-plugin allowance {allowance:?}"));
            }
        }
        for (entry, matched) in self.allowlist.printed.iter().zip(&self.matched) {
            if !*matched {
                failures.push(format!(
                    "no surface printed the {} entry {:?}",
                    entry.kind, entry.text
                ));
            }
        }
        let output = binary::command("python3")
            .args([
                "-I",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/words_literals.py"),
                env!("CARGO_MANIFEST_DIR"),
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let literals: Vec<Literal> = serde_json::from_slice(&output.stdout).unwrap();
        let at = |file: &str, line: usize, text: &str| {
            literals
                .iter()
                .any(|literal| literal.file == file && literal.line == line && literal.text == text)
        };
        let relocate = "run `python3 tests/words_literals.py --relocate tests/words_allowlist.json` if it only moved";
        let scanned = |file: &str| file.starts_with("src/") || file.starts_with("scripts/");
        for entry in &self.allowlist.printed {
            for source in &entry.sources {
                let present = if scanned(&source.file)
                    && at(&source.file, source.line, &source.source)
                {
                    true
                } else {
                    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(&source.file))
                        .is_ok_and(|text| {
                            text.lines()
                                .nth(source.line - 1)
                                .is_some_and(|line| line.contains(source.source.as_str()))
                        })
                };
                if !present {
                    failures.push(format!(
                        "stale Words entry {}:{} {:?}; {relocate}",
                        source.file, source.line, source.source
                    ));
                }
            }
        }
        for group in &self.allowlist.unprinted {
            for (line, text) in &group.literals {
                if !at(&group.file, *line, text) {
                    failures.push(format!(
                        "stale {} entry {}:{line} {text:?}; {relocate}",
                        group.kind, group.file
                    ));
                }
            }
        }
        for unstaged in &self.allowlist.unstaged {
            if !at(&unstaged.file, unstaged.line, &unstaged.text) {
                failures.push(format!(
                    "stale unstaged entry {}:{} {:?}; {relocate}",
                    unstaged.file, unstaged.line, unstaged.text
                ));
            }
        }
        let unstaged = |literal: &Literal| {
            self.allowlist.unstaged.iter().any(|entry| {
                entry.file == literal.file
                    && entry.line == literal.line
                    && entry.text == literal.text
            })
        };
        let classified = |literal: &Literal| {
            self.allowlist.printed.iter().any(|entry| {
                entry.sources.iter().any(|source| {
                    source.file == literal.file
                        && source.line == literal.line
                        && source.source == literal.text
                })
            }) || self.allowlist.unprinted.iter().any(|group| {
                group.file == literal.file
                    && group
                        .literals
                        .iter()
                        .any(|(line, text)| *line == literal.line && *text == literal.text)
            })
        };
        let host_word = |text: &str| {
            let lower = text.to_lowercase();
            [
                "herdr",
                "popup",
                "plugin",
                "prefix",
                "rex space",
                "rex workspace",
                "rex block",
            ]
            .iter()
            .any(|word| lower.contains(word))
        };
        for literal in &literals {
            let place = format!("{}:{}", literal.file, literal.line);
            match literal.role.as_str() {
                "herdr-arm" => {}
                "rex-arm" | "title-format" => {
                    if violation(&literal.text).is_some() {
                        failures.push(format!("the Rex text at {place} names Herdr's words: {:?}", literal.text));
                    } else if !self.reached(literal) && !unstaged(literal) {
                        failures.push(format!("no Rex surface the sweep checks printed {place}: {:?}", literal.text));
                    }
                }
                _ if !host_word(&literal.text) => {}
                _ if classified(literal) => {}
                _ if violation(&literal.text).is_none() && self.reached(literal) => {}
                _ => failures.push(format!(
                    "unclassified host word at {place}: {:?}. Make the sweep reach it on a Rex home, or classify it in tests/words_allowlist.json",
                    literal.text
                )),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
    fn wait_ms(&self, duration_ms: u64) {
        self.0.fetch_add(duration_ms, Ordering::Relaxed);
    }
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new(root: &Path, name: &str, owner: Option<&str>) -> Self {
        let root = root.join(name);
        let home = root.join("home");
        let state = root.join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(state.join("runtime")).unwrap();
        if let Some(owner) = owner {
            fs::write(state.join("runtime/owner"), format!("{owner}\n")).unwrap();
        }
        Self { root, home, state }
    }

    fn runtime<'a, C: Clock>(
        &'a self,
        mux: &'a FakeMux,
        clock: &'a C,
        ids: &'a ProcessIds,
        endpoint: &'a str,
        space: Option<&'a str>,
        pane: Option<&'a str>,
    ) -> LifecycleRuntime<'a, FakeMux, C, ProcessIds> {
        LifecycleRuntime {
            state_directory: &self.state,
            home_directory: Some(&self.home),
            agent_config: AgentConfigDirectories::default(),
            cwd: &self.root,
            workspace_id: space,
            current_pane_id: pane,
            session_socket_id: endpoint,
            mux,
            clock,
            ids,
        }
    }
}

/// Every catalog operation, each with every example the catalog gives and an
/// empty input, through the entry point its token routes to.
fn operations(
    words: &mut Words,
    surface: &str,
    runtime: &LifecycleRuntime<'_, FakeMux, impl Clock, ProcessIds>,
) {
    let catalog = operation_catalog();
    let extra = [
        ("name", json!({"alias":"words"})),
        ("address.create", json!({"alias":"words-two"})),
        ("address.bind", json!({"alias":"words"})),
        (
            "address.allow-remote",
            json!({"address_id":"pane-address-1"}),
        ),
        (
            "address.rename",
            json!({"alias":"words", "new_alias":"renamed"}),
        ),
        ("worktree.pause", json!({})),
        ("worktree.resume", json!({})),
        ("context.grant", json!({"grant_id":"grant-1"})),
        ("context.revoke", json!({"grant_id":"grant-1"})),
        ("deactivate", json!({"confirmed":true})),
        ("reconcile", json!({})),
    ];
    let mut calls: Vec<(String, Value)> = Vec::new();
    for operation in catalog
        .operations
        .iter()
        .chain(&catalog.management_operations)
    {
        let token = if operation.token == "complete.no-reply" {
            "complete".to_owned()
        } else {
            operation.token.clone()
        };
        calls.push((token.clone(), json!({})));
        for example in &operation.examples {
            calls.push((token.clone(), example.input.clone()));
            if token == "snapshot.publish" {
                let mut input = example.input.clone();
                input["capture_git_state"] = json!(false);
                calls.push((token.clone(), input));
            }
        }
    }
    calls.extend(
        extra
            .into_iter()
            .map(|(token, input)| (token.to_owned(), input)),
    );
    for (operation, input) in calls {
        let operation = operation.as_str();
        let result = if operation == "describe" {
            describe_operations_for(None, MuxKind::Rex)
        } else if is_lifecycle_operation(operation) {
            execute_lifecycle_operation(operation, input.clone(), runtime)
        } else if is_integration_operation(operation) {
            execute_integration_operation(operation, input.clone(), runtime)
        } else if is_context_operation(operation) {
            let human = execute_human_context_operation(operation, input.clone(), runtime);
            words.outcome(&format!("{surface} human {operation}"), &human);
            execute_context_operation(operation, input.clone(), runtime)
        } else if is_message_operation(operation) {
            execute_message_operation(operation, input.clone(), runtime)
        } else if is_control_operation(operation) {
            execute_control_operation(operation, input.clone(), runtime)
        } else if operation.starts_with("handoff.") {
            execute_handoff_operation(operation, input.clone(), runtime)
        } else if operation.starts_with("group.") {
            execute_group_operation(operation, input.clone(), runtime)
        } else if operation.starts_with("reminder.") {
            execute_reminder_operation(operation, input.clone(), runtime)
        } else {
            panic!("uncovered catalog operation {operation}")
        };
        words.outcome(&format!("{surface} {operation}"), &result);
        let rendered = [
            "address.list",
            "group.list",
            "worktree.pause",
            "worktree.resume",
            "address.allow-remote",
            "deactivate",
            "address.create",
            "name",
        ]
        .into_iter()
        .find(|token| *token == operation);
        if let (Ok(value), Some(operation)) = (result, rendered) {
            words.check(
                &format!("{surface} {operation} rendered"),
                render_command_result_to_text_for(
                    &CommandResult::success(operation, value),
                    None,
                    MuxKind::Rex,
                ),
            );
        }
    }
}

fn help(words: &mut Words, mut command: clap::Command, path: &str) {
    words.check(
        &format!("help {path}"),
        command.render_long_help().to_string(),
    );
    words.check(&format!("help {path}"), command.render_help().to_string());
    for child in command.get_subcommands().cloned() {
        let name = format!("{path} {}", child.get_name());
        help(words, child, &name);
    }
}

/// The help, catalogs and tour, which read no state.
fn static_surfaces(words: &mut Words) {
    help(words, cli::Cli::command_for(MuxKind::Rex), "namedrop");
    let tool = IntegrationToolInvocation::new(
        "namedrop",
        std::collections::BTreeMap::from([("NAMEDROP_ENDPOINT".into(), ENDPOINT.into())]),
    );
    words.json("tool catalog", &serde_json::to_value(&tool).unwrap());
    words.json(
        "describe",
        &describe_operations_for(None, MuxKind::Rex).unwrap(),
    );
    let catalog = operation_catalog();
    for operation in catalog
        .operations
        .iter()
        .chain(&catalog.management_operations)
    {
        words.json(
            &format!("describe {}", operation.token),
            &describe_operations_for(Some(&operation.token), MuxKind::Rex).unwrap(),
        );
    }
    for presentation in tour::all_for(MuxKind::Rex).unwrap() {
        words.check("tour", &presentation.title);
        for page in &presentation.pages {
            words.check("tour", &page.title);
            for line in &page.body {
                words.check("tour", line);
            }
        }
    }
}

/// One Rex home per agent kind, identified or not, with and without Codex's
/// shared app-server: activation, the installed packages, Doctor, naming,
/// hooks, the screens' launch, and every operation bound and unbound.
fn homes(words: &mut Words, root: &Path) {
    let clock = SystemClock;
    // `daemon`: Codex used its shared app-server, which starts on the next
    // session; `server`: one runs now and serves the Codex pane.
    for (n, (agent, identified, daemon, server)) in [
        (None, false, false, false),
        (Some(AgentRuntime::ClaudeCode), false, false, false),
        (Some(AgentRuntime::ClaudeCode), true, false, false),
        (Some(AgentRuntime::Codex), false, false, false),
        (Some(AgentRuntime::Codex), true, false, false),
        (Some(AgentRuntime::Codex), false, true, false),
        (Some(AgentRuntime::Codex), true, true, false),
        (Some(AgentRuntime::Codex), false, true, true),
        (Some(AgentRuntime::Codex), true, true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let f = Fixture::new(root, &format!("home-{n}"), Some("standalone"));
        if daemon {
            fs::create_dir_all(f.home.join(".codex/app-server-daemon")).unwrap();
        }
        // sleep, named as the server Codex keeps under its home, so the
        // process table shows a running shared app-server.
        let mut running = server.then(|| {
            use std::os::unix::process::CommandExt;
            let program = f
                .home
                .join(".codex/packages/app-server-daemon/releases/0.162.1/bin/codex");
            let child = binary::command("/bin/sleep")
                .arg0(&program)
                .arg("120")
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !String::from_utf8_lossy(
                &binary::command("/bin/ps")
                    .args(["-o", "args=", "-p", &child.id().to_string()])
                    .output()
                    .unwrap()
                    .stdout,
            )
            .contains("app-server-daemon")
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the server's name never showed"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            fs::write(
                f.home.join(".codex/app-server-daemon/daemon.pid"),
                json!({"pid": child.id()}).to_string(),
            )
            .unwrap();
            child
        });
        let checkout = f.root.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let git = binary::command("git")
            .args(["init", "-q"])
            .arg(&checkout)
            .status()
            .unwrap();
        assert!(git.success());
        let mut world = FakeMux::world()
            .endpoint(ENDPOINT)
            .space("s1")
            .checkout(&checkout)
            .pane("p1")
            .tab("window:w")
            .terminal("t1");
        world = match agent {
            Some(AgentRuntime::Codex) => world.codex(),
            Some(AgentRuntime::ClaudeCode) => world.claude(),
            None => world,
        };
        if agent.is_some() && identified {
            world = world.session("agent-1").idle();
        }
        let mux = world.pane("p2").tab("window:w").terminal("t2").build();
        let ids = ProcessIds::default();
        let runtime = f.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), Some("p1"));
        let tag = format!("[{agent:?} identified={identified} daemon={daemon} server={server}]");
        let unconfirmed = execute_lifecycle_operation("activate", json!({}), &runtime);
        words.outcome(&format!("activate unconfirmed {tag}"), &unconfirmed);
        words.json(
            &format!("doctor before activation {tag}"),
            &serde_json::to_value(diagnose_collaboration(&runtime)).unwrap(),
        );
        let activation =
            execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
        words.json(&format!("activate json {tag}"), &activation);
        words.check(
            &format!("activate text {tag}"),
            render_command_result_to_text_for(
                &CommandResult::success("activate", activation),
                None,
                MuxKind::Rex,
            ),
        );
        for runtime in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
            let package = package_directory(&f.home, runtime);
            for file in ["SKILL.md", "tool.json"] {
                let surface = format!("installed {runtime:?}/{file}");
                let text = fs::read_to_string(package.join(file)).unwrap();
                if file == "tool.json" {
                    words.json(&surface, &serde_json::from_str(&text).unwrap());
                } else {
                    words.check(&surface, text);
                }
            }
        }
        let report = diagnose_collaboration(&runtime);
        if agent.is_none() {
            assert!(report.issues.iter().any(|i| i.code == "ND-SETUP-003"));
        }
        if agent.is_some() && !identified && !daemon {
            let native = report
                .issues
                .iter()
                .find(|i| i.code == "ND-SESSION-002")
                .expect("an agent with no session identity");
            // Its summary says what its explanation says: Namedrop has none.
            assert_eq!(
                native.summary,
                "Namedrop has no session identity for pane p1."
            );
            assert_eq!(native.repair.operation.as_deref(), Some("activate"));
            assert_eq!(native.repair.input, Some(json!({"confirmed":true})));
            let command = native.repair.command.as_ref().unwrap();
            assert!(
                command
                    .windows(2)
                    .any(|pair| pair == ["--input-json", "{\"confirmed\":true}"]),
                "{command:?}"
            );
        }
        words.json(
            &format!("doctor before naming {tag}"),
            &serde_json::to_value(&report).unwrap(),
        );
        for (operation, input) in [
            (
                "name",
                json!({"alias":"words", "pane_type": if agent.is_some() {"agent"} else {"terminal"}}),
            ),
            ("address.list", json!({})),
        ] {
            let result = execute_lifecycle_operation(operation, input, &runtime);
            words.outcome(&format!("{operation} {tag}"), &result);
            if let Ok(data) = &result {
                words.check(
                    &format!("{operation} text {tag}"),
                    render_command_result_to_text_for(
                        &CommandResult::success(operation, data.clone()),
                        None,
                        MuxKind::Rex,
                    ),
                );
            }
        }
        for input in [
            r#"{"hook_event_name":"SessionStart"}"#,
            r#"{"hook_event_name":"SessionStart","source":"compact"}"#,
        ] {
            if let Some(value) = execute_session_start_hook(input, &runtime) {
                words.json(&format!("session policy {tag}"), &value);
            }
        }
        if let Some(value) = execute_user_prompt_submit_hook(
            r#"{"hook_event_name":"UserPromptSubmit","session_id":"agent-1","prompt":"hello"}"#,
            &runtime,
        ) {
            words.json(&format!("prompt roster {tag}"), &value);
        }
        let report = diagnose_collaboration(&runtime);
        words.json(
            &format!("doctor json {tag}"),
            &serde_json::to_value(&report).unwrap(),
        );
        if server {
            assert!(
                report.issues.iter().any(|i| i.code == "ND-SESSION-003"),
                "{report:?}"
            );
        }
        words.check(
            &format!("doctor text {tag}"),
            render_health_report_to_text(&report),
        );
        for session in ["", "agent-1", "agent-2"] {
            match request_doctor_assistance(&runtime, session) {
                Ok(accepted) => words.check(&format!("doctor assistance {tag}"), accepted),
                Err(error) => words.check(&format!("doctor assistance {tag}"), error),
            }
        }
        for screen in [
            PopupScreen::Name,
            PopupScreen::Manage,
            PopupScreen::Doctor,
            PopupScreen::Status,
            PopupScreen::Tour,
        ] {
            match open_popup(&runtime, screen, mux.facts("p1")) {
                Ok(value) => words.json(&format!("ui launch {screen:?} {tag}"), &value),
                Err(error) => {
                    words.check(&format!("ui launch {screen:?} {tag}"), error.to_string())
                }
            }
        }
        operations(words, &format!("operation bound {tag}"), &runtime);
        // The operations end deactivated, so the roster the hook showed is gone.
        if let Some(value) = execute_user_prompt_submit_hook(
            r#"{"hook_event_name":"UserPromptSubmit","session_id":"agent-1","prompt":"hello"}"#,
            &runtime,
        ) {
            words.json(&format!("prompt roster gone {tag}"), &value);
        }
        let unbound = LifecycleRuntime {
            current_pane_id: None,
            ..f.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), None)
        };
        operations(words, &format!("operation unbound {tag}"), &unbound);
        if let Err(error) = request_doctor_assistance(&unbound, "agent-1") {
            words.check(&format!("doctor assistance unbound {tag}"), error);
        }
        let spaceless = LifecycleRuntime {
            workspace_id: None,
            current_pane_id: None,
            ..f.runtime(&mux, &clock, &ids, ENDPOINT, None, None)
        };
        if let Err(error) = request_doctor_assistance(&spaceless, "agent-1") {
            words.check(&format!("doctor assistance spaceless {tag}"), error);
        }
        if let Err(error) = execute_terminal("attempt-1", &unbound) {
            words.check(&format!("terminal runner unbound {tag}"), error);
        }
        if let Err(error) = detect_pane_type(&unbound) {
            words.check(&format!("pane type unbound {tag}"), error);
        }
        let pane = discover_worktree_for_workspace(&mux, "s1")
            .unwrap()
            .panes
            .into_iter()
            .find(|pane| pane.pane_id == "p1")
            .unwrap();
        mux.fail(Op::Apply, MuxError::Protocol("no layer id".into()));
        match tour::open_popup(&runtime, &pane) {
            Ok(value) => words.json(&format!("tour open {tag}"), &value),
            Err(error) => words.check(&format!("tour open {tag}"), error),
        }
        typed(words, &mux, &tag);
        if let Some(child) = running.as_mut() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        cleanup::remove_fixture(&f.root).unwrap();
    }
}

/// Rex sessions beside this one: the roster's other-sessions line for one and
/// for two, and a send to a label two of them share.
fn sessions(words: &mut Words, root: &Path) {
    let f = Fixture::new(root, "sessions", Some("standalone"));
    let mux = FakeMux::world()
        .endpoint(ENDPOINT)
        .space("s1")
        .label("here")
        .pane("p1")
        .tab("window:w")
        .claude()
        .session("agent-1")
        .idle()
        .space("s2")
        .label("dup")
        .pane("p2")
        .tab("window:w2")
        .codex()
        .session("agent-2")
        .idle()
        .space("s3")
        .label("dup")
        .pane("p3")
        .tab("window:w3")
        .codex()
        .session("agent-3")
        .idle()
        .build();
    let ids = ProcessIds::default();
    let clock = SystemClock;
    let prompt =
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"agent-1","prompt":"hello"}"#;
    let here = f.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), Some("p1"));
    for (space, pane, alias) in [
        ("s1", "p1", "lead"),
        ("s2", "p2", "helper"),
        ("s3", "p3", "helper"),
    ] {
        let runtime = f.runtime(&mux, &clock, &ids, ENDPOINT, Some(space), Some(pane));
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
        let named = execute_lifecycle_operation(
            "name",
            json!({"alias":alias,"pane_type":"agent"}),
            &runtime,
        );
        words.outcome("name (other sessions)", &named);
        if let Some(value) = execute_user_prompt_submit_hook(prompt, &here) {
            words.json("prompt roster (other sessions)", &value);
        }
    }
    for source in [
        "#dup @helper Check the parser.",
        "#all @helper Check the parser.",
        "#nowhere @helper Check it.",
    ] {
        let sent = execute_message_operation(
            "send",
            json!({"request_id": format!("send-{source}"), "source": source, "author_kind": "agent_session"}),
            &here,
        );
        words.outcome("send (other sessions)", &sent);
    }
    // The lead's name retired: the roster its session was shown is gone.
    let listed = execute_lifecycle_operation("address.list", json!({}), &here).unwrap();
    let lead = listed
        .to_string()
        .split('"')
        .find(|text| text.starts_with("pane-address"))
        .unwrap()
        .to_owned();
    words.outcome(
        "address.retire (other sessions)",
        &execute_lifecycle_operation("address.retire", json!({"address_id": lead}), &here),
    );
    if let Some(value) = execute_user_prompt_submit_hook(prompt, &here) {
        words.json("prompt roster gone (other sessions)", &value);
    }
    words.outcome(
        "address.list (other sessions)",
        &execute_lifecycle_operation("address.list", json!({"workspaces":"all"}), &here),
    );
    cleanup::remove_fixture(&f.root).unwrap();
}

/// What Namedrop typed into the panes: registration challenges, Doctor's
/// request for help, and deliveries.
fn typed(words: &mut Words, mux: &FakeMux, tag: &str) {
    for sent in mux.sent() {
        if let namedrop::mux::fake::SentInput::Prompt(text)
        | namedrop::mux::fake::SentInput::Line(text) = sent.input
        {
            words.check(&format!("typed into {} {tag}", sent.pane.as_str()), text);
        }
    }
}

/// Doctor on mail that has waited past a minute, for each reason it waits.
fn long_waits(words: &mut Words, root: &Path) {
    for (n, (status, edge, screen)) in [
        ("idle", None, None),
        ("working", None, None),
        ("blocked", None, None),
        ("unknown", None, None),
        (
            "working",
            Some(namedrop::turn_phase::TurnEdge::Stop),
            Some("no prompt box drawn here\n"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let f = Fixture::new(root, &format!("wait-{n}"), Some("standalone"));
        let mut world = FakeMux::world()
            .endpoint(ENDPOINT)
            .space("s1")
            .pane("p1")
            .tab("window:w")
            .claude()
            .session("session-1")
            .state(AgentState::parse(status));
        if let Some(screen) = screen {
            world = world.screen(screen);
        }
        let mux = world.build();
        let clock = TestClock(AtomicU64::new(1_000));
        let ids = ProcessIds::default();
        let runtime = f.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), Some("p1"));
        let namespace = StateNamespace::for_space(ENDPOINT, "s1");
        let attribution = MutationAttribution::new(ActorKind::Plugin, "test", 10);
        let draft = |id: &str, alias: &str| PaneAddressDraft {
            id: PaneAddressId::new(id),
            role_id: PaneRoleId::new(format!("role-{id}")),
            role_name: alias.to_owned(),
            role_description: format!("{alias} role"),
            alias: alias.to_owned(),
        };
        let (author, recipient) = (
            draft("address-author", "author"),
            draft("address-recipient", "recipient"),
        );
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.activate_worktree(&namespace, &attribution)?;
                transaction.create_pane_address(&namespace, &author, &attribution)?;
                transaction.create_pane_address(&namespace, &recipient, &attribution)?;
                transaction.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding-recipient"),
                    &recipient.id,
                    "p1",
                    &attribution,
                )?;
                transaction.observe_agent_session(
                    &namespace,
                    "observation-recipient",
                    &recipient.id,
                    &AgentSessionId::new("session-1"),
                    status,
                    &attribution,
                )?;
                transaction.create_thread(
                    &namespace,
                    &ThreadId::new("thread-1"),
                    "Words",
                    &attribution,
                )?;
                transaction.complete_reconciliation(&namespace)?;
                Ok(())
            })
            .unwrap();
        let challenge =
            issue_registration_challenge(&recipient.id, AgentRuntime::ClaudeCode, &runtime)
                .unwrap();
        execute_integration_operation(
            "register",
            json!({"runtime": AgentRuntime::ClaudeCode, "protocol_version": "collab/v1", "nonce": challenge.nonce,
                   "capabilities": complete_capability_profile()}),
            &runtime,
        )
        .unwrap();
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread-1"),
                        message_id: MessageId::new("message-1"),
                        in_reply_to: None,
                        author_address_id: author.id.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "author".to_owned(),
                        author_agent_session_snapshot: None,
                        request_id: "request-1".to_owned(),
                        authored_source: "@recipient Are the tests green?".to_owned(),
                        body: "Are the tests green?".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: recipient.id.clone(),
                            alias_snapshot: "recipient".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-1")),
                            delivery_id: DeliveryId::new("delivery-1"),
                        }],
                        context_references: Vec::new(),
                    },
                    &MutationAttribution::new(ActorKind::Human, "test", 1_000),
                )
            })
            .unwrap();
        if let Some(edge) = edge {
            namedrop::turn_phase::record(
                &f.state,
                ENDPOINT,
                "session-1",
                namedrop::turn_phase::TurnEdge::Prompt,
                2_000,
            );
            namedrop::turn_phase::record(&f.state, ENDPOINT, "session-1", edge, 3_000);
        }
        if status == "idle" {
            // Delivered: the prompt the pane got, and its attestation.
            mux.answer("p1", namedrop::mux::InputOutcome::Delivered);
            drain_mailboxes(&runtime).unwrap();
            for sent in mux.sent() {
                if let namedrop::mux::fake::SentInput::Prompt(prompt) = sent.input {
                    words.check("typed into p1 (delivery)", &prompt);
                    let input = json!({"hook_event_name":"UserPromptSubmit","session_id":"session-1","prompt":prompt});
                    if let Some(value) =
                        execute_user_prompt_submit_hook(&input.to_string(), &runtime)
                    {
                        words.json("delivery attestation", &value);
                    }
                }
            }
            cleanup::remove_fixture(&f.root).unwrap();
            continue;
        }
        clock.0.store(62_000, Ordering::Relaxed);
        let report = diagnose_collaboration(&runtime);
        assert!(
            report.issues.iter().any(|i| i.code == "ND-DELIVERY-005"),
            "{status}: {report:?}"
        );
        words.json(
            &format!("doctor long wait ({status})"),
            &serde_json::to_value(&report).unwrap(),
        );
        words.check(
            &format!("doctor text long wait ({status})"),
            render_health_report_to_text(&report),
        );
        cleanup::remove_fixture(&f.root).unwrap();
    }
}

/// The status panel's rereading of its pane in a Rex layer: closed, replaced,
/// or holding a new session, and the frozen panel's refresh after that.
fn status_panel(words: &mut Words, root: &Path) {
    use agent_status_ui::{Identity, Screen, Sources, latest_identity, popup, sanitize};
    popup_input::set_host(MuxKind::Rex);
    let f = Fixture::new(root, "status", Some("standalone"));
    let mux = FakeMux::world()
        .endpoint(ENDPOINT)
        .space("s1")
        .pane("p1")
        .tab("window:w")
        .terminal("t1")
        .claude()
        .session("agent-1")
        .idle()
        .build();
    let ids = ProcessIds::default();
    let clock = SystemClock;
    let runtime = f.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), Some("p1"));
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let mut changed = Vec::new();
    for (pane, terminal, session) in [
        ("p9", "t1", Some("agent-1")),
        ("p1", "t9", Some("agent-1")),
        ("p1", "t1", Some("agent-9")),
    ] {
        match latest_identity(&runtime, "s1", pane, terminal, session, &cancelled) {
            Identity::Changed(text) => {
                words.check("ui run status (changed pane)", &text);
                changed.push(text);
            }
            other => panic!("the panel kept {pane} {terminal} {session:?}: {other:?}"),
        }
    }
    let collect = |_: &std::sync::atomic::AtomicBool| {
        Ok(
            json!({"origin": {"workspace_id": "s1", "pane_id": "p1", "terminal_id": "t1", "session_id": "agent-1",
            "host": "claude", "label": "lead", "status": "idle"}, "observed_at_ms": 0, "panes": []}),
        )
    };
    let summarize = |_: &Value, _: &std::sync::atomic::AtomicBool| Ok(json!({}));
    let recheck = |_: &std::sync::atomic::AtomicBool| Identity::Changed(changed[1].clone());
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let mut screen = Screen {
        input: std::io::Cursor::new(b"rr".to_vec()),
        output: Vec::new(),
        interactive: false,
        width: 72,
        height: 24,
    };
    popup(&mut screen, &sources).unwrap();
    let text = String::from_utf8(screen.output).unwrap();
    words.check(
        "ui run status (frozen)",
        text.lines().map(sanitize).collect::<Vec<_>>().join("\n"),
    );
    cleanup::remove_fixture(&f.root).unwrap();
}

/// The update notice and Doctor's update issues on a standalone state.
fn updates(words: &mut Words, root: &Path) {
    let f = Fixture::new(root, "updates", Some("standalone"));
    fs::write(
        f.state.join("update-check.json"),
        json!({"schema_version":1,"checked_at":100,"last_success_at":100,"last_error":null,
            "source":{"kind":"standalone","installed_version":env!("CARGO_PKG_VERSION")},
            "remote_version":"9.10.0"})
        .to_string(),
    )
    .unwrap();
    let notice = namedrop::update_check::session_notice(
        r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
        &f.state,
        1000,
        None,
    )
    .expect("a standalone state shows the update notice");
    words.json("update notice", &notice);
    let mux = FakeMux::world()
        .endpoint(ENDPOINT)
        .space("s1")
        .pane("p1")
        .tab("window:w")
        .claude()
        .build();
    let ids = ProcessIds::default();
    let clock = SystemClock;
    let runtime = f.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), Some("p1"));
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    let report = diagnose_collaboration(&runtime);
    assert!(report.issues.iter().any(|i| i.code == "ND-UPDATE-001"));
    words.json(
        "doctor update json",
        &serde_json::to_value(&report).unwrap(),
    );
    words.check("doctor update text", render_health_report_to_text(&report));
    let settings =
        PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config/namedrop/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, "{").unwrap();
    let report = diagnose_collaboration(&runtime);
    assert!(report.issues.iter().any(|i| i.code == "ND-UPDATE-002"));
    words.json(
        "doctor invalid settings json",
        &serde_json::to_value(&report).unwrap(),
    );
    words.check(
        "doctor invalid settings text",
        render_health_report_to_text(&report),
    );
    fs::remove_file(&settings).unwrap();
}

/// A machine whose Herdr install shares this HOME: Herdr owns the packages,
/// Herdr rebaked the packages under the Rex module, or Herdr took a Rex state
/// over. The refusal of a Rex install's writer is `words_screens.py`'s, which
/// runs an installed runtime.
fn shared_home(words: &mut Words, root: &Path) {
    let clock = SystemClock;
    let ids = ProcessIds::default();
    let herdr = Fixture::new(root, "shared", Some("herdr"));
    let standalone = Fixture {
        home: herdr.home.clone(),
        ..Fixture::new(&herdr.root, "standalone", Some("standalone"))
    };
    let herdr_mux = FakeMux::world()
        .endpoint("/tmp/words-herdr.sock")
        .space("h1")
        .pane("hp1")
        .claude()
        .build();
    let herdr_runtime = herdr.runtime(
        &herdr_mux,
        &clock,
        &ids,
        "/tmp/words-herdr.sock",
        Some("h1"),
        Some("hp1"),
    );
    let packages = |words: &mut Words| {
        for runtime in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
            let package = package_directory(&herdr.home, runtime);
            for file in ["SKILL.md", "tool.json"] {
                let surface = format!("installed {runtime:?}/{file} (Herdr owns the packages)");
                let text = fs::read_to_string(package.join(file)).unwrap();
                if file == "tool.json" {
                    words.json(&surface, &serde_json::from_str(&text).unwrap());
                } else {
                    words.check(&surface, text);
                }
            }
        }
    };
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &herdr_runtime).unwrap();
    packages(words);
    let mux = FakeMux::world()
        .endpoint(ENDPOINT)
        .space("s1")
        .pane("p1")
        .tab("window:w")
        .claude()
        .build();
    let runtime = standalone.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), Some("p1"));
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &herdr_runtime).unwrap();
    let registration = standalone.home.join(".config/namedrop/rex.json");
    fs::create_dir_all(registration.parent().unwrap()).unwrap();
    fs::write(
        &registration,
        json!({"state": standalone.state}).to_string(),
    )
    .unwrap();
    let report = diagnose_collaboration(&runtime);
    assert!(
        report.issues.iter().any(|i| i.code == "ND-INSTALL-003"),
        "{report:?}"
    );
    words.json(
        "doctor (Herdr rebaked the packages)",
        &serde_json::to_value(&report).unwrap(),
    );
    words.check(
        "doctor text (Herdr rebaked the packages)",
        render_health_report_to_text(&report),
    );
    packages(words);
    fs::write(
        standalone.state.join("runtime/retired.json"),
        json!({"reason":"herdr_handoff","successor":herdr.state,"at":"2026-11-15T00:00:00Z"})
            .to_string(),
    )
    .unwrap();
    for (operation, input) in [
        ("activate", json!({"confirmed":true})),
        ("name", json!({"alias":"words"})),
        ("address.list", json!({})),
    ] {
        let result = execute_lifecycle_operation(operation, input, &runtime);
        words.outcome(&format!("{operation} (Herdr took the state over)"), &result);
    }
    let report = diagnose_collaboration(&runtime);
    words.json(
        "doctor (Herdr took the state over)",
        &serde_json::to_value(&report).unwrap(),
    );
    words.check(
        "doctor text (Herdr took the state over)",
        render_health_report_to_text(&report),
    );
    // Doctor on the state that took over, from a Rex block whose agent still
    // runs the old hooks.
    let agent = test_process_incarnation(std::process::id()).unwrap();
    fs::write(
        &registration,
        json!({"state": standalone.state, "predecessors": [{"state": standalone.state, "sealed": false,
            "agents": [{"endpoint": ENDPOINT, "block": "p1", "agent": agent}]}]})
        .to_string(),
    )
    .unwrap();
    let successor = herdr.runtime(&mux, &clock, &ids, ENDPOINT, Some("s1"), Some("p1"));
    let report = diagnose_collaboration(&successor);
    assert!(
        report.issues.iter().any(|i| i.code == "ND-INSTALL-001"),
        "{report:?}"
    );
    words.json(
        "doctor (Herdr took the state over)",
        &serde_json::to_value(&report).unwrap(),
    );
    words.check(
        "doctor text (Herdr took the state over)",
        render_health_report_to_text(&report),
    );
    // A Rex pane on the state Herdr owns, with an update waiting: the state
    // is Herdr's install, so its notice and Doctor name Herdr's update
    // command and settings file (the U11 review's F4).
    fs::write(&registration, json!({"state": herdr.state}).to_string()).unwrap();
    fs::write(
        herdr.state.join("update-check.json"),
        json!({"schema_version":1,"checked_at":100,"last_success_at":100,"last_error":null,
            "remote_version":"9.10.0"})
        .to_string(),
    )
    .unwrap();
    let notice = namedrop::update_check::session_notice(
        r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
        &herdr.state,
        1000,
        None,
    )
    .expect("a Herdr-owned state shows Herdr's update notice");
    words.json("update notice (Herdr owns the state)", &notice);
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &successor).unwrap();
    let report = diagnose_collaboration(&successor);
    let mut value = serde_json::to_value(&report).unwrap();
    let update = value["issues"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|issue| issue["code"] == "ND-UPDATE-001")
        .expect("Doctor reports the update");
    // The repair command's words, as the command line they spell.
    let command = update["repair"]["command"].take();
    words.check(
        "doctor update (Herdr owns the state)",
        command
            .as_array()
            .unwrap()
            .iter()
            .map(|argument| argument.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" "),
    );
    words.json("doctor update (Herdr owns the state)", &value);
    words.check(
        "doctor update text (Herdr owns the state)",
        render_health_report_to_text(&report),
    );
    let settings = PathBuf::from(std::env::var_os("HOME").unwrap())
        .join(".config/herdr/plugins/config/namedrop/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, "{").unwrap();
    let report = diagnose_collaboration(&successor);
    assert!(report.issues.iter().any(|i| i.code == "ND-UPDATE-002"));
    words.json(
        "doctor invalid settings (Herdr owns the state)",
        &serde_json::to_value(&report).unwrap(),
    );
    words.check(
        "doctor invalid settings text (Herdr owns the state)",
        render_health_report_to_text(&report),
    );
    fs::remove_file(&settings).unwrap();
    cleanup::remove_fixture(&herdr.root).unwrap();
}

/// A script in `tests/` that writes each output it captured, as JSON lines of
/// `{"surface", "text"}`, to `NAMEDROP_WORDS_CAPTURE`.
fn captured(words: &mut Words, root: &Path, script: &str, args: &[&str]) {
    let capture = root.join(format!("{script}.jsonl"));
    let out = binary::command("python3")
        .args(["-B", "-I"])
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join(script),
        )
        .args(args)
        .env("NAMEDROP_WORDS_CAPTURE", &capture)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{script}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = fs::read_to_string(&capture).unwrap();
    assert!(!lines.is_empty(), "{script} captured nothing");
    for line in lines.lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        words.check(
            record["surface"].as_str().unwrap(),
            record["text"].as_str().unwrap(),
        );
    }
}

#[test]
fn rex_words_sweep() {
    let root =
        update_profile::root(std::env::temp_dir().join(format!("nd-words-{}", std::process::id())));
    let home = root.join("process-home");
    fs::create_dir_all(&home).unwrap();
    if update_profile::isolate(&root, &home) {
        update_profile::cleanup(&root);
        return;
    }
    let mut words = Words::new();
    words.dump =
        std::env::var_os("NAMEDROP_WORDS_DUMP").map(|path| fs::File::create(path).unwrap());
    dedicated_sites(&mut words, &root);
    for (operation, data) in [
        ("deactivate", json!({})),
        ("address.list", json!([])),
        ("address.allow-remote", json!({"alias":"worker"})),
        ("address.deny-remote", json!({"alias":"worker"})),
    ] {
        words.check(
            operation,
            render_command_result_to_text_for(
                &CommandResult::success(operation, data),
                None,
                MuxKind::Rex,
            ),
        );
    }
    static_surfaces(&mut words);
    homes(&mut words, &root);
    sessions(&mut words, &root);
    long_waits(&mut words, &root);
    status_panel(&mut words, &root);
    updates(&mut words, &root);
    shared_home(&mut words, &root);
    let out = binary::namedrop()
        .args(["--endpoint", ENDPOINT, "--help"])
        .output()
        .unwrap();
    words.check("binary help", String::from_utf8(out.stdout).unwrap());
    captured(
        &mut words,
        &root,
        "words_screens.py",
        &[binary::namedrop_path()],
    );
    captured(&mut words, &root, "install_script.py", &[]);
    words.finish();
}

#[test]
fn herdr_words_are_unchanged() {
    // Each Rex rewording, its Herdr text and the Rex text that replaces it:
    // a Herdr home shows the first and never the second.
    fn all_help(mut command: clap::Command, out: &mut String) {
        out.push_str(&command.render_long_help().to_string());
        for child in command.get_subcommands().cloned() {
            all_help(child, out);
        }
    }
    let mut herdr_help = String::new();
    all_help(cli::Cli::command_for(MuxKind::Herdr), &mut herdr_help);
    for (herdr, rex) in [
        ("collaborate inside Herdr", "collaborate inside Rex"),
        (
            "Activate Namedrop for this Herdr space after confirmation",
            "this Rex session",
        ),
        ("Bind a name to a live Herdr pane", "a live Rex pane"),
        ("Doctor, agent status, or tour popup", "tour screen"),
        (
            "Recover after a plugin restart;",
            "Recover after a restart;",
        ),
        (
            "Plugin-owned state directory for this installed hook",
            "Namedrop's state directory",
        ),
        ("Handle one Herdr lifecycle event", "Rex sends none"),
        ("Add Namedrop's popup shortcuts", "Unused on Rex"),
        ("--herdr-socket", "inside Rex"),
    ] {
        assert!(
            herdr_help.contains(herdr),
            "{herdr:?} missing from Herdr's help"
        );
        assert!(!herdr_help.contains(rex), "{rex:?} in Herdr's help");
    }
    let herdr_tour = format!("{:?}", tour::all_for(MuxKind::Herdr).unwrap());
    for phrase in [
        "Names belong to this Herdr workspace.",
        "Press Prefix, then Shift-S to see what each agent is doing.",
        "Press Prefix, then Shift-H to open Namedrop Doctor.",
        "Press Prefix, then Shift-M to manage names:",
        "Press Prefix, then Shift-O to open this tour again.",
    ] {
        assert!(
            herdr_tour.contains(phrase),
            "{phrase:?} missing from Herdr's tour"
        );
    }
    assert!(!herdr_tour.contains("Rex"), "{herdr_tour}");
    let herdr_catalog = describe_operations_for(None, MuxKind::Herdr)
        .unwrap()
        .to_string();
    assert!(herdr_catalog.contains("this Herdr session"));
    assert!(herdr_catalog.contains("the pin writes one ref under refs/herdr-context/"));
    assert!(!herdr_catalog.contains("Rex"), "{herdr_catalog}");
    let root = std::env::temp_dir().join(format!("nd-words-herdr-{}", std::process::id()));
    let f = Fixture::new(&root, "herdr", None);
    let mux = FakeMux::world()
        .endpoint("/tmp/words.sock")
        .space("s1")
        .pane("p1")
        .claude()
        .build();
    let ids = ProcessIds::default();
    let clock = SystemClock;
    let runtime = f.runtime(
        &mux,
        &clock,
        &ids,
        "/tmp/words.sock",
        Some("s1"),
        Some("p1"),
    );
    let activation =
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    assert!(activation.get("host").is_none());
    let rendered = render_command_result_to_text(&CommandResult::success("activate", activation));
    assert!(rendered.contains("  Check native hooks: herdr integration status\n  If setup is needed, choose your runtime:\n    herdr integration install codex\n    herdr integration install claude"));
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        let package = package_directory(&f.home, agent);
        let descriptor = fs::read_to_string(package.join("tool.json")).unwrap();
        assert_eq!(descriptor, integration_package(agent).tool_descriptor);
        let instructions = fs::read_to_string(package.join("SKILL.md")).unwrap();
        assert!(instructions.contains("Name and @mention agents or terminals in Herdr;"));
        assert!(instructions.contains("Registration uses that prefix with Herdr flags."));
    }
    let report = diagnose_collaboration(&runtime);
    let native = report
        .issues
        .iter()
        .find(|i| i.code == "ND-SESSION-002")
        .unwrap();
    assert_eq!(native.summary, "Herdr has no session identity for pane p1.");
    assert_eq!(
        native.repair.operation.as_deref(),
        Some("herdr.integration.install")
    );
    assert_eq!(
        native.repair.command,
        Some(vec![
            "herdr".into(),
            "integration".into(),
            "install".into(),
            "claude".into()
        ])
    );
    cleanup::remove_fixture(&root).unwrap();
}

#[test]
fn words_cli_uses_the_selected_home() {
    for (endpoint, forbidden) in [
        ("rex:/tmp/words.sock", "herdr"),
        ("/tmp/words.sock", "inside Rex"),
    ] {
        let out = binary::namedrop()
            .args(["--endpoint", endpoint, "--help"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(!text.to_lowercase().contains(forbidden), "{text}");
    }
}

include!("support/words_hosts.rs");

#[test]
fn scanner_rejects_unclassified_literal_escapes() {
    let out = binary::command("python3")
        .args([
            "-B",
            "-I",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/words_literals_regression.py"
            ),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn host_pair_sites_are_reached() {
    let root = std::env::temp_dir().join(format!("nd-words-sites-{}", std::process::id()));
    let mut words = Words::new();
    dedicated_sites(&mut words, &root);
    assert!(
        words.violations.is_empty(),
        "{}",
        words.violations.join("\n")
    );
    cleanup::remove_fixture(&root).unwrap();
}

/// A printed entry may start with `<path>`: it matches where a path does,
/// and a text with no path ends the search.
#[test]
fn an_entry_may_start_with_a_path() {
    let pattern = "<path>/herdr/settings.json";
    let text = "path=/home/me/.config/herdr/settings.json, \"/x/herdr/settings.json\"";
    let found = occurrences(text, pattern);
    let matched: Vec<&str> = found
        .iter()
        .map(|(start, end)| &text[*start..*end])
        .collect();
    assert_eq!(
        matched,
        [
            "/home/me/.config/herdr/settings.json",
            "/x/herdr/settings.json"
        ]
    );
    assert!(occurrences("no path here", pattern).is_empty());
    assert!(occurrences("", pattern).is_empty());
    assert!(occurrences("ends with /", pattern).is_empty());
}

#[test]
fn nouns_preserve_protocol_identifiers() {
    for invalid in [
        "No named agents in this workspace.",
        "Open this space.",
        "Rex block",
        "Other workspaces are paused.",
        "  space       Open this workspace.",
        "Usage: space Open this workspace.",
    ] {
        assert!(violation(invalid).is_some(), "{invalid}");
    }
    for identifier in [
        "space_id",
        "workspace_id",
        "namespace",
        "whitespace",
        "--space <SPACE>",
        "routing_scope:space",
        "{space}",
    ] {
        assert_eq!(violation(identifier), None, "{identifier}");
    }
}
