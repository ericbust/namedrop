use namedrop::tour::{self, PageAction};
use namedrop::{
    AgentSessionId, Clock, DiscoveredPane, DiscoveredWorktree, HerdrCommandExecutor,
    HerdrCommandOutput, IdGenerator, LifecycleRuntime, WorktreeId,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("namedrop-tour-{}-{sequence}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn presentation(pages: &str) -> String {
    format!("id: sample-1\ntitle: Sample\n---\n{pages}")
}

fn assert_rejected(source: &str, expected: &str) {
    let error = tour::parse(source).unwrap_err();
    assert!(
        error.contains(expected),
        "expected an error containing {expected:?}, got {error:?}"
    );
}

#[test]
fn shipped_presentations_parse_and_fit_the_popup() {
    let presentations = tour::all().unwrap();
    assert!(!presentations.is_empty());

    let mut ids = presentations
        .iter()
        .map(|presentation| presentation.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), presentations.len(), "presentation ids repeat");

    let current = tour::current().unwrap();
    assert_eq!(current.id, tour::CURRENT_ID);

    for presentation in &presentations {
        assert!(!presentation.title.is_empty());
        assert!(!presentation.pages.is_empty());
        for page in &presentation.pages {
            assert!(!page.title.is_empty());
            assert!(page.title.len() <= tour::BODY_WIDTH, "{:?}", page.title);
            let body = tour::wrapped_body(page, tour::BODY_WIDTH);
            assert!(!body.is_empty(), "{} has an empty page", presentation.id);
            assert!(
                body.len() <= tour::MAX_BODY_LINES,
                "{} page {:?} wraps to {} lines",
                presentation.id,
                page.title,
                body.len()
            );
            for line in &body {
                assert!(
                    line.chars().count() <= tour::BODY_WIDTH,
                    "{line:?} is wider than the popup"
                );
                assert_eq!(line.trim_end(), line, "{line:?} has trailing space");
            }
        }
    }
}

