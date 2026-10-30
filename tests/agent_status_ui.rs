//! Popup behaviour for the agent status panel.
//!
//! The panel lives in the binary, so the module is compiled into this suite
//! directly and driven through its injected data plane. Nothing here starts a
//! host CLI, reaches a Herdr server or spends a model call: the collection,
//! description and identity steps are all supplied by the test.
use serde_json::{Value, json};
use std::{
    io::{self, Cursor, Read, Write},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

#[allow(dead_code)]
#[path = "../src/agent_status_ui.rs"]
mod agent_status_ui;
/// The real shared terminal guard, compiled in exactly as the binary does: the
/// popup module reaches it as `super::popup_input`. It no-ops without a TTY, so
/// nothing here has to mock it.
#[allow(dead_code)]
#[path = "../src/popup_input.rs"]
mod popup_input;
use agent_status_ui::{Identity, Screen, Sources, State, frame, painted, popup, sanitize, wrap};

/// Stands in for the binary's helper of the same name. `run` reaches the real
/// Herdr runtime and is covered by the packaged popup tests, not by this suite.
#[allow(dead_code)]
fn with_runtime_options<T>(
    _options: &namedrop::cli::RuntimeOptions,
    _action: impl FnOnce(
        &namedrop::LifecycleRuntime<
            '_,
            namedrop::TuiHost<&'_ namedrop::ProcessHerdr>,
            namedrop::SystemClock,
            namedrop::ProcessIds,
        >,
    ) -> Result<T, String>,
) -> Result<T, String> {
    Err("the popup entry point is exercised through the binary".to_owned())
}

const NOW: u64 = 1_700_000_000_000;
const WIDTH: usize = 72;
const HEIGHT: usize = 24;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn pane(pane_id: &str, label: &str, status: &str) -> Value {
    json!({
        "pane_id": pane_id,
        "terminal_id": format!("t-{pane_id}"),
        "session_id": format!("s-{pane_id}"),
        "label": label,
        "host": "codex",
        "status": status,
        "excerpt": "",
    })
}

/// Shaped exactly as `collect_agent_status` writes a wait: the recipient alias,
/// the request as `@alias · request`, and a real `DeliveryState`.
fn wait(pane_id: &str, alias: &str, request: &str, state: &str, reason: &str) -> Value {
    json!({
        "pane_id": if pane_id.is_empty() { Value::Null } else { json!(pane_id) },
        "request_id": format!("delivery-{alias}"),
        "alias": alias,
        "description": format!("@{alias} · {request}"),
        "state": state,
        "reason": reason.split_whitespace().collect::<Vec<_>>().join(" "),
    })
}

fn snapshot() -> Value {
    json!({
        "origin": {
            "workspace_id": "w44", "pane_id": "p1", "terminal_id": "t-p1",
            "session_id": "s-p1", "host": "codex", "cwd": "/repo",
            "label": "lead", "status": "working",
        },
        "observed_at_ms": NOW - 2_000,
        "panes": [
            pane("p1", "lead", "working"),
            pane("p2", "reviewer", "working"),
            pane("p3", "tests", "idle"),
            pane("p4", "docs", "working"),
        ],
        "waits": [
            wait("p3", "tests", "Run the popup suite before the release", "held",
                 "held; whether it reached the recipient was never established, so it needs a \
                  decision here"),
            wait("p2", "reviewer", "Review the timeout handling", "acknowledged",
                 "receipt recorded; no result yet"),
        ],
        "handoffs_available": true,
        "warnings": [],
        "omitted_panes": 0,
    })
}

fn summary() -> Value {
    json!({
        "origin_description": "Implementing the status panel and checking popup behavior.",
        "peers": [
            {"pane_id": "p2", "description": "Reviewing the timeout handling."},
            {"pane_id": "p4", "description": "Updating shortcut instructions."},
        ],
        "model": "gpt-5.6-luna",
        "summarized_at_ms": NOW - 1_000,
    })
}

fn ready() -> State {
    State {
        snapshot: Some(snapshot()),
        summary: Some(summary()),
        ..State::default()
    }
}

/// Prose is wrapped and columns are padded, so assertions about wording run
/// against the panel with its layout whitespace collapsed.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn render(state: &State) -> String {
    frame(state, WIDTH, HEIGHT, NOW)
}

/// A pane wide enough that a row's prose is not wrapped around its state
/// column, so a whole sentence can be asserted as one string.
fn wide(state: &State) -> String {
    frame(state, 100, HEIGHT, NOW)
}

/// Scripted frames are written back to back, each ending in its footer.
fn last_frame(text: &str) -> &str {
    text.rsplit("Esc Close").nth(1).unwrap_or(text)
}

fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("expected {needle:?} in:\n{text}"))
}

// ---------------------------------------------------------------------------
// Driving the loop
// ---------------------------------------------------------------------------

/// A collection step the test can hold open, so the frames painted while a
/// pass is still running can be observed exactly rather than raced for.
#[derive(Default)]
struct Gate {
    open: AtomicBool,
    entered: AtomicUsize,
    cancelled: AtomicBool,
}

