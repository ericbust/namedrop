#![allow(dead_code)]

use namedrop::{
    ActorKind, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput, IdGenerator,
    MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId,
    StateNamespace, StateStore,
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
        if let Ok(record) = fs::read(self.path.join("pane-effects-retries.log")) {
            use std::io::Write;
            let _ = std::io::stderr().lock().write_all(&record);
        }
        fs::remove_dir_all(&self.path).unwrap();
    }
}

pub struct FakeHerdr;

impl HerdrCommandExecutor for FakeHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => {
                r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
            }
            ["worktree", "list", "--cwd", "/repo", "--json"] => {
                r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
            }
            ["pane", "list", "--workspace", "workspace-1"] => {
                r#"{"result":{"panes":[{"pane_id":"pane-author","terminal_id":"terminal-author","tab_id":"tab-1","workspace_id":"workspace-1","agent_session":{"value":"session-author"},"agent_status":"idle"}]}}"#
            }
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: stdout.to_owned(),
        })
    }
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
