use super::*;
use crate::{Clock, IdGenerator, LifecycleRuntime, StateStore};
use serde_json::{Value, json};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
mod rpc;

#[derive(Deserialize)]
#[serde(untagged)]
enum StreamLine {
    Notification {
        method: Method,
        params: Value,
    },
    Snapshot {
        terminals: Vec<Terminal>,
        seq: Option<u64>,
    },
    SnapshotWithoutTerminals(EmptySnapshot),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptySnapshot {
    seq: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Method {
    Event,
    Missed,
    Heartbeat,
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: Kind,
    terminal: Option<String>,
    seq: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Created,
    Restarted,
    Lost,
    Finished,
    Removed,
    Status,
    Foreground,
    Turn,
    Labels,
    #[serde(other)]
    Ignored,
}

#[derive(Clone, Default)]
struct DirtyTerminal {
    seq: Option<u64>,
    resume: bool,
    unavailable: bool,
    removed: bool,
}

#[derive(Clone, Default)]
struct Pending {
    snapshot: Option<Vec<Terminal>>,
    seq: Option<u64>,
    full: bool,
    dirty: BTreeMap<String, DirtyTerminal>,
}

impl Pending {
    fn push(&mut self, line: &str, watches: &BTreeMap<String, OwnerWatch>) -> Result<(), String> {
        match serde_json::from_str::<StreamLine>(line)
            .map_err(|error| format!("Invalid hc event: {error}"))?
        {
            StreamLine::Snapshot { terminals, seq } => self.accept_snapshot(Some(terminals), seq),
            StreamLine::SnapshotWithoutTerminals(EmptySnapshot { seq }) => {
                self.accept_snapshot(None, Some(seq))
            }
            StreamLine::Notification {
                method: Method::Missed,
                params,
            } => {
                let seq = params
                    .get("seq")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| format!("Invalid snapshot sequence: {error}"))?;
                let terminals = params
                    .get("terminals")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| format!("Invalid snapshot terminals: {error}"))?;
                self.accept_snapshot(terminals, seq);
            }
            StreamLine::Notification {
                method: Method::Heartbeat,
                ..
            } => (),
            StreamLine::Notification {
                method: Method::Event,
                params,
            } => {
                let event: Event =
                    serde_json::from_value(params).map_err(|error| error.to_string())?;
                if event
                    .seq
                    .zip(self.seq)
                    .is_some_and(|(event, snapshot)| event <= snapshot)
                {
                    return Ok(());
                }
                if !matches!(event.kind, Kind::Ignored) {
                    let pane = event.terminal.ok_or("hc terminal event has no terminal")?;
                    if pane.contains('/')
                        || (matches!(event.kind, Kind::Foreground) && !watches.contains_key(&pane))
                    {
                        return Ok(());
                    }
                    let dirty = self.dirty.entry(pane).or_default();
                    dirty.seq = dirty.seq.max(event.seq);
                    dirty.resume |=
                        matches!(event.kind, Kind::Status | Kind::Turn | Kind::Foreground);
                    dirty.unavailable |=
                        matches!(event.kind, Kind::Lost | Kind::Finished | Kind::Removed);
                    dirty.removed = matches!(event.kind, Kind::Removed);
                }
            }
        }
        Ok(())
    }

    fn accept_snapshot(&mut self, terminals: Option<Vec<Terminal>>, seq: Option<u64>) {
        if seq
            .zip(self.seq)
            .is_some_and(|(incoming, current)| incoming < current)
        {
            return;
        }
        if let Some(seq) = seq {
            self.dirty
                .retain(|_, change| change.seq.is_none_or(|event| event > seq));
        }
        self.snapshot = terminals;
        self.full = self.snapshot.is_none();
        self.seq = seq.or(self.seq);
    }

    fn is_empty(&self) -> bool {
        !self.full && self.snapshot.is_none() && self.dirty.is_empty()
    }
}

#[derive(PartialEq, Eq)]
struct SessionGeneration {
    session: String,
    pid: String,
    start: String,
}

struct OwnerWatch {
    generation: SessionGeneration,
    due_ms: u64,
}

struct ReadRetry {
    due_ms: u64,
    delay_ms: u64,
}

#[derive(Default)]
pub struct Subscriber {
    spaces: BTreeMap<String, String>,
    terminals: BTreeMap<String, Terminal>,
    unavailable: BTreeMap<String, String>,
    read_retries: BTreeMap<String, ReadRetry>,
    pending: Pending,
    initialized: bool,
    next_wake_ms: Option<u64>,
    owner_watches: BTreeMap<String, OwnerWatch>,
}

