//! Agents Channels started, remembered in memory until their session connects
//! or their pane goes away. Claude and Codex give no reliable signal on their
//! first-run screens, so while one is remembered the space asks the person to
//! look at its pane, and a session Herdr reports is connected by naming the
//! pane again.
use super::http::Front;
use super::snapshot::{SpaceKey, StartingAgent};
use crate::store::dashboard::LiveMember;
use crate::{HerdrCommandExecutor, LifecycleRuntime, ProcessHerdr, StateNamespace, StateStore};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
    thread,
    time::Duration,
};

const TICK: Duration = Duration::from_millis(500);
const FAST_CHECK_MS: i64 = 2_000;
const SLOW_CHECK_MS: i64 = 5_000;
const FAST_FOR_MS: i64 = 60_000;
const WATCH_FOR_MS: i64 = 30 * 60_000;

#[derive(Clone, Default)]
pub(crate) struct Launches(Arc<Mutex<Registry>>);

#[derive(Default)]
struct Registry {
    launches: Vec<Launch>,
    /// Moves on every change, so the hub publishes a space whose card left.
    revision: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Launch {
    pub namespace: StateNamespace,
    pub address_id: String,
    pub alias: String,
    pub pane_id: String,
    pub started_at_ms: i64,
    next_check_ms: i64,
    /// The Herdr session the pane was last named for; one session gets one try.
    named_for: Option<String>,
}

impl Launch {
    pub fn new(
        namespace: StateNamespace,
        address_id: String,
        alias: String,
        pane_id: String,
        started_at_ms: i64,
    ) -> Self {
        Self {
            namespace,
            address_id,
            alias,
            pane_id,
            started_at_ms,
            next_check_ms: started_at_ms.saturating_add(FAST_CHECK_MS),
            named_for: None,
        }
    }

    fn is(&self, namespace: &StateNamespace, address_id: &str) -> bool {
        self.namespace == *namespace && self.address_id == address_id
    }
}

impl Launches {
    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn record(&self, launch: Launch) {
        let mut registry = self.registry();
        registry
            .launches
            .retain(|known| !known.is(&launch.namespace, &launch.address_id));
        registry.launches.push(launch);
        registry.revision += 1;
    }

    pub fn forget(&self, namespace: &StateNamespace, address_id: &str) {
        let mut registry = self.registry();
        let before = registry.launches.len();
        registry
            .launches
            .retain(|known| !known.is(namespace, address_id));
        if registry.launches.len() != before {
            registry.revision += 1;
        }
    }

    pub fn revision(&self) -> u64 {
        self.registry().revision
    }

    /// The space's remembered launches as the snapshot reads them.
    pub fn starting(&self, namespace: &StateNamespace) -> Vec<(String, i64)> {
        self.registry()
            .launches
            .iter()
            .filter(|launch| launch.namespace == *namespace)
            .map(|launch| (launch.address_id.clone(), launch.started_at_ms))
            .collect()
    }

    /// Launches whose next check has come, each rescheduled as it is taken.
    fn take_due(&self, now_ms: i64) -> Vec<Launch> {
        let mut registry = self.registry();
        let mut due = Vec::new();
        for launch in &mut registry.launches {
            if now_ms < launch.next_check_ms {
                continue;
            }
            match check_interval_ms(now_ms.saturating_sub(launch.started_at_ms)) {
                Some(interval) => {
                    launch.next_check_ms = now_ms.saturating_add(interval);
                    due.push(launch.clone());
                }
                None => launch.next_check_ms = i64::MAX,
            }
        }
        due
    }

    fn named(&self, launch: &Launch, session: String) {
        let mut registry = self.registry();
        if let Some(known) = registry
            .launches
            .iter_mut()
            .find(|known| known.is(&launch.namespace, &launch.address_id))
        {
            known.named_for = Some(session);
        }
    }
}

pub(crate) fn as_starting(starting: &[(String, i64)]) -> Vec<StartingAgent<'_>> {
    starting
        .iter()
        .map(|(address_id, started_at_ms)| StartingAgent {
            address_id,
            started_at_ms: *started_at_ms,
        })
        .collect()
}

