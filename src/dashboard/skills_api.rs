use super::{HistoryStore, Status};
use crate::{
    AgentRuntime,
    dashboard::{
        skills::{Skill, SkillCatalog, SkillRuntime},
        snapshot::SpaceKey,
    },
    mux::{AgentKind, Budget, Depth, MuxConnector, MuxError, PaneFacts, Scope, Survey},
    store::dashboard::{LiveBinding, LiveMember},
};
use serde::Serialize;
use std::{path::Path, sync::Mutex, time::Duration};

#[derive(Serialize)]
pub(super) struct Reply {
    runtime: Option<SkillRuntime>,
    skills: Vec<Skill>,
}

fn runtime(pane: &PaneFacts, kind: crate::PaneType) -> Option<SkillRuntime> {
    match kind {
        crate::PaneType::Agent => match pane.agent.as_ref().map(|agent| &agent.kind) {
            Some(AgentKind::Known(AgentRuntime::ClaudeCode)) => Some(SkillRuntime::Claude),
            Some(AgentKind::Known(AgentRuntime::Codex)) => Some(SkillRuntime::Codex),
            _ => None,
        },
        crate::PaneType::Terminal => None,
    }
}

fn current(binding: &LiveBinding, survey: Result<Survey, MuxError>) -> Result<PaneFacts, Status> {
    let survey = survey.map_err(|_| Status::Unavailable)?;
    let mut matching = survey
        .spaces
        .into_iter()
        .flat_map(|space| space.panes)
        .filter(|pane| {
            pane.id.as_str() == binding.pane_id && pane.space == binding.namespace.space_id
        });
    let pane = matching.next().ok_or(Status::Gone)?;
    if matching.next().is_some() {
        return Err(Status::Unavailable);
    }
    let actual = pane
        .agent
        .as_ref()
        .and_then(|agent| crate::mux::resolve_session(&agent.session));
    if binding
        .agent_session_id
        .as_ref()
        .zip(actual)
        .is_some_and(|(recorded, actual)| recorded != actual)
    {
        return Err(Status::Gone);
    }
    Ok(pane)
}

pub(super) fn read(
    mux: &dyn MuxConnector,
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
            LiveMember::Bound { binding, .. } => binding,
        }
    };
    let budget = Budget {
        timeout: Duration::from_millis(500),
        max_output_bytes: 2 * 1024 * 1024,
    };
    let survey = mux
        .connect(
            &binding.namespace.session_socket_id,
            budget,
            Default::default(),
        )
        .and_then(|herdr| herdr.survey(Scope::PanesOf(&binding.namespace.space_id), Depth::Panes));
    let pane = current(&binding, survey)?;
    let runtime = runtime(&pane, binding.pane_type);
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
    use crate::{
        AgentSessionId, PaneBindingId, PaneType, SpaceId, StateNamespace,
        mux::{AgentIdentity, Connector, PaneId, SessionClaim, SpaceView, TabId, TerminalId},
    };
    use serde_json::json;

    fn pane(space: &str, kind: Option<AgentKind>, session: Option<&str>) -> PaneFacts {
        PaneFacts {
            id: PaneId::new("pane"),
            terminal: TerminalId::new("terminal"),
            tab: TabId::new("tab"),
            space: SpaceId::new(space),
            cwd: Some("/fixture".into()),
            title: None,
            agent: kind.map(|kind| AgentIdentity {
                kind,
                label: None,
                session: session.map_or(SessionClaim::None, |session| {
                    SessionClaim::Reported(AgentSessionId::new(session))
                }),
            }),
            state: None,
            idle_label: None,
            viewport_rows: None,
            reports_session: true,
        }
    }

    fn survey(panes: Vec<PaneFacts>) -> Result<Survey, MuxError> {
        Ok(Survey {
            spaces: vec![SpaceView {
                id: SpaceId::new("w1"),
                label: None,
                number: None,
                checkout: None,
                worktree: None,
                panes,
            }],
        })
    }

    #[test]
    fn unsupported_agents_and_terminals_have_no_skill_runtime() {
        let codex = pane("w1", Some(AgentKind::Known(AgentRuntime::Codex)), None);
        assert_eq!(runtime(&codex, PaneType::Agent), Some(SkillRuntime::Codex));
        assert_eq!(runtime(&codex, PaneType::Terminal), None);
        let other = pane("w1", Some(AgentKind::Other("other-agent".into())), None);
        assert_eq!(runtime(&other, PaneType::Agent), None);
        assert_eq!(runtime(&pane("w1", None, None), PaneType::Agent), None);
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
            read(
                &Connector::new(&state),
                &history,
                &state,
                &key,
                "missing",
                &catalog
            ),
            Err(Status::NotFound)
        ));
        assert!(matches!(
            read(
                &Connector::new(&state),
                &history,
                &state,
                &key,
                "ended",
                &catalog
            ),
            Err(Status::Gone)
        ));
        let wrong = SpaceKey::of(&StateNamespace::for_space("other-socket", "test-workspace"));
        assert!(matches!(
            read(
                &Connector::new(&state),
                &history,
                &state,
                &wrong,
                "ended",
                &catalog
            ),
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
        let codex = Some(AgentKind::Known(AgentRuntime::Codex));
        let found = pane("w1", codex.clone(), Some("session"));
        assert_eq!(
            current(&binding, survey(vec![found.clone()])).ok(),
            Some(found.clone())
        );
        assert!(matches!(
            current(
                &binding,
                survey(vec![pane("w2", codex.clone(), Some("session"))])
            ),
            Err(Status::Gone)
        ));
        assert!(matches!(
            current(
                &binding,
                survey(vec![pane("w1", codex, Some("replacement"))])
            ),
            Err(Status::Gone)
        ));
        assert!(matches!(
            current(&binding, survey(vec![found.clone(), found])),
            Err(Status::Unavailable)
        ));
        assert!(matches!(
            current(&binding, Err(MuxError::Protocol("garbage".into()))),
            Err(Status::Unavailable)
        ));
    }
}
