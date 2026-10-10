//! The agent status popup.
//!
//! The popup paints before any data exists, then fills in facts and, last, one
//! short description pass. Facts always come from the snapshot; the model only
//! supplies prose, so nothing it returns can change an identity, a status or a
//! delivery state. Pane transcripts reach this module as untrusted text and are
//! stripped of terminal control sequences before they are ever printed.
use super::popup_input::InputMode;
use namedrop::{
    HerdrCommandExecutor, HerdrCommandOutput, LifecycleRuntime, cli::RuntimeOptions,
    discover_worktree_for_workspace,
};
use serde_json::Value;
use std::{
    env,
    io::{self, IsTerminal, Read, Write},
    process::{Command, ExitCode, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The dimensions `naming_ui::launch` asks Herdr for. Real size is read from
/// the popup's own terminal so a resized or clamped pane still fits.
const DEFAULT_WIDTH: usize = 72;
const DEFAULT_HEIGHT: usize = 24;
/// Above these heights the panel can afford a blank line for breathing room.
/// Below them it spends every row on content; a small pane is rendered at its
/// real size, never padded out past the pane it has to fit in.
const SPACED_HEIGHT: usize = 8;
const ROOMY_HEIGHT: usize = 10;
/// Wider than this and lines get hard to read, so they stop growing.
const MAX_WIDTH: usize = 200;
/// Below this the two-column rows stop fitting and every row becomes stacked.
const NARROW_WIDTH: usize = 48;
/// How much of a pane's terminal tail the panel will show. Enough to recognise
/// what a pane is doing, bounded so one noisy pane cannot crowd out the rest.
const MAX_EXCERPT_CELLS: usize = 160;
/// Longest a non-interactive run defers reading its scripted input. This does
/// not apply to the interactive popup, which polls input while the AI runs.
const BATCH_WAIT: Duration = Duration::from_secs(12);
/// How often a running popup re-measures its own pane. Slow on purpose: each
/// measurement spawns stty, and the loop itself ticks ten times a second.
const SIZE_INTERVAL: Duration = Duration::from_secs(1);
/// One short budget shared by every Herdr call the identity recheck makes. It
/// runs after the description, with a person waiting on the result, and Esc
/// must not have to wait out a default Herdr timeout.
const IDENTITY_BUDGET: Duration = Duration::from_millis(1_500);
const MAX_IDENTITY_OUTPUT_BYTES: usize = 512 * 1_024;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(super) fn run(options: &RuntimeOptions) -> ExitCode {
    match run_popup(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Keep actionable errors visible instead of closing immediately.
            eprintln!("\nNamedrop: {}", error.escape_debug());
            if io::stdin().is_terminal() {
                eprint!("\nEnter or Esc closes: ");
                let _ = io::stderr().flush();
                let mut byte = [0];
                let _ = io::stdin().read(&mut byte);
            }
            ExitCode::from(65)
        }
    }
}

fn run_popup(options: &RuntimeOptions) -> Result<(), String> {
    let workspace = env::var("NAMEDROP_TARGET_WORKSPACE")
        .map_err(|_| "This popup is missing its invoking space")?;
    let pane =
        env::var("NAMEDROP_TARGET_PANE").map_err(|_| "This popup is missing its invoking pane")?;
    let terminal = env::var("NAMEDROP_TARGET_TERMINAL")
        .map_err(|_| "This popup is missing its invoking terminal")?;
    // Empty when the pane held no agent session as the popup opened.
    let session = env::var("NAMEDROP_TARGET_SESSION")
        .ok()
        .filter(|value| !value.is_empty());
    let (width, height) = popup_size();
    let interactive = io::stdin().is_terminal();
    // Entered before the first frame and restored on every return, including
    // the error path above. Polling, not blocking: a slow pass must never hold
    // the keyboard.
    let _input_mode = InputMode::polling()?;
    super::with_runtime_options(options, |runtime| {
        let targeted = LifecycleRuntime {
            workspace_id: Some(&workspace),
            current_pane_id: Some(&pane),
            ..*runtime
        };
        let collect = |cancelled: &AtomicBool| {
            namedrop::collect_agent_status(&targeted, &terminal, session.as_deref(), cancelled)
        };
        let summarize = |snapshot: &Value, cancelled: &AtomicBool| {
            namedrop::summarize_agent_status(snapshot, cancelled)
        };
        let recheck = |cancelled: &AtomicBool| {
            latest_identity(
                &targeted,
                &workspace,
                &pane,
                &terminal,
                session.as_deref(),
                cancelled,
            )
        };
        let sources = Sources {
            collect: &collect,
            summarize: &summarize,
            recheck: &recheck,
        };
        let mut screen = Screen {
            input: io::stdin(),
            output: io::stdout(),
            interactive,
            width,
            height,
        };
        let result = popup(&mut screen, &sources);
        if interactive {
            // Painting hid the cursor; an error prompt after this needs it.
            let _ = write!(screen.output, "\x1b[?25h");
            let _ = screen.output.flush();
        }
        result
    })
}

/// What rereading the invoking pane established.
#[derive(Debug, PartialEq)]
pub(crate) enum Identity {
    /// Same physical terminal, same agent session.
    Same,
    /// Confirmed to be a different pane or a different session.
    Changed(String),
    /// The check did not complete. This is missing evidence, not proof of a
    /// replacement, so it must never be reported as one.
    Unconfirmed(String),
}

/// Forwards `discover_worktree_for_workspace`'s ordinary reads through bounded
/// ones. That call makes up to two Herdr requests on the default ten-second
/// timeout; here they share one short deadline and the pass's cancellation
/// flag, so closing the popup ends the recheck instead of waiting it out.
struct BoundedReads<'a, E> {
    inner: &'a E,
    deadline: Instant,
    cancelled: &'a AtomicBool,
}

impl<E: HerdrCommandExecutor> HerdrCommandExecutor for BoundedReads<'_, E> {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("confirming the invoking pane ran out of time".to_owned());
        }
        self.inner
            .execute_bounded(args, remaining, self.cancelled, MAX_IDENTITY_OUTPUT_BYTES)
    }
}

