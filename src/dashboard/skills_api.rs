use super::{HistoryStore, Status};
use crate::{
    HerdrCommandExecutor, ProcessHerdr,
    dashboard::{
        skills::{Skill, SkillCatalog, SkillRuntime},
        snapshot::SpaceKey,
    },
    store::dashboard::{LiveBinding, LiveMember},
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Mutex, atomic::AtomicBool},
    time::Duration,
};

#[derive(Serialize)]
pub(super) struct Reply {
    runtime: Option<SkillRuntime>,
    skills: Vec<Skill>,
}

#[derive(Deserialize)]
struct Envelope {
    result: Panes,
}
#[derive(Deserialize)]
struct Panes {
    panes: Vec<Pane>,
}
#[derive(Deserialize)]
struct Pane {
    pane_id: String,
    workspace_id: String,
    agent: Option<String>,
    cwd: Option<String>,
    agent_session: Option<Session>,
}
impl Pane {
    fn runtime(&self, kind: crate::PaneType) -> Option<SkillRuntime> {
        match kind {
            crate::PaneType::Agent => self.agent.as_deref().and_then(SkillRuntime::parse),
            crate::PaneType::Terminal => None,
        }
    }
}

#[derive(Deserialize)]
struct Session {
    value: String,
}

fn current(binding: &LiveBinding, output: &str) -> Result<Pane, Status> {
    let reply: Envelope = serde_json::from_str(output).map_err(|_| Status::Unavailable)?;
    let mut matching = reply.result.panes.into_iter().filter(|pane| {
        pane.pane_id == binding.pane_id && pane.workspace_id == binding.namespace.space_id.as_str()
    });
    let pane = matching.next().ok_or(Status::Gone)?;
    if matching.next().is_some() {
        return Err(Status::Unavailable);
    }
    if binding
        .agent_session_id
        .as_ref()
        .zip(pane.agent_session.as_ref())
        .is_some_and(|(recorded, actual)| recorded.as_str() != actual.value)
    {
        return Err(Status::Gone);
    }
    Ok(pane)
}

pub(super) fn read(
    history: &Mutex<HistoryStore>,
    state: &Path,
    key: &SpaceKey,
    address: &str,
    catalog: &SkillCatalog,
) -> Result<Reply, Status> {
    let binding = {
        let mut history = history.lock().map_err(|_| Status::Unavailable)?;
        match history
            .store(state)?
            .dashboard_live_member(key, address)
            .map_err(|_| Status::Unavailable)?
        {
            LiveMember::Missing => return Err(Status::NotFound),
            LiveMember::Gone => return Err(Status::Gone),
            LiveMember::Bound(binding) => binding,
        }
    };
    let output = ProcessHerdr::default()
        .with_socket(Some(binding.namespace.session_socket_id.clone()))
        .execute_bounded(
            &[
                "pane",
                "list",
                "--workspace",
                binding.namespace.space_id.as_str(),
            ],
            Duration::from_millis(500),
            &AtomicBool::new(false),
            2 * 1024 * 1024,
        )
        .map_err(|_| Status::Unavailable)?;
    let pane = current(&binding, &output.stdout)?;
    let runtime = pane.runtime(binding.pane_type);
    let skills = runtime
        .zip(pane.cwd.as_deref())
        .map_or_else(Vec::new, |(runtime, cwd)| {
            catalog.for_pane(runtime, Path::new(cwd))
        });
    Ok(Reply { runtime, skills })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentSessionId, PaneBindingId, PaneType, StateNamespace};
    use serde_json::json;

    #[test]
    fn unsupported_agents_and_terminals_have_no_skill_runtime() {
        let mut pane: Pane =
            serde_json::from_value(json!({"pane_id":"pane","workspace_id":"w1","agent":"codex"}))
                .unwrap();
        assert_eq!(pane.runtime(PaneType::Agent), Some(SkillRuntime::Codex));
        assert_eq!(pane.runtime(PaneType::Terminal), None);
        pane.agent = Some("other-agent".into());
        assert_eq!(pane.runtime(PaneType::Agent), None);
        pane.agent = None;
        assert_eq!(pane.runtime(PaneType::Agent), None);
        assert_eq!(
            serde_json::to_value(Reply {
                runtime: None,
                skills: Vec::new()
            })
            .unwrap(),
            json!({"runtime":null,"skills":[]})
        );
    }

    #[test]
    fn missing_keys_and_unbound_members_are_refused_before_discovery() {
        use crate::{
            ActorKind, MutationAttribution, PaneAddressDraft, PaneAddressId, PaneRoleId, StateStore,
        };
        let state =
            std::env::temp_dir().join(format!("namedrop-skill-members-{}", std::process::id()));
        let namespace = StateNamespace::for_space("test-skill-socket", "test-workspace");
        let key = SpaceKey::of(&namespace);
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
        let mut store = StateStore::open(&state).unwrap();
        store
            .transaction(|tx| {
                tx.activate_worktree(&namespace, &attribution)?;
                tx.create_pane_address(
                    &namespace,
                    &PaneAddressDraft {
                        id: PaneAddressId::new("ended"),
                        role_id: PaneRoleId::new("role"),
                        role_name: "ended".into(),
                        role_description: "fixture".into(),
                        alias: "ended".into(),
                    },
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
        let history = Mutex::new(HistoryStore::Unopened);
        let catalog = SkillCatalog::default();
        assert!(matches!(
            read(&history, &state, &key, "missing", &catalog),
            Err(Status::NotFound)
        ));
        assert!(matches!(
            read(&history, &state, &key, "ended", &catalog),
            Err(Status::Gone)
        ));
        let wrong = SpaceKey::of(&StateNamespace::for_space("other-socket", "test-workspace"));
        assert!(matches!(
            read(&history, &state, &wrong, "ended", &catalog),
            Err(Status::NotFound)
        ));
        drop(history);
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn pane_selection_requires_workspace_and_current_session() {
        let binding = LiveBinding {
            namespace: StateNamespace::for_space("test-socket", "w1"),
            binding_id: PaneBindingId::new("binding"),
            pane_id: "pane".into(),
            pane_type: PaneType::Agent,
            agent_session_id: Some(AgentSessionId::new("session")),
        };
        let pane = json!({"pane_id":"pane","workspace_id":"w1","agent":"codex","cwd":"/fixture","agent_session":{"value":"session"}});
        let reply = |panes: Vec<serde_json::Value>| json!({"result":{"panes":panes}}).to_string();
        assert_eq!(
            current(&binding, &reply(vec![pane.clone()]))
                .unwrap()
                .agent
                .as_deref(),
            Some("codex")
        );
        let mut wrong = pane.clone();
        wrong["workspace_id"] = json!("w2");
        assert!(matches!(
            current(&binding, &reply(vec![wrong])),
            Err(Status::Gone)
        ));
        let mut wrong = pane.clone();
        wrong["agent_session"]["value"] = json!("replacement");
        assert!(matches!(
            current(&binding, &reply(vec![wrong])),
            Err(Status::Gone)
        ));
        assert!(matches!(
            current(&binding, &reply(vec![pane.clone(), pane])),
            Err(Status::Unavailable)
        ));
        assert!(matches!(
            current(&binding, "garbage"),
            Err(Status::Unavailable)
        ));
    }
}