impl Subscriber {
    pub fn handle_line<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        line: &str,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        self.handle_lines(std::iter::once(line), runtime)
    }

    pub fn handle_lines<'a, E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        lines: impl IntoIterator<Item = &'a str>,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let mut errors = Vec::new();
        for line in lines {
            if let Err(error) = self.pending.push(line, &self.owner_watches) {
                errors.push(error);
            }
        }
        if let Err(error) = self.reconcile_pending(runtime) {
            errors.push(error);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn discovery(&self) -> TerminalDiscovery {
        TerminalDiscovery {
            terminals: self
                .terminals
                .values()
                .filter(|terminal| {
                    terminal.state == "live" && !self.unavailable.contains_key(&terminal.address)
                })
                .cloned()
                .collect(),
            warnings: self
                .unavailable
                .iter()
                .map(|(terminal, message)| DiscoveryWarning {
                    terminal: terminal.clone(),
                    message: message.clone(),
                })
                .collect(),
        }
    }

    fn snapshots<E: HerdrCommandExecutor + Copy, C, I>(
        &self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<BTreeMap<String, crate::DiscoveredWorktree>, String> {
        let mut spaces: BTreeSet<String> = self
            .terminals
            .values()
            .map(|terminal| terminal.workspace().to_owned())
            .collect();
        spaces.extend(self.spaces.values().cloned());
        spaces.extend(active_spaces(runtime)?);
        if let Some(space) = runtime.workspace_id {
            spaces.insert(space.to_owned());
        }
        spaces
            .into_iter()
            .map(|space| {
                let discovered = HcHost::<E>::space_from_discovery(
                    SpaceLocator::Workspace(&space),
                    self.discovery(),
                )?;
                Ok((space, discovered))
            })
            .collect()
    }

    fn operation<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        operation: &str,
        input: Value,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let snapshot = self.snapshots(runtime)?;
        let _lock = label_lock(runtime.state_directory)?;
        let result = crate::lifecycle::execute_lifecycle_operation_with_discovery(
            operation,
            input,
            runtime,
            Some(&snapshot),
        )?;
        if let Some(drain) = result.get("drain") {
            self.remember_drain(drain, runtime.clock.now_unix_ms());
        }
        Ok(())
    }

    fn schedule_read_retry(&mut self, pane: &str, now: u64) {
        let delay_ms = self
            .read_retries
            .get(pane)
            .map_or(500, |retry| retry.delay_ms.saturating_mul(2).min(30_000));
        self.read_retries.insert(
            pane.to_owned(),
            ReadRetry {
                due_ms: now.saturating_add(delay_ms),
                delay_ms,
            },
        );
    }

    fn remember_drain(&mut self, value: &Value, now: u64) {
        if let Ok(report) = serde_json::from_value::<crate::DrainReport>(value.clone()) {
            let due = if report.awaits_retry() {
                Some(now.saturating_add(500))
            } else {
                report
                    .next_grace_due_ms()
                    .into_iter()
                    .chain(report.next_lease_due_ms)
                    .min()
            };
            if let Some(due) = due {
                self.next_wake_ms =
                    Some(self.next_wake_ms.map_or(due, |previous| previous.min(due)));
            }
        }
    }

    pub fn tick<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let result = self.tick_due(runtime);
        if result.is_err() {
            self.next_wake_ms = Some(runtime.clock.now_unix_ms().saturating_add(500));
        }
        result
    }

    fn tick_due<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let now = runtime.clock.now_unix_ms();
        let mut owner_due = false;
        for (pane, watch) in &mut self.owner_watches {
            if watch.due_ms <= now {
                watch.due_ms = now.saturating_add(500);
                if !self.read_retries.contains_key(pane)
                    && self
                        .terminals
                        .get(pane)
                        .is_some_and(|terminal| terminal.session_owner().is_none())
                {
                    owner_due = true;
                    self.pending.dirty.entry(pane.clone()).or_default();
                }
            }
        }
        let mut read_due = false;
        for (pane, retry) in &self.read_retries {
            if retry.due_ms <= now {
                read_due = true;
                self.pending.dirty.entry(pane.clone()).or_default().resume = true;
            }
        }
        if owner_due || read_due || self.next_wake_ms.is_some_and(|due| due <= now) {
            self.reconcile_pending(runtime)?;
        }
        if self
            .next_wake_ms
            .is_some_and(|due| due <= runtime.clock.now_unix_ms())
        {
            self.next_wake_ms = None;
            let mut errors = Vec::new();
            for space in active_spaces(runtime)? {
                let scoped = LifecycleRuntime {
                    workspace_id: Some(&space),
                    current_pane_id: None,
                    ..*runtime
                };
                let discovered = if self.initialized {
                    HcHost::<E>::space_from_discovery(scoped.space_locator(), self.discovery())?
                } else {
                    runtime.host.space(scoped.space_locator(), None)?
                };
                match crate::delivery::drain_mailboxes_with_discovery(&scoped, discovered, false) {
                    Ok(report) => self.remember_drain(
                        &serde_json::to_value(report).map_err(|error| error.to_string())?,
                        runtime.clock.now_unix_ms(),
                    ),
                    Err(error) => errors.push(error),
                }
                if let Err(error) = self.publish_counters(&scoped) {
                    errors.push(error);
                }
            }
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
        }
        Ok(())
    }

    fn reconcile_pending<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut pending = std::mem::take(&mut self.pending);
        self.pending.seq = pending.seq;
        if !self.initialized && pending.snapshot.is_none() {
            pending.full = true;
        }
        let result = self
            .reconcile_batch(&mut pending, runtime)
            .and_then(|()| self.schedule_owner_watches(&pending, runtime));
        if result.is_err() {
            pending
                .dirty
                .retain(|pane, _| !self.read_retries.contains_key(pane));
            self.pending = pending;
            self.next_wake_ms = Some(runtime.clock.now_unix_ms().saturating_add(500));
        }
        result
    }

    fn schedule_owner_watches<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        pending: &Pending,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let active: BTreeSet<_> = active_spaces(runtime)?.into_iter().collect();
        let mut panes: BTreeSet<_> = BTreeSet::new();
        panes.extend(pending.dirty.keys().cloned());
        if pending.full || pending.snapshot.is_some() {
            panes.extend(self.terminals.keys().cloned());
            panes.extend(self.owner_watches.keys().cloned());
        }
        for pane in panes {
            let generation = self
                .terminals
                .get(&pane)
                .filter(|terminal| {
                    terminal.state == "live" && active.contains(terminal.workspace())
                })
                .and_then(|terminal| {
                    let session = terminal.labels.get("namedrop.session")?;
                    let pid = terminal.labels.get("namedrop.session.pid")?;
                    let start = terminal.labels.get("namedrop.session.start")?;
                    if session.is_empty() || pid.is_empty() || start.is_empty() {
                        return None;
                    }
                    Some(SessionGeneration {
                        session: session.clone(),
                        pid: pid.clone(),
                        start: start.clone(),
                    })
                });
            match generation {
                Some(generation) => {
                    if self
                        .owner_watches
                        .get(&pane)
                        .is_none_or(|watch| watch.generation != generation)
                    {
                        self.owner_watches.insert(
                            pane,
                            OwnerWatch {
                                generation,
                                due_ms: runtime.clock.now_unix_ms().saturating_add(500),
                            },
                        );
                    }
                }
                _ => {
                    self.owner_watches.remove(&pane);
                }
            }
        }
        Ok(())
    }

    fn reconcile_batch<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        pending: &mut Pending,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let _lock = label_lock(runtime.state_directory)?;
        let full = pending.full || pending.snapshot.is_some();
        let mut errors = Vec::new();
        let mut affected = BTreeSet::new();
        let active_spaces: BTreeSet<_> = active_spaces(runtime)?.into_iter().collect();
        let mut store =
            StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
        let mut bound_spaces: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for space in &active_spaces {
            let namespace = crate::StateNamespace::for_space(runtime.session_socket_id, space);
            for binding in store
                .transaction(|tx| tx.active_pane_bindings(&namespace))
                .map_err(|error| error.to_string())?
            {
                self.spaces
                    .entry(binding.pane_id.clone())
                    .or_insert_with(|| space.clone());
                bound_spaces
                    .entry(binding.pane_id)
                    .or_default()
                    .insert(space.clone());
            }
        }
        if full {
            let discovery = if pending.full {
                runtime
                    .host
                    .discover(None)
                    .map_err(|error| error.to_string())?
            } else {
                TerminalDiscovery {
                    terminals: pending.snapshot.clone().unwrap(),
                    warnings: vec![],
                }
            };
            for warning in &discovery.warnings {
                self.schedule_read_retry(&warning.terminal, runtime.clock.now_unix_ms());
            }
            self.unavailable = discovery
                .warnings
                .into_iter()
                .map(|warning| (warning.terminal, warning.message))
                .collect();
            let mut terminals: BTreeMap<_, _> = discovery
                .terminals
                .into_iter()
                .filter(|terminal| !terminal.address.contains('/'))
                .map(|terminal| (terminal.address.clone(), terminal))
                .collect();
            for pane in self.unavailable.keys() {
                if let Some(terminal) = self.terminals.get(pane) {
                    terminals.insert(pane.clone(), terminal.clone());
                }
            }
            self.terminals = terminals;
            self.initialized = true;
            affected.extend(active_spaces.iter().cloned());
        }
        let mut dirty: BTreeMap<String, DirtyTerminal> = if full {
            self.terminals
                .keys()
                .map(|pane| {
                    (
                        pane.clone(),
                        DirtyTerminal {
                            resume: true,
                            ..Default::default()
                        },
                    )
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        for (pane, change) in &pending.dirty {
            let accumulated = dirty.entry(pane.clone()).or_default();
            accumulated.resume |= change.resume;
            accumulated.unavailable |= change.unavailable;
            accumulated.removed = change.removed;
        }
        if full {
            for pane in bound_spaces.keys() {
                dirty.entry(pane.clone()).or_default();
            }
        }
        for (pane, change) in &dirty {
            if let Some(space) = self.spaces.get(pane) {
                affected.insert(space.clone());
            }
            let refresh = if pending.full {
                false
            } else if pending.dirty.contains_key(pane) {
                true
            } else if let Some(terminal) = self.terminals.get(pane) {
                let namespace = crate::StateNamespace::for_space(
                    runtime.session_socket_id,
                    terminal.workspace(),
                );
                let stored = committed_alias(runtime.state_directory, &namespace, pane)?;
                HcHost::<E>::session_needs_reconciliation(terminal)
                    || stored.as_deref().unwrap_or("")
                        != terminal
                            .labels
                            .get("namedrop.name")
                            .map_or("", String::as_str)
                    || bound_spaces.get(pane).is_some_and(|spaces| {
                        terminal.state != "live"
                            || spaces.iter().any(|space| space != terminal.workspace())
                    })
            } else {
                true
            };
            if refresh {
                match runtime.host.terminal(pane, None) {
                    Ok(terminal) => {
                        self.terminals.insert(pane.clone(), terminal);
                        self.unavailable.remove(pane);
                        self.read_retries.remove(pane);
                    }
                    Err(error)
                        if change.removed
                            || (full
                                && matches!(
                                    error.code.as_deref(),
                                    Some("not_found" | "finished")
                                )) =>
                    {
                        self.terminals.remove(pane);
                        self.unavailable.remove(pane);
                        self.read_retries.remove(pane);
                    }
                    Err(error) => {
                        self.unavailable.insert(pane.clone(), error.to_string());
                        self.schedule_read_retry(pane, runtime.clock.now_unix_ms());
                    }
                }
            }
            if self.unavailable.contains_key(pane) {
                continue;
            }
            if let Some(terminal) = self.terminals.get_mut(pane)
                && active_spaces.contains(terminal.workspace())
                && let Err(error) = runtime.host.reconcile_session_fresh(
                    runtime.state_directory,
                    runtime.session_socket_id,
                    terminal,
                    &_lock,
                )
            {
                self.unavailable.insert(pane.clone(), error.to_string());
                errors.push(error.to_string());
            }
        }
        for (pane, terminal) in &self.terminals {
            if !dirty.contains_key(pane) && HcHost::<E>::session_needs_reconciliation(terminal) {
                self.unavailable
                    .entry(pane.clone())
                    .or_insert_with(|| "Session observation awaits a terminal event".to_owned());
            }
        }
        for (pane, change) in &dirty {
            if self.unavailable.contains_key(pane) {
                continue;
            }
            if let Some(mut terminal) = self.terminals.get(pane).cloned() {
                affected.insert(terminal.workspace().to_owned());
                if let Some(space) = self.spaces.get(pane) {
                    affected.insert(space.clone());
                }
                if let Err(error) = self.reconcile_move(&mut terminal, runtime) {
                    errors.push(error);
                    continue;
                }
                self.terminals.insert(pane.clone(), terminal.clone());
                let scoped = LifecycleRuntime {
                    workspace_id: Some(terminal.workspace()),
                    current_pane_id: None,
                    ..*runtime
                };
                if change.resume
                    && let Err(error) = resume_settled_recipient(&terminal, &scoped)
                {
                    errors.push(error);
                }
                if terminal.state == "live"
                    && active_spaces.contains(terminal.workspace())
                    && let Err(error) = self.reconcile_name(&terminal, &scoped)
                {
                    errors.push(error);
                }
            }
            if change.unavailable
                && let Some(space) = self.spaces.get(pane).cloned()
            {
                let scoped = LifecycleRuntime {
                    workspace_id: Some(&space),
                    current_pane_id: None,
                    ..*runtime
                };
                if active_spaces.contains(&space) {
                    let kind = if change.removed {
                        "pane_closed"
                    } else {
                        "pane_exited"
                    };
                    if let Err(error) = self.operation(
                        "event.pane-lifecycle",
                        json!({"type":kind,"pane_id":pane,"workspace_id":space}),
                        &scoped,
                    ) {
                        errors.push(error);
                        continue;
                    }
                }
                if change.removed {
                    self.spaces.remove(pane);
                }
            }
        }
        let attribution = crate::MutationAttribution::new(
            crate::ActorKind::Plugin,
            "namedrop-hc-reconcile",
            runtime.clock.now_unix_ms(),
        );
        let unavailable_panes = self.unavailable.keys().cloned().collect();
        for space in affected {
            let scoped = LifecycleRuntime {
                workspace_id: Some(&space),
                current_pane_id: None,
                ..*runtime
            };
            let namespace = crate::StateNamespace::for_space(runtime.session_socket_id, &space);
            let live = self
                .terminals
                .values()
                .filter(|terminal| terminal.state == "live" && terminal.workspace() == space)
                .filter_map(|terminal| {
                    terminal
                        .session()
                        .map(|session| (terminal.address.clone(), AgentSessionId::new(session)))
                })
                .collect::<Vec<_>>();
            if let Err(error) = store.transaction(|transaction| {
                transaction.reconcile_prompt_recipients(
                    &namespace,
                    &live,
                    &unavailable_panes,
                    &attribution,
                )
            }) {
                errors.push(error.to_string());
            }
            if !active_spaces.contains(&space) {
                continue;
            }
            if let Err(error) = self.operation("event.reconcile", json!({}), &scoped) {
                errors.push(error);
            }
            if let Err(error) = self.publish_counters(&scoped) {
                errors.push(error);
            }
        }
        self.read_retries
            .retain(|pane, _| self.unavailable.contains_key(pane));
        if errors.is_empty() {
            Ok(())
        } else {
            pending.full = false;
            pending.snapshot = None;
            pending.dirty = dirty;
            Err(errors.join("; "))
        }
    }

    fn publish_counters<E: HerdrCommandExecutor + Copy, C, I>(
        &mut self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let Some(space) = runtime.workspace_id else {
            return Ok(());
        };
        let namespace = crate::StateNamespace::for_space(runtime.session_socket_id, space);
        let (waiting, attention, bindings) = StateStore::open(runtime.state_directory)
            .map_err(|error| error.to_string())?
            .transaction(|transaction| {
                let (waiting, _) = transaction.delivery_health(&namespace)?;
                Ok((
                    waiting.to_string(),
                    transaction
                        .unresolved_attention_items(&namespace)?
                        .len()
                        .to_string(),
                    transaction.active_pane_bindings(&namespace)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        for binding in bindings {
            if self.unavailable.contains_key(&binding.pane_id) {
                continue;
            }
            if let Some(terminal) = self.terminals.get_mut(&binding.pane_id) {
                runtime.host.update_labels(
                    terminal,
                    &[
                        ("namedrop.awaiting", &waiting),
                        ("namedrop.attention", &attention),
                    ],
                )?;
                terminal
                    .labels
                    .insert("namedrop.awaiting".into(), waiting.clone());
                terminal
                    .labels
                    .insert("namedrop.attention".into(), attention.clone());
            }
        }
        Ok(())
    }

    fn reconcile_move<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        terminal: &mut Terminal,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let _lock = label_lock(runtime.state_directory)?;
        let pane = &terminal.address;
        let destination = terminal.workspace().to_owned();
        let mut previous: BTreeSet<String> = self.spaces.get(pane).cloned().into_iter().collect();
        let mut store =
            StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
        for space in active_spaces(runtime)? {
            let namespace = crate::StateNamespace::for_space(runtime.session_socket_id, &space);
            if store
                .transaction(|transaction| transaction.active_pane_bindings(&namespace))
                .map_err(|error| error.to_string())?
                .iter()
                .any(|binding| binding.pane_id == *pane)
            {
                previous.insert(space);
            }
        }
        previous.retain(|source| source != &destination);
        if !previous.is_empty() {
            let namespace =
                crate::StateNamespace::for_space(runtime.session_socket_id, &destination);
            let alias = committed_alias(runtime.state_directory, &namespace, pane)?;
            runtime.host.update_labels(
                terminal,
                &[("namedrop.name", alias.as_deref().unwrap_or(""))],
            )?;
            terminal
                .labels
                .insert("namedrop.name".to_owned(), alias.unwrap_or_default());
            self.terminals.insert(pane.clone(), terminal.clone());
        }
        for source in previous {
            let scoped = LifecycleRuntime {
                workspace_id: Some(&source),
                ..*runtime
            };
            if active(&scoped)? {
                self.operation("event.pane-lifecycle", json!({
                    "type":"pane_moved", "previous_pane_id":pane,
                    "previous_workspace_id":source, "previous_tab_id":"",
                    "pane": {"pane_id":pane, "terminal_id":pane, "workspace_id":destination,
                        "tab_id":terminal.labels.get("herdr.tab").cloned().unwrap_or_default(),
                        "focused":false, "agent_status":terminal.status.as_ref().map(|status| status.state.as_str()).unwrap_or("unknown"), "revision":0}
                }), &scoped)?;
            }
        }
        self.spaces.insert(pane.clone(), destination.to_owned());
        Ok(())
    }

    fn reconcile_name<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        terminal: &Terminal,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let _lock = label_lock(runtime.state_directory)?;
        let pane = terminal.address.as_str();
        let requested = terminal
            .labels
            .get("namedrop.name")
            .map_or("", String::as_str);
        let namespace =
            crate::StateNamespace::for_space(runtime.session_socket_id, terminal.workspace());
        let stored = committed_alias(runtime.state_directory, &namespace, pane)?;
        if stored.as_deref().unwrap_or("") == requested {
            return Ok(());
        }
        let observation = std::sync::Mutex::new(terminal.clone());
        let runtime = &LifecycleRuntime {
            host: HcHost {
                transport: runtime.host.transport,
                label_observation: Some(&observation),
            },
            ..*runtime
        };
        if requested.is_empty() {
            if let Some(alias) = stored {
                runtime
                    .host
                    .set_labels(pane, &[("namedrop.name", &alias)])?;
            }
            self.terminals.insert(
                pane.to_owned(),
                observation
                    .into_inner()
                    .map_err(|_| "Label observation lock poisoned")?,
            );
            return Ok(());
        }
        let scoped = LifecycleRuntime {
            current_pane_id: Some(pane),
            ..*runtime
        };
        let snapshot = self.snapshots(&scoped)?;
        let pane_type = StateStore::open(runtime.state_directory)
            .map_err(|error| error.to_string())?
            .transaction(|transaction| {
                Ok(transaction
                    .list_pane_addresses(&namespace)?
                    .into_iter()
                    .find(|address| address.pane_id.as_deref() == Some(pane))
                    .map(|address| address.pane_type))
            })
            .map_err(|error| error.to_string())?
            .or_else(|| match terminal.occupancy() {
                Occupancy::Agent { .. } => Some(crate::PaneType::Agent),
                Occupancy::Terminal => terminal
                    .pid
                    .filter(|pid| {
                        *pid > 0
                            && process_is_shell(*pid)
                            && terminal.foreground.as_ref().is_some_and(|foreground| {
                                i64::from(foreground.pid) == i64::from(*pid)
                            })
                    })
                    .map(|_| crate::PaneType::Terminal),
            });
        let result = match pane_type {
            Some(pane_type) => crate::lifecycle::execute_lifecycle_operation_with_discovery(
                "event.name",
                json!({"alias":requested, "pane_type":pane_type}),
                &scoped,
                Some(&snapshot),
            ),
            None => {
                Err("pane type is uncertain; choose --type terminal or --type agent".to_owned())
            }
        };
        if let Ok(value) = &result
            && let Some(drain) = value.get("drain")
        {
            self.remember_drain(drain, runtime.clock.now_unix_ms());
        }
        let result = match result {
            Err(error) => {
                let committed = committed_alias(runtime.state_directory, &namespace, pane)
                    .map_err(|read| format!("{error}; could not read committed name: {read}"))?;
                let message: String = error.chars().take(1024).collect();
                let requested: String = requested.chars().take(256).collect();
                let response = json!({"requested":requested,"message":message}).to_string();
                runtime
                    .host
                    .set_labels(
                        pane,
                        &[
                            ("namedrop.name", committed.as_deref().unwrap_or("")),
                            ("namedrop.name.error", &response),
                        ],
                    )
                    .map_err(|publish| {
                        format!("{error}; could not publish name rejection: {publish}")
                    })?;
                Err(error)
            }
            Ok(_) => {
                runtime
                    .host
                    .set_labels(pane, &[("namedrop.name.error", "")])?;
                Ok(())
            }
        };
        self.terminals.insert(
            pane.to_owned(),
            observation
                .into_inner()
                .map_err(|_| "Label observation lock poisoned")?,
        );
        result
    }
}

fn committed_alias(
    state: &Path,
    namespace: &crate::StateNamespace,
    pane: &str,
) -> Result<Option<String>, String> {
    StateStore::open(state)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| {
            Ok(transaction
                .list_pane_addresses(namespace)?
                .into_iter()
                .find(|address| !address.retired && address.pane_id.as_deref() == Some(pane))
                .map(|address| address.alias))
        })
        .map_err(|error| error.to_string())
}

fn active<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
) -> Result<bool, String> {
    let Some(space) = runtime.workspace_id else {
        return Ok(false);
    };
    let namespace = crate::StateNamespace::for_space(runtime.session_socket_id, space);
    StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| transaction.worktree_state(&namespace))
        .map(|state| state.is_some_and(|state| state.activated && !state.deactivated))
        .map_err(|error| error.to_string())
}

