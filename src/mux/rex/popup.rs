use super::*;
use crate::mux::{PopupScreen, PopupSpec, SessionClaim};
use std::os::unix::fs::PermissionsExt;

impl<W: Wire> RexMux<W> {
    pub(super) fn open_popup(
        &self,
        popup: &PopupSpec,
        launcher: &Path,
    ) -> Result<Applied, MuxError> {
        if !launcher
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            return Err(MuxError::Unavailable(format!(
                "Namedrop's launcher {} is missing. Run sh scripts/install.sh again.",
                launcher.display()
            )));
        }
        let target = &popup.target;
        let (title, from) = words(popup.screen);
        // A layer floats over a window, and a detached block is in none.
        if target.tab.as_str().is_empty() {
            return Err(MuxError::Unavailable(format!(
                "This block is not in a window, so {title} cannot open over it. Open {title} \
                 from a block in a window."
            )));
        }
        let mut call = self.call()?;
        call.attach(target.space.as_str())?;
        let process: ProcessReply = wire::parse(call.block(
            target.space.as_str(),
            target.id.as_str(),
            "process",
            json!({}),
        )?)?;
        match process.foreground.as_ref() {
            Some(foreground) if foreground.name == "herdr" => {
                return Err(MuxError::Unavailable(format!(
                    "This pane is running Herdr. Use Herdr's own {title}."
                )));
            }
            // A launch typed in the target pane is that pane's foreground
            // itself. The server is proven local, so its pids are ours.
            Some(foreground)
                if foreground.name == "namedrop" && foreground.pid != std::process::id() =>
            {
                return Err(MuxError::Unavailable(format!(
                    "This pane is running Namedrop. Open {title} from {from}."
                )));
            }
            _ => {}
        }
        let bounds = match popup.screen {
            PopupScreen::Status => json!({"x": 0.15, "y": 0.15, "w": 0.7, "h": 0.7}),
            _ => json!({"x": 0.2, "y": 0.2, "w": 0.6, "h": 0.6}),
        };
        let request = json!({
            "session_id": target.space.as_str(),
            "window_id": target.tab.as_str(),
            "bounds": bounds,
            "focus": true,
            "layout": {"block": {"flavor": "com.superlogical.terminal.shell", "options": {
                "shell": "none", "login": false,
                "command": popup_argv(popup, &self.endpoint, launcher),
            }}},
        });
        call.request("session.new_layer", request, Gone::Pane)
            .map(Applied::Opened)
    }
}

/// Each screen's title, as Herdr's menu and the palette name it, and the pane
/// it is opened from.
fn words(screen: PopupScreen) -> (&'static str, &'static str) {
    match screen {
        PopupScreen::Name => ("Name this pane", "the pane to name"),
        PopupScreen::Manage => (
            "Manage Namedrop names and agents",
            "a pane in the space to manage",
        ),
        PopupScreen::Doctor => ("Namedrop Doctor", "the pane to check"),
        PopupScreen::Status => ("Agent status", "the agent's pane"),
        PopupScreen::Tour => ("Namedrop tour", "a pane in the space"),
    }
}