/// Every 2 seconds for the first minute, then every 5, and not after 30 minutes.
fn check_interval_ms(age_ms: i64) -> Option<i64> {
    if age_ms < FAST_FOR_MS {
        Some(FAST_CHECK_MS)
    } else if age_ms < WATCH_FOR_MS {
        Some(SLOW_CHECK_MS)
    } else {
        None
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Observed {
    /// Missing, retired, or no longer bound to a pane.
    AddressGone,
    Occupied,
    PaneGone,
    Pane {
        session: Option<String>,
    },
}

#[derive(Debug, Eq, PartialEq)]
enum Step {
    Forget,
    Name(String),
    Wait,
}

fn step(observed: Observed, named_for: Option<&str>) -> Step {
    match observed {
        Observed::AddressGone | Observed::Occupied | Observed::PaneGone => Step::Forget,
        Observed::Pane {
            session: Some(session),
        } if named_for != Some(session.as_str()) => Step::Name(session),
        Observed::Pane { .. } => Step::Wait,
    }
}

pub(crate) fn watch(front: Arc<Front>) {
    while !front.restarting.load(Ordering::Acquire) {
        thread::sleep(TICK);
        for launch in front.launches.take_due(super::hub::now_ms()) {
            check(&front, &launch);
        }
    }
}

fn check(front: &Front, launch: &Launch) {
    let Some(observed) = observe(front.files.state(), launch) else {
        return;
    };
    match step(observed, launch.named_for.as_deref()) {
        Step::Wait => {}
        Step::Forget => front.launches.forget(&launch.namespace, &launch.address_id),
        Step::Name(session) => {
            let Ok(_admitted) = front.admit_mutation() else {
                return;
            };
            let named = run_lifecycle(
                front.files.state(),
                &launch.namespace,
                Some(&launch.pane_id),
                "name",
                json!({"alias": launch.alias}),
            );
            if named.is_ok_and(|result| result["registered"] == true) {
                front.launches.forget(&launch.namespace, &launch.address_id);
            } else {
                front.launches.named(launch, session);
            }
        }
    }
}

/// `None` when the state or Herdr can't be read this time.
fn observe(state: &Path, launch: &Launch) -> Option<Observed> {
    let mut store = StateStore::open_read_only(state).ok()??;
    store.set_busy_timeout(Duration::from_millis(100)).ok()?;
    match store
        .dashboard_live_member(&SpaceKey::of(&launch.namespace), &launch.address_id)
        .ok()?
    {
        LiveMember::Missing | LiveMember::Gone => return Some(Observed::AddressGone),
        LiveMember::Bound { binding, .. } if binding.agent_session_id.is_some() => {
            return Some(Observed::Occupied);
        }
        LiveMember::Bound { binding, .. } if binding.pane_id != launch.pane_id => {
            return Some(Observed::AddressGone);
        }
        LiveMember::Bound { .. } => {}
    }
    let output = ProcessHerdr::default()
        .with_socket(Some(launch.namespace.session_socket_id.clone()))
        .execute(&[
            "pane",
            "list",
            "--workspace",
            launch.namespace.space_id.as_str(),
        ])
        .ok()?;
    let panes: Value = serde_json::from_str(&output.stdout).ok()?;
    let pane = panes["result"]["panes"]
        .as_array()?
        .iter()
        .find(|pane| pane["pane_id"] == launch.pane_id.as_str());
    Some(match pane {
        None => Observed::PaneGone,
        Some(pane) => Observed::Pane {
            session: pane["agent_session"]["value"]
                .as_str()
                .filter(|session| !session.is_empty())
                .map(str::to_owned),
        },
    })
}

/// Runs a lifecycle operation in one space as the person would from a pane
/// there, against that space's own Herdr socket.
pub(crate) fn run_lifecycle(
    state: &Path,
    namespace: &StateNamespace,
    pane: Option<&str>,
    operation: &str,
    input: Value,
) -> Result<Value, String> {
    let directories = crate::InheritedDirectories::from_environment();
    let herdr = ProcessHerdr::default().with_socket(Some(namespace.session_socket_id.clone()));
    let ids = crate::ProcessIds::default();
    crate::execute_lifecycle_operation(
        operation,
        input,
        &LifecycleRuntime {
            state_directory: state,
            home_directory: directories.home(),
            agent_config: directories.agent_config(),
            cwd: Path::new("/"),
            workspace_id: Some(namespace.space_id.as_str()),
            current_pane_id: pane,
            session_socket_id: &namespace.session_socket_id,
            herdr: &herdr,
            clock: &crate::SystemClock,
            ids: &ids,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(started_at_ms: i64) -> Launch {
        Launch::new(
            StateNamespace::for_space("socket", "w1"),
            "address".into(),
            "coder".into(),
            "w1:p2".into(),
            started_at_ms,
        )
    }

    #[test]
    fn checks_every_two_seconds_then_five_and_stop_after_half_an_hour() {
        let launches = Launches::default();
        launches.record(launch(0));
        assert!(launches.take_due(1_999).is_empty());
        assert_eq!(launches.take_due(2_000).len(), 1);
        assert!(launches.take_due(3_999).is_empty());
        assert_eq!(launches.take_due(4_000).len(), 1);
        assert_eq!(launches.take_due(61_000).len(), 1);
        assert!(launches.take_due(65_999).is_empty());
        assert_eq!(launches.take_due(66_000).len(), 1);
        assert!(launches.take_due(WATCH_FOR_MS).is_empty());
        assert!(launches.take_due(i64::MAX - 1).is_empty());
        assert_eq!(
            launches.starting(&StateNamespace::for_space("socket", "w1")),
            vec![("address".to_owned(), 0)],
            "a launch past its watch is kept for the snapshot to settle"
        );
    }

    #[test]
    fn forgetting_and_recording_move_the_revision_only_on_change() {
        let launches = Launches::default();
        let namespace = StateNamespace::for_space("socket", "w1");
        launches.record(launch(0));
        let recorded = launches.revision();
        launches.forget(&namespace, "other");
        assert_eq!(launches.revision(), recorded);
        launches.forget(&namespace, "address");
        assert_eq!(launches.revision(), recorded + 1);
        assert!(launches.starting(&namespace).is_empty());
    }

    fn rows(
        addresses: Vec<crate::store::dashboard::AddressRow>,
    ) -> crate::dashboard::rows::DashboardRows {
        crate::dashboard::rows::DashboardRows {
            image_directory: None,
            namespace: StateNamespace::for_space("socket", "w1"),
            worktree: Some(crate::WorktreeState {
                activated: true,
                paused: false,
                deactivated: false,
            }),
            addresses,
            registrations: Vec::new(),
            messages: Vec::new(),
            legs: Vec::new(),
            mailbox: Vec::new(),
            threads: Vec::new(),
            coordinations: Vec::new(),
            assignments: Vec::new(),
            terminal_runs: Vec::new(),
            attention: Vec::new(),
            coordination_warnings: Vec::new(),
            review_grants: Vec::new(),
            granted_clears: Vec::new(),
        }
    }

    fn address(id: &str, pane_type: crate::PaneType) -> crate::store::dashboard::AddressRow {
        crate::store::dashboard::AddressRow {
            address_id: id.into(),
            alias: id.into(),
            role_name: String::new(),
            persona: None,
            pane_type,
            retired: false,
            closed_at_ms: None,
            bound: true,
            occupied: false,
            agent_status: None,
            blocked_since_ms: None,
            blocked_message_id: None,
            blocked_conversation: None,
        }
    }

    #[test]
    fn a_launch_needs_the_person_after_twenty_seconds_until_it_settles() {
        use crate::dashboard::snapshot::{self, AgentStarts, SpaceHealth};
        let agent = |id| address(id, crate::PaneType::Agent);
        let rows = rows(vec![
            agent("waiting"),
            agent("young"),
            crate::store::dashboard::AddressRow {
                occupied: true,
                ..agent("connected")
            },
            crate::store::dashboard::AddressRow {
                retired: true,
                ..agent("retired")
            },
            crate::store::dashboard::AddressRow {
                bound: false,
                ..agent("closed")
            },
            address("shell", crate::PaneType::Terminal),
        ]);
        let started = [
            ("waiting".to_owned(), 0),
            ("young".to_owned(), 5_000),
            ("connected".to_owned(), 0),
            ("retired".to_owned(), 0),
            ("closed".to_owned(), 0),
            ("shell".to_owned(), 0),
            ("missing".to_owned(), 0),
        ];
        let mut built = snapshot::build(&rows, None, 20_000);
        assert_eq!(built.health, SpaceHealth::Calm);
        let starts = built.add_agent_start_cards(&rows, &as_starting(&started), 20_000);
        assert_eq!(
            starts,
            AgentStarts {
                next_due_ms: Some(25_000),
                settled: ["connected", "retired", "closed", "shell", "missing"]
                    .map(str::to_owned)
                    .into(),
            }
        );
        let card = serde_json::to_value(&built.attention).unwrap();
        assert_eq!(
            card,
            serde_json::json!([{
                "attention_id": "agent-start-waiting",
                "source_id": "waiting",
                "coordination_id": null,
                "conversation": null,
                "response": {"kind": "pane", "address_id": "waiting"},
                "source_kind": "agent_start",
                "summary": {"text": "@waiting hasn't connected yet. Open its pane. It may be asking you to trust the folder or to log in. A new Codex connects after its first prompt.", "truncated": false},
                "opened_at_ms": 20_000,
                "subject_alias": "waiting",
                "message_id": null,
            }])
        );
        assert_eq!(built.health, SpaceHealth::NeedsYou { items: 1 });
        let mut early = snapshot::build(&rows, None, 19_999);
        early.add_agent_start_cards(&rows, &as_starting(&started[..1]), 19_999);
        assert!(early.attention.is_empty());
        assert_eq!(early.health, SpaceHealth::Calm);
    }

    #[test]
    fn a_reported_session_is_named_once_and_any_end_forgets() {
        let session = |value: &str| Observed::Pane {
            session: Some(value.to_owned()),
        };
        assert_eq!(step(Observed::AddressGone, None), Step::Forget);
        assert_eq!(step(Observed::Occupied, None), Step::Forget);
        assert_eq!(step(Observed::PaneGone, Some("s")), Step::Forget);
        assert_eq!(step(Observed::Pane { session: None }, None), Step::Wait);
        assert_eq!(step(session("s"), None), Step::Name("s".into()));
        assert_eq!(step(session("s"), Some("s")), Step::Wait);
        assert_eq!(step(session("t"), Some("s")), Step::Name("t".into()));
    }
}