impl Gate {
    fn wait(&self, cancelled: &AtomicBool) -> Result<(), String> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        // Bounded so a popup that forgets to cancel fails the assertion below
        // instead of hanging the suite.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.open.load(Ordering::SeqCst) {
            if cancelled.load(Ordering::SeqCst) {
                self.cancelled.store(true, Ordering::SeqCst);
                return Err("cancelled".to_owned());
            }
            if Instant::now() > deadline {
                return Err("gate was never released and never cancelled".to_owned());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}

fn drive<W: Write>(
    sources: &Sources<'_>,
    input: &str,
    interactive: bool,
    output: W,
) -> (Result<(), String>, W) {
    let mut screen = Screen {
        input: Cursor::new(input.as_bytes().to_vec()),
        output,
        interactive,
        width: WIDTH,
        height: HEIGHT,
    };
    let result = popup(&mut screen, sources);
    (result, screen.output)
}

fn text(sources: &Sources<'_>, input: &str, interactive: bool) -> String {
    let (result, output) = drive(sources, input, interactive, Vec::new());
    result.expect("popup should close cleanly");
    String::from_utf8(output).expect("popup output should be valid UTF-8")
}

/// Blocks until the pass has actually reached the gate. A test about
/// interrupting stalled work has to interrupt stalled work, not race it.
fn await_gate(gate: &Gate) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while gate.entered.load(Ordering::SeqCst) == 0 {
        if Instant::now() > deadline {
            return Err(io::Error::other("the pass never reached the gate"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

/// Serves scripted keys, or fails, only once the pass is stalled at the gate.
struct GatedReader<'a> {
    gate: &'a Gate,
    keys: Cursor<Vec<u8>>,
    armed: bool,
    fail: bool,
}

impl<'a> GatedReader<'a> {
    fn new(gate: &'a Gate, keys: &str) -> Self {
        Self {
            gate,
            keys: Cursor::new(keys.as_bytes().to_vec()),
            armed: false,
            fail: false,
        }
    }

    fn failing(gate: &'a Gate) -> Self {
        Self {
            fail: true,
            ..Self::new(gate, "")
        }
    }
}

impl Read for GatedReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.armed {
            await_gate(self.gate)?;
            self.armed = true;
        }
        if self.fail {
            return Err(io::Error::other("input closed"));
        }
        self.keys.read(buffer)
    }
}

/// Fails the nth write. With a gate, it waits for the pass to stall first, so
/// the failure lands while there is something to cancel.
struct FailingWriter<'a> {
    writes: usize,
    fail_at: usize,
    gate: Option<&'a Gate>,
}

impl Write for FailingWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self.writes >= self.fail_at {
            if let Some(gate) = self.gate {
                await_gate(gate)?;
            }
            return Err(io::Error::other("popup output closed"));
        }
        Ok(buffer.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs the popup with keys that are released only once the pass is stalled.
/// The screen is interactive, so its colour is stripped for the assertions.
fn stalled(gate: &Gate, sources: &Sources<'_>, keys: &str) -> String {
    let mut screen = Screen {
        input: GatedReader::new(gate, keys),
        output: Vec::new(),
        interactive: true,
        width: WIDTH,
        height: HEIGHT,
    };
    popup(&mut screen, sources).expect("popup should close cleanly");
    let text = String::from_utf8(screen.output).expect("popup output should be valid UTF-8");
    text.lines().map(sanitize).collect::<Vec<_>>().join("\n")
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[test]
fn the_panel_answers_its_three_questions_in_order() {
    let text = render(&ready());
    // Herdr titles the popup's border "Agent status"; the header names the
    // agent instead of repeating it.
    assert!(text.starts_with("@lead · Codex"), "{text}");
    assert!(!text.contains("Agent status"), "{text}");
    assert!(
        text.contains("Implementing the status panel and checking popup behavior."),
        "{text}"
    );
    // Waiting-on agents come before the rest of the workspace.
    let now = line_of(&text, "Now");
    let awaiting = line_of(&text, "Awaiting 2 replies");
    let elsewhere = line_of(&text, "Elsewhere in this workspace");
    assert!(now < awaiting && awaiting < elsewhere, "{text}");
    assert!(line_of(&text, "@reviewer") < elsewhere, "{text}");
    assert!(line_of(&text, "@docs") > elsewhere, "{text}");
    // Attention first, then requests already sent. The snapshot orders the
    // waits; the panel names what each block means.
    assert!(
        line_of(&text, "Needs attention here") < line_of(&text, "Sent, no result yet"),
        "{text}"
    );
    assert!(
        line_of(&text, "@tests") < line_of(&text, "@reviewer"),
        "{text}"
    );
    // Nothing claims the agent can or cannot continue; that is not in the facts.
    assert!(!text.contains("can continue working"), "{text}");
    assert!(!text.contains("Waiting on"), "{text}");
    // Identifiers, delivery IDs and raw JSON stay out of the panel.
    for hidden in [
        "w44",
        "pane_id",
        "session_id",
        "delivery-",
        "gpt-5.6-luna",
        "{",
    ] {
        assert!(!text.contains(hidden), "{hidden:?} leaked into:\n{text}");
    }
    assert!(text.contains("R Refresh"), "{text}");
    assert!(text.contains("Esc Close"), "{text}");
}

#[test]
fn a_persona_is_shown_apart_from_what_an_agent_is_doing() {
    let mut state = ready();
    let snapshot = state.snapshot.as_mut().unwrap();
    snapshot["origin"]["persona"] = json!("Leads the release.");
    snapshot["panes"][0]["persona"] = json!("Leads the release.");
    snapshot["panes"][1]["persona"] = json!("Reviews Rust changes.");
    snapshot["panes"][3]["persona"] = json!("Keeps the docs current.");
    let text = wide(&state);

    // The origin's persona sits under Now, beside the description rather than
    // inside it: what the agent is for is not what it is doing.
    let now = line_of(&text, "Now");
    let persona = line_of(&text, "Persona: Leads the release.");
    let awaiting = line_of(&text, "Awaiting 2 replies");
    assert!(now < persona && persona < awaiting, "{text}");
    assert_ne!(
        line_of(&text, "Implementing the status panel"),
        persona,
        "{text}"
    );
    // An agent states its own wherever it appears, under its row and never in
    // the status column: an agent being waited on, and an ordinary peer.
    let reviews = line_of(&text, "Persona: Reviews Rust changes.");
    assert!(reviews > line_of(&text, "@reviewer"), "{text}");
    assert!(
        !text.lines().nth(reviews).unwrap().contains("Received"),
        "{text}"
    );
    assert!(
        line_of(&text, "Persona: Keeps the docs current.") > line_of(&text, "@docs"),
        "{text}"
    );
    // Agents with none say nothing; the origin says Generic outright.
    assert_eq!(text.matches("Persona:").count(), 3, "{text}");
    let mut generic = ready();
    let text = wide(&generic);
    assert!(
        text.contains("Persona: Generic, no specialization."),
        "{text}"
    );
    // Nothing is claimed before the facts arrive.
    generic.snapshot = None;
    generic.working = true;
    assert!(!wide(&generic).contains("Persona"), "{}", wide(&generic));
}

#[test]
fn collection_and_description_times_are_shown_separately_when_they_differ() {
    assert!(render(&ready()).contains("Facts 2s ago · summary 1s ago"));
    let mut together = ready();
    together.summary.as_mut().unwrap()["summarized_at_ms"] = json!(NOW - 2_000);
    assert!(render(&together).contains("Updated 2s ago"));
    // Facts in hand, description still running.
    let pending = State {
        snapshot: Some(snapshot()),
        working: true,
        ..State::default()
    };
    let text = render(&pending);
    assert!(text.contains("Facts 2s ago · summarizing"), "{text}");
    assert!(text.contains("Preparing summary…"), "{text}");
}

#[test]
fn handoff_states_keep_the_distinctions_the_report_makes() {
    // Every DeliveryState the snapshot can put in a wait, with the group it
    // belongs to. Nothing else is a delivery state.
    let states = [
        ("held", "Held", "Needs attention here"),
        ("failed", "Failed", "Needs attention here"),
        ("cancelled", "Cancelled", "Needs attention here"),
        ("settled", "Settled", "Sent, no result yet"),
        ("acknowledged", "Received", "Sent, no result yet"),
        ("submitted", "Sent", "Sent, no result yet"),
        ("queued", "Queued", "Not sent to the recipient yet"),
        ("dispatching", "Sending", "Not sent to the recipient yet"),
    ];
    let mut state = ready();
    for (delivery, shown, heading) in states {
        state.snapshot.as_mut().unwrap()["waits"] = json!([wait(
            "p2",
            "reviewer",
            "Review the timeout handling",
            delivery,
            "a factual reason",
        )]);
        let text = wide(&state);
        assert!(
            text.contains(shown),
            "{delivery} should read {shown}:\n{text}"
        );
        assert!(
            text.contains(heading),
            "{delivery} belongs under {heading}:\n{text}"
        );
        // A request never submitted is not evidence about its recipient.
        if heading.starts_with("Not sent") {
            assert!(!text.contains("Sent, no result yet"), "{text}");
        }
    }
    // A state the panel does not know is labelled as such, never guessed into
    // one of the real groups.
    state.snapshot.as_mut().unwrap()["waits"] = json!([wait(
        "p2",
        "reviewer",
        "Review the timeout handling",
        "teleported",
        "",
    )]);
    let text = wide(&state);
    assert!(
        text.contains("Delivery state this panel does not recognise"),
        "{text}"
    );
    assert!(text.contains("Teleported"), "{text}");

    // The structured reason is kept beside the request.
    assert!(
        wide(&ready()).contains("receipt recorded; no result yet"),
        "{}",
        wide(&ready())
    );
}

#[test]
fn unreadable_handoff_state_is_never_reported_as_nothing_outstanding() {
    let mut state = ready();
    state.snapshot.as_mut().unwrap()["waits"] = json!([]);
    state.snapshot.as_mut().unwrap()["handoffs_available"] = json!(false);
    let text = render(&state);
    assert!(text.contains("outstanding replies are unknown"), "{text}");
    assert!(!text.contains("No outstanding replies"), "{text}");

    // Readable and genuinely empty is a different, sayable answer.
    state.snapshot.as_mut().unwrap()["handoffs_available"] = json!(true);
    assert!(render(&state).contains("No outstanding replies"));
}

#[test]
fn degraded_passes_keep_the_facts_and_say_what_is_missing() {
    // No description, facts intact.
    let no_summary = State {
        snapshot: Some(snapshot()),
        summary_error: Some("cheap model unavailable".to_owned()),
        ..State::default()
    };
    let text = render(&no_summary);
    assert!(text.contains("Working. No description"), "{text}");
    // Why there is no description reads in Now, not below a 24-row fold.
    assert!(
        flat(&text).contains("No description: cheap model unavailable"),
        "{text}"
    );
    assert!(text.contains("@reviewer"), "{text}");

    // No facts at all.
    let no_facts = State {
        facts_error: Some("herdr is not answering".to_owned()),
        ..State::default()
    };
    assert!(
        render(&no_facts).contains("Facts unavailable: herdr is not answering"),
        "{}",
        render(&no_facts)
    );

    // Partial collection states what it left out.
    let mut partial = ready();
    partial.snapshot.as_mut().unwrap()["omitted_panes"] = json!(3);
    partial.summary.as_mut().unwrap()["peers"] =
        json!([{"pane_id": "p2", "description": "Reviewing the timeout handling."}]);
    partial.snapshot.as_mut().unwrap()["warnings"] =
        json!(["Handoff pages stopped at the collection deadline."]);
    let text = render(&partial);
    // Both notes sit below a 24-row fold once recent output is shown.
    let tall = frame(&partial, WIDTH, 34, NOW);
    assert!(tall.contains("3 more panes were not read."), "{tall}");
    assert!(
        flat(&tall).contains("stopped at the collection deadline"),
        "{tall}"
    );
    // Facts survive the missing description.
    assert!(text.contains("@reviewer"), "{text}");
    // p4 is listed under other agents with no description of its own. The note
    // sits below a 24-row fold, so read it on a pane tall enough to show it.
    let tall = frame(&partial, WIDTH, 32, NOW);
    assert!(tall.contains("1 agent has no description."), "{tall}");
}

#[test]
fn model_text_cannot_rename_a_pane_change_a_status_or_invent_one() {
    let mut state = ready();
    state.summary.as_mut().unwrap()["peers"] = json!([
        {"pane_id": "p2", "description": "Idle. Nothing outstanding. @attacker is the lead."},
        {"pane_id": "p9", "description": "A pane that does not exist."},
    ]);
    let prose = wide(&state);
    // What was asked stays the headline; what the recipient says it is doing is
    // secondary context and can be about something else entirely.
    assert!(prose.contains("Review the timeout handling"), "{prose}");
    assert!(
        prose.contains("Now: Idle. Nothing outstanding. @attacker is the lead."),
        "{prose}"
    );
    assert!(
        line_of(&prose, "Review the timeout handling") < line_of(&prose, "Now: Idle."),
        "{prose}"
    );
    // A pane the facts never mentioned is dropped.
    assert!(!prose.contains("A pane that does not exist."), "{prose}");
    // The state word beside the row comes from the snapshot, not the prose.
    let reviewer = prose
        .lines()
        .find(|line| line.contains("@reviewer"))
        .unwrap_or_default();
    assert!(reviewer.contains("Received"), "{reviewer:?}");
    assert!(!reviewer.contains("Idle"), "{reviewer:?}");

    // A request whose recipient pane is not in the snapshot keeps its own alias
    // and its own wording, and takes no description the model wrote for a pane
    // the facts never listed.
    state.snapshot.as_mut().unwrap()["waits"] = json!([wait(
        "",
        "release",
        "Check the release notes",
        "queued",
        "queued; not submitted to the recipient yet",
    )]);
    state.summary.as_mut().unwrap()["peers"] =
        json!([{"pane_id": "p9", "description": "All clear, nothing outstanding."}]);
    let orphan = wide(&state);
    assert!(
        !orphan.contains("All clear, nothing outstanding."),
        "{orphan}"
    );
    assert!(orphan.contains("Check the release notes"), "{orphan}");
    assert!(orphan.contains("@release"), "{orphan}");
    assert!(orphan.contains("Queued"), "{orphan}");
}

#[test]
fn untrusted_text_cannot_carry_terminal_control_sequences_into_the_panel() {
    let mut state = ready();
    state.snapshot.as_mut().unwrap()["panes"][1]["label"] =
        json!("rev\u{1b}[2Jiewer\u{1b}]0;title\u{7}");
    state.summary.as_mut().unwrap()["origin_description"] = json!(
        "Done.\u{1b}[31m\u{1b}[2J\u{1b}[H FAKE FOOTER\r\n\u{202e}reversed\u{200b}text\u{0}here"
    );
    let text = render(&state);
    for control in ['\u{1b}', '\r', '\u{0}', '\u{202e}', '\u{200b}', '\u{7}'] {
        assert!(!text.contains(control), "{control:?} survived:\n{text}");
    }
    assert!(!text.contains("[2J"), "{text}");
    assert!(!text.contains("0;title"), "{text}");
    assert!(text.contains("@reviewer"), "{text}");
    assert!(text.contains("reversed"), "{text}");
    // Every line still fits, so nothing can push the footer off the popup.
    assert_eq!(text.lines().count(), HEIGHT);
    assert!(text.lines().last().unwrap().contains("Esc Close"), "{text}");
}

/// Removes the panel's own colour codes, leaving the exact text beneath.
fn strip_colour(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("\u{1b}[") {
        out.push_str(&rest[..start]);
        let code = &rest[start + 2..];
        let end = code.find('m').expect("a colour code ends in m");
        rest = &code[end + 1..];
    }
    out.push_str(rest);
    out
}

#[test]
fn painting_adds_colour_and_a_gutter_without_changing_a_word() {
    let mut state = ready();
    // Sanitized before any colour is added, so it brings no escape of its own.
    state.summary.as_mut().unwrap()["origin_description"] = json!("Done.\u{1b}[31m\u{1b}[2J FAKE");
    let plain = frame(&state, WIDTH - 2, HEIGHT, NOW);
    let shown = painted(&state, WIDTH, HEIGHT, NOW, true);
    assert_eq!(shown.lines().count(), HEIGHT, "{shown}");
    for (shown, plain) in shown.lines().zip(plain.lines()) {
        let text = strip_colour(shown);
        assert_eq!(text, format!(" {plain}"));
        assert!(UnicodeWidthStr::width(text.as_str()) <= WIDTH, "{text:?}");
    }
    for coloured in [
        "\u{1b}[1mNow\u{1b}[0m",
        "\u{1b}[1;36m@lead\u{1b}[0m",
        "\u{1b}[1;36m@reviewer\u{1b}[0m",
        "\u{1b}[33mHeld\u{1b}[0m",
        "\u{1b}[34mReceived\u{1b}[0m",
        "\u{1b}[32mWorking\u{1b}[0m",
        "\u{1b}[1mEsc\u{1b}[0m",
    ] {
        assert!(shown.contains(coloured), "{coloured:?} missing:\n{shown}");
    }
    assert!(!shown.contains("[31m") && !shown.contains("[2J"), "{shown}");
    // Colour off keeps the gutter and adds nothing else.
    let uncoloured = painted(&state, WIDTH, HEIGHT, NOW, false);
    let gutter: Vec<_> = plain.lines().map(|line| format!(" {line}")).collect();
    assert_eq!(uncoloured, gutter.join("\n"));
}

#[test]
fn sanitize_and_wrap_are_safe_on_multibyte_text() {
    assert_eq!(sanitize("a\u{1b}[1;32mb\tc  d"), "ab c d");
    assert_eq!(sanitize("\u{1b}]8;;http://x\u{7}link"), "link");
    let wrapped = wrap("日本語のテキストがここにあります", 6);
    assert!(wrapped.iter().all(|line| line.chars().count() <= 6));
    assert_eq!(wrapped.concat(), "日本語のテキストがここにあります");
    // A single word longer than the popup is broken, not dropped.
    let long = "x".repeat(200);
    let wrapped = wrap(&long, 10);
    assert_eq!(wrapped.len(), 20);
    assert_eq!(wrapped.concat(), long);
}

#[test]
fn narrow_and_short_popups_keep_every_column_and_the_footer() {
    for (width, height) in [(72, 24), (40, 12), (28, 9), (24, 8), (20, 8)] {
        let columns = width;
        let rows = height;
        let mut reachable = String::new();
        for scroll in 0..40 {
            let state = State { scroll, ..ready() };
            let text = frame(&state, width, height, NOW);
            assert_eq!(text.lines().count(), rows, "{width}x{height}:\n{text}");
            for line in text.lines() {
                assert!(
                    UnicodeWidthStr::width(line) <= columns,
                    "{width}x{height} overflowed: {line:?}"
                );
            }
            // However small the pane gets, the reader keeps the way out and
            // the way to reload.
            let footer = text.lines().last().unwrap();
            assert!(
                footer.contains("Esc") && footer.contains('R'),
                "{width}x{height} lost its controls: {footer:?}"
            );
            reachable.push_str(&text);
        }
        // Nothing is stranded off the bottom: scrolling reaches every section,
        // including the state words that carry the dependency facts.
        for needed in ["Now", "Awaiting 2", "Held", "Working", "Elsewhere", "@docs"] {
            assert!(
                reachable.contains(needed),
                "{needed:?} unreachable at {width}x{height}"
            );
        }
    }
}

#[test]
fn long_lists_scroll_and_clamp_at_both_ends() {
    let mut state = ready();
    let panes: Vec<Value> = (1..40)
        .map(|index| pane(&format!("p{index}"), &format!("agent-{index}"), "working"))
        .collect();
    state.snapshot.as_mut().unwrap()["panes"] = json!(panes);
    let top = render(&state);
    assert!(top.contains("R Refresh · ↑/↓ Scroll 1/"), "{top}");
    assert!(top.contains("Now"), "{top}");

    state.scroll = 6;
    let middle = render(&state);
    assert_ne!(top, middle);
    assert!(middle.contains("Scroll 7/"), "{middle}");

    // Past the end clamps to the last page instead of showing blank rows.
    state.scroll = 10_000;
    let bottom = render(&state);
    assert!(bottom.contains("@agent-39"), "{bottom}");
    let last = bottom.lines().rev().nth(2).unwrap_or_default();
    assert!(!last.trim().is_empty(), "{bottom}");
    state.scroll = 9_999;
    assert_eq!(bottom, render(&state));
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

#[test]
fn the_first_frame_is_painted_before_any_collection_finishes() {
    let gate = Gate::default();
    let collect = |cancelled: &AtomicBool| {
        gate.wait(cancelled)?;
        Ok(snapshot())
    };
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    // Esc is released only once collection is stalled, so the frame under test
    // really is the one painted before any facts exist.
    let text = stalled(&gate, &sources, "\u{1b}");
    assert!(text.contains("Collecting facts…"), "{text}");
    assert!(text.contains("Esc Close"), "{text}");
    // Closing cancelled the pass rather than waiting it out.
    assert!(gate.cancelled.load(Ordering::SeqCst));
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
}

#[test]
fn facts_then_descriptions_replace_the_loading_frame() {
    let collect = |_: &AtomicBool| Ok(snapshot());
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = text(&sources, "", false);
    assert!(text.contains("Collecting facts…"), "{text}");
    assert!(
        text.contains("Implementing the status panel and checking popup behavior."),
        "{text}"
    );
    assert!(text.contains("Awaiting 2 replies"), "{text}");
}

#[test]
fn refresh_recollects_and_a_second_refresh_joins_the_pass_in_flight() {
    let passes = AtomicUsize::new(0);
    let collect = |_: &AtomicBool| {
        passes.fetch_add(1, Ordering::SeqCst);
        Ok(snapshot())
    };
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    text(&sources, "r", false);
    assert_eq!(passes.load(Ordering::SeqCst), 2);

    // R pressed twice while a pass is running pays for one pass, not three.
    let gate = Gate::default();
    let held = |cancelled: &AtomicBool| {
        gate.wait(cancelled)?;
        Ok(snapshot())
    };
    let sources = Sources {
        collect: &held,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = stalled(&gate, &sources, "rr\u{1b}");
    assert!(text.contains("Already refreshing."), "{text}");
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
}

#[test]
fn arrow_keys_move_the_window_while_the_panel_is_open() {
    let mut big = snapshot();
    big["panes"] = json!(
        (1..40)
            .map(|index| pane(&format!("p{index}"), &format!("agent-{index}"), "working"))
            .collect::<Vec<_>>()
    );
    let collect = |_: &AtomicBool| Ok(big.clone());
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = text(&sources, "\u{1b}[B\u{1b}[B\u{1b}[6~", false);
    assert!(text.contains("Scroll 1/"), "{text}");
    assert!(text.contains("Scroll 3/"), "{text}");
    // Page Down moves by a screenful, not a line.
    assert!(text.contains("Scroll 22/"), "{text}");
}

#[test]
fn holding_down_at_the_bottom_does_not_delay_scrolling_back_up() {
    let mut big = snapshot();
    big["panes"] = json!(
        (1..40)
            .map(|index| pane(&format!("p{index}"), &format!("agent-{index}"), "working"))
            .collect::<Vec<_>>()
    );
    let collect = |_: &AtomicBool| Ok(big.clone());
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    // Page to the bottom, keep holding Down, then press Up once.
    let keys = format!(
        "{}{}\u{1b}[A",
        "\u{1b}[6~".repeat(10),
        "\u{1b}[B".repeat(20)
    );
    let text = text(&sources, &keys, false);
    let positions: Vec<(usize, usize)> = text
        .match_indices("Scroll ")
        .filter_map(|(at, _)| {
            let (shown, total) = text[at + 7..].split_whitespace().next()?.split_once('/')?;
            Some((shown.parse().ok()?, total.parse().ok()?))
        })
        .collect();
    let &(last, total) = positions.last().unwrap();
    assert!(positions.contains(&(total, total)), "{text}");
    assert_eq!(last, total - 1, "one Up must leave the bottom: {text}");
}

#[test]
fn a_replaced_origin_discards_the_facts_collected_for_the_old_one() {
    let collect = |_: &AtomicBool| Ok(snapshot());
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| {
        Identity::Changed(
            "This pane started a new agent session. Close this popup and open it again.".to_owned(),
        )
    };
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = text(&sources, "r", false);
    assert!(
        flat(&text).contains("This pane started a new agent session"),
        "{text}"
    );
    let final_frame = last_frame(&text);
    // Nothing collected for the replaced session is still on screen.
    for stale in [
        "@lead",
        "@reviewer",
        "Awaiting 2 replies",
        "Implementing the status panel",
        "Run the popup suite",
        "Held",
    ] {
        assert!(
            !final_frame.contains(stale),
            "{stale:?} still attributed to a replaced pane:\n{text}"
        );
    }
    assert!(
        final_frame.contains("Nothing collected here describes it any more."),
        "{text}"
    );
    // R after a replacement does not start another pass against the wrong pane.
    assert!(
        flat(final_frame).contains("Close this popup and open it again from your pane"),
        "{text}"
    );
}

#[test]
fn a_failing_screen_cancels_the_pass_instead_of_waiting_for_it() {
    // Facts land, so the panel has a second frame to paint; the description
    // step is still running when that paint fails.
    let gate = Gate::default();
    let collect = |_: &AtomicBool| Ok(snapshot());
    let summarize = |_: &Value, cancelled: &AtomicBool| {
        gate.wait(cancelled)?;
        Ok(summary())
    };
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let writer = FailingWriter {
        writes: 0,
        fail_at: 3,
        gate: Some(&gate),
    };
    let (result, _) = drive(&sources, "", true, writer);
    assert!(result.is_err(), "a closed popup output must be reported");
    assert!(
        gate.cancelled.load(Ordering::SeqCst),
        "the pass must be cancelled, not waited out"
    );

    // A keyboard that stops answering cancels the same way.
    let gate = Gate::default();
    let collect = |cancelled: &AtomicBool| {
        gate.wait(cancelled)?;
        Ok(snapshot())
    };
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let mut screen = Screen {
        input: GatedReader::failing(&gate),
        output: Vec::new(),
        interactive: true,
        width: WIDTH,
        height: HEIGHT,
    };
    assert!(popup(&mut screen, &sources).is_err());
    assert!(gate.cancelled.load(Ordering::SeqCst));
}

#[test]
fn a_collection_failure_leaves_the_panel_usable_and_refreshable() {
    let passes = AtomicUsize::new(0);
    let collect = |_: &AtomicBool| {
        if passes.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err("herdr is not answering".to_owned());
        }
        Ok(snapshot())
    };
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = text(&sources, "r", false);
    assert!(
        text.contains("Facts unavailable: herdr is not answering"),
        "{text}"
    );
    // R recovered: the second pass succeeded and is on screen.
    assert!(text.contains("Awaiting 2 replies"), "{text}");
    assert_eq!(passes.load(Ordering::SeqCst), 2);
}

/// Not an assertion: prints the panel states a person needs to look at for the
/// readability review. `cargo test --test agent_status_ui -- --ignored
/// --nocapture capture`
#[test]
#[ignore]
fn capture_panels_for_review() {
    let show = |name: &str, text: String| {
        println!("\n===== {name} =====\n{text}\n===== end =====");
    };
    show("normal 72x24", render(&ready()));
    show("painted 84x24", painted(&ready(), 84, 24, NOW, true));
    let mut loading = State {
        working: true,
        ..State::default()
    };
    show("loading 72x24", render(&loading));
    loading.snapshot = Some(snapshot());
    show("facts only 72x24", render(&loading));
    let mut degraded = ready();
    degraded.snapshot.as_mut().unwrap()["handoffs_available"] = json!(false);
    degraded.snapshot.as_mut().unwrap()["waits"] = json!([]);
    degraded.snapshot.as_mut().unwrap()["omitted_panes"] = json!(4);
    degraded.summary = None;
    degraded.summary_error = Some("cheap model not available for this account".to_owned());
    show("degraded 72x24", render(&degraded));
    show("narrow 40x18", frame(&ready(), 40, 18, NOW));
    let pending_output = State {
        snapshot: Some(with_outputs()),
        working: true,
        ..State::default()
    };
    show("compact loading 72x24", render(&pending_output));
    let mut quiet = snapshot();
    quiet["warnings"] = json!(["Collection stopped at its 2000 ms budget."]);
    show(
        "no output, model timed out 84x24",
        frame(
            &State {
                snapshot: Some(quiet),
                summary_error: Some("the status worker did not answer within 10000 ms".to_owned()),
                ..State::default()
            },
            84,
            24,
            NOW,
        ),
    );
    show(
        "recent output, model timed out 72x24",
        render(&State {
            snapshot: Some(with_outputs()),
            summary_error: Some("the status worker did not answer within 10000 ms".to_owned()),
            ..State::default()
        }),
    );
    show("tiny 20x6", frame(&ready(), 20, 6, NOW));
    let mut groups = ready();
    groups.snapshot.as_mut().unwrap()["waits"] = json!([
        wait(
            "p3",
            "tests",
            "Run the popup suite before the release",
            "failed",
            "delivery failed and needs attention"
        ),
        wait(
            "p2",
            "reviewer",
            "Review the timeout handling",
            "submitted",
            "submitted to the recipient's session; no receipt recorded yet"
        ),
        wait(
            "",
            "release",
            "Draft the release notes",
            "queued",
            "not submitted yet: the recipient has an earlier delivery pending"
        ),
    ]);
    show("all three wait groups 72x24", render(&groups));
    let mut wide_text = ready();
    wide_text.snapshot.as_mut().unwrap()["origin"]["label"] = json!("統括");
    wide_text.snapshot.as_mut().unwrap()["panes"][1]["label"] = json!("レビュー担当者");
    wide_text.snapshot.as_mut().unwrap()["waits"] = json!([wait(
        "p2",
        "レビュー担当者",
        "タイムアウト処理のレビューをお願いします",
        "acknowledged",
        "receipt recorded; no result yet",
    )]);
    wide_text.summary.as_mut().unwrap()["origin_description"] =
        json!("進捗を確認しています 🚀 cafe\u{301} na\u{308}chste Schritte");
    show("double-width text 72x24", render(&wide_text));
    show("double-width text 34x14", frame(&wide_text, 34, 14, NOW));
    let mut many = ready();
    many.snapshot.as_mut().unwrap()["panes"] = json!(
        (1..30)
            .map(|index| pane(&format!("p{index}"), &format!("agent-{index}"), "working"))
            .collect::<Vec<_>>()
    );
    show("overflow 72x24", render(&many));
    many.scroll = 12;
    show("overflow scrolled 72x24", render(&many));
}

#[test]
fn a_pane_smaller_than_the_layout_is_drawn_at_its_real_size() {
    // Rounding a tiny pane up to a comfortable size draws past its edge. Every
    // one of these must come back at exactly the size it was asked for, still
    // carrying the way out.
    for (width, height) in [(20, 5), (12, 3), (8, 2), (30, 1), (1, 1), (4, 6), (200, 40)] {
        let text = frame(&ready(), width, height, NOW);
        assert_eq!(
            text.lines().count(),
            height,
            "{width}x{height} changed its own height:\n{text}"
        );
        for line in text.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= width,
                "{width}x{height} overflowed: {line:?}"
            );
        }
        assert!(
            text.lines().last().unwrap().contains('R'),
            "{width}x{height} lost its controls:\n{text}"
        );
    }
    // Wider than the readable maximum stops growing rather than stretching.
    let very_wide = frame(&ready(), 400, 12, NOW);
    assert!(
        very_wide
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 200)
    );
}

#[test]
fn an_unconfirmed_identity_keeps_the_facts_and_withholds_only_the_description() {
    let collect = |_: &AtomicBool| Ok(snapshot());
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |_: &AtomicBool| {
        Identity::Unconfirmed("Could not confirm the invoking pane: herdr timed out".to_owned())
    };
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = text(&sources, "", false);
    // Not knowing is not knowing it changed: no replacement is claimed and the
    // facts, which were collected against a validated identity, stay.
    assert!(!text.contains("was replaced"), "{text}");
    assert!(!text.contains("started a new agent session"), "{text}");
    assert!(text.contains("Awaiting 2 replies"), "{text}");
    assert!(text.contains("@reviewer"), "{text}");
    // The description is the part nothing has vouched for, so it is withheld.
    assert!(
        !text.contains("Implementing the status panel and checking popup behavior."),
        "{text}"
    );
    assert!(
        flat(&text).contains("Could not confirm the invoking pane: herdr timed out"),
        "{text}"
    );
    assert!(
        flat(&text).contains("description is withheld until the pane can be confirmed"),
        "{text}"
    );
}

#[test]
fn closing_cancels_an_identity_recheck_that_is_still_running() {
    // The recheck is a Herdr round trip that runs after the description. Esc
    // must end it, not wait it out.
    let gate = Gate::default();
    let collect = |_: &AtomicBool| Ok(snapshot());
    let summarize = |_: &Value, _: &AtomicBool| Ok(summary());
    let recheck = |cancelled: &AtomicBool| match gate.wait(cancelled) {
        Ok(()) => Identity::Same,
        Err(error) => Identity::Unconfirmed(error),
    };
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = stalled(&gate, &sources, "\u{1b}");
    assert!(text.contains("Esc Close"), "{text}");
    assert!(
        gate.cancelled.load(Ordering::SeqCst),
        "Esc must cancel a recheck in flight"
    );

    // A screen that stops accepting output cancels a stalled recheck too.
    let gate = Gate::default();
    let recheck = |cancelled: &AtomicBool| match gate.wait(cancelled) {
        Ok(()) => Identity::Same,
        Err(error) => Identity::Unconfirmed(error),
    };
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let writer = FailingWriter {
        writes: 0,
        fail_at: 3,
        gate: Some(&gate),
    };
    let (result, _) = drive(&sources, "", true, writer);
    assert!(result.is_err());
    assert!(gate.cancelled.load(Ordering::SeqCst));
}

#[test]
fn a_cancelled_pass_never_spends_a_herdr_call_confirming_it() {
    // Nothing will be displayed, so the recheck must be skipped entirely rather
    // than run and discarded.
    let gate = Gate::default();
    let rechecks = AtomicUsize::new(0);
    let collect = |_: &AtomicBool| Ok(snapshot());
    let summarize = |_: &Value, cancelled: &AtomicBool| {
        gate.wait(cancelled)?;
        Ok(summary())
    };
    let recheck = |_: &AtomicBool| {
        rechecks.fetch_add(1, Ordering::SeqCst);
        Identity::Same
    };
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    stalled(&gate, &sources, "\u{1b}");
    assert!(gate.cancelled.load(Ordering::SeqCst));
    assert_eq!(rechecks.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// The real identity recheck
//
// Everything above injects `recheck`. These exercise the function the popup
// actually wires in, including the executor adapter that bounds its reads.
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Call {
    Unbounded(String),
    Bounded(String, Duration),
}

#[derive(Default)]
struct FakeHerdr {
    calls: std::sync::Mutex<Vec<Call>>,
    terminal: String,
    session: Option<String>,
    closed: bool,
}

impl FakeHerdr {
    fn new(terminal: &str, session: Option<&str>) -> Self {
        Self {
            terminal: terminal.to_owned(),
            session: session.map(str::to_owned),
            ..Self::default()
        }
    }

    fn answer(&self, args: &[&str]) -> Result<namedrop::HerdrCommandOutput, String> {
        let stdout = match (args.first(), args.get(1)) {
            (Some(&"worktree"), Some(&"list")) => {
                r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"w44"}]}}"#
                    .to_owned()
            }
            (Some(&"pane"), Some(&"list")) if self.closed => {
                r#"{"result":{"panes":[]}}"#.to_owned()
            }
            (Some(&"pane"), Some(&"list")) => {
                let session = self
                    .session
                    .as_ref()
                    .map(|value| format!(r#""agent_session":{{"value":"{value}"}},"#))
                    .unwrap_or_default();
                format!(
                    r#"{{"result":{{"panes":[{{"pane_id":"p1","terminal_id":"{}","tab_id":"tab1","workspace_id":"w44","agent":"codex",{session}"agent_status":"working"}}]}}}}"#,
                    self.terminal
                )
            }
            _ => "{}".to_owned(),
        };
        Ok(namedrop::HerdrCommandOutput { stdout })
    }

    fn bounded_calls(&self) -> Vec<Duration> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|call| match call {
                Call::Bounded(_, timeout) => Some(*timeout),
                Call::Unbounded(_) => None,
            })
            .collect()
    }

    fn unbounded_calls(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| matches!(call, Call::Unbounded(_)))
            .count()
    }
}

