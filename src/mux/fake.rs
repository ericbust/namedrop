//! `FakeMux`, an in-memory multiplexer for tests. It models spaces, tabs,
//! panes, screens, and processes, and answers the port's questions from that
//! world rather than matching command arguments. It is compiled for the
//! crate's own tests and under the `test-support` feature, which the crate's
//! dev-dependency on itself turns on, so tests in `tests/` reach it as
//! `namedrop::mux::fake::FakeMux` and a release build leaves it out.

use super::{
    AgentIdentity, AgentKind, AgentState, Applied, Budget, Checkout, Depth, Effect, Endpoint,
    InputKind, InputOutcome, Key, MuxAct, MuxConnector, MuxError, MuxInput, MuxKind, MuxRead,
    PaneFacts, PaneId, PaneInput, PaneProcess, Recipient, Retry, Scope, Screen, ScreenRequest,
    SessionClaim, SpaceView, Style, Survey, TabId, TerminalId, Wrap,
    gate::{self, ChannelError, RawChannel, StatusTap},
};
use crate::{AgentRuntime, AgentSessionId, SpaceId, WorktreeId};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Input that may have reached a pane, in the order it was typed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sent {
    pub pane: PaneId,
    pub input: SentInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SentInput {
    Prompt(String),
    Line(String),
    Keys {
        recipient: Recipient,
        keys: Vec<Key>,
    },
}

/// An input the code tried to type, whether or not it reached the pane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attempt {
    pub pane: PaneId,
    pub input: SentInput,
    pub outcome: InputOutcome,
}

/// A survey's scope without the space or checkout it names, so a world can
/// say which surveys it answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeKind {
    Space,
    PanesOf,
    CheckoutOf,
    All,
}

/// A survey's scope as the read log keeps it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Surveyed {
    Space(SpaceId),
    PanesOf(SpaceId),
    CheckoutOf(PathBuf),
    All,
}

impl Surveyed {
    fn of(scope: Scope<'_>) -> Self {
        match scope {
            Scope::Space(id) => Self::Space(id.clone()),
            Scope::PanesOf(id) => Self::PanesOf(id.clone()),
            Scope::CheckoutOf(path) => Self::CheckoutOf(path.to_path_buf()),
            Scope::All => Self::All,
        }
    }

    pub fn kind(&self) -> ScopeKind {
        match self {
            Self::Space(_) => ScopeKind::Space,
            Self::PanesOf(_) => ScopeKind::PanesOf,
            Self::CheckoutOf(_) => ScopeKind::CheckoutOf,
            Self::All => ScopeKind::All,
        }
    }
}

/// A read the code made, logged before the fake answers it, so a read that
/// failed is logged too.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Read {
    Survey {
        scope: Surveyed,
        depth: Depth,
    },
    Screen {
        pane: PaneId,
        request: ScreenRequest,
    },
    Process(PaneId),
    TabLabel(TabId),
    IntegrationRoot,
}

/// The screen reads a pane answers, as a Herdr fake answered one `pane read`
/// argv. `lines: None` answers any number of lines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenShape {
    pub wrap: Wrap,
    pub style: Style,
    pub lines: Option<usize>,
}

impl ScreenShape {
    fn answers(self, request: ScreenRequest) -> bool {
        self.wrap == request.wrap
            && self.style == request.style
            && self.lines.is_none_or(|lines| lines == request.lines)
    }
}

/// A port call whose next use fails with an injected error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Op {
    Survey,
    Screen,
    Process,
    TabLabel,
    IntegrationRoot,
    Apply,
    /// The next prompt is typed and then fails, so it is `Ambiguous`.
    Deliver,
}

/// The checkout a space listing names for a space. Herdr stamps it on the
/// space when the space opens a worktree, and answers `worktree list
/// --workspace` from the checkout itself, so the two can differ.
#[derive(Clone, Debug)]
pub(crate) enum Stamp {
    /// The listing names the space's checkout.
    Checkout,
    /// The listing names this checkout, or none.
    Other(Option<Checkout>),
}

#[derive(Clone, Debug)]
pub(crate) struct FakeSpace {
    pub(crate) id: SpaceId,
    pub(crate) label: Option<String>,
    pub(crate) number: Option<u32>,
    /// What `worktree list --workspace` answers for the space.
    pub(crate) checkout: Option<Checkout>,
    pub(crate) stamp: Stamp,
    /// `false` leaves the space out of `workspace list`, though a lookup by
    /// its id still answers.
    pub(crate) listed: bool,
    pub(crate) panes: Vec<FakePane>,
}

impl FakeSpace {
    /// The checkout `workspace list` and `workspace get` name.
    pub(crate) fn stamped(&self) -> Option<&Checkout> {
        match &self.stamp {
            Stamp::Checkout => self.checkout.as_ref(),
            Stamp::Other(stamped) => stamped.as_ref(),
        }
    }

    /// The panes a pane listing names, which leaves out hidden ones.
    pub(crate) fn listed_panes(&self) -> impl Iterator<Item = &FakePane> {
        self.panes.iter().filter(|pane| !pane.hidden)
    }

