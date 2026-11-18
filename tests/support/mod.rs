#![allow(dead_code)]

use crate::cleanup;

use namedrop::{
    ActorKind, AgentSessionId, Clock, IdGenerator, MutationAttribution, PaneAddressDraft,
    PaneAddressId, PaneBindingId, PaneRoleId, StateNamespace, StateStore,
    mux::fake::{CHECKOUT_LISTINGS, FakeMux, StrictSpace, Takes},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

pub struct TestDirectory {
    path: PathBuf,
    home: PathBuf,
}

impl TestDirectory {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let home = path.join("home");
        fs::create_dir(&home).unwrap();
        Self { path, home }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stands in for the user's home directory, so no test installs an Agent
    /// Integration package into the developer's real user-level skill roots.
    pub fn home(&self) -> &Path {
        &self.home
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.path).unwrap();
    }
}

/// The space `setup_addresses` names: checkout `/repo` open in `workspace-1`,
/// and one idle pane, `pane-author`, whose session Herdr reports without
/// naming its agent. Messaging only reads it: it answers the three
/// listings `/repo` needs, and takes no input and no effect.
pub fn author_mux() -> StrictSpace {
    let mux = FakeMux::world()
        .space("workspace-1")
        .checkout("/repo")
        .pane("pane-author")
        .tab("tab-1")
        .session("session-author")
        .idle()
        .build();
    StrictSpace::new(mux, CHECKOUT_LISTINGS, Takes::nothing())
}

pub struct FixedClock;

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        42
    }
}

pub struct SequenceIds(pub AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

pub fn address(id: &str, alias: &str) -> PaneAddressDraft {
    PaneAddressDraft {
        id: PaneAddressId::new(id),
        role_id: PaneRoleId::new(format!("role-{id}")),
        role_name: alias.to_owned(),
        role_description: format!("{alias} test role"),
        alias: alias.to_owned(),
    }
}

pub fn setup_addresses(directory: &TestDirectory) {
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &attribution)?;
            for draft in [
                address("address-author", "author"),
                address("address-reviewer", "reviewer"),
                address("address-planner", "planner"),
                address("address-researcher", "researcher"),
            ] {
                transaction.create_pane_address(&namespace, &draft, &attribution)?;
            }
            transaction.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-author"),
                &PaneAddressId::new("address-author"),
                "pane-author",
                &attribution,
            )?;
            transaction.observe_agent_session(
                &namespace,
                "observation-author",
                &PaneAddressId::new("address-author"),
                &AgentSessionId::new("session-author"),
                "idle",
                &attribution,
            )
        })
        .unwrap();
}