impl namedrop::HerdrCommandExecutor for FakeHerdr {
    fn execute(&self, args: &[&str]) -> Result<namedrop::HerdrCommandOutput, String> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Unbounded(args.join(" ")));
        self.answer(args)
    }

    fn execute_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        _max_output_bytes: usize,
    ) -> Result<namedrop::HerdrCommandOutput, String> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Bounded(args.join(" "), timeout));
        if cancelled.load(Ordering::SeqCst) {
            return Err("Herdr read cancelled".to_owned());
        }
        self.answer(args)
    }
}

struct FakeClock;
impl namedrop::Clock for FakeClock {
    fn now_unix_ms(&self) -> u64 {
        NOW
    }
}

struct FakeIds;
impl namedrop::IdGenerator for FakeIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-1")
    }
}

fn check(herdr: &FakeHerdr, session: Option<&str>, cancelled: &AtomicBool) -> Identity {
    let cwd = std::path::Path::new("/repo");
    let runtime = namedrop::LifecycleRuntime {
        agent_config: namedrop::AgentConfigDirectories::default(),
        state_directory: cwd,
        home_directory: None,
        cwd,
        workspace_id: Some("w44"),
        current_pane_id: Some("p1"),
        session_socket_id: "socket",
        host: &namedrop::TuiHost::new(herdr),
        clock: &FakeClock,
        ids: &FakeIds,
    };
    agent_status_ui::latest_identity(&runtime, "w44", "p1", "t1", session, cancelled)
}