    /// Matches the Herdr adapter, so stored state keys do not depend on
    /// which implementation a test uses.
    pub(crate) fn worktree(&self) -> WorktreeId {
        match &self.checkout {
            Some(checkout) => WorktreeId::new(checkout.path.to_string_lossy()),
            None => WorktreeId::new(format!("herdr-space:{}", self.id.as_str())),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FakePane {
    pub(crate) facts: PaneFacts,
    /// `None` until a test gives the pane a screen.
    pub(crate) screen: Option<String>,
    /// `None` answers a read of any shape, unless reads are strict.
    pub(crate) screen_shape: Option<ScreenShape>,
    /// `None` until a test gives the pane a process.
    pub(crate) process: Option<PaneProcess>,
    /// Left out of every pane listing. Reads and input still reach it.
    pub(crate) hidden: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct World {
    pub(crate) spaces: Vec<FakeSpace>,
    pub(crate) tab_labels: BTreeMap<TabId, String>,
    pub(crate) integration_root: Option<PathBuf>,
    sent: Vec<Sent>,
    attempts: Vec<Attempt>,
    /// Each read with the budget of the connection that made it.
    reads: Vec<(Read, Option<Budget>)>,
    effects: Vec<Effect>,
    attempted_effects: Vec<Effect>,
    /// Outcomes for a pane's next inputs, first first.
    scripted: BTreeMap<PaneId, VecDeque<InputOutcome>>,
    /// The outcome of every input to a pane once its scripted ones are used.
    answers: BTreeMap<PaneId, InputOutcome>,
    failures: Vec<(Op, MuxError)>,
    /// Every status change, per pane, in order. A gate tap reads the tail.
    changes: BTreeMap<PaneId, Vec<AgentState>>,
    prompts_stay_settled: bool,
    strict_reads: bool,
    /// `None` answers every survey.
    surveys: Option<Vec<(ScopeKind, Depth)>>,
}

fn gone(what: super::Gone, id: &str) -> MuxError {
    MuxError::Gone(what, format!("{id} is gone"))
}

impl World {
    pub(crate) fn pane(&self, pane: &PaneId) -> Option<&FakePane> {
        self.spaces
            .iter()
            .flat_map(|space| &space.panes)
            .find(|candidate| &candidate.facts.id == pane)
    }

    pub(crate) fn pane_mut(&mut self, pane: &PaneId) -> Option<&mut FakePane> {
        self.spaces
            .iter_mut()
            .flat_map(|space| &mut space.panes)
            .find(|candidate| &candidate.facts.id == pane)
    }

    fn take_failure(&mut self, op: Op) -> Result<(), MuxError> {
        match self.failures.iter().position(|(failing, _)| *failing == op) {
            Some(index) => Err(self.failures.remove(index).1),
            None => Ok(()),
        }
    }

    /// What a read the test did not give answers: the type's empty value,
    /// or under `strict_reads` an error.
    fn unscripted<T: Default>(&self, what: String) -> Result<T, MuxError> {
        if self.strict_reads {
            return Err(MuxError::Unavailable(format!("the test gave no {what}")));
        }
        Ok(T::default())
    }

    fn scripted_outcome(&mut self, pane: &PaneId) -> Option<InputOutcome> {
        self.scripted
            .get_mut(pane)
            .and_then(VecDeque::pop_front)
            .or_else(|| self.answers.get(pane).cloned())
    }

    fn known_pane(&mut self, pane: &str) -> &mut FakePane {
        self.pane_mut(&PaneId::new(pane))
            .unwrap_or_else(|| panic!("the world has no pane {pane}"))
    }

    fn known_space(&mut self, space: &str) -> &mut FakeSpace {
        self.spaces
            .iter_mut()
            .find(|candidate| candidate.id.as_str() == space)
            .unwrap_or_else(|| panic!("the world has no space {space}"))
    }

    #[cfg(test)]
    pub(crate) fn record_sent(&mut self, sent: Sent) {
        self.sent.push(sent);
    }

    pub(crate) fn set_state(&mut self, pane: &PaneId, state: AgentState) {
        if let Some(found) = self.pane_mut(pane) {
            found.facts.state = Some(state.clone());
            self.changes.entry(pane.clone()).or_default().push(state);
        }
    }

    fn panes(space: &FakeSpace) -> Vec<PaneFacts> {
        space
            .listed_panes()
            .map(|pane| pane.facts.clone())
            .collect()
    }

    /// What a checkout listing reports: the checkout and its key, but no
    /// space label or number, and no repository name.
    fn checkout_view(space: &FakeSpace, panes: bool) -> SpaceView {
        SpaceView {
            id: space.id.clone(),
            label: None,
            number: None,
            checkout: space.checkout.clone().map(|checkout| Checkout {
                repo_name: None,
                ..checkout
            }),
            worktree: Some(space.worktree()),
            panes: if panes {
                Self::panes(space)
            } else {
                Vec::new()
            },
        }
    }

    /// What a space listing reports: the label, the number, and the stamped
    /// checkout without its label or branch.
    fn listed_view(space: &FakeSpace) -> SpaceView {
        SpaceView {
            id: space.id.clone(),
            label: space.label.clone(),
            number: space.number,
            checkout: space.stamped().cloned().map(|checkout| Checkout {
                label: None,
                branch: None,
                ..checkout
            }),
            worktree: None,
            panes: Vec::new(),
        }
    }

    /// Mirrors what the Herdr adapter reads for each scope and depth, so a
    /// test sees the same partial facts from either implementation.
    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        let kind = Surveyed::of(scope).kind();
        if let Some(surveys) = &self.surveys
            && !surveys.contains(&(kind, depth))
        {
            return Err(MuxError::Unavailable(format!(
                "the test answers no {kind:?} survey at {depth:?} depth"
            )));
        }
        let panes = depth == Depth::Panes;
        let spaces = match scope {
            Scope::Space(id) => {
                let space = self
                    .spaces
                    .iter()
                    .find(|space| &space.id == id)
                    .ok_or_else(|| gone(super::Gone::Space, id.as_str()))?;
                vec![if panes {
                    Self::checkout_view(space, true)
                } else {
                    Self::listed_view(space)
                }]
            }
            Scope::PanesOf(id) => {
                let space = self
                    .spaces
                    .iter()
                    .find(|space| &space.id == id)
                    .ok_or_else(|| gone(super::Gone::Space, id.as_str()))?;
                vec![SpaceView {
                    id: space.id.clone(),
                    label: None,
                    number: None,
                    checkout: None,
                    worktree: None,
                    panes: Self::panes(space),
                }]
            }
            Scope::CheckoutOf(path) => {
                let owners: Vec<_> = self
                    .spaces
                    .iter()
                    .filter(|space| {
                        space
                            .checkout
                            .as_ref()
                            .is_some_and(|checkout| checkout.path == path)
                    })
                    .collect();
                match owners.as_slice() {
                    [owner] => vec![Self::checkout_view(owner, panes)],
                    [] => {
                        return Err(MuxError::Unavailable(format!(
                            "no space holds the checkout {}",
                            path.display()
                        )));
                    }
                    _ => {
                        return Err(MuxError::Unavailable(format!(
                            "more than one space holds the checkout {}",
                            path.display()
                        )));
                    }
                }
            }
            Scope::All if panes => self
                .spaces
                .iter()
                .filter(|space| space.listed_panes().next().is_some())
                .map(|space| SpaceView {
                    id: space.id.clone(),
                    label: None,
                    number: None,
                    checkout: None,
                    worktree: None,
                    panes: Self::panes(space),
                })
                .collect(),
            Scope::All => self
                .spaces
                .iter()
                .filter(|space| space.listed)
                .map(Self::listed_view)
                .collect(),
        };
        Ok(Survey { spaces })
    }
}

/// An in-memory multiplexer. Clones share one world, so a test keeps a handle
/// while the code under test holds another.
#[derive(Clone)]
pub struct FakeMux {
    shared: Arc<Shared>,
    emulated: bool,
    /// What `connect` was given. `None` for a handle the builder made.
    connection: Option<Connection>,
}

#[derive(Clone)]
struct Connection {
    budget: Budget,
    cancelled: Arc<AtomicBool>,
}

type InputObserver = Arc<dyn Fn(&FakeMux, &Attempt) + Send + Sync>;
type ReadHook = Arc<dyn Fn(&FakeMux, &Read) -> Result<(), MuxError> + Send + Sync>;

struct Shared {
    endpoint: Endpoint,
    world: Mutex<World>,
    observers: Mutex<Vec<InputObserver>>,
    read_hooks: Mutex<Vec<ReadHook>>,
}

/// A poisoned lock still guards a consistent world. A test whose observer
/// panicked must still be able to read the world.
fn unpoisoned<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeMux {
    /// `FakeMux::world().space("workspace-1").pane("pane-author").claude().session("session-author").idle().build()`
    pub fn world() -> WorldBuilder {
        WorldBuilder {
            endpoint: Endpoint {
                kind: MuxKind::Herdr,
                key: "fake-socket".to_owned(),
            },
            world: World::default(),
        }
    }

    /// Prompts go through the real `gate::emulate_prompt`, as on a
    /// multiplexer without a native gate. By default the fake gates like
    /// Herdr: it refuses a busy agent before typing and checks no identity.
    pub fn emulated(mut self) -> Self {
        self.emulated = true;
        self
    }

    /// A prompted agent keeps its status instead of starting a turn, so an
    /// emulated delivery cannot be confirmed.
    pub fn prompts_stay_settled(self) -> Self {
        self.lock().prompts_stay_settled = true;
        self
    }

    /// A screen, process, tab label, or integration root the test did not
    /// give the world fails to read, as a Herdr fake fails a command it was
    /// not given an answer for. A screen answers only the reads its pane's
    /// `screen_shape` names. Nothing turns strict reads off again.
    pub fn strict_reads(self) -> Self {
        self.lock().strict_reads = true;
        self
    }

    /// Only surveys of these scopes and depths answer. Any other fails with
    /// `MuxError::Unavailable`, as a Herdr fake fails a listing it was not
    /// given.
    ///
    /// # Panics
    ///
    /// When the world already answers only some surveys, so that a clone
    /// cannot widen what a `StrictSpace` answers.
    pub fn strict_surveys(self, answered: &[(ScopeKind, Depth)]) -> Self {
        {
            let mut world = self.lock();
            if let Some(surveys) = &world.surveys {
                panic!("the world already answers only the surveys {surveys:?}");
            }
            world.surveys = Some(answered.to_vec());
        }
        self
    }

    /// `observe` runs once for every input the code tried, with its outcome,
    /// after the fake answered it and before the delivery that tried it
    /// returns. So it sees declined and refused input, and the store as it
    /// was while the input was dispatched. It is handed the world rather
    /// than holding it, and the world is unlocked while it runs, so it can
    /// change what the pane shows. Observers run in the order they were
    /// added. An observer that models how the pane reacts checks
    /// `attempt.outcome` first, because a declined or refused input typed
    /// nothing. Under `emulated()` the gate has confirmed or given up on a
    /// prompt before any observer runs, so an observer cannot change the
    /// status inside its confirmation window.
    pub fn on_input(&self, observe: impl Fn(&FakeMux, &Attempt) + Send + Sync + 'static) {
        unpoisoned(&self.shared.observers).push(Arc::new(observe));
    }

    /// `hook` runs before each read is answered, with the world unlocked, so
    /// it can change what the read answers. An `Err` fails the read, so a
    /// hook that fails every `Scope::PanesOf` of one space stands for a space
    /// Herdr cannot list. Hooks run in the order they were added, and the
    /// first `Err` ends the read.
    pub fn on_read(
        &self,
        hook: impl Fn(&FakeMux, &Read) -> Result<(), MuxError> + Send + Sync + 'static,
    ) {
        unpoisoned(&self.shared.read_hooks).push(Arc::new(hook));
    }

    /// Drops every observer and read hook, with any handle to the world
    /// they captured.
    fn release_hooks(&self) {
        unpoisoned(&self.shared.observers).clear();
        unpoisoned(&self.shared.read_hooks).clear();
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, World> {
        unpoisoned(&self.shared.world)
    }

    /// The input that was typed, in the order typing started.
    pub fn sent(&self) -> Vec<Sent> {
        self.lock().sent.clone()
    }

    pub fn sent_to(&self, pane: &str) -> Vec<Sent> {
        let pane = PaneId::new(pane);
        self.lock()
            .sent
            .iter()
            .filter(|sent| sent.pane == pane)
            .cloned()
            .collect()
    }

    /// Every input the code tried to type, with its outcome, including the
    /// ones that were declined or refused before anything was typed. They
    /// are in the order their deliveries finished, which differs from
    /// `sent()`'s order only when inputs overlap on several threads.
    pub fn attempts(&self) -> Vec<Attempt> {
        self.lock().attempts.clone()
    }

    /// Every read the code made, in order, including the ones that failed.
    pub fn reads(&self) -> Vec<Read> {
        self.lock()
            .reads
            .iter()
            .map(|(read, _)| read.clone())
            .collect()
    }

    /// Every read, with the budget of the connection that made it, or
    /// `None` for a read made through a handle the builder made.
    pub fn budgeted_reads(&self) -> Vec<(Read, Option<Budget>)> {
        self.lock().reads.clone()
    }

    /// The budget `connect` gave this handle, or `None` for a handle the
    /// builder made. A read hook is handed the connection that reads, so it
    /// can check the budget the read runs under.
    pub fn budget(&self) -> Option<Budget> {
        self.connection.as_ref().map(|connection| connection.budget)
    }

    /// Whether the code set the cancellation flag it gave `connect`.
    pub fn cancelled(&self) -> bool {
        self.connection
            .as_ref()
            .is_some_and(|connection| connection.cancelled.load(Ordering::SeqCst))
    }

    /// The pane's facts as the world holds them, hidden or not. Unlike a
    /// survey it is not logged and runs no hook, so a hook can inspect the
    /// world without re-entering itself.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn facts(&self, pane: &str) -> PaneFacts {
        self.lock().known_pane(pane).facts.clone()
    }

    /// How many times the code read `pane`'s screen, in any shape.
    pub fn screen_reads(&self, pane: &str) -> usize {
        let pane = PaneId::new(pane);
        self.reads()
            .iter()
            .filter(
                |read| matches!(read, Read::Screen { pane: read_pane, .. } if *read_pane == pane),
            )
            .count()
    }

    /// How many surveys of this scope kind and depth the code made.
    pub fn surveys_made(&self, kind: ScopeKind, depth: Depth) -> usize {
        self.reads()
            .iter()
            .filter(|read| match read {
                Read::Survey { scope, depth: made } => scope.kind() == kind && *made == depth,
                _ => false,
            })
            .count()
    }

    /// The effects that applied.
    pub fn effects(&self) -> Vec<Effect> {
        self.lock().effects.clone()
    }

    /// Every effect the code tried to apply, including the ones that failed.
    pub fn attempted_effects(&self) -> Vec<Effect> {
        self.lock().attempted_effects.clone()
    }

    /// The agent in `pane` changes status, as a running agent would.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn set_state(&self, pane: &str, state: AgentState) {
        let mut world = self.lock();
        world.known_pane(pane);
        world.set_state(&PaneId::new(pane), state);
    }

    /// `pane` now reports `session`, as when another program replaces the
    /// one that was running there. Like the builder's `.session(..)`, it
    /// keeps the agent's kind and label, and makes a free pane an agent whose
    /// kind is unreported.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn set_session(&self, pane: &str, session: &str) {
        unreported_agent(&mut self.lock().known_pane(pane).facts).session =
            SessionClaim::Reported(AgentSessionId::new(session));
    }

    /// `pane` shows `text`, or with `None` nothing a read can answer. The
    /// pane keeps its `screen_shape`.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn set_screen(&self, pane: &str, text: Option<&str>) {
        self.lock().known_pane(pane).screen = text.map(str::to_owned);
    }