#[test]
fn parser_rejects_malformed_presentations() {
    let valid = presentation("# First\nHello.\naction: name\n---\n# Second\n\n  a command\n");
    let parsed = tour::parse(&valid).unwrap();
    assert_eq!(parsed.id, "sample-1");
    assert_eq!(parsed.title, "Sample");
    assert_eq!(parsed.pages.len(), 2);
    assert_eq!(parsed.pages[0].title, "First");
    assert_eq!(parsed.pages[0].body, ["Hello."]);
    assert_eq!(parsed.pages[0].action, Some(PageAction::Name));
    assert_eq!(parsed.pages[1].body, ["  a command"]);
    assert_eq!(parsed.pages[1].action, None);

    assert_rejected("title: Sample\n---\n# Page\nBody\n", "missing `id`");
    assert_rejected(
        "id: Sample\ntitle: Sample\n---\n# Page\n",
        "presentation id",
    );
    assert_rejected("id: ../x\ntitle: Sample\n---\n# Page\n", "presentation id");
    assert_rejected("id: sample-1\n---\n# Page\n", "missing a title");
    assert_rejected("id: sample-1\ntitle:\n---\n# Page\n", "missing a title");
    assert_rejected(
        "id: sample-1\ntitle: Sample\nauthor: me\n---\n# Page\n",
        "unknown key",
    );
    assert_rejected("id: sample-1\ntitle: Sample\n", "`---`");
    assert_rejected(&presentation("Body without a title\n"), "`# <title>`");
    assert_rejected(&presentation("# First\n---\n#\n"), "page 2");
    assert_rejected(&presentation(""), "`# <title>`");
    assert_rejected(
        &presentation(&format!("# {}\n", "t".repeat(69))),
        "title is wider",
    );
    assert_rejected(&presentation("# Page\naction: open\n"), "unknown action");
    assert_rejected(
        &presentation("# Page\naction: name\naction: name\n"),
        "repeats its action",
    );
    assert_rejected(
        &presentation("# Page\nclear \u{1b}[2J screen\n"),
        "control character",
    );

    // Sixteen paragraphs with blank lines between them are 31 lines.
    let tall = (1..=16)
        .map(|n| format!("Paragraph {n}."))
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_rejected(&presentation(&format!("# Tall\n{tall}\n")), "wraps to 31");

    // One paragraph that wraps past the limit is just as tall.
    let long = "word ".repeat(14 * 17);
    assert_rejected(&presentation(&format!("# Long\n{long}\n")), "wraps to");

    // Exactly sixteen lines still fits.
    let fits = (1..=16)
        .map(|n| format!("  line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    tour::parse(&presentation(&format!("# Fits\n{fits}\n"))).unwrap();

    let other = "id: other-1\ntitle: Other\n---\n# Page\n";
    assert_eq!(tour::parse_all(&[&valid, other]).unwrap().len(), 2);
    let error = tour::parse_all(&[&valid, other, &valid]).unwrap_err();
    assert!(error.contains("sample-1 is used more than once"), "{error}");

    assert_page_layout();
}

/// Part of the parser gate: how page text becomes paragraphs and lines.
fn assert_page_layout() {
    let page = tour::parse(&presentation(
        "# Page\n\nFirst line\nsame paragraph.\n\n\n  keep   as written\nNext.\n\n",
    ))
    .unwrap()
    .pages
    .remove(0);
    assert_eq!(
        page.body,
        [
            "First line same paragraph.",
            "",
            "  keep   as written",
            "Next."
        ]
    );

    let wrapped = tour::wrapped_body(
        &tour::Page {
            title: "Page".to_owned(),
            body: vec![
                "alpha beta gamma".to_owned(),
                String::new(),
                "  one two three".to_owned(),
                "abcdefghij".to_owned(),
            ],
            action: None,
        },
        10,
    );
    assert_eq!(
        wrapped,
        [
            "alpha beta",
            "gamma",
            "",
            "  one two",
            "  three",
            "abcdefghij"
        ]
    );
    let unbroken = tour::Page {
        title: "Page".to_owned(),
        body: vec!["abcdefghijkl".to_owned()],
        action: None,
    };
    assert_eq!(tour::wrapped_body(&unbroken, 5), ["abcde", "fghij", "kl"]);
}

#[test]
fn shown_marker_is_claimed_exactly_once_and_can_be_reset() {
    let directory = TestDirectory::new();
    let state = Arc::new(directory.path().to_owned());
    assert!(!tour::is_shown(&state, "welcome-1").unwrap());

    let claimants = 8;
    let barrier = Arc::new(Barrier::new(claimants));
    let claims = (0..claimants)
        .map(|_| {
            let state = Arc::clone(&state);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                tour::mark_shown(&state, "welcome-1").unwrap()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|claim| claim.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claims.iter().filter(|claimed| **claimed).count(), 1);

    assert!(tour::is_shown(&state, "welcome-1").unwrap());
    assert!(!tour::mark_shown(&state, "welcome-1").unwrap());
    // Markers are per presentation: a new id is still unshown.
    assert!(!tour::is_shown(&state, "welcome-2").unwrap());

    let markers = state.join("tours-shown");
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&markers), 0o700);
    assert_eq!(mode(&markers.join("welcome-1")), 0o600);
    assert_eq!(fs::read(markers.join("welcome-1")).unwrap(), b"");

    assert!(tour::reset(&state, "welcome-1").unwrap());
    assert!(!tour::is_shown(&state, "welcome-1").unwrap());
    assert!(!tour::reset(&state, "welcome-1").unwrap());
    assert!(tour::mark_shown(&state, "welcome-1").unwrap());
    assert!(tour::is_shown(&state, "welcome-1").unwrap());
}

#[test]
fn marker_rejects_unsafe_presentation_ids() {
    let directory = TestDirectory::new();
    let state = directory.path();
    let too_long = "a".repeat(65);
    for id in [
        "../x",
        "a/b",
        "/etc/passwd",
        "..",
        ".",
        "A",
        "",
        "-a",
        "a_b",
        "a.b",
        "a b",
        "a\0b",
        "é",
        too_long.as_str(),
    ] {
        assert!(tour::is_shown(state, id).is_err(), "is_shown({id:?})");
        assert!(tour::mark_shown(state, id).is_err(), "mark_shown({id:?})");
        assert!(tour::reset(state, id).is_err(), "reset({id:?})");
    }
    assert_eq!(fs::read_dir(state).unwrap().count(), 0);

    // The longest valid id is accepted.
    let longest = "a".repeat(64);
    assert!(tour::mark_shown(state, &longest).unwrap());
}

#[derive(Default)]
struct PopupHerdr {
    commands: Mutex<Vec<Vec<String>>>,
    fail: bool,
}

impl HerdrCommandExecutor for PopupHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.commands
            .lock()
            .unwrap()
            .push(args.iter().map(|argument| (*argument).to_owned()).collect());
        if self.fail {
            return Err("plugin pane open failed".to_owned());
        }
        Ok(HerdrCommandOutput {
            stdout: r#"{"result":{"pane_id":"popup-1"}}"#.to_owned(),
        })
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        100
    }
    fn wait_ms(&self, _duration_ms: u64) {}
}

struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

fn pane(id: &str, agent: Option<&str>) -> DiscoveredPane {
    DiscoveredPane {
        pane_id: id.to_owned(),
        terminal_id: format!("terminal-{id}"),
        tab_id: "tab-1".to_owned(),
        workspace_id: "workspace-1".to_owned(),
        cwd: Some("/repo".to_owned()),
        pane_label: None,
        agent_session_id: agent.map(|_| AgentSessionId::new(format!("session-{id}"))),
        agent: agent.map(str::to_owned),
        occupant_label: None,
        agent_status: agent.map(|_| "idle".to_owned()),
        status_reported: None,
        idle_state_label: None,
    }
}

fn worktree(panes: Vec<DiscoveredPane>) -> DiscoveredWorktree {
    DiscoveredWorktree {
        unavailable_panes: vec![],
        worktree_id: WorktreeId::new("worktree-1"),
        workspace_id: "workspace-1".to_owned(),
        worktree_attached: true,
        label: None,
        branch: None,
        panes,
    }
}

fn runtime<'a>(
    state: &'a Path,
    herdr: &'a PopupHerdr,
    ids: &'a SequentialIds,
) -> LifecycleRuntime<'a, namedrop::TuiHost<&'a PopupHerdr>, FixedClock, SequentialIds> {
    LifecycleRuntime {
        state_directory: state,
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: Some("workspace-1"),
        current_pane_id: None,
        session_socket_id: "/run/herdr.sock",
        host: namedrop::TuiHost::new(herdr),
        clock: &FixedClock,
        ids,
    }
}

#[test]
fn first_agent_show_opens_the_tour_popup_once() {
    let directory = TestDirectory::new();
    let state = directory.path();
    let ids = SequentialIds(AtomicU64::new(1));
    let herdr = PopupHerdr::default();
    let runtime = runtime(state, &herdr, &ids);
    let space = worktree(vec![
        pane("pane-shell", None),
        pane("pane-1", Some("codex")),
        pane("pane-2", Some("claude")),
    ]);

    let result = tour::show_for_first_agent(&runtime, &space, Some("pane-2"));
    assert_eq!(
        result,
        serde_json::json!({
            "shown": true,
            "presentation_id": "welcome-1",
            "pane_id": "pane-2",
            "popup": {"result": {"pane_id": "popup-1"}},
        })
    );
    let expected = [
        "plugin",
        "pane",
        "open",
        "--plugin",
        "namedrop",
        "--entrypoint",
        "tour",
        "--placement",
        "popup",
        "--width",
        "72",
        "--height",
        "24",
        "--env",
        "NAMEDROP_TARGET_WORKSPACE=workspace-1",
        "--env",
        "NAMEDROP_TARGET_PANE=pane-2",
        "--env",
        "NAMEDROP_TARGET_TERMINAL=terminal-pane-2",
        "--env",
        "NAMEDROP_TARGET_SESSION=session-pane-2",
        "--env",
        &format!("HERDR_PLUGIN_STATE_DIR={}", state.display()),
        "--env",
        "HERDR_SOCKET_PATH=/run/herdr.sock",
        "--focus",
    ];
    assert_eq!(*herdr.commands.lock().unwrap(), [expected]);
    assert!(tour::is_shown(state, tour::CURRENT_ID).unwrap());

    let again = tour::show_for_first_agent(&runtime, &space, Some("pane-2"));
    assert_eq!(
        again,
        serde_json::json!({"shown": false, "presentation_id": "welcome-1", "reason": "already_shown"})
    );
    assert_eq!(herdr.commands.lock().unwrap().len(), 1, "opened twice");

    // Without a usable preference the first agent pane in discovery order wins.
    assert!(tour::reset(state, tour::CURRENT_ID).unwrap());
    let result = tour::show_for_first_agent(&runtime, &space, Some("pane-shell"));
    assert_eq!(result["pane_id"], "pane-1");
    let result_args = herdr.commands.lock().unwrap()[1].clone();
    assert!(result_args.contains(&"NAMEDROP_TARGET_PANE=pane-1".to_owned()));
    assert!(tour::reset(state, tour::CURRENT_ID).unwrap());
    assert_eq!(
        tour::show_for_first_agent(&runtime, &space, None)["pane_id"],
        "pane-1"
    );

    let fresh = TestDirectory::new();
    let quiet = PopupHerdr::default();
    let shells = worktree(vec![pane("pane-shell", None)]);
    let result = tour::show_for_first_agent(
        &self::runtime(fresh.path(), &quiet, &ids),
        &shells,
        Some("pane-shell"),
    );
    assert_eq!(
        result,
        serde_json::json!({"shown": false, "presentation_id": "welcome-1", "reason": "no_agent_pane"})
    );
    assert!(quiet.commands.lock().unwrap().is_empty());
    assert!(!tour::is_shown(fresh.path(), tour::CURRENT_ID).unwrap());

    let failing = PopupHerdr {
        fail: true,
        ..PopupHerdr::default()
    };
    let result =
        tour::show_for_first_agent(&self::runtime(fresh.path(), &failing, &ids), &space, None);
    assert_eq!(
        result,
        serde_json::json!({
            "shown": false,
            "presentation_id": "welcome-1",
            "reason": "open_failed",
            "error": "plugin pane open failed",
            "pane_id": "pane-1",
        })
    );
    assert_eq!(failing.commands.lock().unwrap().len(), 1);
    // The marker stays so a broken popup is not retried on every event.
    assert!(tour::is_shown(fresh.path(), tour::CURRENT_ID).unwrap());
    let result =
        tour::show_for_first_agent(&self::runtime(fresh.path(), &failing, &ids), &space, None);
    assert_eq!(result["reason"], "already_shown");
    assert_eq!(failing.commands.lock().unwrap().len(), 1);

    assert_marker_error_is_reported();
}