#[test]
fn the_identity_recheck_reads_through_bounded_calls_only() {
    let herdr = FakeHerdr::new("t1", Some("s1"));
    let cancelled = AtomicBool::new(false);
    assert_eq!(check(&herdr, Some("s1"), &cancelled), Identity::Same);
    // Discovery makes two ordinary reads on Herdr's ten-second default. Both
    // must have gone through the bounded path instead, sharing one short
    // budget, or Esc would wait on them.
    assert_eq!(herdr.unbounded_calls(), 0, "{:?}", herdr.calls.lock());
    let timeouts = herdr.bounded_calls();
    assert_eq!(timeouts.len(), 2, "{timeouts:?}");
    assert!(
        timeouts.iter().all(|t| *t <= Duration::from_millis(1_500)),
        "{timeouts:?}"
    );
    // Shared, not per call: the second read gets what the first left.
    assert!(timeouts[1] < timeouts[0], "{timeouts:?}");
}

#[test]
fn the_identity_recheck_stops_the_moment_the_pass_is_cancelled() {
    let herdr = FakeHerdr::new("t1", Some("s1"));
    let cancelled = AtomicBool::new(true);
    let verdict = check(&herdr, Some("s1"), &cancelled);
    // A cancelled read is missing evidence, never a claim the pane changed.
    assert!(matches!(verdict, Identity::Unconfirmed(_)), "{verdict:?}");
    assert_eq!(herdr.bounded_calls().len(), 1, "it must stop, not retry");
}