    /// The pane is gone for good: no listing names it, and reads and input
    /// answer that it is gone. For a pane that comes back, use `hide_pane`.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn close_pane(&self, pane: &str) {
        let mut world = self.lock();
        world.known_pane(pane);
        let pane = PaneId::new(pane);
        for space in &mut world.spaces {
            space.panes.retain(|candidate| candidate.facts.id != pane);
        }
    }

    /// No pane listing names `pane` until `show_pane`, as when Herdr stops
    /// listing a pane that closed or moved. Reads and input still reach it,
    /// as the old fakes still answered them, and it keeps its place in its
    /// space's listing.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn hide_pane(&self, pane: &str) {
        self.lock().known_pane(pane).hidden = true;
    }

    /// Pane listings name `pane` again, in its old place.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn show_pane(&self, pane: &str) {
        self.lock().known_pane(pane).hidden = false;
    }

    /// Opens panes in `space` mid-test. `describe` is handed a builder whose
    /// current space is `space`, and adds panes with the builder's own pane
    /// methods, as in `|space| space.pane("pane-2").claude().idle()`. They
    /// join the end of the space's listing.
    ///
    /// # Panics
    ///
    /// When the world has no such space, when `describe` adds a space, a
    /// tab label, or an integration root, or when a new pane's id is taken.
    pub fn open_pane(&self, space: &str, describe: impl FnOnce(WorldBuilder) -> WorldBuilder) {
        let blank = FakeSpace {
            panes: Vec::new(),
            ..self.lock().known_space(space).clone()
        };
        let World {
            mut spaces,
            tab_labels,
            integration_root,
            ..
        } = describe(WorldBuilder {
            endpoint: self.shared.endpoint.clone(),
            world: World {
                spaces: vec![blank],
                ..World::default()
            },
        })
        .world;
        assert!(
            spaces.len() == 1 && tab_labels.is_empty() && integration_root.is_none(),
            "open_pane describes panes only"
        );
        let panes = spaces.pop().expect("one space").panes;
        let mut world = self.lock();
        for pane in &panes {
            assert!(
                world.pane(&pane.facts.id).is_none(),
                "the world already has a pane {}",
                pane.facts.id.as_str()
            );
        }
        world.known_space(space).panes.extend(panes);
    }

    /// `space` now holds `checkout`, or none, as `worktree list --workspace`
    /// answers. A space whose listing names its checkout names the new one.
    ///
    /// # Panics
    ///
    /// When the world has no such space.
    pub fn set_checkout(&self, space: &str, checkout: Option<&Path>) {
        self.lock().known_space(space).checkout = checkout.map(bare_checkout);
    }

    /// The space listing names `space` with `label`, or with none.
    ///
    /// # Panics
    ///
    /// When the world has no such space.
    pub fn set_label(&self, space: &str, label: Option<&str>) {
        self.lock().known_space(space).label = label.map(str::to_owned);
    }

    /// `pane` reports an agent of `kind`, named as the builder names it, and
    /// keeps its session. With `None` it reports no agent kind: a pane that
    /// still reports a session stays an agent whose kind is unreported, and
    /// one that reports none becomes a free terminal pane.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn set_agent(&self, pane: &str, kind: Option<AgentKind>) {
        let mut world = self.lock();
        let facts = &mut world.known_pane(pane).facts;
        match kind {
            Some(kind) => {
                let agent = unreported_agent(facts);
                agent.label = agent_word(&kind);
                agent.kind = kind;
            }
            None => {
                if let Some(agent) = &mut facts.agent {
                    agent.kind = AgentKind::Unreported;
                    agent.label = None;
                }
                drop_empty_agent(facts);
            }
        }
    }

    /// `pane` reports no session. An agent whose kind is unreported then
    /// reports nothing, so the pane becomes a free terminal pane.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn clear_session(&self, pane: &str) {
        let mut world = self.lock();
        let facts = &mut world.known_pane(pane).facts;
        if let Some(agent) = &mut facts.agent {
            agent.session = SessionClaim::None;
        }
        drop_empty_agent(facts);
    }

    /// `pane`'s process, or with `None` nothing a read can answer.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn set_process(&self, pane: &str, process: Option<PaneProcess>) {
        self.lock().known_pane(pane).process = process;
    }

    /// Where Namedrop's Herdr integration is installed, or with `None`
    /// nothing a read can answer.
    pub fn set_integration_root(&self, root: Option<&Path>) {
        self.lock().integration_root = root.map(Path::to_path_buf);
    }

    /// The next input to `pane` returns `outcome` without consulting the
    /// world. Scripted outcomes queue per pane. `Delivered` and `Ambiguous`
    /// are recorded as sent. A `StrictSpace` fails the test for a script no
    /// input used.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn script(&self, pane: &str, outcome: InputOutcome) {
        let mut world = self.lock();
        world.known_pane(pane);
        world
            .scripted
            .entry(PaneId::new(pane))
            .or_default()
            .push_back(outcome);
    }

    /// Every input to `pane` that no `script` covers returns `outcome`
    /// without consulting the world, as `script` does. An answer covers any
    /// number of inputs, none included.
    ///
    /// # Panics
    ///
    /// When the world has no such pane.
    pub fn answer(&self, pane: &str, outcome: InputOutcome) {
        let mut world = self.lock();
        world.known_pane(pane);
        world.answers.insert(PaneId::new(pane), outcome);
    }

    /// The next call of `op` fails with `error`. A `StrictSpace` fails the
    /// test for an injection no call used. For a failure that lasts, use
    /// `on_read`.
    pub fn fail(&self, op: Op, error: MuxError) {
        self.lock().failures.push((op, error));
    }

    /// `MuxInput::deliver` minus the lock token, which only `pane_effects`
    /// can mint. Records the attempt, then shows it to the observers.
    pub(crate) fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome {
        let attempt = Attempt {
            pane: pane.clone(),
            input: sent_input(kind),
            outcome: self.attempt(pane, kind),
        };
        self.lock().attempts.push(attempt.clone());
        let observers = unpoisoned(&self.shared.observers).clone();
        for observe in observers {
            observe(self, &attempt);
        }
        attempt.outcome
    }

    fn attempt(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome {
        let scripted = self.lock().scripted_outcome(pane);
        if let Some(outcome) = scripted {
            if matches!(
                outcome,
                InputOutcome::Delivered | InputOutcome::Ambiguous(_)
            ) {
                self.typed(pane, sent_input(kind));
            }
            return outcome;
        }
        if self.lock().pane(pane).is_none() {
            return InputOutcome::Declined(
                Retry::OnEvent,
                format!("pane {} is gone", pane.as_str()),
            );
        }
        let InputKind::Prompt(request) = kind else {
            self.typed(pane, sent_input(kind));
            return InputOutcome::Delivered;
        };
        if self.emulated {
            return gate::emulate_prompt(&Channel(self), pane, &request);
        }
        let admitted = {
            let world = self.lock();
            let facts = &world.pane(pane).expect("checked above").facts;
            facts.agent.is_some()
                && facts.state.as_ref().is_some_and(|state| {
                    state.accepts_prompt()
                        || (*state == AgentState::Working
                            && request.admit == super::Admit::BetweenTurns)
                })
        };
        if !admitted {
            return InputOutcome::Declined(
                Retry::OnEvent,
                format!("pane {} cannot take a prompt now", pane.as_str()),
            );
        }
        match self.type_prompt(pane, request.text) {
            Ok(()) => InputOutcome::Delivered,
            Err(error) => InputOutcome::Ambiguous(error.0),
        }
    }

    fn typed(&self, pane: &PaneId, input: SentInput) {
        self.lock().sent.push(Sent {
            pane: pane.clone(),
            input,
        });
    }

    fn type_prompt(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError> {
        let mut world = self.lock();
        if world.pane(pane).is_none() {
            return Err(ChannelError(format!("pane {} is gone", pane.as_str())));
        }
        world.sent.push(Sent {
            pane: pane.clone(),
            input: SentInput::Prompt(text.to_owned()),
        });
        if let Err(error) = world.take_failure(Op::Deliver) {
            return Err(ChannelError(format!("{error:?}")));
        }
        if !world.prompts_stay_settled {
            world.set_state(pane, AgentState::Working);
        }
        Ok(())
    }

    /// Logs `read` and runs the read hooks with the world unlocked.
    fn read(&self, read: Read) -> Result<(), MuxError> {
        self.lock().reads.push((read.clone(), self.budget()));
        let hooks = unpoisoned(&self.shared.read_hooks).clone();
        hooks.iter().try_for_each(|hook| hook(self, &read))
    }
}

impl MuxRead for FakeMux {
    fn endpoint(&self) -> &Endpoint {
        &self.shared.endpoint
    }

    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        self.read(Read::Survey {
            scope: Surveyed::of(scope),
            depth,
        })?;
        let mut world = self.lock();
        world.take_failure(Op::Survey)?;
        world.survey(scope, depth)
    }

    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError> {
        self.read(Read::Screen {
            pane: pane.clone(),
            request,
        })?;
        let mut world = self.lock();
        world.take_failure(Op::Screen)?;
        let found = world
            .pane(pane)
            .ok_or_else(|| gone(super::Gone::Pane, pane.as_str()))?;
        let answers = match found.screen_shape {
            Some(shape) => shape.answers(request),
            None => !world.strict_reads,
        };
        let screen = match &found.screen {
            Some(screen) if answers => screen.clone(),
            _ => world.unscripted(format!(
                "screen for {} read as {:?} {:?} with {} lines",
                pane.as_str(),
                request.wrap,
                request.style,
                request.lines
            ))?,
        };
        let lines: Vec<_> = screen.lines().collect();
        let text = lines[lines.len().saturating_sub(request.lines)..].join("\n");
        Ok(Screen { text })
    }

    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError> {
        self.read(Read::Process(pane.clone()))?;
        let mut world = self.lock();
        world.take_failure(Op::Process)?;
        match &world
            .pane(pane)
            .ok_or_else(|| gone(super::Gone::Pane, pane.as_str()))?
            .process
        {
            Some(process) => Ok(process.clone()),
            None => world.unscripted(format!("process for {}", pane.as_str())),
        }
    }

    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError> {
        self.read(Read::TabLabel(tab.clone()))?;
        let mut world = self.lock();
        world.take_failure(Op::TabLabel)?;
        match world.tab_labels.get(tab) {
            Some(label) => Ok(Some(label.clone())),
            None => world.unscripted(format!("label for {}", tab.as_str())),
        }
    }

    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError> {
        self.read(Read::IntegrationRoot)?;
        let mut world = self.lock();
        world.take_failure(Op::IntegrationRoot)?;
        match &world.integration_root {
            Some(root) => Ok(Some(root.clone())),
            None => world.unscripted("integration root".to_owned()),
        }
    }
}

