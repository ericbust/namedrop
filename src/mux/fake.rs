//! `FakeMux`, an in-memory multiplexer for tests. It models spaces, tabs,
//! panes, screens, and processes, and answers the port's questions from that
//! world rather than matching command arguments. Always compiled, so tests in
//! `tests/` reach it as `namedrop::mux::fake::FakeMux`.

use super::{
    AgentIdentity, AgentKind, AgentState, Applied, Checkout, Depth, Effect, Endpoint, InputKind,
    InputOutcome, Key, MuxAct, MuxConnector, MuxError, MuxInput, MuxKind, MuxRead, PaneFacts,
    PaneId, PaneInput, PaneProcess, Recipient, Retry, Scope, Screen, ScreenRequest, SessionClaim,
    SpaceView, Survey, TabId, TerminalId,
    gate::{self, ChannelError, RawChannel, StatusTap},
};
use crate::{AgentRuntime, AgentSessionId, SpaceId, WorktreeId};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
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

#[derive(Clone, Debug)]
pub(crate) struct FakeSpace {
    pub(crate) id: SpaceId,
    pub(crate) label: Option<String>,
    pub(crate) number: Option<u32>,
    pub(crate) checkout: Option<Checkout>,
    pub(crate) panes: Vec<FakePane>,
}