#[test]
fn the_identity_recheck_separates_a_real_change_from_a_failed_check() {
    let cancelled = AtomicBool::new(false);
    // A replaced physical terminal.
    let swapped = FakeHerdr::new("t9", Some("s1"));
    assert!(matches!(
        check(&swapped, Some("s1"), &cancelled),
        Identity::Changed(_)
    ));
    // A new session in the same terminal.
    let resumed = FakeHerdr::new("t1", Some("s2"));
    assert!(matches!(
        check(&resumed, Some("s1"), &cancelled),
        Identity::Changed(_)
    ));
    // A pane that had no agent and now has one: still a different subject.
    let started = FakeHerdr::new("t1", Some("s1"));
    assert!(matches!(
        check(&started, None, &cancelled),
        Identity::Changed(_)
    ));
    // A pane that had an agent and now has none.
    let ended = FakeHerdr::new("t1", None);
    assert!(matches!(
        check(&ended, Some("s1"), &cancelled),
        Identity::Changed(_)
    ));
    // Neither ever had one: unchanged.
    let never = FakeHerdr::new("t1", None);
    assert_eq!(check(&never, None, &cancelled), Identity::Same);
    // A closed pane is a confirmed change, not a failed check.
    let closed = FakeHerdr {
        closed: true,
        ..FakeHerdr::new("t1", Some("s1"))
    };
    assert!(matches!(
        check(&closed, Some("s1"), &cancelled),
        Identity::Changed(_)
    ));
}