impl MuxAct for FakeMux {
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError> {
        let mut world = self.lock();
        world.attempted_effects.push(effect.clone());
        world.take_failure(Op::Apply)?;
        let mut applied = Applied::Done;
        match effect {
            Effect::Identify { pane, alias, texts } => {
                let pane = world
                    .pane_mut(pane)
                    .ok_or_else(|| gone(super::Gone::Pane, pane.as_str()))?;
                pane.facts.title = Some(format!("@{alias}"));
                pane.facts.idle_label = Some(texts.ready.clone());
            }
            Effect::Badges { pane, .. } => {
                world
                    .pane(pane)
                    .ok_or_else(|| gone(super::Gone::Pane, pane.as_str()))?;
            }
            Effect::RenameTab { tab, label } => {
                world.tab_labels.insert(tab.clone(), label.clone());
            }
            Effect::OpenPopup(popup) => {
                applied =
                    Applied::Opened(serde_json::json!({"opened_for": popup.target.id.as_str()}));
            }
            Effect::Notify { .. } | Effect::ReloadKeybindings => {}
        }
        world.effects.push(effect.clone());
        Ok(applied)
    }
}

impl MuxInput for FakeMux {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome {
        self.send(input.pane(), input.kind())
    }
}

impl MuxConnector for FakeMux {
    /// The connection keeps `budget` and `cancelled`, which `budget()`,
    /// `cancelled()`, and `budgeted_reads()` report. The fake enforces
    /// neither, so a read hook that models a stalled, oversized, or
    /// cancelled read checks them on the connection it is handed.
    fn connect(
        &self,
        endpoint: &str,
        budget: Budget,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Arc<dyn super::Mux>, MuxError> {
        if endpoint != self.shared.endpoint.key {
            return Err(MuxError::Unavailable(format!(
                "no multiplexer answers at {endpoint}"
            )));
        }
        Ok(Arc::new(FakeMux {
            connection: Some(Connection { budget, cancelled }),
            ..self.clone()
        }))
    }
}

struct Channel<'a>(&'a FakeMux);