fn popup_argv(popup: &PopupSpec, endpoint: &Endpoint, launcher: &Path) -> Vec<String> {
    let target = &popup.target;
    let session = match target.agent.as_ref().map(|a| &a.session) {
        Some(SessionClaim::Reported(session)) => session.as_str(),
        _ => "",
    };
    let mode = match popup.screen {
        PopupScreen::Name => "name",
        PopupScreen::Manage => "manage",
        PopupScreen::Doctor => "doctor",
        PopupScreen::Status => "status",
        PopupScreen::Tour => "tour",
    };
    vec![
        "/usr/bin/env".into(),
        format!("NAMEDROP_TARGET_WORKSPACE={}", target.space.as_str()),
        format!("NAMEDROP_TARGET_PANE={}", target.id.as_str()),
        format!("NAMEDROP_TARGET_TERMINAL={}", target.terminal.as_str()),
        format!("NAMEDROP_TARGET_SESSION={session}"),
        format!("NAMEDROP_STATE_DIR={}", popup.state_dir.display()),
        launcher.to_string_lossy().into_owned(),
        "--endpoint".into(),
        endpoint.key.clone(),
        "ui".into(),
        "run".into(),
        mode.into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Local, Script, State, fixture, one_block, rex};
    use super::*;
    use crate::mux::{AgentIdentity, AgentKind, AgentState, TerminalId};
    use clap::Parser;

    fn popup(screen: PopupScreen, state: &State) -> PopupSpec {
        PopupSpec {
            screen,
            target: PaneFacts {
                id: PaneId::new("block:b"),
                space: SpaceId::new("session:s"),
                tab: TabId::new("window:w"),
                terminal: TerminalId::new("terminal:original"),
                agent: Some(AgentIdentity {
                    kind: AgentKind::Known(crate::AgentRuntime::Codex),
                    session: SessionClaim::Reported(crate::AgentSessionId::new("agent-session")),
                    label: Some("codex".into()),
                }),
                state: Some(AgentState::Idle),
                cwd: None,
                title: None,
                idle_label: None,
                viewport_rows: None,
                reports_session: true,
            },
            state_dir: state.0.join("state with spaces"),
            socket: "rex:/tmp/r.sock".into(),
        }
    }

    fn launcher(state: &State) -> PathBuf {
        let path = state.0.join("launcher with spaces");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn every_screen_opens_the_pinned_layer_and_argv_round_trips() {
        for (screen, mode, bounds) in [
            (
                PopupScreen::Name,
                "name",
                json!({"x":0.2,"y":0.2,"w":0.6,"h":0.6}),
            ),
            (
                PopupScreen::Manage,
                "manage",
                json!({"x":0.2,"y":0.2,"w":0.6,"h":0.6}),
            ),
            (
                PopupScreen::Doctor,
                "doctor",
                json!({"x":0.2,"y":0.2,"w":0.6,"h":0.6}),
            ),
            (
                PopupScreen::Tour,
                "tour",
                json!({"x":0.2,"y":0.2,"w":0.6,"h":0.6}),
            ),
            (
                PopupScreen::Status,
                "status",
                json!({"x":0.15,"y":0.15,"w":0.7,"h":0.7}),
            ),
        ] {
            let state = State::new();
            let launcher = launcher(&state);
            let spec = popup(screen, &state);
            let expected_argv = vec![
                "/usr/bin/env".to_owned(),
                "NAMEDROP_TARGET_WORKSPACE=session:s".into(),
                "NAMEDROP_TARGET_PANE=block:b".into(),
                "NAMEDROP_TARGET_TERMINAL=terminal:original".into(),
                "NAMEDROP_TARGET_SESSION=agent-session".into(),
                format!("NAMEDROP_STATE_DIR={}", spec.state_dir.display()),
                launcher.display().to_string(),
                "--endpoint".into(),
                "rex:/tmp/r.sock".into(),
                "ui".into(),
                "run".into(),
                mode.into(),
            ];
            let argv = expected_argv.clone();
            let base = one_block(fixture("idle-login"));
            let script = Script::new(move |request, events| {
                if request.short() == "session.new_layer" {
                    assert_eq!(
                        request.payload,
                        json!({
                            "session_id":"session:s", "window_id":"window:w", "bounds":bounds,
                        "focus":true, "layout":{"block":{"flavor":"com.superlogical.terminal.shell","options":{
                                "shell":"none", "login":false, "command":argv,
                            }}},
                        })
                    );
                    Ok(json!({"block_id":"layer:b"}))
                } else {
                    base.answer(request, events)
                }
            });
            let mux = rex(script, &state, Local::default());
            assert_eq!(
                mux.open_popup(&spec, &launcher).unwrap(),
                Applied::Opened(json!({"block_id":"layer:b"}))
            );
            assert_eq!(
                mux.wire.calls().last().unwrap(),
                "session.new_layer session:s"
            );
            let argv = popup_argv(&spec, &mux.endpoint, &launcher);
            assert_eq!(argv, expected_argv);
            let env = argv[1..6]
                .iter()
                .map(|s| s.split_once('=').unwrap())
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(env.len(), 5);
            assert_eq!(env["NAMEDROP_TARGET_TERMINAL"], "terminal:original");
            let cli = crate::cli::Cli::try_parse_from(&argv[6..]).unwrap();
            assert_eq!(cli.runtime.endpoint.as_deref(), Some("rex:/tmp/r.sock"));
            assert!(matches!(cli.naming_ui(), Some((false, _))));
        }
    }

    #[test]
    fn unreported_session_is_passed_as_empty_without_changing_the_target() {
        let state = State::new();
        let mut spec = popup(PopupScreen::Name, &state);
        spec.target.agent.as_mut().unwrap().session = SessionClaim::Unreported {
            incarnation: crate::mux::Incarnation("123@456".into()),
        };
        let argv = popup_argv(
            &spec,
            &Endpoint::parse(&spec.socket),
            Path::new("/launcher"),
        );
        assert_eq!(argv[4], "NAMEDROP_TARGET_SESSION=");
        spec.target.agent = None;
        assert_eq!(
            popup_argv(
                &spec,
                &Endpoint::parse(&spec.socket),
                Path::new("/launcher")
            ),
            argv
        );
    }

    fn opened_layer(mux: &RexMux<Script>) -> bool {
        mux.wire
            .calls()
            .iter()
            .any(|s| s.starts_with("session.new_layer"))
    }

    fn layer_script(facts: Value) -> Script {
        let base = one_block(facts);
        Script::new(move |request, events| {
            if request.short() == "session.new_layer" {
                Ok(json!({"block_ids":["layer:b"]}))
            } else {
                base.answer(request, events)
            }
        })
    }

    #[test]
    fn herdr_and_any_other_namedrop_foreground_are_refused_before_a_layer_request() {
        for (screen, herdr, namedrop) in [
            (
                PopupScreen::Name,
                "This pane is running Herdr. Use Herdr's own Name this pane.",
                "This pane is running Namedrop. Open Name this pane from the pane to name.",
            ),
            (
                PopupScreen::Manage,
                "This pane is running Herdr. Use Herdr's own Manage Namedrop names and agents.",
                "This pane is running Namedrop. Open Manage Namedrop names and agents from a pane in the space to manage.",
            ),
            (
                PopupScreen::Doctor,
                "This pane is running Herdr. Use Herdr's own Namedrop Doctor.",
                "This pane is running Namedrop. Open Namedrop Doctor from the pane to check.",
            ),
            (
                PopupScreen::Status,
                "This pane is running Herdr. Use Herdr's own Agent status.",
                "This pane is running Namedrop. Open Agent status from the agent's pane.",
            ),
            (
                PopupScreen::Tour,
                "This pane is running Herdr. Use Herdr's own Namedrop tour.",
                "This pane is running Namedrop. Open Namedrop tour from a pane in the space.",
            ),
        ] {
            for (name, message) in [("herdr", herdr), ("namedrop", namedrop)] {
                let state = State::new();
                let launcher = launcher(&state);
                let mut facts = fixture("idle-login");
                facts["process"]["foreground"]["name"] = json!(name);
                let mux = rex(layer_script(facts), &state, Local::default());
                let error = mux
                    .open_popup(&popup(screen, &state), &launcher)
                    .unwrap_err();
                assert_eq!(error.to_string(), message);
                assert!(!opened_layer(&mux));
            }
        }
    }

    #[test]
    fn a_launch_typed_in_the_target_pane_opens_its_layer() {
        let state = State::new();
        let launcher = launcher(&state);
        let mut facts = fixture("idle-login");
        facts["process"]["foreground"]["name"] = json!("namedrop");
        facts["process"]["foreground"]["pid"] = json!(std::process::id());
        let mux = rex(layer_script(facts), &state, Local::default());
        mux.open_popup(&popup(PopupScreen::Doctor, &state), &launcher)
            .unwrap();
        assert!(opened_layer(&mux));
    }

    #[test]
    fn a_detached_target_is_refused_before_any_request() {
        for screen in [PopupScreen::Name, PopupScreen::Tour] {
            let state = State::new();
            let launcher = launcher(&state);
            let mut spec = popup(screen, &state);
            spec.target.tab = TabId::new("");
            let mux = rex(
                layer_script(fixture("idle-login")),
                &state,
                Local::default(),
            );
            let error = mux.open_popup(&spec, &launcher).unwrap_err().to_string();
            assert!(
                error.starts_with("This block is not in a window, so ")
                    && error.contains(words(screen).0),
                "{error}"
            );
            assert!(mux.wire.calls().is_empty(), "{:?}", mux.wire.calls());
        }
    }

    #[test]
    fn missing_nonexecutable_and_nonfile_launchers_send_no_requests() {
        let state = State::new();
        let file = launcher(&state);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        for path in [state.0.join("missing"), file, state.0.clone()] {
            let mux = rex(one_block(fixture("idle-login")), &state, Local::default());
            assert_eq!(
                mux.open_popup(&popup(PopupScreen::Name, &state), &path)
                    .unwrap_err()
                    .to_string(),
                format!(
                    "Namedrop's launcher {} is missing. Run sh scripts/install.sh again.",
                    path.display()
                )
            );
            assert!(mux.wire.calls().is_empty());
        }
    }
}