/// Part of the first-agent gate: a marker failure is reported, never raised.
fn assert_marker_error_is_reported() {
    let directory = TestDirectory::new();
    // A file where the state directory should be makes every marker call fail.
    let state = directory.path().join("not-a-directory");
    fs::write(&state, b"").unwrap();
    let ids = SequentialIds(AtomicU64::new(1));
    let herdr = PopupHerdr::default();
    let result = tour::show_for_first_agent(
        &runtime(&state, &herdr, &ids),
        &worktree(vec![pane("pane-1", Some("codex"))]),
        None,
    );
    assert_eq!(result["shown"], false);
    assert_eq!(result["reason"], "marker_error");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
    assert!(herdr.commands.lock().unwrap().is_empty());
}

#[test]
fn welcome_presentation_covers_the_basics() {
    let welcome = tour::current().unwrap();
    assert_eq!(welcome.id, "welcome-1");
    assert_eq!(welcome.title, "Welcome to Namedrop");
    assert_eq!(welcome.pages.len(), 4);
    assert_eq!(welcome.pages[0].title, "Welcome to Namedrop");
    assert_eq!(welcome.pages[0].action, Some(PageAction::Name));
    assert!(
        welcome.pages[1..].iter().all(|page| page.action.is_none()),
        "only the first page names the pane"
    );
    assert_eq!(
        welcome
            .pages
            .iter()
            .map(|page| page.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Welcome to Namedrop",
            "Mention an agent",
            "Terminals, groups and status",
            "When something is off",
        ]
    );

    let text = welcome
        .pages
        .iter()
        .flat_map(|page| page.body.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "@",
        "Press N to name this pane now.",
        "Ask @reviewer to check the retry logic.",
        "@reviewer Check the retry logic and report any bugs.",
        "namedrop name @build --type terminal",
        "@build cargo test",
        "stdout, stderr and the exit status",
        "terminal",
        "group",
        "Create the Namedrop group @review-team with @reviewer and @tester.",
        "Shift-S",
        "Shift-H",
        "namedrop doctor",
        "Shift-M",
        "Shift-O",
    ] {
        assert!(
            text.contains(needle),
            "welcome-1 does not mention {needle:?}"
        );
    }
}