struct Tap<'a> {
    mux: &'a FakeMux,
    pane: PaneId,
    next: usize,
}

impl StatusTap for Tap<'_> {
    fn next_change(&mut self, _within: Duration) -> Option<AgentState> {
        let change = self
            .mux
            .lock()
            .changes
            .get(&self.pane)
            .and_then(|changes| changes.get(self.next).cloned());
        if change.is_some() {
            self.next += 1;
        }
        change
    }
}

impl<'a> RawChannel for Channel<'a> {
    type Tap = Tap<'a>;

    fn facts(
        &self,
        pane: &PaneId,
    ) -> Result<(Option<AgentIdentity>, Option<AgentState>), ChannelError> {
        self.0
            .lock()
            .pane(pane)
            .map(|pane| (pane.facts.agent.clone(), pane.facts.state.clone()))
            .ok_or_else(|| ChannelError(format!("pane {} is gone", pane.as_str())))
    }

    fn tap(&self, pane: &PaneId) -> Result<Tap<'a>, ChannelError> {
        let next = self.0.lock().changes.get(pane).map_or(0, Vec::len);
        Ok(Tap {
            mux: self.0,
            pane: pane.clone(),
            next,
        })
    }

    fn paste_submit(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError> {
        self.0.type_prompt(pane, text)
    }
}