fn active_spaces<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
) -> Result<Vec<String>, String> {
    Ok(StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .dashboard_spaces()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|row| {
            row.namespace.session_socket_id == runtime.session_socket_id
                && row.state.activated
                && !row.state.deactivated
        })
        .map(|row| row.namespace.space_id.as_str().to_owned())
        .collect())
}

#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
struct BinaryIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl BinaryIdentity {
    fn read(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        })
    }
}

#[derive(Deserialize, serde::Serialize, PartialEq, Eq)]
struct SubscriberRecord {
    pid: u32,
    process_start: String,
    launch_path: PathBuf,
    launch_identity: BinaryIdentity,
    binary: BinaryIdentity,
}

impl SubscriberRecord {
    fn is_running(&self) -> bool {
        process_start(self.pid).as_deref() == Some(self.process_start.as_str())
            && running_binary(self.pid).as_ref() == Some(&self.binary)
    }

    fn should_stop(&self, directory: &Path, state: &Path) -> bool {
        STOP.load(std::sync::atomic::Ordering::Relaxed)
            || !self.is_current(state)
            || std::fs::read(directory.join("stop-request.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
                .is_some_and(|request| request == *self)
    }

    fn is_current(&self, state: &Path) -> bool {
        BinaryIdentity::read(&self.launch_path).as_ref() == Ok(&self.launch_identity)
            && (!state.join("runtime/namedrop").exists()
                || BinaryIdentity::read(&state.join("runtime/current/namedrop")).as_ref()
                    == Ok(&self.binary))
    }
}

#[cfg(target_os = "linux")]
fn running_binary(pid: u32) -> Option<BinaryIdentity> {
    BinaryIdentity::read(&PathBuf::from(format!("/proc/{pid}/exe"))).ok()
}

#[cfg(target_os = "macos")]
fn running_binary(pid: u32) -> Option<BinaryIdentity> {
    BinaryIdentity::read(&crate::terminal::process_executable(pid)?).ok()
}

fn subscriber_directory(state: &Path, socket: &str) -> PathBuf {
    use sha2::Digest;
    state
        .join("hc-subscribers")
        .join(crate::lower_hex(&sha2::Sha256::digest(socket.as_bytes())))
}

fn lock_file(directory: &Path, name: &str) -> Result<std::fs::File, String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|error| error.to_string())?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory.join(name))
        .map_err(|error| error.to_string())
}