#[test]
fn double_width_and_combining_text_is_measured_in_display_cells() {
    // A CJK glyph and a simple emoji take two columns; a combining accent takes
    // none. Counting characters would let any of them push a row past the edge
    // of the pane or, at a one-column pane, stall the wrap forever.
    let mut state = ready();
    state.snapshot.as_mut().unwrap()["origin"]["label"] = json!("統括");
    state.snapshot.as_mut().unwrap()["panes"][1]["label"] = json!("レビュー担当者");
    state.snapshot.as_mut().unwrap()["panes"][3]["label"] = json!("cafe\u{301}-de\u{0301}v");
    state.snapshot.as_mut().unwrap()["waits"] = json!([
        wait(
            "p2",
            "レビュー担当者",
            "タイムアウト処理のレビューをお願いします",
            "acknowledged",
            "receipt recorded; no result yet",
        ),
        wait(
            "p3",
            "tests",
            "Ship it 🚀🚀🚀 before the release 🎉",
            "queued",
            "queued; not submitted to the recipient yet",
        ),
        // An unrecognised delivery state is snapshot text, so the right-hand
        // column can hold wide glyphs too.
        wait("p4", "docs", "Update the shortcut page", "配達済み", ""),
    ]);
    state.summary.as_mut().unwrap()["origin_description"] =
        json!("進捗を確認しています 🚀 cafe\u{301} na\u{0308}chste Schritte");
    state.summary.as_mut().unwrap()["peers"] =
        json!([{"pane_id": "p2", "description": "レビュー中です 🎉"}]);

    for (width, height) in [(72, 24), (40, 12), (28, 9), (20, 6), (9, 4), (2, 3), (1, 1)] {
        for scroll in [0, 3, 9, 40] {
            let text = frame(
                &State {
                    scroll,
                    ..State {
                        snapshot: state.snapshot.clone(),
                        summary: state.summary.clone(),
                        ..State::default()
                    }
                },
                width,
                height,
                NOW,
            );
            assert_eq!(text.lines().count(), height, "{width}x{height}:\n{text}");
            for line in text.lines() {
                assert!(
                    UnicodeWidthStr::width(line) <= width,
                    "{width}x{height} overflowed to {} cells: {line:?}",
                    UnicodeWidthStr::width(line)
                );
            }
        }
    }

    // Wide text still reaches the reader rather than being dropped.
    let roomy = frame(&state, 100, 30, NOW);
    assert!(
        roomy.contains("タイムアウト処理のレビューをお願いします"),
        "{roomy}"
    );
    assert!(roomy.contains("進捗を確認しています 🚀"), "{roomy}");
    assert!(
        roomy.contains("Ship it 🚀🚀🚀 before the release 🎉"),
        "{roomy}"
    );
    assert!(roomy.contains("cafe\u{301}-de\u{0301}v"), "{roomy}");

    // A name wider than its column is clipped to the column, in cells, with the
    // elision marked; the columns after it still line up.
    let row = roomy
        .lines()
        .find(|line| line.contains("タイムアウト処理"))
        .unwrap();
    assert!(row.starts_with("@レビュー"), "{row:?}");
    assert!(row.contains('…'), "{row:?}");
    assert!(row.ends_with("Received"), "{row:?}");
    assert_eq!(UnicodeWidthStr::width(row), 100, "{row:?}");
    // The right-hand column is right-aligned by cells as well, so a wide state
    // word ends flush with the edge instead of two columns past it.
    let unknown = roomy
        .lines()
        .find(|line| line.contains("Update the shortcut page"))
        .unwrap();
    assert!(unknown.ends_with("配達済み"), "{unknown:?}");
    assert_eq!(UnicodeWidthStr::width(unknown), 100, "{unknown:?}");
    assert!(
        roomy.contains("Delivery state this panel does not recognise"),
        "{roomy}"
    );

    // The same row rendered from an all-ASCII fixture is the same width, so the
    // alignment does not drift with the script.
    let ascii = frame(&ready(), 100, 30, NOW);
    let ascii_row = ascii
        .lines()
        .find(|line| line.contains("Review the timeout handling"))
        .unwrap();
    assert_eq!(UnicodeWidthStr::width(ascii_row), 100, "{ascii_row:?}");
}