/// Rereads the pane the popup was opened for. A different physical terminal or
/// a different agent session means the popup is now looking at someone else's
/// work, so results collected for the original identity must be discarded.
pub(crate) fn latest_identity<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    workspace: &str,
    pane: &str,
    terminal: &str,
    session: Option<&str>,
    cancelled: &AtomicBool,
) -> Identity {
    let reads = BoundedReads {
        inner: runtime.herdr,
        deadline: Instant::now() + IDENTITY_BUDGET,
        cancelled,
    };
    let panes = match discover_worktree_for_workspace(&reads, workspace) {
        Ok(worktree) => worktree.panes,
        Err(error) => {
            return Identity::Unconfirmed(format!("Could not confirm the invoking pane: {error}"));
        }
    };
    let Some(latest) = panes
        .into_iter()
        .find(|candidate| candidate.pane_id == pane)
    else {
        return Identity::Changed(
            "The invoking pane has closed. Open the popup again from your pane.".to_owned(),
        );
    };
    if latest.terminal_id != terminal {
        return Identity::Changed(
            "The invoking pane was replaced. Close this popup and open it again.".to_owned(),
        );
    }
    // Compared exactly, both ways: a pane that gained an agent since the popup
    // opened is as much a different subject as one that swapped sessions.
    let now = latest.agent_session_id.map(|id| id.as_str().to_owned());
    if now.as_deref() != session {
        return Identity::Changed(
            "This pane started a new agent session. Close this popup and open it again.".to_owned(),
        );
    }
    Identity::Same
}

/// The data plane, injected so the loop can be exercised without a host CLI,
/// a Herdr server or a model call.
pub(crate) struct Sources<'a> {
    pub(crate) collect: &'a (dyn Fn(&AtomicBool) -> Result<Value, String> + Sync),
    pub(crate) summarize: &'a (dyn Fn(&Value, &AtomicBool) -> Result<Value, String> + Sync),
    pub(crate) recheck: &'a (dyn Fn(&AtomicBool) -> Identity + Sync),
}

pub(crate) struct Screen<R, W> {
    pub(crate) input: R,
    pub(crate) output: W,
    /// A real terminal polls for keys and repaints in place. Without one a read
    /// of zero bytes is end of input rather than an idle tick.
    pub(crate) interactive: bool,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) snapshot: Option<Value>,
    pub(crate) summary: Option<Value>,
    pub(crate) facts_error: Option<String>,
    pub(crate) summary_error: Option<String>,
    /// A collection or description pass is in flight.
    pub(crate) working: bool,
    pub(crate) notice: Option<String>,
    /// The origin changed underneath the popup; nothing more is collected.
    pub(crate) frozen: bool,
    pub(crate) scroll: usize,
}

enum Update {
    Facts {
        pass: u64,
        result: Result<Value, String>,
    },
    /// The description, delivered with the identity verdict taken right beside
    /// it so the loop never has to make a Herdr call of its own.
    Summary {
        pass: u64,
        identity: Identity,
        summary: Result<Value, String>,
    },
}

impl Update {
    fn pass(&self) -> u64 {
        match self {
            Self::Facts { pass, .. } | Self::Summary { pass, .. } => *pass,
        }
    }
}

/// One cancellation flag per pass. Cancelling a pass can then never revive an
/// earlier one, and leaving the popup cancels every pass still running, so
/// `thread::scope` does not join a worker doing work nobody will read.
#[derive(Default)]
struct Passes(std::cell::RefCell<Vec<Arc<AtomicBool>>>);

impl Passes {
    fn start(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.0.borrow_mut().push(flag.clone());
        flag
    }

