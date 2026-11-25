fn host_pair(surface: &str, actual: &str, herdr: &str, rex: &str) {
    assert!(actual.contains(herdr), "{surface} lost {herdr:?}: {actual}");
    assert!(
        !actual.contains(rex),
        "{surface} contains Rex replacement {rex:?}: {actual}"
    );
}

fn dedicated_sites(words: &mut Words, root: &Path) {
    herdr_delivery_attestation(root);
    herdr_deferred_state(root);
    let command = cli::Cli::command_for(MuxKind::Rex);
    assert!(command.find_subcommand("event").unwrap().is_hide_set());
    assert!(
        command
            .find_subcommand("ui")
            .unwrap()
            .find_subcommand("install-shortcuts")
            .unwrap()
            .is_hide_set()
    );
    let notice = namedrop::test_codex_app_server_hook_output(MuxKind::Rex);
    words.json("app-server SessionStart notice", &notice);
    for (label, endpoint) in [("herdr", "/tmp/words-host.sock"), ("rex", ENDPOINT)] {
        let f = Fixture::new(root, &format!("host-{label}"), None);
        let mux = FakeMux::world()
            .endpoint(endpoint)
            .space("s1")
            .pane("p1")
            .tab("window:w")
            .terminal("t1")
            .claude()
            .session("agent-1")
            .idle()
            .build();
        let clock = SystemClock;
        let ids = ProcessIds::default();
        let runtime = f.runtime(&mux, &clock, &ids, endpoint, Some("s1"), Some("p1"));
        let check = |words: &mut Words, surface: &str, actual: &str, herdr: &str, rex: &str| {
            if label == "herdr" {
                host_pair(surface, actual, herdr, rex);
            } else {
                assert!(actual.contains(rex), "{surface}: {actual}");
                words.check(surface, actual);
            }
        };
        let registration = namedrop::test_registration_prompt(
            &f.state,
            endpoint,
            "s1",
            "p1",
            Path::new(binary::namedrop_path()),
            AgentRuntime::ClaudeCode,
            "nonce",
        )
        .unwrap();
        check(
            words,
            "registration challenge",
            &registration,
            "plugin-authored",
            "Namedrop-authored",
        );
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
        let policy =
            execute_session_start_hook(r#"{"hook_event_name":"SessionStart"}"#, &runtime).unwrap();
        check(
            words,
            "session selection policy",
            &policy.to_string(),
            "address Herdr panes",
            "address Rex panes",
        );
        let missing = f.runtime(&mux, &clock, &ids, endpoint, None, None);
        check(
            words,
            "doctor missing invoking session",
            &request_doctor_assistance(&missing, "agent-1").unwrap_err(),
            "Asking the agent requires the popup's invoking space",
            "Asking the agent requires the screen's invoking session",
        );
        let no_pane = f.runtime(&mux, &clock, &ids, endpoint, Some("s1"), None);
        for (op, input, herdr, rex) in [
            (
                "name",
                json!({"alias":"words"}),
                "name must run from the Herdr pane being named",
                "name must run from the Rex pane being named",
            ),
            (
                "address.create",
                json!({"alias":"words"}),
                "pane_id is required outside a Herdr pane action",
                "pane_id is required outside a Rex pane action",
            ),
        ] {
            check(
                words,
                op,
                &execute_lifecycle_operation(op, input, &no_pane).unwrap_err(),
                herdr,
                rex,
            );
        }
        check(
            words,
            "terminal missing pane",
            &execute_terminal("attempt", &no_pane).unwrap_err(),
            "terminal runner requires its current Herdr pane",
            "terminal runner requires its current Rex pane",
        );
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let agent_status_ui::Identity::Changed(closed) = agent_status_ui::latest_identity(
            &runtime,
            "s1",
            "p9",
            "t1",
            Some("agent-1"),
            &cancelled,
        ) else {
            panic!("closed pane remained current");
        };
        check(
            words,
            "status closed pane",
            &closed,
            "The invoking pane has closed. Open the popup again from your pane.",
            "The invoking pane has closed. Open the screen again from your pane.",
        );
        mux.answer("p1", InputOutcome::Delivered);
        request_doctor_assistance(&runtime, "agent-1").unwrap();
        let prompts: Vec<_> = mux
            .sent()
            .into_iter()
            .filter_map(|sent| match sent.input {
                namedrop::mux::fake::SentInput::Prompt(prompt)
                    if prompt.starts_with("Namedrop Doctor assistance") =>
                {
                    Some(prompt)
                }
                _ => None,
            })
            .collect();
        assert_eq!(prompts.len(), 1);
        check(
            words,
            "doctor dispatched prompt",
            &prompts[0],
            "Namedrop Doctor popup",
            "Namedrop Doctor screen",
        );
        let discovered = discover_worktree_for_workspace(&mux, "s1").unwrap();
        let target = &discovered.panes[0];
        mux.fail(Op::Apply, MuxError::Protocol("words-invalid-json".into()));
        let error = tour::open_popup(&runtime, target).unwrap_err();
        check(
            words,
            "tour manual unreadable result",
            &error,
            "Herdr returned an unreadable popup result",
            "Rex returned an unreadable screen result",
        );
        tour::reset(&f.state, tour::CURRENT_ID).unwrap();
        mux.fail(Op::Apply, MuxError::Protocol("words-invalid-json".into()));
        let shown = tour::show_for_first_agent(&runtime, &discovered, Some("p1"));
        assert_eq!(shown["reason"], "open_failed");
        check(
            words,
            "tour first activation unreadable result",
            shown["error"].as_str().unwrap(),
            "Herdr returned an unreadable popup result",
            "Rex returned an unreadable screen result",
        );
        let unidentified = FakeMux::world()
            .endpoint(endpoint)
            .space("s1")
            .pane("p1")
            .claude()
            .build();
        let runtime = f.runtime(
            &unidentified,
            &clock,
            &ids,
            endpoint,
            Some("s1"),
            Some("p1"),
        );
        let report = diagnose_collaboration(&runtime);
        let issue = report
            .issues
            .iter()
            .find(|issue| issue.code == "ND-SESSION-002")
            .unwrap();
        check(
            words,
            "doctor missing identity repair",
            &serde_json::to_string(issue).unwrap(),
            "Install the Herdr native integration if needed, then restart this agent.",
            "Run Activate Namedrop again, then restart this agent to load the Namedrop hooks.",
        );
        cleanup::remove_fixture(&f.root).unwrap();
    }
}

fn herdr_delivery_attestation(root: &Path) {
    let f = Fixture::new(root, "herdr-attestation", None);
    let world = FakeMux::world()
        .endpoint("/tmp/words-attestation.sock")
        .space("s1")
        .pane("p1")
        .tab("window:w")
        .claude()
        .session("session-1")
        .state(AgentState::parse("idle"));
    let mux = world.build();
    let clock = TestClock(AtomicU64::new(1_000));
    let ids = ProcessIds::default();
    let runtime = f.runtime(
        &mux,
        &clock,
        &ids,
        "/tmp/words-attestation.sock",
        Some("s1"),
        Some("p1"),
    );
    let namespace = StateNamespace::for_space("/tmp/words-attestation.sock", "s1");
    let attribution = MutationAttribution::new(ActorKind::Plugin, "test", 10);
    let draft = |id: &str, alias: &str| PaneAddressDraft {
        id: PaneAddressId::new(id),
        role_id: PaneRoleId::new(format!("role-{id}")),
        role_name: alias.to_owned(),
        role_description: format!("{alias} role"),
        alias: alias.to_owned(),
    };
    let (author, recipient) = (
        draft("address-author", "author"),
        draft("address-recipient", "recipient"),
    );
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &attribution)?;
            transaction.create_pane_address(&namespace, &author, &attribution)?;
            transaction.create_pane_address(&namespace, &recipient, &attribution)?;
            transaction.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-recipient"),
                &recipient.id,
                "p1",
                &attribution,
            )?;
            transaction.observe_agent_session(
                &namespace,
                "observation-recipient",
                &recipient.id,
                &AgentSessionId::new("session-1"),
                "idle",
                &attribution,
            )?;
            transaction.create_thread(
                &namespace,
                &ThreadId::new("thread-1"),
                "Words",
                &attribution,
            )?;
            transaction.complete_reconciliation(&namespace)?;
            Ok(())
        })
        .unwrap();
    let challenge =
        issue_registration_challenge(&recipient.id, AgentRuntime::ClaudeCode, &runtime).unwrap();
    execute_integration_operation(
            "register",
            json!({"runtime": AgentRuntime::ClaudeCode, "protocol_version": "collab/v1", "nonce": challenge.nonce,
                   "capabilities": complete_capability_profile()}),
            &runtime,
        )
        .unwrap();
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.append_message(
                &namespace,
                &MessageDraft {
                    thread_id: ThreadId::new("thread-1"),
                    message_id: MessageId::new("message-1"),
                    in_reply_to: None,
                    author_address_id: author.id.clone(),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "author".to_owned(),
                    author_agent_session_snapshot: None,
                    request_id: "request-1".to_owned(),
                    authored_source: "@recipient Are the tests green?".to_owned(),
                    body: "Are the tests green?".to_owned(),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: recipient.id.clone(),
                        alias_snapshot: "recipient".to_owned(),
                        agent_session_snapshot: Some(AgentSessionId::new("session-1")),
                        delivery_id: DeliveryId::new("delivery-1"),
                    }],
                    context_references: Vec::new(),
                },
                &MutationAttribution::new(ActorKind::Human, "test", 1_000),
            )
        })
        .unwrap();
    mux.answer("p1", InputOutcome::Delivered);
    drain_mailboxes(&runtime).unwrap();
    let mut attested = false;
    for sent in mux.sent() {
        if let namedrop::mux::fake::SentInput::Prompt(prompt) = sent.input {
            let input = json!({"hook_event_name":"UserPromptSubmit","session_id":"session-1","prompt":prompt});
            if let Some(value) = execute_user_prompt_submit_hook(&input.to_string(), &runtime) {
                let text = value.to_string();
                if text.contains("Namedrop verification:") {
                    host_pair(
                        "authenticated delivery",
                        &text,
                        "plugin-authored",
                        "Namedrop-authored",
                    );
                    attested = true;
                }
            }
        }
    }
    assert!(attested, "no authenticated delivery reached the hook");
    cleanup::remove_fixture(&f.root).unwrap();
}