/// Builds a world one space and pane at a time. Pane methods apply to the
/// pane added last, and space methods to the space added last.
pub struct WorldBuilder {
    endpoint: Endpoint,
    world: World,
}

impl WorldBuilder {
    /// The `session_socket_id` the fake answers to.
    pub fn endpoint(mut self, key: &str) -> Self {
        self.endpoint.key = key.to_owned();
        self
    }

    pub fn space(mut self, id: &str) -> Self {
        let number = u32::try_from(self.world.spaces.len() + 1).ok();
        self.world.spaces.push(FakeSpace {
            id: SpaceId::new(id),
            label: None,
            number,
            checkout: None,
            stamp: Stamp::Checkout,
            listed: true,
            panes: Vec::new(),
        });
        self
    }

    /// `workspace list` leaves the space out. A lookup by its id still
    /// answers.
    pub fn unlisted(mut self) -> Self {
        self.current_space().listed = false;
        self
    }

    /// The space listing names `path`, of the repository `repo_name`, as the
    /// space's checkout, whatever `worktree list --workspace` answers.
    pub fn stamped(mut self, path: impl AsRef<Path>, repo_name: Option<&str>) -> Self {
        self.current_space().stamp = Stamp::Other(Some(Checkout {
            repo_name: repo_name.map(str::to_owned),
            ..bare_checkout(path.as_ref())
        }));
        self
    }

