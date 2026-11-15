use crate::mux::{self, AgentKind, Budget, Depth, Endpoint, MuxKind, Scope, home::Inherited};
use serde_json::{Value, json};
use std::path::Path;

/// The incoming executable's read-only adapter survey, before publication.
/// Taking the selected runtime's command gate or re-execing it here would
/// survey with the old adapter during an upgrade.
pub fn sessions(
    state: &Path,
    endpoint: Option<&str>,
    inherited: &dyn Inherited,
) -> Result<Value, String> {
    let endpoint = match endpoint {
        Some(endpoint) => Endpoint::parse(endpoint),
        None => Endpoint::parse(&format!(
            "rex:{}",
            mux::home::rex_socket(inherited)
                .ok_or("Start Rex, or set REX_SERVER to its Unix socket.")?
                .display()
        )),
    };
    if endpoint.kind != MuxKind::Rex {
        return Err("namedrop rex sessions needs a rex: endpoint".into());
    }
    let mux = mux::connect(
        state,
        &endpoint,
        Budget::ADAPTER_DEFAULT,
        Default::default(),
    )
    .map_err(|e| e.to_string())?;
    let survey = mux
        .survey(Scope::All, Depth::Panes)
        .map_err(|e| e.to_string())?;
    Ok(report(&endpoint, survey))
}

fn report(endpoint: &Endpoint, survey: mux::Survey) -> Value {
    let panes: Vec<_> = survey
        .spaces
        .into_iter()
        .flat_map(|s| s.panes)
        .map(|pane| {
            let (agent, session) = match pane.agent {
                Some(identity) => {
                    let kind = match identity.kind {
                        AgentKind::Known(kind) => Some(kind.as_str().to_owned()),
                        AgentKind::Other(kind) => Some(kind),
                        AgentKind::Unreported => None,
                    };
                    (
                        kind,
                        mux::resolve_session(&identity.session).map(|s| s.as_str().to_owned()),
                    )
                }
                None => (None, None),
            };
            json!({"endpoint":endpoint.key,"space":pane.space.as_str(),"block":pane.id.as_str(),
               "agent":agent,"session":session,"status":pane.state,
               "reports_session":pane.reports_session})
        })
        .collect();
    json!({"endpoint":endpoint.key,"panes":panes})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentRuntime, mux::fake::FakeMux};

    #[test]
    fn survey_report_preserves_blocks_kinds_sessions_and_states() {
        use crate::mux::MuxRead;
        let mux = FakeMux::world()
            .endpoint("rex:/tmp/u8.sock")
            .space("s1")
            .pane("shell")
            .pane("agent")
            .build();
        let mut survey = mux.survey(Scope::All, Depth::Panes).unwrap();
        let agent = &mut survey.spaces[0].panes[1];
        agent.agent = Some(mux::AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::Codex),
            label: None,
            session: mux::SessionClaim::Reported(crate::AgentSessionId::new("session-1")),
        });
        agent.state = Some(mux::AgentState::Working);
        agent.reports_session = true;
        let value = report(&Endpoint::parse("rex:/tmp/u8.sock"), survey);
        assert_eq!(value["endpoint"], "rex:/tmp/u8.sock");
        assert_eq!(value["panes"].as_array().unwrap().len(), 2);
        assert_eq!(value["panes"][0]["block"], "shell");
        assert_eq!(value["panes"][0]["agent"], Value::Null);
        assert_eq!(value["panes"][0]["session"], Value::Null);
        assert_eq!(
            value["panes"][1],
            json!({"endpoint":"rex:/tmp/u8.sock", "space":"s1", "block":"agent",
                   "agent":"codex", "session":"session-1", "status":"working", "reports_session":true})
        );
    }
}