fn read_record(directory: &Path) -> Option<SubscriberRecord> {
    serde_json::from_slice(&std::fs::read(directory.join("subscriber.json")).ok()?).ok()
}

fn stop_verified(directory: &Path, record: &SubscriberRecord) -> Result<(), String> {
    use std::io::Write;
    let mut request = lock_file(directory, ".stop-request.json")?;
    request.set_len(0).map_err(|error| error.to_string())?;
    request
        .write_all(&serde_json::to_vec(record).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    std::fs::rename(
        directory.join(".stop-request.json"),
        directory.join("stop-request.json"),
    )
    .map_err(|error| error.to_string())?;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, record.pid, 0) };
        if fd < 0 {
            return Ok(());
        }
        let _fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) };
        if record.is_running()
            && unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd,
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    Ok(())
}

fn wait_for_lifetime(directory: &Path) -> Result<std::fs::File, String> {
    let lifetime = lock_file(directory, "lifetime.lock")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match lifetime.try_lock() {
            Ok(()) => return Ok(lifetime),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(format!("Could not stop HC subscriber: {error}")),
        }
    }
}

pub fn stop(state: &Path, socket: &str) -> Result<(), String> {
    let directory = subscriber_directory(state, socket);
    if !directory.exists() {
        return Ok(());
    }
    let launch = lock_file(&directory, "launch.lock")?;
    launch.lock().map_err(|error| error.to_string())?;
    if let Some(record) = read_record(&directory) {
        stop_verified(&directory, &record)?;
    }
    drop(wait_for_lifetime(&directory)?);
    Ok(())
}