fn herdr_deferred_state(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    struct Empty;
    impl namedrop::mux::home::Inherited for Empty {
        fn var(&self, _: &str) -> Option<std::ffi::OsString> {
            None
        }
        fn id(&self) -> u32 {
            0
        }
        fn ancestors(&self) -> Vec<u32> {
            Vec::new()
        }
        fn peer(&self, _: &Path) -> std::io::Result<u32> {
            Err(std::io::ErrorKind::PermissionDenied.into())
        }
        fn claimed(&self, _: &Path, _: &str) -> Result<namedrop::mux::home::Claimed, String> {
            Err("no claim".into())
        }
        fn incarnation(&self, _: u32) -> Option<String> {
            None
        }
    }
    let denied = root.join("denied-deferred");
    fs::create_dir_all(&denied).unwrap();
    let socket = denied.join("h.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    let options = cli::RuntimeOptions {
        endpoint: Some(socket.display().to_string()),
        space: Some("s1".into()),
        pane: Some("p1".into()),
        ..Default::default()
    };
    let result =
        namedrop::deferred_commands::try_defer("address.list", &json!({}), &options, &Empty);
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
    drop(listener);
    let error = result
        .expect("permission denied must enter deferred fallback")
        .unwrap_err();
    host_pair(
        "deferred missing state",
        &error,
        "plugin state is required for deferred commands",
        "a state directory is required for deferred commands",
    );
}