    /// The space listing names no checkout for the space.
    pub fn unstamped(mut self) -> Self {
        self.current_space().stamp = Stamp::Other(None);
        self
    }

    pub fn label(mut self, label: &str) -> Self {
        self.current_space().label = Some(label.to_owned());
        self
    }

    pub fn checkout(mut self, path: impl AsRef<Path>) -> Self {
        self.current_space().checkout = Some(bare_checkout(path.as_ref()));
        self
    }

    pub fn branch(mut self, branch: &str) -> Self {
        self.current_checkout().branch = Some(branch.to_owned());
        self
    }

    pub fn repo(mut self, key: &str, name: &str) -> Self {
        let checkout = self.current_checkout();
        checkout.repo_key = Some(key.to_owned());
        checkout.repo_name = Some(name.to_owned());
        self
    }

    /// A free terminal pane in the current space, in the space's first tab.
    pub fn pane(mut self, id: &str) -> Self {
        let space = self.current_space();
        let tab = TabId::new(format!("{}:tab-1", space.id.as_str()));
        let facts = PaneFacts {
            id: PaneId::new(id),
            terminal: TerminalId::new(format!("terminal-{id}")),
            tab,
            space: space.id.clone(),
            cwd: None,
            title: None,
            agent: None,
            state: None,
            idle_label: None,
            viewport_rows: None,
            reports_session: true,
        };
        space.panes.push(FakePane {
            facts,
            screen: None,
            screen_shape: None,
            process: None,
            hidden: false,
        });
        self
    }

    /// No pane listing names the pane until `FakeMux::show_pane`.
    pub fn hidden(mut self) -> Self {
        self.current_pane().hidden = true;
        self
    }

    /// With `false`, a pane listing leaves out the pane's `agent_session`
    /// field. By default it reports the field, null when there is no session.
    pub fn reports_session(mut self, reports: bool) -> Self {
        self.current_pane().facts.reports_session = reports;
        self
    }

    pub fn tab(mut self, tab: &str) -> Self {
        self.current_pane().facts.tab = TabId::new(tab);
        self
    }