pub fn status(state: &Path, socket: &str) -> Result<(), String> {
    let directory = subscriber_directory(state, socket);
    let record = read_record(&directory).ok_or("The HC event subscriber is missing")?;
    if !record.is_running() {
        return Err("The HC event subscriber is stopped".to_owned());
    }
    if !record.is_current(state) {
        return Err("The HC event subscriber is running an older Namedrop binary".to_owned());
    }
    Ok(())
}

pub fn start_with_environment<E: HerdrCommandExecutor>(
    host: &HcHost<E>,
    executable: &Path,
    state: &Path,
    environment: &crate::environment::EnvironmentSnapshot,
) -> Result<(), HostError> {
    let socket = host.core_identity()?;
    let endpoint = environment.hc_endpoint()?;
    let directory = subscriber_directory(state, &socket);
    let launch = lock_file(&directory, "launch.lock")?;
    launch.lock().map_err(|error| error.to_string())?;
    let launch_identity = BinaryIdentity::read(executable)?;
    let binary = BinaryIdentity::read(&if executable == state.join("runtime/namedrop") {
        state.join("runtime/current/namedrop")
    } else {
        executable.to_owned()
    })?;
    if let Some(record) = read_record(&directory)
        && record.is_running()
    {
        if record.binary == binary
            && record.launch_identity == launch_identity
            && record.launch_path == executable
            && record.is_current(state)
        {
            return Ok(());
        }
        stop_verified(&directory, &record)?;
    }
    drop(wait_for_lifetime(&directory)?);
    let mut command = environment.hc_command(executable);
    command
        .env("HC_SOCKET", endpoint)
        .env("NAMEDROP_SUBSCRIBER_LAUNCH_PATH", executable)
        .args(["--host", "hc", "--plugin-state"])
        .arg(state)
        .arg("events")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not launch HC subscriber: {error}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if read_record(&directory)
            .is_some_and(|record| record.pid == child.id() && record.is_running())
        {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("HC subscriber exited before becoming ready: {status}").into());
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("HC subscriber did not become ready within five seconds"
                .to_owned()
                .into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn request_stop(_: libc::c_int) {
    STOP.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn run_with_environment(
    state: &Path,
    environment: &crate::environment::EnvironmentSnapshot,
) -> Result<(), String> {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_stop as *const () as libc::sighandler_t,
        );
    }
    let transport = rpc::SubscriberRpc::new(environment, state, &STOP)?;
    let host = HcHost::new(&transport);
    let socket = match host.core_identity() {
        Ok(socket) => socket,
        Err(_) if STOP.load(std::sync::atomic::Ordering::Relaxed) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let directory = subscriber_directory(state, &socket);
    let lifetime = lock_file(&directory, "lifetime.lock")?;
    match lifetime.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    let running = std::env::current_exe().map_err(|error| error.to_string())?;
    let launch_path = environment
        .var_os("NAMEDROP_SUBSCRIBER_LAUNCH_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| running.clone());
    let record = SubscriberRecord {
        launch_identity: BinaryIdentity::read(&launch_path)?,
        pid: std::process::id(),
        process_start: process_start(std::process::id()).ok_or("Missing process start identity")?,
        launch_path,
        binary: BinaryIdentity::read(&running)?,
    };
    if !record.is_current(state) {
        return Ok(());
    }
    use std::io::Write;
    let temporary_record = format!(".subscriber-{}.json", record.pid);
    let mut record_file = lock_file(&directory, &temporary_record)?;
    record_file.set_len(0).map_err(|error| error.to_string())?;
    record_file
        .write_all(&serde_json::to_vec(&record).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    std::fs::rename(
        directory.join(temporary_record),
        directory.join("subscriber.json"),
    )
    .map_err(|error| error.to_string())?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let clock = crate::SystemClock;
    let ids = crate::ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: state,
        home_directory: environment.home(),
        agent_config: environment.agent_config(),
        cwd: &cwd,
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: &socket,
        host,
        clock: &clock,
        ids: &ids,
    };
    let mut subscriber = Subscriber::default();
    let mut reconnect: Option<(u8, std::time::Instant)> = None;
    while !record.should_stop(&directory, state) {
        let now = std::time::Instant::now();
        let deadline = if let Some((attempts, started)) = &mut reconnect {
            if *attempts == 3 || started.elapsed() >= Duration::from_secs(15) {
                break;
            }
            let scheduled = *started + Duration::from_secs(u64::from(*attempts) * 5);
            while std::time::Instant::now() < scheduled {
                if record.should_stop(&directory, state) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            *attempts += 1;
            eprintln!("Namedrop hc reconnect attempt {attempts}/3");
            (*started + Duration::from_secs(15))
                .min(std::time::Instant::now() + Duration::from_secs(2))
        } else {
            now + Duration::from_secs(2)
        };
        let (mut stream, snapshot) = match transport.subscribe(deadline) {
            Ok(subscription) => subscription,
            Err(error) => {
                eprintln!("Namedrop hc reconnect: {error}");
                reconnect.get_or_insert((0, std::time::Instant::now()));
                continue;
            }
        };
        reconnect = None;
        subscriber.pending = Pending::default();
        let mut snapshot = Some(snapshot);
        'stream: loop {
            let line = match snapshot.take() {
                Some(snapshot) => Ok(Some(snapshot)),
                None => stream.next(
                    std::time::Instant::now() + Duration::from_millis(250),
                    &STOP,
                ),
            };
            if record.should_stop(&directory, state) {
                break;
            }
            let guard = loop {
                match crate::runtime_command_guard(state, Some(Duration::from_millis(250))) {
                    Ok(guard) => break guard,
                    Err(error) => {
                        eprintln!("Namedrop hc runtime: {error}");
                        if record.should_stop(&directory, state) {
                            break 'stream;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
            };
            if record.should_stop(&directory, state) {
                break;
            }
            match line {
                Ok(Some(line)) => {
                    let mut lines = vec![line];
                    while lines.len() < 1024 {
                        match stream.next(std::time::Instant::now(), &STOP) {
                            Ok(Some(line)) => lines.push(line),
                            _ => break,
                        }
                    }
                    if let Err(error) =
                        subscriber.handle_lines(lines.iter().map(String::as_str), &runtime)
                    {
                        eprintln!("Namedrop hc event: {error}");
                    }
                }
                Err(_) => break,
                Ok(None) => (),
            }
            if let Err(error) = subscriber.tick(&runtime) {
                eprintln!("Namedrop hc delivery: {error}");
            }
            drop(guard);
        }
        reconnect.get_or_insert((0, std::time::Instant::now()));
    }
    Ok(())
}

fn resume_settled_recipient<E: HerdrCommandExecutor, C: Clock, I>(
    terminal: &Terminal,
    runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
) -> Result<(), String> {
    let Some(status_ms) = terminal.status_ms else {
        return Ok(());
    };
    let Occupancy::Agent {
        session: Some(session),
        state: AgentState::Foreground(Some(StatusWord::Idle | StatusWord::Done)),
        ..
    } = terminal.occupancy()
    else {
        return Ok(());
    };
    if terminal.state != "live" || runtime.workspace_id != Some(terminal.workspace()) {
        return Ok(());
    }
    let namespace =
        crate::StateNamespace::for_space(runtime.session_socket_id, terminal.workspace());
    let attribution = crate::MutationAttribution::new(
        crate::ActorKind::Plugin,
        "namedrop-hc-settle",
        runtime.clock.now_unix_ms(),
    );
    let _prompt_guard = prompt_lock(
        runtime.state_directory,
        runtime.session_socket_id,
        &terminal.address,
    )?;
    StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| {
            if transaction.worktree_state(&namespace)?.is_none() {
                return Ok(());
            }
            transaction.resume_prompt_after_settle(
                &namespace,
                &terminal.address,
                &AgentSessionId::new(session),
                status_ms,
                &attribution,
            )?;
            transaction.resume_mailbox_after_settle(
                &namespace,
                &terminal.address,
                &AgentSessionId::new(session),
                status_ms,
                &attribution,
            )
        })
        .map_err(|error| error.to_string())
}