#[test]
fn wrapping_makes_progress_when_a_glyph_is_wider_than_the_column() {
    // One column cannot hold a two-cell glyph. It must be elided deliberately,
    // never emitted whole (which overflows) and never skipped (which loops).
    let wrapped = wrap("日本語", 1);
    assert_eq!(wrapped.len(), 3, "{wrapped:?}");
    assert!(
        wrapped
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 1),
        "{wrapped:?}"
    );
    // Two columns fit exactly one glyph each, with nothing lost.
    let wrapped = wrap("日本語", 2);
    assert_eq!(wrapped, ["日", "本", "語"]);
    // Mixed content wraps on cells, and a combining accent costs nothing.
    let wrapped = wrap("ab cafe\u{301} 日本 xy", 4);
    assert!(
        wrapped
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 4),
        "{wrapped:?}"
    );
    assert!(
        wrapped.iter().any(|line| line.contains("cafe\u{301}")),
        "{wrapped:?}"
    );
    // Clipping prefers a real glyph over a marker when only one cell is left.
    assert_eq!(sanitize("  a\tb  "), "a b");
    assert_eq!(wrap("", 8), [""]);
}

// ---------------------------------------------------------------------------
// Recent output when the description is unavailable
// ---------------------------------------------------------------------------

/// Puts real terminal tail text on a pane fixture.
fn output(mut pane: Value, text: &str) -> Value {
    pane["excerpt"] = json!(text);
    pane
}

fn with_outputs() -> Value {
    let mut snapshot = snapshot();
    snapshot["panes"] = json!([
        output(
            pane("p1", "lead", "working"),
            "cargo test --test agent_status_ui\nrunning 26 tests"
        ),
        output(
            pane("p2", "reviewer", "working"),
            "reading src/agent_status_ui.rs"
        ),
        output(pane("p3", "tests", "idle"), "waiting for the popup suite"),
        output(pane("p4", "docs", "working"), "editing docs/operations.md"),
    ]);
    snapshot
}

#[test]
fn pending_summaries_keep_facts_without_dumping_terminal_fragments() {
    let pending = State {
        snapshot: Some(with_outputs()),
        working: true,
        ..State::default()
    };
    let text = wide(&pending);
    assert!(text.contains("Preparing summary…"), "{text}");
    for fragment in [
        "Recent output",
        "running 26 tests",
        "editing docs/operations.md",
        "reading src/agent_status_ui.rs",
        "waiting for the popup suite",
    ] {
        assert!(!text.contains(fragment), "{text}");
    }
    for fact in [
        "@docs",
        "Working",
        "Review the timeout handling",
        "Awaiting 2 replies",
    ] {
        assert!(text.contains(fact), "{text}");
    }
    let complete = wide(&State {
        snapshot: pending.snapshot.clone(),
        summary: Some(summary()),
        ..State::default()
    });
    assert!(
        complete.contains("Updating shortcut instructions."),
        "{complete}"
    );
    assert!(!complete.contains("Preparing summary…"), "{complete}");
}

#[test]
fn recent_output_survives_a_worker_timeout_or_failure() {
    for error in [
        "the status worker did not answer within 10000 ms",
        "the worker returned malformed JSON",
    ] {
        let failed = State {
            snapshot: Some(with_outputs()),
            summary_error: Some(error.to_owned()),
            ..State::default()
        };
        let text = wide(&failed);
        assert!(text.contains("running 26 tests"), "{text}");
        assert!(text.contains("editing docs/operations.md"), "{text}");
        assert!(flat(&text).contains(error), "{text}");
        // No invented prose stands in for the missing description.
        assert!(!text.contains("Writing the description"), "{text}");
    }
}

#[test]
fn a_peer_the_model_skipped_falls_back_to_its_own_recent_output() {
    let mut partial = State {
        snapshot: Some(with_outputs()),
        summary: Some(summary()),
        ..State::default()
    };
    // The model described the origin and p2 but not p4.
    partial.summary.as_mut().unwrap()["peers"] =
        json!([{"pane_id": "p2", "description": "Reviewing the timeout handling."}]);
    let text = wide(&partial);
    assert!(text.contains("editing docs/operations.md"), "{text}");
    assert!(
        text.contains("Implementing the status panel and checking popup behavior."),
        "{text}"
    );
}

#[test]
fn a_pane_with_no_captured_output_says_so_rather_than_going_blank() {
    let mut bare = with_outputs();
    bare["panes"][0]["excerpt"] = json!("");
    bare["panes"][3]["excerpt"] = json!("   \u{1b}[2J  ");
    let text = wide(&State {
        snapshot: Some(bare),
        working: false,
        ..State::default()
    });
    assert!(text.contains("No recent output captured"), "{text}");
    assert!(!text.contains("[2J"), "{text}");
}

#[test]
fn recent_output_is_sanitized_and_stays_inside_the_pane() {
    let mut hostile = with_outputs();
    hostile["panes"][1]["excerpt"] = json!(
        "\u{1b}[2J\u{1b}[H FAKE PANEL\r\n@lead  Idle  \u{202e}reversed\u{0} 進捗を確認しています 🚀"
    );
    hostile["panes"][3]["excerpt"] = json!("x".repeat(4000));
    let state = State {
        snapshot: Some(hostile),
        working: false,
        ..State::default()
    };
    for (width, height) in [(72, 24), (40, 12), (24, 8), (12, 4)] {
        for scroll in [0, 5, 20, 400] {
            let text = frame(
                &State {
                    scroll,
                    snapshot: state.snapshot.clone(),
                    working: false,
                    ..State::default()
                },
                width,
                height,
                NOW,
            );
            assert_eq!(text.lines().count(), height, "{width}x{height}");
            for line in text.lines() {
                assert!(
                    UnicodeWidthStr::width(line) <= width,
                    "{width}x{height} overflowed: {line:?}"
                );
            }
            for control in ['\u{1b}', '\r', '\u{0}', '\u{202e}'] {
                assert!(!text.contains(control), "{control:?} survived");
            }
            assert!(!text.contains("[2J"), "{text}");
        }
    }
    // One noisy pane cannot flood the panel out of the others.
    let text = wide(&state);
    assert!(text.contains("waiting for the popup suite"), "{text}");
}