    pub fn tab_label(mut self, tab: &str, label: &str) -> Self {
        self.world
            .tab_labels
            .insert(TabId::new(tab), label.to_owned());
        self
    }

    pub fn terminal(mut self, terminal: &str) -> Self {
        self.current_pane().facts.terminal = TerminalId::new(terminal);
        self
    }

    pub fn viewport_rows(mut self, rows: u64) -> Self {
        self.current_pane().facts.viewport_rows = Some(rows);
        self
    }

    pub fn cwd(mut self, cwd: &str) -> Self {
        self.current_pane().facts.cwd = Some(cwd.to_owned());
        self
    }

    pub fn title(mut self, title: &str) -> Self {
        self.current_pane().facts.title = Some(title.to_owned());
        self
    }

    pub fn claude(self) -> Self {
        self.agent(AgentKind::Known(AgentRuntime::ClaudeCode))
    }

    pub fn codex(self) -> Self {
        self.agent(AgentKind::Known(AgentRuntime::Codex))
    }

    /// An agent the port does not recognize, named as the multiplexer names it.
    pub fn other_agent(self, name: &str) -> Self {
        self.agent(AgentKind::Other(name.to_owned()))
    }

    fn agent(mut self, kind: AgentKind) -> Self {
        let agent = self.current_agent();
        agent.label = agent_word(&kind);
        agent.kind = kind;
        self
    }

    pub fn session(mut self, session: &str) -> Self {
        self.current_agent().session = SessionClaim::Reported(AgentSessionId::new(session));
        self
    }

    pub fn idle(self) -> Self {
        self.state(AgentState::Idle)
    }

    pub fn working(self) -> Self {
        self.state(AgentState::Working)
    }

    pub fn done(self) -> Self {
        self.state(AgentState::Done)
    }

    pub fn blocked(self) -> Self {
        self.state(AgentState::Blocked)
    }

    pub fn state(mut self, state: AgentState) -> Self {
        self.current_pane().facts.state = Some(state);
        self
    }

    pub fn idle_label(mut self, label: &str) -> Self {
        self.current_pane().facts.idle_label = Some(label.to_owned());
        self
    }

    pub fn screen(mut self, text: &str) -> Self {
        self.current_pane().screen = Some(text.to_owned());
        self
    }

    /// The pane's screen answers only reads of this shape. Without strict
    /// reads, a read of another shape answers an empty screen.
    pub fn screen_shape(mut self, shape: ScreenShape) -> Self {
        self.current_pane().screen_shape = Some(shape);
        self
    }

    pub fn process(mut self, process: PaneProcess) -> Self {
        self.current_pane().process = Some(process);
        self
    }

    pub fn integration_root(mut self, root: impl AsRef<Path>) -> Self {
        self.world.integration_root = Some(root.as_ref().to_path_buf());
        self
    }

    pub fn build(self) -> FakeMux {
        FakeMux {
            shared: Arc::new(Shared {
                endpoint: self.endpoint,
                world: Mutex::new(self.world),
                observers: Mutex::default(),
                read_hooks: Mutex::default(),
            }),
            emulated: false,
            connection: None,
        }
    }

    fn current_space(&mut self) -> &mut FakeSpace {
        self.world
            .spaces
            .last_mut()
            .expect("call .space(..) before describing a space")
    }

    fn current_checkout(&mut self) -> &mut Checkout {
        self.current_space()
            .checkout
            .as_mut()
            .expect("call .checkout(..) before describing a checkout")
    }

    fn current_pane(&mut self) -> &mut FakePane {
        self.current_space()
            .panes
            .last_mut()
            .expect("call .pane(..) before describing a pane")
    }

    fn current_agent(&mut self) -> &mut AgentIdentity {
        unreported_agent(&mut self.current_pane().facts)
    }
}

fn sent_input(kind: InputKind<'_>) -> SentInput {
    match kind {
        InputKind::Prompt(request) => SentInput::Prompt(request.text.to_owned()),
        InputKind::Line(line) => SentInput::Line(line.to_owned()),
        InputKind::Keys { recipient, keys } => SentInput::Keys {
            recipient,
            keys: keys.to_vec(),
        },
    }
}

fn bare_checkout(path: &Path) -> Checkout {
    Checkout {
        path: path.to_path_buf(),
        label: None,
        branch: None,
        repo_key: None,
        repo_name: None,
    }
}

/// The word the builder names an agent of `kind` by.
fn agent_word(kind: &AgentKind) -> Option<String> {
    match kind {
        AgentKind::Known(AgentRuntime::ClaudeCode) => Some("claude".to_owned()),
        AgentKind::Known(AgentRuntime::Codex) => Some("codex".to_owned()),
        AgentKind::Other(name) => Some(name.clone()),
        AgentKind::Unreported => None,
    }
}

/// A pane that reports neither an agent kind nor a session is a free
/// terminal pane, as the Herdr adapter reads it.
fn drop_empty_agent(facts: &mut PaneFacts) {
    if facts.agent.as_ref().is_some_and(|agent| {
        agent.kind == AgentKind::Unreported && agent.session == SessionClaim::None
    }) {
        facts.agent = None;
    }
}

/// The pane's agent, made one whose kind is unreported if the pane was free.
fn unreported_agent(facts: &mut PaneFacts) -> &mut AgentIdentity {
    facts.agent.get_or_insert(AgentIdentity {
        kind: AgentKind::Unreported,
        label: None,
        session: SessionClaim::None,
    })
}

mod strict;
#[cfg(test)]
mod tests;

pub use strict::{CHECKOUT_LISTINGS, EVERY_LISTING, Listing, StrictSpace, Takes, listings, runs};