impl FakeSpace {
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
    /// `None` until a test gives the pane a process.
    pub(crate) process: Option<PaneProcess>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct World {
    pub(crate) spaces: Vec<FakeSpace>,
    pub(crate) tab_labels: BTreeMap<TabId, String>,
    pub(crate) integration_root: Option<PathBuf>,
    sent: Vec<Sent>,
    effects: Vec<Effect>,
    scripted: VecDeque<InputOutcome>,
    failures: Vec<(Op, MuxError)>,
    /// Every status change, per pane, in order. A gate tap reads the tail.
    changes: BTreeMap<PaneId, Vec<AgentState>>,
    prompts_stay_settled: bool,
    strict_reads: bool,
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

    /// What a read the test did not script answers: the type's empty value,
    /// or under `strict_reads` an error.
    fn unscripted<T: Default>(&self, what: String) -> Result<T, MuxError> {
        if self.strict_reads {
            return Err(MuxError::Unavailable(format!("the test gave no {what}")));
        }
        Ok(T::default())
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
        space.panes.iter().map(|pane| pane.facts.clone()).collect()
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
            checkout: space.checkout.clone().map(|checkout| Checkout {
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
                .filter(|space| !space.panes.is_empty())
                .map(|space| SpaceView {
                    id: space.id.clone(),
                    label: None,
                    number: None,
                    checkout: None,
                    worktree: None,
                    panes: Self::panes(space),
                })
                .collect(),
            Scope::All => self.spaces.iter().map(Self::listed_view).collect(),
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
}

type PromptObserver = Box<dyn Fn(&PaneId, &str) + Send + Sync>;

struct Shared {
    endpoint: Endpoint,
    world: Mutex<World>,
    on_prompt: OnceLock<PromptObserver>,
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
    /// not given an answer for.
    pub fn strict_reads(self) -> Self {
        self.lock().strict_reads = true;
        self
    }

    /// `observe` runs as each prompt is typed, before the delivery that typed
    /// it returns, so it sees the store as the pane's agent would.
    ///
    /// # Panics
    ///
    /// When the world already has an observer.
    pub fn on_prompt(&self, observe: impl Fn(&PaneId, &str) + Send + Sync + 'static) {
        assert!(
            self.shared.on_prompt.set(Box::new(observe)).is_ok(),
            "a world takes one prompt observer"
        );
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, World> {
        self.shared
            .world
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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

    pub fn effects(&self) -> Vec<Effect> {
        self.lock().effects.clone()
    }

    /// The agent in `pane` changes status, as a running agent would.
    pub fn set_state(&self, pane: &str, state: AgentState) {
        self.lock().set_state(&PaneId::new(pane), state);
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
        let mut world = self.lock();
        let found = world
            .pane_mut(&PaneId::new(pane))
            .unwrap_or_else(|| panic!("the world has no pane {pane}"));
        unreported_agent(&mut found.facts).session =
            SessionClaim::Reported(AgentSessionId::new(session));
    }

    pub fn set_screen(&self, pane: &str, text: &str) {
        if let Some(found) = self.lock().pane_mut(&PaneId::new(pane)) {
            found.screen = Some(text.to_owned());
        }
    }

    pub fn close_pane(&self, pane: &str) {
        let pane = PaneId::new(pane);
        for space in &mut self.lock().spaces {
            space.panes.retain(|candidate| candidate.facts.id != pane);
        }
    }

    /// The next delivery returns `outcome` without consulting the world.
    /// `Delivered` and `Ambiguous` are recorded as sent.
    pub fn script(&self, outcome: InputOutcome) {
        self.lock().scripted.push_back(outcome);
    }

    /// The next call of `op` fails with `error`.
    pub fn fail(&self, op: Op, error: MuxError) {
        self.lock().failures.push((op, error));
    }

    /// `MuxInput::deliver` minus the lock token, which only `pane_effects`
    /// can mint.
    pub(crate) fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome {
        let scripted = self.lock().scripted.pop_front();
        if let Some(outcome) = scripted {
            if matches!(
                outcome,
                InputOutcome::Delivered | InputOutcome::Ambiguous(_)
            ) {
                self.record(pane, &kind);
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
            self.record(pane, &kind);
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

    fn record(&self, pane: &PaneId, kind: &InputKind<'_>) {
        let input = match *kind {
            InputKind::Prompt(request) => SentInput::Prompt(request.text.to_owned()),
            InputKind::Line(line) => SentInput::Line(line.to_owned()),
            InputKind::Keys { recipient, keys } => SentInput::Keys {
                recipient,
                keys: keys.to_vec(),
            },
        };
        self.typed(Sent {
            pane: pane.clone(),
            input,
        });
    }

    /// Records `sent` and shows a prompt to the observer. The world is
    /// unlocked while the observer runs, so it may read the fake.
    fn typed(&self, sent: Sent) {
        self.lock().sent.push(sent.clone());
        if let (SentInput::Prompt(text), Some(observe)) = (&sent.input, self.shared.on_prompt.get())
        {
            observe(&sent.pane, text);
        }
    }

    fn type_prompt(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError> {
        if self.lock().pane(pane).is_none() {
            return Err(ChannelError(format!("pane {} is gone", pane.as_str())));
        }
        self.typed(Sent {
            pane: pane.clone(),
            input: SentInput::Prompt(text.to_owned()),
        });
        let mut world = self.lock();
        if let Err(error) = world.take_failure(Op::Deliver) {
            return Err(ChannelError(format!("{error:?}")));
        }
        if !world.prompts_stay_settled {
            world.set_state(pane, AgentState::Working);
        }
        Ok(())
    }
}

impl MuxRead for FakeMux {
    fn endpoint(&self) -> &Endpoint {
        &self.shared.endpoint
    }

    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        let mut world = self.lock();
        world.take_failure(Op::Survey)?;
        world.survey(scope, depth)
    }

    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError> {
        let mut world = self.lock();
        world.take_failure(Op::Screen)?;
        let screen = match &world
            .pane(pane)
            .ok_or_else(|| gone(super::Gone::Pane, pane.as_str()))?
            .screen
        {
            Some(screen) => screen.clone(),
            None => world.unscripted(format!("screen for {}", pane.as_str()))?,
        };
        let lines: Vec<_> = screen.lines().collect();
        let text = lines[lines.len().saturating_sub(request.lines)..].join("\n");
        Ok(Screen { text })
    }

    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError> {
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
        let mut world = self.lock();
        world.take_failure(Op::TabLabel)?;
        match world.tab_labels.get(tab) {
            Some(label) => Ok(Some(label.clone())),
            None => world.unscripted(format!("label for {}", tab.as_str())),
        }
    }

    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError> {
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
    fn connect(
        &self,
        endpoint: &str,
        _budget: super::Budget,
        _cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Arc<dyn super::Mux>, MuxError> {
        if endpoint != self.shared.endpoint.key {
            return Err(MuxError::Unavailable(format!(
                "no multiplexer answers at {endpoint}"
            )));
        }
        Ok(Arc::new(self.clone()))
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
            panes: Vec::new(),
        });
        self
    }

    pub fn label(mut self, label: &str) -> Self {
        self.current_space().label = Some(label.to_owned());
        self
    }

    pub fn checkout(mut self, path: impl AsRef<Path>) -> Self {
        self.current_space().checkout = Some(Checkout {
            path: path.as_ref().to_path_buf(),
            label: None,
            branch: None,
            repo_key: None,
            repo_name: None,
        });
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
            process: None,
        });
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
        self.agent(AgentKind::Known(AgentRuntime::ClaudeCode), "claude")
    }

    pub fn codex(self) -> Self {
        self.agent(AgentKind::Known(AgentRuntime::Codex), "codex")
    }

    /// An agent the port does not recognize, named as the multiplexer names it.
    pub fn other_agent(self, name: &str) -> Self {
        self.agent(AgentKind::Other(name.to_owned()), name)
    }

    fn agent(mut self, kind: AgentKind, label: &str) -> Self {
        let agent = self.current_agent();
        agent.kind = kind;
        agent.label = Some(label.to_owned());
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
                on_prompt: OnceLock::new(),
            }),
            emulated: false,
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

/// The pane's agent, made one whose kind is unreported if the pane was free.
fn unreported_agent(facts: &mut PaneFacts) -> &mut AgentIdentity {
    facts.agent.get_or_insert(AgentIdentity {
        kind: AgentKind::Unreported,
        label: None,
        session: SessionClaim::None,
    })
}

#[cfg(test)]
mod tests;