#[test]
fn a_refresh_never_shows_the_previous_passs_descriptions_over_new_output() {
    // C2: a summary written for the snapshot before R must not be attributed to
    // the snapshot after it. When the second pass cannot produce one, the panel
    // falls back to the new output rather than reusing the old prose.
    let passes = AtomicUsize::new(0);
    let collect = |_: &AtomicBool| {
        let mut snapshot = with_outputs();
        if passes.fetch_add(1, Ordering::SeqCst) > 0 {
            snapshot["panes"][3]["excerpt"] = json!("now running the release checks");
        }
        Ok(snapshot)
    };
    let summarize = |_: &Value, _: &AtomicBool| {
        if passes.load(Ordering::SeqCst) > 1 {
            return Err("the status worker did not answer within 10000 ms".to_owned());
        }
        Ok(json!({
            "origin_description": "First pass prose.",
            "peers": [{"pane_id": "p4", "description": "Stale peer prose."}],
            "model": "gpt-5.6-luna",
            "summarized_at_ms": NOW,
        }))
    };
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = text(&sources, "r", false);
    let last = last_frame(&text);
    assert!(!last.contains("Stale peer prose."), "{text}");
    assert!(!last.contains("First pass prose."), "{text}");
    assert!(last.contains("now running the release checks"), "{text}");
}

#[test]
fn a_stalled_worker_keeps_facts_and_still_takes_esc() {
    let gate = Gate::default();
    let collect = |_: &AtomicBool| Ok(with_outputs());
    let summarize = |_: &Value, cancelled: &AtomicBool| {
        gate.wait(cancelled)?;
        Ok(summary())
    };
    let recheck = |_: &AtomicBool| Identity::Same;
    let sources = Sources {
        collect: &collect,
        summarize: &summarize,
        recheck: &recheck,
    };
    let text = stalled(&gate, &sources, "x\u{1b}");
    assert!(text.contains("Preparing summary…"), "{text}");
    assert!(!text.contains("Recent output"), "{text}");
    assert!(text.contains("@docs"), "{text}");
    assert!(
        gate.cancelled.load(Ordering::SeqCst),
        "Esc must still end a stalled pass"
    );
}

#[test]
fn recent_output_shows_the_newest_lines_not_the_oldest() {
    // An excerpt is up to 120 lines of transcript. Showing its first 160 cells
    // would label the oldest scrollback "Recent output".
    let mut long = with_outputs();
    let mut transcript = vec!["OLD_SENTINEL starting the run".to_owned()];
    for index in 0..120 {
        transcript.push(format!("step {index} of a long and noisy build log line"));
        transcript.push(String::new());
    }
    transcript.push("NEWEST_SENTINEL finished".to_owned());
    long["panes"][0]["excerpt"] = json!(transcript.join("\n"));
    long["panes"][3]["excerpt"] = json!(transcript.join("\n"));
    let text = wide(&State {
        snapshot: Some(long),
        working: false,
        ..State::default()
    });
    assert!(text.contains("NEWEST_SENTINEL finished"), "{text}");
    assert!(!text.contains("OLD_SENTINEL"), "{text}");
    // The cut is marked, and the row still fits.
    assert!(text.contains('…'), "{text}");
    for line in text.lines() {
        assert!(UnicodeWidthStr::width(line) <= 100, "{line:?}");
    }
}

#[test]
fn panes_with_no_output_do_not_spend_rows_saying_so() {
    // Filler rows pushed the real failure notes below the fold at 84x24.
    let mut quiet = snapshot();
    quiet["warnings"] = json!(["Collection stopped at its 2000 ms budget."]);
    let text = frame(
        &State {
            snapshot: Some(quiet),
            summary_error: Some("the status worker did not answer within 10000 ms".to_owned()),
            ..State::default()
        },
        84,
        24,
        NOW,
    );
    assert_eq!(
        text.matches("No recent output captured.").count(),
        1,
        "{text}"
    );
    assert!(
        flat(&text).contains("Collection stopped at its 2000 ms budget."),
        "{text}"
    );
    assert!(
        flat(&text).contains("No description: the status worker did not answer"),
        "{text}"
    );
}

#[test]
fn host_prompt_and_footer_chrome_never_stands_in_for_task_output() {
    // Both hosts paint a prompt box, a status line and a footer below their
    // task output, so the chrome is literally the newest text in a tail. The
    // installed forced-timeout run showed exactly that: a footer where the work
    // should have been. These are the shapes the hosts actually print.
    let claude_tail = [
        "TASK_SENTINEL test result: ok. 35 passed; 0 failed",
        "",
        "✻ Crunched for 1m 59s · done 9:15 AM",
        "✔ Update installed · Restart to update",
        "╭────────────────────────────────────────────╮",
        "│ ❯                                          │",
        "╰────────────────────────────────────────────╯",
        "  worktree ✓ git main · Opus 4.5 · 32% ctx",
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent",
    ]
    .join("\n");
    let codex_tail = [
        "CODEX_SENTINEL applied patch to src/store/coordination.rs",
        "• Working (20m 18s • esc to interrupt)",
        "⠋",
        "› Ask Codex to do anything",
        "⠙",
        "gpt-6-astra high · ~/code/namedrop · Context 50% left",
    ]
    .join("\n");

    let hosts = |claude: &str, codex: &str| {
        let mut snapshot = snapshot();
        snapshot["panes"] = json!([
            {"pane_id":"p1","terminal_id":"t-p1","session_id":"s-p1","label":"lead",
             "host":"claude","status":"working","excerpt":claude},
            {"pane_id":"p4","terminal_id":"t-p4","session_id":"s-p4","label":"docs",
             "host":"codex","status":"working","excerpt":codex},
        ]);
        snapshot["origin"]["host"] = json!("claude");
        snapshot["waits"] = json!([]);
        snapshot
    };
    let panel = |snapshot: Value| {
        wide(&State {
            snapshot: Some(snapshot),
            working: false,
            ..State::default()
        })
    };

    let text = panel(hosts(&claude_tail, &codex_tail));
    // The work survives.
    assert!(
        flat(&text).contains("TASK_SENTINEL test result: ok."),
        "{text}"
    );
    assert!(
        flat(&text).contains("CODEX_SENTINEL applied patch to src/store/coordination.rs"),
        "{text}"
    );
    // The chrome does not.
    for chrome in [
        "❯",
        "›",
        "Crunched for",
        "Update installed",
        "ctx",
        "auto mode",
        "shift+tab",
        "Working (20m",
        "esc to interrupt",
        "Ask Codex to do anything",
        "Context 50% left",
        "gpt-6-astra",
        "╭",
        "╰",
        "─",
        "⠋",
        "⠙",
    ] {
        assert!(
            !text.contains(chrome),
            "{chrome:?} reached the panel:\n{text}"
        );
    }

    // Narrow signatures only: real output that merely carries one of those
    // words or glyphs is task text and must survive, on a known host. Rendered
    // wide so a row's prose is not wrapped around its state column.
    let kept_panel = |snapshot: Value| {
        frame(
            &State {
                snapshot: Some(snapshot),
                working: false,
                ..State::default()
            },
            200,
            40,
            NOW,
        )
    };
    let text = kept_panel(hosts(
        "the ctx budget is fine, and ⠿ came out of the build log\nKEPT_CLAUDE done",
        "Working through the queue now\nContext switching is expensive\nKEPT_CODEX done",
    ));
    for kept in [
        "the ctx budget is fine",
        "came out of the build log",
        "KEPT_CLAUDE done",
        "Working through the queue now",
        "Context switching is expensive",
        "KEPT_CODEX done",
    ] {
        assert!(flat(&text).contains(kept), "{kept:?} was eaten:\n{text}");
    }

    // An unknown host keeps every line, chrome-shaped or not: a rule written
    // for one host must not eat another's real output.
    let mut unknown = hosts(&claude_tail, &codex_tail);
    unknown["panes"][1]["host"] = json!("someshell");
    unknown["panes"][1]["excerpt"] = json!("› my own prompt marker\n• Working (2m 04s • esc)");
    let text = panel(unknown);
    assert!(flat(&text).contains("my own prompt marker"), "{text}");
}