    fn cancel(&self) {
        for flag in self.0.borrow().iter() {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for Passes {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(crate) fn popup<R: Read, W: Write>(
    screen: &mut Screen<R, W>,
    sources: &Sources<'_>,
) -> Result<(), String> {
    let mut state = State {
        working: true,
        ..State::default()
    };
    thread::scope(|scope| -> Result<(), String> {
        let passes = Passes::default();
        let (sender, receiver) = mpsc::channel::<Update>();
        let begin = |pass: u64, sender: mpsc::Sender<Update>| {
            let cancelled = passes.start();
            scope.spawn(move || match (sources.collect)(&cancelled) {
                Ok(snapshot) => {
                    let _ = sender.send(Update::Facts {
                        pass,
                        result: Ok(snapshot.clone()),
                    });
                    if cancelled.load(Ordering::SeqCst) {
                        return;
                    }
                    let summary = (sources.summarize)(&snapshot, &cancelled);
                    // Nothing will be displayed, so do not spend a Herdr round
                    // trip confirming an identity for a pass already abandoned.
                    if cancelled.load(Ordering::SeqCst) {
                        return;
                    }
                    // Rechecked here rather than in the loop: this is a Herdr
                    // round trip, and the keyboard must stay free while it runs.
                    let identity = (sources.recheck)(&cancelled);
                    let _ = sender.send(Update::Summary {
                        pass,
                        identity,
                        summary,
                    });
                }
                Err(error) => {
                    let _ = sender.send(Update::Facts {
                        pass,
                        result: Err(error),
                    });
                }
            });
        };
        // The first frame is painted before any collection starts, so the popup
        // is never blank while Herdr and the host CLI are still being asked.
        let mut shown = String::new();
        render(screen, &state, &mut shown)?;
        let mut running = true;
        let mut pass = 0u64;
        let mut sized = Instant::now();
        begin(pass, sender.clone());
        loop {
            // Anything from an abandoned pass is dropped here: it describes a
            // moment the panel has already moved on from.
            while let Ok(update) = receiver.try_recv() {
                if update.pass() == pass {
                    apply(update, &mut state, &mut running);
                }
            }
            // Without a terminal there is no concurrent typist, so settle the
            // pass in hand before the next scripted key is applied.
            while running && !screen.interactive {
                match receiver.recv_timeout(BATCH_WAIT) {
                    Ok(update) if update.pass() == pass => apply(update, &mut state, &mut running),
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            // Keep polling the worker and keyboard. A slow AI request remains
            // pending; closing the popup cancels it through the pass guard.
            // Popups can be resized under us. Re-measure on a slow cadence
            // rather than spawning stty on every hundred-millisecond tick.
            if screen.interactive && sized.elapsed() >= SIZE_INTERVAL {
                sized = Instant::now();
                if let Some((width, height)) = terminal_size() {
                    screen.width = width;
                    screen.height = height;
                }
            }
            state.working = running;
            render(screen, &state, &mut shown)?;
            let pressed = key(&mut screen.input, screen.interactive)?;
            match pressed {
                Key::Close => break,
                Key::Refresh if state.frozen => {
                    state.notice = Some(
                        "This pane changed. Close this popup and open it again from your pane."
                            .to_owned(),
                    );
                }
                // One pass per origin: a second R during a pass joins the pass
                // already running instead of paying for another one.
                Key::Refresh if running => {
                    state.notice = Some("Already refreshing.".to_owned());
                }
                Key::Refresh => {
                    state = State {
                        scroll: state.scroll,
                        working: true,
                        ..State::default()
                    };
                    running = true;
                    pass += 1;
                    begin(pass, sender.clone());
                }
                Key::Up | Key::Down | Key::PageUp | Key::PageDown => {
                    let step = match pressed {
                        Key::Up | Key::Down => 1,
                        _ => page_step(screen.height),
                    };
                    // Start from what is on screen: content can shrink after a
                    // refresh, and Up must move the view on its first press.
                    let limit = scroll_limit(screen, &state);
                    let scroll = state.scroll.min(limit);
                    state.scroll = match pressed {
                        Key::Up | Key::PageUp => scroll.saturating_sub(step),
                        _ => scroll.saturating_add(step).min(limit),
                    };
                }
                Key::Idle | Key::Other => {}
            }
        }
        Ok(())
    })
}

fn apply(update: Update, state: &mut State, running: &mut bool) {
    match update {
        Update::Facts {
            result: Ok(snapshot),
            ..
        } => {
            state.snapshot = Some(snapshot);
            state.facts_error = None;
        }
        Update::Facts {
            result: Err(error), ..
        } => {
            state.facts_error = Some(error);
            *running = false;
        }
        Update::Summary {
            identity, summary, ..
        } => {
            *running = false;
            match identity {
                Identity::Same => match summary {
                    Ok(summary) => {
                        state.summary = Some(summary);
                        state.summary_error = None;
                    }
                    Err(error) => state.summary_error = Some(error),
                },
                // The pane the popup opened for is gone or is running someone
                // else's session. Every fact in hand described the old one, so
                // the panel drops all of it rather than misattributing work.
                Identity::Changed(error) => {
                    *state = State {
                        notice: Some(error),
                        frozen: true,
                        scroll: state.scroll,
                        ..State::default()
                    };
                }
                // Not knowing is not the same as knowing it changed. The facts
                // were collected against a validated identity and stay; only
                // the description, which nothing has vouched for, is withheld.
                Identity::Unconfirmed(reason) => {
                    state.summary = None;
                    state.summary_error = Some(format!(
                        "{reason}. The description is withheld until the pane can be confirmed."
                    ));
                }
            }
        }
    }
}

fn page_step(height: usize) -> usize {
    height.saturating_sub(5).max(1)
}

/// Repaints only when the panel differs from what is already on screen. The
/// loop ticks ten times a second; redrawing every tick is visible flicker.
fn render<R, W: Write>(
    screen: &mut Screen<R, W>,
    state: &State,
    shown: &mut String,
) -> Result<(), String> {
    // A real terminal gets the painted frame; scripted output stays plain text.
    // NO_COLOR is the common convention for turning colour off.
    let text = if screen.interactive {
        let color = env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
        painted(state, screen.width, screen.height, now_ms(), color)
    } else {
        frame(state, screen.width, screen.height, now_ms())
    };
    if text == *shown {
        return Ok(());
    }
    if screen.interactive {
        // Hidden, or the cursor sits blinking after the footer.
        write!(screen.output, "\x1b[?25l\x1b[2J\x1b[H").map_err(|error| error.to_string())?;
    }
    screen
        .output
        .write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    screen.output.flush().map_err(|error| error.to_string())?;
    *shown = text;
    Ok(())
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Key {
    Refresh,
    Close,
    Up,
    Down,
    PageUp,
    PageDown,
    /// The poll expired with no key pressed.
    Idle,
    Other,
}

fn key<R: Read>(input: &mut R, interactive: bool) -> Result<Key, String> {
    let mut byte = [0];
    if input.read(&mut byte).map_err(|e| e.to_string())? == 0 {
        return Ok(if interactive { Key::Idle } else { Key::Close });
    }
    Ok(match byte[0] {
        b'r' | b'R' => Key::Refresh,
        b'q' | b'Q' | b'\n' | b'\r' | 0x04 => Key::Close,
        0x1b => {
            // Arrow and page keys start with Esc; a bare Esc closes. The poll
            // set below returns zero bytes rather than waiting for a second key.
            if input.read(&mut byte).map_err(|e| e.to_string())? == 0 {
                return Ok(Key::Close);
            }
            if !matches!(byte[0], b'[' | b'O') {
                return Ok(Key::Other);
            }
            let mut sequence = Vec::new();
            while input.read(&mut byte).map_err(|e| e.to_string())? != 0 {
                sequence.push(byte[0]);
                if (0x40..=0x7e).contains(&byte[0]) {
                    break;
                }
                if sequence.len() == 16 {
                    return Ok(Key::Other);
                }
            }
            match sequence.as_slice() {
                b"A" => Key::Up,
                b"B" => Key::Down,
                b"5~" => Key::PageUp,
                b"6~" => Key::PageDown,
                _ => Key::Other,
            }
        }
        _ => Key::Other,
    })
}

fn popup_size() -> (usize, usize) {
    if let Some(size) = terminal_size() {
        return size;
    }
    let read = |name: &str, fallback: usize| {
        env::var(name)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
    };
    (
        read("COLUMNS", DEFAULT_WIDTH),
        read("LINES", DEFAULT_HEIGHT),
    )
}

fn terminal_size() -> Option<(usize, usize)> {
    let output = Command::new("/bin/stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut parts = text.split_whitespace();
    let rows: usize = parts.next()?.parse().ok()?;
    let columns: usize = parts.next()?.parse().ok()?;
    (rows > 0 && columns > 0).then_some((columns, rows))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rendering
//
// Everything below is pure: a state and a size in, the exact popup text out.
// Layout and colour are separate steps: `frame` is the plain text, and
// `painted` is that same text as a terminal shows it. Colour only ever wraps
// text that was already sanitized, so nothing untrusted picks a colour.
// ---------------------------------------------------------------------------

/// How a popup colours a span on a real terminal. Plain output ignores it.
/// Shared with the other popups so every Namedrop panel reads the same way.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum Tone {
    #[default]
    Plain,
    Strong,
    Name,
    Muted,
    Good,
    Warn,
    Bad,
    Info,
    /// A command someone may copy.
    Code,
    /// The selected entry of a menu.
    Selected,
    /// A key hint: each key bold, its label muted.
    Keys,
    /// A menu of actions: each key bold, its label plain.
    Menu,
}

impl Tone {
    /// Basic ANSI colours, so the panel follows the terminal's own theme.
    fn sgr(self) -> &'static str {
        match self {
            Self::Plain | Self::Keys | Self::Menu => "",
            Self::Strong => "1",
            Self::Name => "1;36",
            Self::Muted => "2",
            Self::Good => "32",
            Self::Warn => "33",
            Self::Bad => "31",
            Self::Info => "34",
            Self::Code => "36",
            Self::Selected => "7",
        }
    }
}

/// `text` wrapped in `tone`'s colour codes. Callers pass text that is already
/// sanitized or their own, so these are the only codes that reach the screen.
pub(crate) fn tint(tone: Tone, text: &str) -> String {
    if let Tone::Keys | Tone::Menu = tone {
        let label = if tone == Tone::Keys {
            Tone::Muted
        } else {
            Tone::Plain
        };
        return text
            .split(' ')
            .map(|word| tint(if is_key(word) { Tone::Strong } else { label }, word))
            .collect::<Vec<_>>()
            .join(" ");
    }
    let code = tone.sgr();
    if code.is_empty() || text.trim().is_empty() {
        text.to_owned()
    } else {
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

/// A word of a key hint that names a key rather than what it does.
fn is_key(word: &str) -> bool {
    matches!(word, "↑/↓" | "←/→" | "Enter" | "Esc" | "Enter/Esc")
        || (word.len() == 1 && word.as_bytes()[0].is_ascii_uppercase())
}

/// How one line is coloured: a name in its first `lead` cells, a state word in
/// its last `tail` cells, and `body` for everything between.
#[derive(Clone, Copy, Debug, Default)]
struct Look {
    lead: usize,
    body: Tone,
    tail: usize,
    tone: Tone,
}

type Line = (Look, String);

fn toned(tone: Tone, texts: Vec<String>) -> Vec<Line> {
    let look = Look {
        body: tone,
        ..Look::default()
    };
    texts.into_iter().map(|text| (look, text)).collect()
}

fn heading(text: &str) -> Line {
    let look = Look {
        body: Tone::Strong,
        ..Look::default()
    };
    (look, text.to_owned())
}

/// Renders the whole popup, header and footer included, as exactly `height`
/// lines of at most `width` columns.
pub(crate) fn frame(state: &State, width: usize, height: usize, now_ms: u64) -> String {
    layout(state, width, height, now_ms)
        .0
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The frame as a terminal shows it: inside a one-cell gutter so text never
/// touches the popup's border, and coloured unless `color` is off.
pub(crate) fn painted(
    state: &State,
    width: usize,
    height: usize,
    now_ms: u64,
    color: bool,
) -> String {
    let gutter = gutter(width);
    let margin = " ".repeat(gutter);
    layout(state, width - 2 * gutter, height, now_ms)
        .0
        .iter()
        .map(|line| {
            let text = if color { paint(line) } else { line.1.clone() };
            format!("{margin}{text}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Columns kept blank on each side of a painted frame.
fn gutter(width: usize) -> usize {
    usize::from(width > 2)
}

/// The furthest the body can scroll on this screen. Scroll keys clamp to it,
/// so holding Down at the bottom does not bank presses Up must work off.
fn scroll_limit<R, W: Write>(screen: &Screen<R, W>, state: &State) -> usize {
    let width = if screen.interactive {
        screen.width - 2 * gutter(screen.width)
    } else {
        screen.width
    };
    layout(state, width, screen.height, now_ms()).1
}

/// The popup's lines, and the furthest its body can scroll.
fn layout(state: &State, width: usize, height: usize, now_ms: u64) -> (Vec<Line>, usize) {
    // The pane's real size wins. Rounding a small popup up to a comfortable
    // size would simply draw past the edge of the pane it has to fit in.
    let width = width.clamp(1, MAX_WIDTH);
    let height = height.max(1);
    let with_header = height >= 2;
    let space_below_header = height >= SPACED_HEIGHT;
    let space_above_footer = height >= ROOMY_HEIGHT;
    let reserved = 1
        + usize::from(with_header)
        + usize::from(space_below_header)
        + usize::from(space_above_footer);
    let visible = height - reserved;

    let (title, name, timing) = header_parts(state, now_ms);
    let mut raw = Vec::new();
    let header = if timing.is_empty() || count(&title) + 2 + count(&timing) > width {
        // Too narrow for both halves: the timing keeps its own body line rather
        // than pushing the title off the screen.
        if !timing.is_empty() {
            raw.extend(toned(Tone::Muted, vec![timing, String::new()]));
        }
        let title = clip(&title, width);
        let look = Look {
            lead: name.min(count(&title)),
            ..Look::default()
        };
        (look, title)
    } else {
        let gap = width - count(&title) - count(&timing);
        let look = Look {
            lead: name,
            tail: count(&timing),
            tone: Tone::Muted,
            ..Look::default()
        };
        (look, format!("{title}{}{timing}", " ".repeat(gap)))
    };
    raw.extend(sections(state, width));
    // Rows arrive already fitted; headings, notes and the narrow-popup timing
    // line pass through one guard so no line can ever run past the popup.
    let mut body = Vec::new();
    for (look, text) in raw {
        if count(&text) <= width {
            body.push((look, text));
        } else {
            body.extend(toned(look.body, wrap(&text, width)));
        }
    }
    while body.last().is_some_and(|(_, text)| text.is_empty()) {
        body.pop();
    }
    let furthest = body.len().saturating_sub(visible);
    let scroll = state.scroll.min(furthest);

    let mut lines = Vec::with_capacity(height);
    if with_header {
        lines.push(header);
    }
    if space_below_header {
        lines.push(Line::default());
    }
    for index in 0..visible {
        lines.push(body.get(scroll + index).cloned().unwrap_or_default());
    }
    if space_above_footer {
        lines.push(Line::default());
    }
    let footer = clip(&footer(scroll, furthest, width), width);
    lines.extend(toned(Tone::Keys, vec![footer]));
    (lines, furthest)
}

fn paint((look, text): &Line) -> String {
    let (lead, rest) = split_at_cells(text, look.lead);
    let (body, tail) = split_at_cells(rest, count(rest).saturating_sub(look.tail));
    tint(Tone::Name, lead) + &tint(look.body, body) + &tint(look.tone, tail)
}

/// Splits after `at` display cells. A zero-width mark stays with the character
/// it combines with instead of starting the next colour run.
fn split_at_cells(text: &str, at: usize) -> (&str, &str) {
    let mut used = 0;
    for (index, character) in text.char_indices() {
        let cell = cells(character);
        if used >= at && cell > 0 {
            return text.split_at(index);
        }
        used += cell;
    }
    (text, "")
}

fn footer(scroll: usize, furthest: usize, width: usize) -> String {
    let mut options = Vec::new();
    if furthest > 0 {
        options.push(format!(
            "R Refresh · ↑/↓ Scroll {}/{} · Esc Close",
            scroll + 1,
            furthest + 1
        ));
        options.push("R Refresh · ↑/↓ · Esc Close".to_owned());
        options.push("R · ↑/↓ · Esc".to_owned());
    } else {
        options.push("R Refresh · Esc Close".to_owned());
        options.push("R · Esc".to_owned());
    }
    let last = options.len() - 1;
    options
        .into_iter()
        .enumerate()
        .find(|(index, option)| *index == last || count(option) <= width)
        .map(|(_, option)| option)
        .unwrap_or_default()
}

/// The title, the cells its leading name takes, and the timing. Herdr already
/// writes "Agent status" on the popup's border, so the title does not repeat it.
fn header_parts(state: &State, now_ms: u64) -> (String, usize, String) {
    let origin = state.snapshot.as_ref().and_then(|s| s.get("origin"));
    let mut title = String::new();
    let mut name = 0;
    if let Some(origin) = origin {
        let label = sanitize(field(origin, "label"));
        if !label.is_empty() {
            title = mention(&label);
            name = count(&title);
        }
        let host = host_name(field(origin, "host"));
        if !host.is_empty() {
            if !title.is_empty() {
                title.push_str(" · ");
            }
            title.push_str(&host);
        }
    }
    (title, name, timing(state, now_ms))
}

fn timing(state: &State, now_ms: u64) -> String {
    let Some(collected) = state
        .snapshot
        .as_ref()
        .and_then(|s| number(s, "observed_at_ms"))
    else {
        return if state.facts_error.is_some() || state.frozen {
            String::new()
        } else {
            "Collecting…".to_owned()
        };
    };
    let summarized = state
        .summary
        .as_ref()
        .and_then(|s| number(s, "summarized_at_ms"));
    match summarized {
        // Separate stamps whenever the description is meaningfully older or
        // newer than the facts it describes.
        Some(summarized) if summarized.abs_diff(collected) >= 1_000 => format!(
            "Facts {} · summary {}",
            ago(now_ms, collected),
            ago(now_ms, summarized)
        ),
        Some(_) => format!("Updated {}", ago(now_ms, collected)),
        None if state.working => format!("Facts {} · summarizing", ago(now_ms, collected)),
        None => format!("Facts {}", ago(now_ms, collected)),
    }
}

fn sections(state: &State, width: usize) -> Vec<Line> {
    let snapshot = state.snapshot.as_ref();
    let panes = array(snapshot, "panes");
    let waits = array(snapshot, "waits");
    let origin = snapshot.and_then(|s| s.get("origin"));
    let origin_pane = origin.map(|o| field(o, "pane_id")).unwrap_or_default();
    let descriptions = peer_descriptions(state, &panes);
    let mut lines = vec![heading("Now")];
    lines.extend(toned(Tone::Plain, wrap(&now_text(state, origin), width)));
    // Keep loading compact. Raw output is a fallback after summarization ends,
    // not a temporary description that is replaced moments later.
    if snapshot.is_some() && !state.working && !state.frozen && origin_description(state).is_none()
    {
        let pane = panes
            .iter()
            .find(|pane| field(pane, "pane_id") == origin_pane);
        let output = recent_output(pane).unwrap_or_else(|| "No recent output captured.".to_owned());
        lines.extend(toned(Tone::Muted, wrap(&output, width)));
    }
    // What the agent is *for*, kept on its own line so it can never be read as
    // what it is doing. The origin says so even when it has none, because
    // Generic is a choice the user made rather than an absence.
    if let Some(origin) = origin {
        lines.extend(toned(Tone::Muted, wrap(&origin_persona(origin), width)));
    }
    lines.push(Line::default());
    lines.extend(dependency_section(
        state,
        &waits,
        &panes,
        &descriptions,
        width,
    ));
    lines.push(Line::default());

    let mut missing = 0usize;
    let mut peers = Vec::new();
    let mut previous = None;
    for pane in &panes {
        let pane_id = field(pane, "pane_id");
        if pane_id == origin_pane || waits.iter().any(|w| field(w, "pane_id") == pane_id) {
            continue;
        }
        let mut description = lookup(&descriptions, pane_id);
        if description.is_empty() {
            missing += 1;
            // A pane with nothing to show keeps an empty column rather than a
            // row of filler; filler pushed the real notes below the fold.
            description = if state.working {
                "Preparing summary…".to_owned()
            } else {
                recent_output(Some(pane)).unwrap_or_default()
            };
        }
        let entry = row(
            &mention(&sanitize(field(pane, "label"))),
            &description,
            &status_word(field(pane, "status")),
            &persona_note(pane),
            width,
        );
        separate(&mut peers, &mut previous, entry.len());
        peers.extend(entry);
    }
    lines.push(heading("Elsewhere in this workspace"));
    if peers.is_empty() {
        lines.extend(toned(
            Tone::Muted,
            indented("No other agents in this workspace.", width),
        ));
    } else {
        lines.extend(peers);
    }

    let notes = notes(state, snapshot, missing);
    if !notes.is_empty() {
        lines.push(Line::default());
        lines.push(heading("Notes"));
        let mut bullets = Vec::new();
        for note in notes {
            let mut wrapped = wrap(&note, width.saturating_sub(2)).into_iter();
            if let Some(first) = wrapped.next() {
                bullets.push(format!("- {first}"));
            }
            bullets.extend(wrapped.map(|line| format!("  {line}")));
        }
        lines.extend(toned(Tone::Plain, bullets));
    }
    lines
}

/// The newest end of a pane's terminal tail, sanitized and bounded. `None` when
/// the pane printed nothing legible, so a caller can leave the row alone rather
/// than spend a line saying so.
///
/// An excerpt is up to 120 lines of transcript and the *last* lines are what
/// just happened, so the newest lines are taken first and the cut, when one is
/// needed, falls at the old end. Always labelled as output: it is raw text the
/// pane printed, never a statement about what the agent is doing, and nothing
/// is inferred from it.
fn recent_output(pane: Option<&Value>) -> Option<String> {
    let raw = pane.map(|pane| field(pane, "excerpt")).unwrap_or_default();
    let host = pane.map(|pane| field(pane, "host")).unwrap_or_default();
    let mut newest_first = Vec::new();
    let mut cells = 0;
    for line in raw.lines().rev() {
        let line = sanitize(line);
        if line.is_empty() || is_host_chrome(host, &line) {
            continue;
        }
        cells += count(&line) + 1;
        newest_first.push(line);
        if cells >= MAX_EXCERPT_CELLS {
            break;
        }
    }
    newest_first.reverse();
    let text = newest_first.join(" ");
    (!text.is_empty()).then(|| format!("Recent output: {}", keep_newest(&text, MAX_EXCERPT_CELLS)))
}

/// The interface each host paints below its task output: box rules, the input
/// prompt, the spinner or status line, and the footer. Those lines are the
/// newest text in a tail, so without this the panel shows a prompt box instead
/// of what the agent was doing.
///
/// Every rule describes one known host's current interface, and the host gate
/// comes first: an unrecognized or absent host keeps every line, because a rule
/// written for one host must not eat another's real output. The signatures are
/// deliberately narrow, so a sentence that merely mentions a word or carries a
/// stray glyph survives. Nothing here judges activity.
fn is_host_chrome(host: &str, line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || !matches!(host, "claude" | "codex") {
        return false;
    }
    // A border or a spinner frame standing alone. Only a line made entirely of
    // these is chrome; real output that happens to contain one is not.
    if line.chars().all(is_rule_or_spinner) {
        return true;
    }
    let inner = line.trim_start_matches(['│', '┃', '║', ' ']).trim();
    match host {
        // The prompt caret, the "✻ Crunched for …" status glyph, the "⏵⏵ auto
        // mode on …" permission footer, the "✔ Update installed …" notice, and
        // the status line ending in the context percentage.
        "claude" => {
            inner.starts_with('❯')
                || inner.starts_with(['✻', '✽', '✳', '✶', '✢'])
                || inner.starts_with("⏵⏵")
                || (inner.starts_with('✔') && inner.contains("Update installed"))
                || inner.ends_with(" ctx")
                || (inner.contains(" git:(")
                    && inner.rsplit_once(" ctx:").is_some_and(|(_, percentage)| {
                        percentage.strip_suffix('%').is_some_and(|digits| {
                            !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
                        })
                    }))
        }
        // The prompt caret, the "• Working (…)" status line, and the model and
        // context footer.
        "codex" => {
            inner.starts_with('›')
                || (inner.starts_with('•') && inner.contains("Working ("))
                || (inner.starts_with("gpt-") && inner.contains("Context "))
        }
        _ => false,
    }
}

fn is_rule_or_spinner(character: char) -> bool {
    matches!(
        character,
        '─' | '│'
            | '╭'
            | '╮'
            | '╰'
            | '╯'
            | '┌'
            | '┐'
            | '└'
            | '┘'
            | '├'
            | '┤'
            | '━'
            | '┃'
            | '═'
            | '║'
            | ' '
    ) || ('\u{2800}'..='\u{28ff}').contains(&character)
}

/// Clips from the front, keeping the end. The opposite of `clip`, because the
/// interesting end of a transcript tail is its last characters.
fn keep_newest(text: &str, width: usize) -> String {
    if count(text) <= width {
        return text.to_owned();
    }
    let budget = width.saturating_sub(1);
    let mut kept = std::collections::VecDeque::new();
    let mut used = 0;
    for character in text.chars().rev() {
        let cell = cells(character);
        if used + cell > budget {
            break;
        }
        kept.push_front(character);
        used += cell;
    }
    let mut out = String::from("…");
    out.extend(kept);
    out
}

fn origin_description(state: &State) -> Option<String> {
    state
        .summary
        .as_ref()
        .map(|summary| sanitize(field(summary, "origin_description")))
        .filter(|text| !text.is_empty())
}

/// The origin's persona, always stated. `Generic` is spelled out rather than
/// left blank: an agent with no specialization still takes ordinary work, and a
/// reader cannot tell that from an empty line.
fn origin_persona(origin: &Value) -> String {
    let persona = sanitize(field(origin, "persona"));
    if persona.is_empty() {
        "Persona: Generic, no specialization.".to_owned()
    } else {
        format!("Persona: {persona}")
    }
}

/// A peer's persona, as a secondary line under its row. Peers state it only
/// when they have one: a column of "Generic" lines pushes the notes that do
/// carry information below the fold.
fn persona_note(pane: &Value) -> Vec<String> {
    let persona = sanitize(field(pane, "persona"));
    if persona.is_empty() {
        Vec::new()
    } else {
        vec![format!("Persona: {persona}")]
    }
}

fn now_text(state: &State, origin: Option<&Value>) -> String {
    if let Some(described) = origin_description(state) {
        return described;
    }
    if state.frozen {
        return "This pane changed. Nothing collected here describes it any more.".to_owned();
    }
    if state.snapshot.is_none() {
        return match state.facts_error.as_deref() {
            Some(error) => format!("Facts unavailable: {}", sanitize(error)),
            None => "Collecting facts…".to_owned(),
        };
    }
    let status = status_word(origin.map(|o| field(o, "status")).unwrap_or_default());
    let status = if status.is_empty() {
        "No status reported".to_owned()
    } else {
        status
    };
    if state.working {
        return format!("Preparing summary… ({status})");
    }
    // Why there is no description belongs where the reader is already looking.
    // In Notes it fell below the fold on a 24-row popup, which is how a failed
    // worker came to look like an empty panel.
    match state.summary_error.as_deref() {
        Some(error) => format!("{status}. No description: {}", sanitize(error)),
        None => format!("{status}. No description available."),
    }
}

fn dependency_section(
    state: &State,
    waits: &[Value],
    panes: &[Value],
    descriptions: &[(String, String)],
    width: usize,
) -> Vec<Line> {
    let available = state
        .snapshot
        .as_ref()
        .map(|s| s.get("handoffs_available").and_then(Value::as_bool) == Some(true));
    let mut lines = Vec::new();
    match available {
        None => {
            lines.push(heading("Awaiting replies"));
            lines.extend(toned(Tone::Muted, indented("Not known yet.", width)));
            return lines;
        }
        // An unreadable handoff store is missing evidence, never a clean bill.
        Some(false) => {
            lines.push(heading("Awaiting replies"));
            lines.extend(toned(
                Tone::Warn,
                indented(
                    "Handoff state could not be read, so outstanding replies are unknown.",
                    width,
                ),
            ));
            return lines;
        }
        Some(true) if waits.is_empty() => {
            lines.push(heading("No outstanding replies"));
            return lines;
        }
        Some(true) => {}
    }
    lines.push(heading(&format!(
        "Awaiting {} {}",
        waits.len(),
        if waits.len() == 1 { "reply" } else { "replies" }
    )));
    // Delivery states do not all mean the same kind of wait, and the panel must
    // not blur them: a request that was never submitted is not evidence about
    // its recipient, and one needing a decision here is not progress at all.
    // The snapshot already orders waits attention-first, so grouping only adds
    // the heading that names what each block actually means.
    let mut group = None;
    let mut previous = None;
    for wait in waits {
        let delivery = field(wait, "state");
        let pane_id = field(wait, "pane_id");
        let alias = sanitize(field(wait, "alias"));
        // The recipient's own pane label when the snapshot can see it, and the
        // delivery's alias when it cannot.
        let name = panes
            .iter()
            .find(|pane| field(pane, "pane_id") == pane_id)
            .map(|pane| sanitize(field(pane, "label")))
            .filter(|label| !label.is_empty())
            .unwrap_or(alias.clone());
        // What was asked stays the headline. The snapshot writes it as
        // "@alias · request"; the alias is already this row's name.
        let asked = sanitize(field(wait, "description"));
        let asked = asked
            .strip_prefix(&format!("@{alias} · "))
            .unwrap_or(&asked)
            .to_owned();
        let mut notes = Vec::new();
        let reason = sanitize(field(wait, "reason"));
        if !reason.is_empty() {
            notes.push(reason);
        }
        // What the recipient is for, before what it happens to be doing. Both
        // are context for the wait and neither is the wait itself.
        notes.extend(
            panes
                .iter()
                .find(|pane| field(pane, "pane_id") == pane_id)
                .map(persona_note)
                .unwrap_or_default(),
        );
        // The recipient's current work is context for the wait, never a
        // replacement for it: what it is doing may have nothing to do with
        // what was asked.
        let doing = lookup(descriptions, pane_id);
        if !doing.is_empty() {
            notes.push(format!("Now: {doing}"));
        } else if !state.working
            && let Some(output) = panes
                .iter()
                .find(|pane| field(pane, "pane_id") == pane_id)
                .and_then(|pane| recent_output(Some(pane)))
        {
            notes.push(output);
        }
        let entry = row(&mention(&name), &asked, &wait_word(delivery), &notes, width);
        separate(&mut lines, &mut previous, entry.len());
        let (rank, label, tone) = wait_group(delivery);
        if group != Some(rank) {
            group = Some(rank);
            lines.extend(toned(tone, vec![label.to_owned()]));
        }
        lines.extend(entry);
    }
    lines
}

/// Which kind of wait a Delivery state is, the heading that says so, and its
/// colour. The ranks follow the snapshot's own ordering, so a single pass over
/// the ordered waits emits each heading once.
fn wait_group(state: &str) -> (u8, &'static str, Tone) {
    match state.trim().to_ascii_lowercase().as_str() {
        "held" | "failed" | "cancelled" => (0, "Needs attention here", Tone::Warn),
        "settled" | "acknowledged" | "submitted" => (1, "Sent, no result yet", Tone::Info),
        "queued" | "dispatching" => (2, "Not sent to the recipient yet", Tone::Muted),
        _ => (
            3,
            "Delivery state this panel does not recognise",
            Tone::Muted,
        ),
    }
}

fn notes(state: &State, snapshot: Option<&Value>, missing: usize) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(notice) = &state.notice {
        notes.push(sanitize(notice));
    }
    if let Some(error) = &state.facts_error
        && snapshot.is_some()
    {
        notes.push(format!("Refresh failed: {}", sanitize(error)));
    }
    for warning in array(snapshot, "warnings") {
        if let Some(warning) = warning.as_str() {
            notes.push(sanitize(warning));
        }
    }
    let omitted = snapshot
        .and_then(|s| number(s, "omitted_panes"))
        .unwrap_or(0);
    if omitted > 0 {
        notes.push(format!(
            "{omitted} more {} not read.",
            if omitted == 1 {
                "pane was"
            } else {
                "panes were"
            }
        ));
    }
    if missing > 0 && state.summary.is_some() {
        notes.push(format!(
            "{missing} {} no description.",
            if missing == 1 {
                "agent has"
            } else {
                "agents have"
            }
        ));
    }
    notes
}

/// Descriptions the model returned, kept only for pane IDs the snapshot
/// already knows. The model never names an agent the facts did not.
fn peer_descriptions(state: &State, panes: &[Value]) -> Vec<(String, String)> {
    let Some(summary) = &state.summary else {
        return Vec::new();
    };
    summary
        .get("peers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|peer| {
            let pane_id = field(peer, "pane_id");
            if pane_id.is_empty() || !panes.iter().any(|pane| field(pane, "pane_id") == pane_id) {
                return None;
            }
            let description = sanitize(field(peer, "description"));
            (!description.is_empty()).then(|| (pane_id.to_owned(), description))
        })
        .collect()
}

fn lookup(descriptions: &[(String, String)], pane_id: &str) -> String {
    descriptions
        .iter()
        .find(|(id, _)| id == pane_id)
        .map(|(_, description)| description.clone())
        .unwrap_or_default()
}

/// One agent: name, the headline, the plain state word, and any secondary
/// lines. Narrow popups stack the columns rather than dropping the state word,
/// which is the load-bearing fact in the row.
fn row(name: &str, headline: &str, state: &str, notes: &[String], width: usize) -> Vec<Line> {
    let state = clip(state, 12);
    let tone = state_tone(&state);
    let notes = notes.iter().filter(|note| !note.is_empty());
    let mut lines = Vec::new();
    if width < NARROW_WIDTH {
        let head = if state.is_empty() {
            name.to_owned()
        } else {
            format!("{name}  {state}")
        };
        let clipped = clip(&head, width);
        // A clipped head may have lost its state word, so only an intact one
        // is coloured as one.
        let look = if clipped == head {
            Look {
                lead: count(name),
                tail: count(&state),
                tone,
                ..Look::default()
            }
        } else {
            Look {
                lead: count(name).min(count(&clipped)),
                ..Look::default()
            }
        };
        lines.push((look, clipped));
        let hang = |text: &str| -> Vec<String> {
            wrap(text, width.saturating_sub(2))
                .into_iter()
                .map(|line| format!("  {line}"))
                .collect()
        };
        if !headline.is_empty() {
            lines.extend(toned(Tone::Plain, hang(headline)));
        }
        for note in notes {
            lines.extend(toned(Tone::Muted, hang(note)));
        }
        return lines;
    }
    let name_width = 12;
    let name = clip(name, name_width);
    let indent = " ".repeat(name_width + 1);
    let text_width = width - name_width - 1;
    let state_width = if state.is_empty() {
        0
    } else {
        count(&state) + 2
    };
    // Only the first line shares the row with the state word, so the lines
    // under it take the full width instead of leaving that column empty.
    let chunks = wrap_hanging(headline, text_width - state_width, text_width);
    for (index, chunk) in chunks.into_iter().enumerate() {
        if index > 0 {
            lines.push((Look::default(), format!("{indent}{chunk}")));
            continue;
        }
        let mut line = format!("{} {chunk}", pad(&name, name_width));
        if !state.is_empty() {
            line = pad(&line, width - state_width) + &pad_left(&state, state_width);
        }
        let look = Look {
            lead: count(&name),
            tail: count(&state),
            tone,
            ..Look::default()
        };
        lines.push((look, line.trim_end().to_owned()));
    }
    for note in notes {
        let wrapped = wrap(note, text_width);
        lines.extend(toned(
            Tone::Muted,
            wrapped
                .into_iter()
                .map(|l| format!("{indent}{l}"))
                .collect(),
        ));
    }
    lines
}

/// A blank line between two rows when either wraps, so a wrapped row reads as
/// one unit while a list of one-line rows stays dense.
fn separate(lines: &mut Vec<Line>, previous: &mut Option<usize>, next: usize) {
    if previous.is_some_and(|rows| rows > 1 || next > 1) {
        lines.push(Line::default());
    }
    *previous = Some(next);
}

/// The colour of a state word, keyed on the words `status_word` and
/// `wait_word` write. Anything else, including an unrecognised state, stays
/// uncoloured rather than guessed into a meaning.
fn state_tone(word: &str) -> Tone {
    match word {
        "Working" => Tone::Good,
        "Needs you" | "Held" | "Cancelled" => Tone::Warn,
        "Failed" => Tone::Bad,
        "Sent" | "Received" | "Settled" => Tone::Info,
        "Idle" | "No agent" | "Queued" | "Sending" => Tone::Muted,
        _ => Tone::Plain,
    }
}

fn indented(text: &str, width: usize) -> Vec<String> {
    wrap(text, width.saturating_sub(2))
        .into_iter()
        .map(|line| format!("  {line}"))
        .collect()
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Pane transcripts and model output are untrusted text. Terminal control
/// sequences are removed rather than escaped so nothing printed here can move
/// the cursor, repaint the popup, or hide a fact behind a colour run. Bidi and
/// zero-width formatting characters go too: they change what a reader sees
/// without changing the characters.
pub(crate) fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.peek() {
                // CSI: parameter and intermediate bytes up to a final byte.
                Some('[') => {
                    characters.next();
                    for character in characters.by_ref() {
                        if matches!(character, '\u{40}'..='\u{7e}') {
                            break;
                        }
                    }
                }
                // OSC: a string ended by BEL or by ESC backslash.
                Some(']') => {
                    characters.next();
                    while let Some(character) = characters.next() {
                        if character == '\u{7}' {
                            break;
                        }
                        if character == '\u{1b}' {
                            characters.next();
                            break;
                        }
                    }
                }
                // Any other escape swallows the single byte that follows it.
                _ => {
                    characters.next();
                }
            }
            continue;
        }
        out.push(if is_display_unsafe(character) {
            ' '
        } else {
            character
        });
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_display_unsafe(character: char) -> bool {
    character.is_control()
        || matches!(character,
            '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}'
            | '\u{feff}')
}

/// Popup widths are terminal columns, so every measurement here is in display
/// cells rather than characters: one CJK glyph or emoji occupies two, and a
/// combining accent occupies none.
fn count(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn cells(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// Pads to `width` cells. `format!`'s own alignment counts characters, so a
/// name holding two-cell glyphs would be padded two columns too far and push
/// the rest of the row past the edge.
fn pad(text: &str, width: usize) -> String {
    let mut out = text.to_owned();
    out.push_str(&" ".repeat(width.saturating_sub(count(text))));
    out
}

fn pad_left(text: &str, width: usize) -> String {
    let mut out = " ".repeat(width.saturating_sub(count(text)));
    out.push_str(text);
    out
}

fn clip(text: &str, width: usize) -> String {
    if count(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    // One cell is kept for the marker, so a clipped row always says it was
    // clipped rather than ending mid-thought.
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cell = cells(character);
        if used + cell > budget {
            break;
        }
        out.push(character);
        used += cell;
    }
    if out.is_empty() {
        // No room for content and a marker together. One real character says
        // more than a marker does, so keep it when it fits on its own.
        return match text.chars().next() {
            Some(character) if cells(character) <= width => character.to_string(),
            _ => "…".to_owned(),
        };
    }
    out.push('…');
    out
}

/// Splits off as much as fits in `width` cells, always consuming at least one
/// character. A glyph wider than the whole column is replaced by the elision
/// marker: overflowing the pane would draw past its edge, and consuming
/// nothing would loop forever.
fn split_cells(text: &str, width: usize) -> (String, &str) {
    let mut used = 0;
    let mut end = 0;
    for (index, character) in text.char_indices() {
        let cell = cells(character);
        if used + cell > width {
            break;
        }
        end = index + character.len_utf8();
        used += cell;
    }
    if end == 0 {
        let first = text.chars().next().map_or(0, char::len_utf8);
        return ("…".to_owned(), &text[first..]);
    }
    (text[..end].to_owned(), &text[end..])
}

/// Greedy wrap on display cells, so multi-byte and double-width text never
/// splits a character and never runs past the column it was given.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    wrap_hanging(text, width, width)
}

/// `wrap`, with the first line `first` cells wide and every later one `rest`.
fn wrap_hanging(text: &str, first: usize, rest: usize) -> Vec<String> {
    let (first, rest) = (first.max(1), rest.max(1));
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        loop {
            let width = if lines.is_empty() { first } else { rest };
            let room = width
                .saturating_sub(count(&line))
                .saturating_sub(usize::from(!line.is_empty()));
            if count(word) <= room {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
                break;
            }
            // Retried on a fresh line, whose width may differ from this one.
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            let (head, tail) = split_cells(word, width);
            lines.push(head);
            word = tail;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn mention(label: &str) -> String {
    let label = sanitize(label);
    if label.is_empty() {
        "(unnamed)".to_owned()
    } else if label.starts_with('@') {
        label
    } else {
        format!("@{label}")
    }
}

fn host_name(host: &str) -> String {
    match host.trim().to_ascii_lowercase().as_str() {
        "codex" => "Codex".to_owned(),
        "claude" => "Claude".to_owned(),
        "" => String::new(),
        other => capitalize(other),
    }
}

/// Herdr's own pane statuses in plain words. `blocked` means the pane needs a
/// person, which is not the same as waiting on another agent.
fn status_word(status: &str) -> String {
    match status.trim().to_ascii_lowercase().as_str() {
        "" => String::new(),
        "working" | "busy" | "running" => "Working".to_owned(),
        "idle" | "ready" => "Idle".to_owned(),
        "blocked" | "attention" => "Needs you".to_owned(),
        "empty" | "none" => "No agent".to_owned(),
        other => capitalize(other),
    }
}

/// Handoff states in plain words, keeping the distinctions the report makes:
/// queued is not seen, held is not working, and a reply that exists may still
/// not have reached the agent that asked for it.
/// `DeliveryState` in plain words, keeping every distinction it carries.
/// Submitted means the request reached the recipient's host, which is not proof
/// the recipient saw it. Acknowledged means its session recorded receipt, which
/// is not proof it acted. Held can follow a submission whose outcome was never
/// established, so it is a decision for this end, not a delivery failure.
fn wait_word(state: &str) -> String {
    match state.trim().to_ascii_lowercase().as_str() {
        "queued" => "Queued".to_owned(),
        "dispatching" => "Sending".to_owned(),
        "submitted" => "Sent".to_owned(),
        "acknowledged" => "Received".to_owned(),
        "settled" => "Settled".to_owned(),
        "held" => "Held".to_owned(),
        "failed" => "Failed".to_owned(),
        "cancelled" => "Cancelled".to_owned(),
        other => capitalize(other),
    }
}

fn capitalize(text: &str) -> String {
    let text = sanitize(&text.replace(['_', '-'], " "));
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn ago(now_ms: u64, then_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(then_ms) / 1_000;
    match seconds {
        0 => "just now".to_owned(),
        1..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        _ => format!("{}h ago", seconds / 3_600),
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn array(value: Option<&Value>, key: &str) -> Vec<Value> {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}
