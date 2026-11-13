//! Pins the Herdr arguments each port call runs, over a scripted transport.

use super::{wire::input_failure, *};
use crate::{
    AgentRuntime, AgentSessionId, SpaceId, WorktreeId,
    mux::{
        AgentIdentity, AgentKind, AgentState, Badge, Checkout, Gone, Key, MuxEvent, PaneFacts,
        PaneSlot, PopupScreen, PopupSpec, PromptRequest, Recipient, Retry, SessionClaim,
        StateTexts, Style, TerminalId, Wrap,
    },
};
use serde_json::json;
use std::{
    path::Path,
    sync::{Mutex, atomic::AtomicBool},
    time::Duration,
};

type Reply = Box<dyn Fn(&[&str]) -> Result<String, String> + Send + Sync>;

/// Records every command and answers from `reply`.
struct Scripted {
    calls: Mutex<Vec<String>>,
    reply: Reply,
}

impl Transport for Scripted {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.calls.lock().unwrap().push(args.join(" "));
        (self.reply)(args)
    }
}

fn herdr(
    reply: impl Fn(&[&str]) -> Result<String, String> + Send + Sync + 'static,
) -> HerdrMux<Scripted> {
    HerdrMux::new(
        Scripted {
            calls: Mutex::new(Vec::new()),
            reply: Box::new(reply),
        },
        "socket-a",
    )
}

fn calls(mux: &HerdrMux<Scripted>) -> Vec<String> {
    mux.transport.calls.lock().unwrap().clone()
}

fn error(code: &str) -> String {
    json!({"error": {"code": code}}).to_string()
}

fn pane_list() -> String {
    json!({"result": {"panes": [
        {
            "pane_id": "p1", "terminal_id": "t1", "tab_id": "tab1", "workspace_id": "w1",
            "cwd": "/repo", "label": null, "title": "shell title",
            "terminal_title_stripped": "stripped", "agent": "claude",
            "display_agent": "Claude Code", "agent_session": {"value": "s1"},
            "agent_status": "idle", "state_labels": {"idle": "@author ready", "working": "w"},
        },
        {
            "pane_id": "p2", "terminal_id": "t2", "tab_id": "tab1", "workspace_id": "w2",
            "terminal_title_stripped": "zsh",
        },
        {
            "pane_id": "p3", "terminal_id": "t3", "tab_id": "tab2", "workspace_id": "w1",
            "label": "@reviewer", "agent_status": "thinking",
            "agent_session": {"value": "s3"},
        },
    ]}})
    .to_string()
}

fn worktrees(open: &str, path: &str) -> String {
    json!({"result": {
        "source": {"repo_key": "repo-key"},
        "worktrees": [
            {"path": "/elsewhere", "open_workspace_id": "w9"},
            {"path": path, "open_workspace_id": open, "label": "feature", "branch": "feat/x",
             "is_linked_worktree": true},
        ],
    }})
    .to_string()
}

fn space(id: &str) -> SpaceId {
    SpaceId::new(id)
}

#[test]
fn a_space_with_panes_is_one_worktree_listing_and_one_pane_listing() {
    let mux = herdr(|args| match args {
        ["worktree", "list", "--workspace", "w1", "--json"] => Ok(worktrees("w1", "/repo")),
        ["pane", "list", "--workspace", "w1"] => Ok(pane_list()),
        _ => Err("unexpected".into()),
    });
    let survey = mux
        .survey(Scope::Space(&space("w1")), Depth::Panes)
        .unwrap();
    assert_eq!(
        calls(&mux),
        [
            "worktree list --workspace w1 --json",
            "pane list --workspace w1"
        ]
    );
    let view = &survey.spaces[0];
    assert_eq!(view.worktree, Some(WorktreeId::new("/repo")));
    assert_eq!(
        view.checkout,
        Some(Checkout {
            path: "/repo".into(),
            label: Some("feature".into()),
            branch: Some("feat/x".into()),
            repo_key: Some("repo-key".into()),
            repo_name: None,
        })
    );
    assert_eq!(
        view.panes[0],
        PaneFacts {
            id: PaneId::new("p1"),
            terminal: TerminalId::new("t1"),
            tab: TabId::new("tab1"),
            space: space("w1"),
            cwd: Some("/repo".into()),
            title: Some("shell title".into()),
            agent: Some(AgentIdentity {
                kind: AgentKind::Known(AgentRuntime::ClaudeCode),
                label: Some("Claude Code".into()),
                session: SessionClaim::Reported(AgentSessionId::new("s1")),
            }),
            state: Some(AgentState::Idle),
            idle_label: Some("@author ready".into()),
            viewport_rows: None,
            reports_session: true,
        }
    );
    assert_eq!(view.panes[1].title.as_deref(), Some("zsh"));
    assert_eq!(view.panes[1].agent, None, "a free terminal pane");
    assert_eq!(
        view.panes[2].agent.as_ref().map(|agent| &agent.kind),
        Some(&AgentKind::Unreported)
    );
    assert_eq!(
        view.panes[2].state,
        Some(AgentState::Unknown("thinking".into()))
    );
}

#[test]
fn a_space_outside_git_keeps_the_synthetic_worktree_key() {
    let mux = herdr(|args| match args {
        ["worktree", ..] => Err(error("not_git_worktree")),
        ["pane", "list", "--workspace", "w1"] => Ok(json!({"result": {"panes": []}}).to_string()),
        _ => Err("unexpected".into()),
    });
    let survey = mux
        .survey(Scope::Space(&space("w1")), Depth::Panes)
        .unwrap();
    assert_eq!(survey.spaces[0].checkout, None);
    assert_eq!(
        survey.spaces[0].worktree,
        Some(WorktreeId::new("herdr-space:w1"))
    );
    assert_eq!(calls(&mux).len(), 2);
}

#[test]
fn a_closed_space_is_gone_without_listing_its_panes() {
    let mux = herdr(|_| Err(error("workspace_not_found")));
    assert_eq!(
        mux.survey(Scope::Space(&space("w1")), Depth::Panes),
        Err(MuxError::Gone(Gone::Space, error("workspace_not_found")))
    );
    assert_eq!(calls(&mux), ["worktree list --workspace w1 --json"]);
}

#[test]
fn a_space_is_gone_only_by_its_error_code_not_its_message() {
    let transient =
        json!({"error": {"code": "transport_unavailable", "message": "workspace_not_found"}})
            .to_string();
    let reply = transient.clone();
    let mux = herdr(move |_| Err(reply.clone()));
    assert_eq!(
        mux.survey(Scope::Space(&space("w1")), Depth::Panes),
        Err(MuxError::Unavailable(transient))
    );
}

#[test]
fn a_listing_that_omits_the_space_is_a_protocol_error() {
    let mux = herdr(|_| Ok(worktrees("w2", "/repo")));
    assert!(matches!(
        mux.survey(Scope::Space(&space("w1")), Depth::Panes),
        Err(MuxError::Protocol(message)) if message.contains("does not contain space w1")
    ));
}

#[test]
fn a_space_summary_is_one_workspace_get() {
    let mux = herdr(|_| {
        Ok(
            json!({"result": {"workspace": {"label": "main", "number": 3, "worktree": {
            "checkout_path": "/repo", "repo_key": "k", "repo_name": "repo"}}}})
            .to_string(),
        )
    });
    let survey = mux
        .survey(Scope::Space(&space("w1")), Depth::Spaces)
        .unwrap();
    assert_eq!(calls(&mux), ["workspace get w1"]);
    let view = &survey.spaces[0];
    assert_eq!(
        (view.label.as_deref(), view.number),
        (Some("main"), Some(3))
    );
    assert_eq!(
        view.checkout.as_ref().and_then(|c| c.repo_name.as_deref()),
        Some("repo")
    );
    assert_eq!(view.worktree, None);
}

#[test]
fn every_space_is_one_workspace_list() {
    let mux = herdr(|_| {
        Ok(json!({"result": {"workspaces": [
            {"workspace_id": "w2", "label": null, "number": 2},
            {"workspace_id": "w1", "label": "main", "number": 1,
             "worktree": {"checkout_path": "/repo", "repo_name": "repo"}},
        ]}})
        .to_string())
    });
    let spaces = mux.survey(Scope::All, Depth::Spaces).unwrap().spaces;
    assert_eq!(calls(&mux), ["workspace list"]);
    assert_eq!(
        spaces
            .iter()
            .map(|view| (view.id.as_str(), view.number, view.checkout.is_some()))
            .collect::<Vec<_>>(),
        [("w2", Some(2), false), ("w1", Some(1), true)]
    );
}

#[test]
fn every_pane_is_one_pane_list_grouped_by_space() {
    let mux = herdr(|_| Ok(pane_list()));
    let spaces = mux.survey(Scope::All, Depth::Panes).unwrap().spaces;
    assert_eq!(calls(&mux), ["pane list"]);
    assert_eq!(
        spaces
            .iter()
            .map(|view| (
                view.id.as_str(),
                view.panes
                    .iter()
                    .map(|pane| pane.id.as_str())
                    .collect::<Vec<_>>()
            ))
            .collect::<Vec<_>>(),
        [("w1", vec!["p1", "p3"]), ("w2", vec!["p2"])]
    );
}

#[test]
fn one_space_s_panes_are_one_pane_list_for_that_space() {
    let mux = herdr(|_| {
        Ok(json!({"result": {"panes": [
            {"pane_id": "p1", "terminal_id": "t1", "tab_id": "tab1", "workspace_id": "w1",
             "agent_session": null, "scroll": {"viewport_rows": 40}},
            {"pane_id": "p2", "terminal_id": "t2", "tab_id": "tab1", "workspace_id": "w1",
             "scroll": {"viewport_rows": "40"}},
            {"pane_id": "p3", "terminal_id": "t3", "tab_id": "tab1", "workspace_id": "w1",
             "agent_session": {"value": "s3"}},
        ]}})
        .to_string())
    });
    for depth in [Depth::Spaces, Depth::Panes] {
        let spaces = mux
            .survey(Scope::PanesOf(&space("w1")), depth)
            .unwrap()
            .spaces;
        let [view] = spaces.as_slice() else {
            panic!("one space: {spaces:?}");
        };
        assert_eq!(view.id, space("w1"));
        assert_eq!(
            (&view.label, &view.checkout, &view.worktree),
            (&None, &None, &None)
        );
        assert_eq!(
            view.panes
                .iter()
                .map(|pane| (pane.id.as_str(), pane.viewport_rows, pane.reports_session))
                .collect::<Vec<_>>(),
            [
                ("p1", Some(40), true),
                ("p2", None, false),
                ("p3", None, true)
            ]
        );
        assert_eq!(
            view.panes[0].agent, None,
            "a null session is still a free pane"
        );
    }
    assert_eq!(
        calls(&mux),
        ["pane list --workspace w1", "pane list --workspace w1"]
    );
}

#[test]
fn an_unreadable_shell_pid_is_no_shell_beside_a_readable_tty() {
    let mux = herdr(|_| {
        Ok(
            json!({"result": {"process_info": {"tty": "/dev/pts/3", "shell_pid": "bad"}}})
                .to_string(),
        )
    });
    let process = mux.process(&PaneId::new("p1")).unwrap();
    assert_eq!(
        (process.shell_pid, process.tty),
        (None, Some("/dev/pts/3".into()))
    );
}

/// `w1` opens `/repo`. `w2` is stamped on another checkout, `w3` is in
/// another repository, `w4` closed while listing, and `w5` is outside Git.
fn checkout_world(args: &[&str]) -> Result<String, String> {
    match args {
        ["worktree", "list", "--cwd", "/repo", "--json"] => Ok(worktrees("w1", "/repo")),
        ["workspace", "list"] => Ok(json!({"result": {"workspaces": [
            {"workspace_id": "w1"},
            {"workspace_id": "w2", "worktree": {"checkout_path": "/other"}},
            {"workspace_id": "w3"},
            {"workspace_id": "w4"},
            {"workspace_id": "w5"},
        ]}})
        .to_string()),
        ["worktree", "list", "--workspace", "w3", "--json"] => Ok(json!({"result": {
            "source": {"repo_key": "another"},
            "worktrees": [{"path": "/repo", "open_workspace_id": "w3"}],
        }})
        .to_string()),
        ["worktree", "list", "--workspace", "w4", "--json"] => Err(error("workspace_not_found")),
        ["worktree", "list", "--workspace", "w5", "--json"] => Err(error("not_git_worktree")),
        ["pane", "list", "--workspace", "w1"] => Ok(pane_list()),
        _ => Err(format!("unexpected {args:?}")),
    }
}

#[test]
fn a_checkout_lookup_proves_one_owner_before_listing_its_panes() {
    let mux = herdr(checkout_world);
    let survey = mux
        .survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes)
        .unwrap();
    assert_eq!(
        calls(&mux),
        [
            "worktree list --cwd /repo --json",
            "workspace list",
            "worktree list --workspace w3 --json",
            "worktree list --workspace w4 --json",
            "worktree list --workspace w5 --json",
            "pane list --workspace w1",
        ]
    );
    assert_eq!(survey.spaces[0].id, space("w1"));
    assert_eq!(survey.spaces[0].panes.len(), 3);

    let spaces_only = herdr(checkout_world);
    spaces_only
        .survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Spaces)
        .unwrap();
    assert_eq!(calls(&spaces_only).len(), 5, "no pane listing");
}

#[test]
fn a_checkout_open_in_two_spaces_is_refused_without_listing_panes() {
    let mux = herdr(|args| match args {
        ["workspace", "list"] => Ok(json!({"result": {"workspaces": [
            {"workspace_id": "w1"},
            {"workspace_id": "w2", "worktree": {"checkout_path": "/repo"}},
        ]}})
        .to_string()),
        _ => checkout_world(args),
    });
    let refused = mux.survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes);
    assert_eq!(
        refused,
        Err(MuxError::Unavailable(
            "Checkout /repo is open in more than one Herdr space (w1, w2), so Namedrop can't \
             tell which one this command is for. Pass --herdr-workspace <space-id> (or \
             --herdr-pane <pane-id>)."
                .into()
        ))
    );
    assert!(!calls(&mux).iter().any(|call| call.starts_with("pane list")));
}

#[test]
fn workspace_discovery_rejects_successful_snapshots_that_omit_the_requested_space() {
    let mux = herdr(|args| match args {
        ["worktree", "list", "--workspace", "space-a", "--json"] => Ok(
            r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"space-b"}]}}"#
                .to_owned(),
        ),
        _ => panic!("pane discovery must not run after a mismatched snapshot: {args:?}"),
    });

    let error = crate::discover_worktree_for_workspace(&mux, "space-a").unwrap_err();
    assert!(error.contains("does not contain space space-a"), "{error}");
}

#[test]
fn workspace_discovery_attaches_a_second_space_on_the_source_checkout() {
    let mux = herdr(|args| {
        match args {
        ["worktree", "list", "--workspace", "space-b", "--json"] => Ok(r#"{"result":{"source":{"repo_key":"/repo/.git","repo_name":"repo","repo_root":"/repo","source_checkout_path":"/repo","source_workspace_id":"space-b"},"worktrees":[{"path":"/repo","open_workspace_id":"space-a","label":"repo","branch":"main","is_linked_worktree":false}]}}"#.to_owned()),
        ["pane", "list", "--workspace", "space-b"] => Ok(r#"{"result":{"panes":[{"pane_id":"space-b:pane-1","terminal_id":"terminal-1","tab_id":"tab-1","workspace_id":"space-b"}]}}"#.to_owned()),
        _ => panic!("unexpected Herdr command: {args:?}"),
    }
    });

    let discovered = crate::discover_worktree_for_workspace(&mux, "space-b").unwrap();
    assert_eq!(discovered.workspace_id, "space-b");
    assert_eq!(discovered.worktree_id, WorktreeId::new("/repo"));
    assert!(discovered.worktree_attached);
    assert_eq!(discovered.label.as_deref(), Some("repo"));
    assert_eq!(discovered.branch.as_deref(), Some("main"));
    assert_eq!(discovered.panes.len(), 1);
    assert_eq!(discovered.panes[0].pane_id, "space-b:pane-1");
    assert_eq!(discovered.panes[0].workspace_id, "space-b");
}

#[test]
fn workspace_discovery_keeps_failing_closed_when_source_names_another_space() {
    let mux = herdr(|args| {
        match args {
        ["worktree", "list", "--workspace", "space-b", "--json"] => Ok(r#"{"result":{"source":{"source_checkout_path":"/repo","source_workspace_id":"space-c"},"worktrees":[{"path":"/repo","open_workspace_id":"space-c","label":"repo","branch":"main"},{"path":"/repo-feature","open_workspace_id":"space-a","label":"repo-feature","branch":"feature","is_linked_worktree":true}]}}"#.to_owned()),
        _ => panic!("pane discovery must not run for a linked-worktree duplicate: {args:?}"),
    }
    });

    let error = crate::discover_worktree_for_workspace(&mux, "space-b").unwrap_err();
    assert!(error.contains("does not contain space space-b"), "{error}");
}

/// Herdr with checkout `/repo` (of repository `/repo/.git`, linked when
/// `linked`, unreported when `None`) first opened by `opener`. Each space in
/// `stamped` carries Herdr's `worktree.checkout_path` in `workspace list`;
/// each space in `listings` answers `worktree list --workspace` instead.
struct Spaces {
    opener: &'static str,
    linked: Option<bool>,
    stamped: Vec<(&'static str, &'static str)>,
    listings: Vec<(&'static str, Result<&'static str, &'static str>)>,
}

impl Spaces {
    fn mux(self) -> HerdrMux<Scripted> {
        herdr(move |args| match args {
            ["worktree", "list", "--cwd", "/repo", "--json"] => {
                let mut checkout = json!({"path": "/repo", "open_workspace_id": self.opener});
                if let Some(linked) = self.linked {
                    checkout["is_linked_worktree"] = linked.into();
                }
                Ok(json!({"result": {"source": {"repo_key": "/repo/.git",
                    "source_checkout_path": "/repo", "source_workspace_id": self.opener},
                    "worktrees": [checkout]}})
                .to_string())
            }
            ["workspace", "list"] => {
                let spaces = std::iter::once(json!({"workspace_id": self.opener}))
                    .chain(self.stamped.iter().map(|(id, checkout)| {
                        json!({"workspace_id": id, "worktree": {
                            "checkout_path": checkout, "repo_key": "/repo/.git"}})
                    }))
                    .chain(
                        self.listings
                            .iter()
                            .map(|(id, _)| json!({"workspace_id": id})),
                    )
                    .collect::<Vec<_>>();
                Ok(json!({"result": {"workspaces": spaces}}).to_string())
            }
            ["worktree", "list", "--workspace", id, "--json"] => {
                match self.listings.iter().find(|(space, _)| space == id) {
                    Some((_, listing)) => listing.map(str::to_owned).map_err(str::to_owned),
                    None => panic!("unexpected space {id}"),
                }
            }
            ["pane", "list", "--workspace", id] if *id == self.opener => {
                Ok(r#"{"result":{"panes":[]}}"#.to_owned())
            }
            _ => panic!("unexpected Herdr command: {args:?}"),
        })
    }
}

const SECOND_SPACE_ON_REPO: &str = r#"{"result":{"source":{"source_checkout_path":"/repo","source_workspace_id":"space-a"},"worktrees":[{"path":"/repo","open_workspace_id":"space-b"}]}}"#;
const NOT_GIT_WORKTREE: &str = r#"{"error":{"code":"not_git_worktree"}}"#;

#[test]
fn cwd_discovery_refuses_a_checkout_two_spaces_share() {
    let herdr = Spaces {
        opener: "space-b",
        linked: Some(false),
        stamped: vec![],
        listings: vec![("space-a", Ok(SECOND_SPACE_ON_REPO))],
    }
    .mux();

    let error = crate::discover_worktree(&herdr, Path::new("/repo")).unwrap_err();
    assert!(error.contains("/repo"), "{error}");
    assert!(error.contains("(space-a, space-b)"), "{error}");
    assert!(error.contains("--herdr-workspace"), "{error}");
}

#[test]
fn cwd_discovery_ignores_spaces_on_other_checkouts_or_none() {
    let herdr = Spaces {
        opener: "space-a",
        linked: Some(false),
        stamped: vec![],
        listings: vec![
            ("space-c", Err(NOT_GIT_WORKTREE)),
            (
                "space-d",
                Ok(
                    r#"{"result":{"source":{"source_checkout_path":"/other","source_workspace_id":"space-d"},"worktrees":[{"path":"/repo","open_workspace_id":"space-a"},{"path":"/other","open_workspace_id":"space-e"}]}}"#,
                ),
            ),
        ],
    }
    .mux();

    let discovered = crate::discover_worktree(&herdr, Path::new("/repo")).unwrap();
    assert_eq!(discovered.workspace_id, "space-a");
    assert_eq!(discovered.worktree_id, WorktreeId::new("/repo"));
}

#[test]
fn cwd_discovery_fails_closed_when_a_space_cannot_be_read() {
    let herdr = Spaces {
        opener: "space-a",
        linked: Some(true),
        stamped: vec![],
        listings: vec![("space-b", Err("herdr: connection reset"))],
    }
    .mux();

    let error = crate::discover_worktree(&herdr, Path::new("/repo")).unwrap_err();
    assert_eq!(error, "herdr: connection reset");
}

// What Herdr 0.9.1 answers a second space created with `workspace create
// --cwd` on a linked worktree: the path's first opener, and `source` naming
// the repository root's opener.
const SECOND_SPACE_ON_LINKED: &str = r#"{"result":{"source":{"repo_key":"/repo/.git","source_checkout_path":"/root"},"worktrees":[{"path":"/root"},{"path":"/repo","is_linked_worktree":true,"open_workspace_id":"space-a"}]}}"#;

#[test]
fn cwd_discovery_refuses_a_linked_checkout_a_second_space_may_share() {
    let herdr = Spaces {
        opener: "space-a",
        linked: Some(true),
        stamped: vec![],
        listings: vec![("space-b", Ok(SECOND_SPACE_ON_LINKED))],
    }
    .mux();

    let error = crate::discover_worktree(&herdr, Path::new("/repo")).unwrap_err();
    assert!(error.contains("/repo"), "{error}");
    assert!(error.contains("space-a"), "{error}");
    assert!(error.contains("space-b"), "{error}");
    assert!(error.contains("--herdr-workspace"), "{error}");
}

#[test]
fn cwd_discovery_treats_an_unreported_checkout_kind_as_linked() {
    let herdr = Spaces {
        opener: "space-a",
        linked: None,
        stamped: vec![],
        listings: vec![("space-b", Ok(SECOND_SPACE_ON_LINKED))],
    }
    .mux();

    let error = crate::discover_worktree(&herdr, Path::new("/repo")).unwrap_err();
    assert!(error.contains("space-b"), "{error}");
}

#[test]
fn cwd_discovery_counts_a_space_herdr_stamped_on_the_checkout() {
    let herdr = Spaces {
        opener: "space-a",
        linked: Some(false),
        stamped: vec![("space-b", "/repo"), ("space-c", "/repo-linked")],
        listings: vec![],
    }
    .mux();

    let error = crate::discover_worktree(&herdr, Path::new("/repo")).unwrap_err();
    assert!(error.contains("(space-a, space-b)"), "{error}");
}

#[test]
fn cwd_discovery_places_an_unreported_space_off_a_root_checkout() {
    let herdr = Spaces {
        opener: "space-a",
        linked: Some(false),
        stamped: vec![],
        listings: vec![("space-b", Ok(SECOND_SPACE_ON_LINKED))],
    }
    .mux();

    assert_eq!(
        crate::discover_worktree(&herdr, Path::new("/repo"))
            .unwrap()
            .workspace_id,
        "space-a"
    );
}

#[test]
fn cwd_discovery_skips_spaces_that_cannot_own_a_linked_checkout() {
    let herdr = Spaces {
        opener: "space-a",
        linked: Some(true),
        stamped: vec![("space-e", "/repo-other")],
        listings: vec![
            (
                "space-c",
                Err(
                    r#"{"error":{"code":"workspace_not_found","message":"workspace space-c not found"}}"#,
                ),
            ),
            (
                "space-d",
                Ok(
                    r#"{"result":{"source":{"repo_key":"/other/.git","source_checkout_path":"/other"},"worktrees":[{"path":"/other"},{"path":"/other-linked","is_linked_worktree":true,"open_workspace_id":"space-x"}]}}"#,
                ),
            ),
            (
                "space-f",
                Ok(
                    r#"{"result":{"source":{"repo_key":"/repo/.git","source_checkout_path":"/root","source_workspace_id":"space-f"},"worktrees":[{"path":"/root","open_workspace_id":"space-x"},{"path":"/repo","is_linked_worktree":true,"open_workspace_id":"space-a"}]}}"#,
                ),
            ),
        ],
    }
    .mux();

    assert_eq!(
        crate::discover_worktree(&herdr, Path::new("/repo"))
            .unwrap()
            .workspace_id,
        "space-a"
    );
}

#[test]
fn reads_run_one_command_each() {
    let mux = herdr(|args| match args {
        ["pane", "read", ..] => Ok("screen text".into()),
        ["pane", "process-info", ..] => Ok(json!({"result": {"process_info": {
            "shell_pid": 10, "foreground_process_group_id": 11,
            "foreground_processes": [{"pid": 11}, {"pid": 12}], "tty": "/dev/ttys001"}}})
        .to_string()),
        ["tab", "get", "tab1"] => Ok(json!({"result": {"tab": {"label": "@author"}}}).to_string()),
        ["plugin", ..] => Ok(json!({"result": {"plugins": [
            {"plugin_id": "other", "plugin_root": "/x"},
            {"plugin_id": "namedrop", "plugin_root": "/plugins/namedrop"},
        ]}})
        .to_string()),
        _ => Err("unexpected".into()),
    });
    let pane = PaneId::new("p1");
    for (wrap, style) in [(Wrap::AsDrawn, Style::Ansi), (Wrap::Joined, Style::Plain)] {
        let request = ScreenRequest {
            lines: 60,
            wrap,
            style,
        };
        assert_eq!(mux.screen(&pane, request).unwrap().text, "screen text");
    }
    assert_eq!(
        mux.process(&pane),
        Ok(PaneProcess {
            shell_pid: Some(10),
            foreground_group: Some(11),
            foreground_pids: vec![11, 12],
            tty: Some("/dev/ttys001".into()),
        })
    );
    assert_eq!(
        mux.tab_label(&TabId::new("tab1")),
        Ok(Some("@author".into()))
    );
    assert_eq!(mux.integration_root(), Ok(Some("/plugins/namedrop".into())));
    assert_eq!(
        calls(&mux),
        [
            "pane read p1 --source recent --lines 60 --format ansi",
            "pane read p1 --source recent-unwrapped --lines 60",
            "pane process-info --pane p1",
            "tab get tab1",
            "plugin list --plugin namedrop --json",
        ]
    );
}

#[test]
fn transport_failures_map_to_port_errors() {
    for (failure, expected) in [
        (
            error("pane_not_found"),
            MuxError::Gone(Gone::Pane, error("pane_not_found")),
        ),
        (
            "Herdr command timed out after 10000 ms".to_owned(),
            MuxError::Timeout("Herdr command timed out after 10000 ms".to_owned()),
        ),
        (
            "Herdr read exceeded its output limit".to_owned(),
            MuxError::TooLarge("Herdr read exceeded its output limit".to_owned()),
        ),
        (
            error("server_busy"),
            MuxError::Unavailable(error("server_busy")),
        ),
    ] {
        let reply = failure.clone();
        let mux = herdr(move |_| Err(reply.clone()));
        assert_eq!(mux.process(&PaneId::new("p1")), Err(expected), "{failure}");
    }
    let garbled = herdr(|_| Ok("not json".into()));
    assert!(matches!(
        garbled.process(&PaneId::new("p1")),
        Err(MuxError::Protocol(_))
    ));
}

fn target() -> PaneFacts {
    PaneFacts {
        id: PaneId::new("p1"),
        terminal: TerminalId::new("t1"),
        tab: TabId::new("tab1"),
        space: space("w1"),
        cwd: None,
        title: None,
        agent: Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::ClaudeCode),
            label: None,
            session: SessionClaim::Reported(AgentSessionId::new("s1")),
        }),
        state: None,
        idle_label: None,
        viewport_rows: None,
        reports_session: true,
    }
}

#[test]
fn each_effect_runs_today_s_command() {
    let mux = herdr(|_| Ok("{}".into()));
    let texts = StateTexts {
        ready: "@a ready".into(),
        working: "@a working".into(),
        attention: "@a needs attention".into(),
        unknown: "@a status unknown".into(),
    };
    for effect in [
        Effect::Identify {
            pane: PaneId::new("p1"),
            alias: "a".into(),
            texts,
        },
        Effect::Badges {
            pane: PaneId::new("p1"),
            badges: vec![
                Badge::Awaiting(2),
                Badge::Attention(0),
                Badge::Relay {
                    alias: "b".into(),
                    summary: "done".into(),
                },
            ],
        },
        Effect::RenameTab {
            tab: TabId::new("tab1"),
            label: "@a".into(),
        },
        Effect::Notify {
            title: "@b replied".into(),
            body: "done".into(),
            sound: true,
        },
        Effect::Notify {
            title: "Namedrop".into(),
            body: "Sent to @b".into(),
            sound: false,
        },
        Effect::OpenPopup(Box::new(PopupSpec {
            screen: PopupScreen::Status,
            target: target(),
            state_dir: "/state".into(),
            socket: "socket-a".into(),
        })),
        Effect::OpenPopup(Box::new(PopupSpec {
            screen: PopupScreen::Tour,
            target: target(),
            state_dir: "/state".into(),
            socket: "socket-a".into(),
        })),
        Effect::ReloadKeybindings,
    ] {
        let applied = if matches!(effect, Effect::OpenPopup(_)) {
            Applied::Opened(json!({}))
        } else {
            Applied::Done
        };
        assert_eq!(mux.apply(&effect), Ok(applied));
    }
    let popup_env = "--env NAMEDROP_TARGET_WORKSPACE=w1 --env NAMEDROP_TARGET_PANE=p1 \
                     --env NAMEDROP_TARGET_TERMINAL=t1 --env NAMEDROP_TARGET_SESSION=s1 \
                     --env HERDR_PLUGIN_STATE_DIR=/state --env HERDR_SOCKET_PATH=socket-a --focus";
    assert_eq!(
        calls(&mux),
        [
            "pane report-metadata p1 --source namedrop-identity --title @a \
             --state-label idle=@a ready --state-label done=@a ready \
             --state-label working=@a working --state-label blocked=@a needs attention \
             --state-label unknown=@a status unknown"
                .to_owned(),
            "pane report-metadata p1 --source namedrop --token namedrop-awaiting=2 \
             --token namedrop-attention=0"
                .to_owned(),
            "pane report-metadata p1 --source namedrop-relay --token namedrop-relay=@b: done \
             --ttl-ms 15000"
                .to_owned(),
            "tab rename tab1 @a".to_owned(),
            "notification show @b replied --body done --position bottom-right --sound done"
                .to_owned(),
            "notification show Namedrop --body Sent to @b --position bottom-right".to_owned(),
            format!(
                "plugin pane open --plugin namedrop --entrypoint agent-status --placement popup \
                 --width 84 --height 24 {popup_env}"
            ),
            format!(
                "plugin pane open --plugin namedrop --entrypoint tour --placement popup \
                 --width 72 --height 24 {popup_env}"
            ),
            "server reload-config".to_owned(),
        ]
    );
}

fn tour_popup() -> Effect {
    Effect::OpenPopup(Box::new(PopupSpec {
        screen: PopupScreen::Tour,
        target: target(),
        state_dir: "/state".into(),
        socket: "/tmp/herdr.sock".into(),
    }))
}

#[test]
fn a_popup_returns_herdr_s_reply_and_an_unreadable_one_is_a_protocol_error() {
    let reply = json!({"result": {"type": "plugin_pane_opened", "pane_id": "popup-1"}});
    let opened = herdr(move |_| Ok(reply.to_string()));
    assert_eq!(
        opened.apply(&tour_popup()),
        Ok(Applied::Opened(
            json!({"result": {"type": "plugin_pane_opened", "pane_id": "popup-1"}})
        ))
    );
    let garbled = herdr(|_| Ok("not json".into()));
    assert!(matches!(
        garbled.apply(&tour_popup()),
        Err(MuxError::Protocol(_))
    ));
    let refused = herdr(|_| Err(error("plugin_not_found")));
    assert_eq!(
        refused.apply(&tour_popup()),
        Err(MuxError::Unavailable(error("plugin_not_found")))
    );
}

/// The socket comes from the popup's spec, which `lifecycle::open_popup`
/// fills from the runtime's `session_socket_id`, never from the endpoint of
/// the connection that opens it.
#[test]
fn a_popup_names_the_socket_its_spec_carries_whatever_the_connection() {
    let argv = "plugin pane open --plugin namedrop --entrypoint tour --placement popup --width 72 \
                --height 24 --env NAMEDROP_TARGET_WORKSPACE=w1 --env NAMEDROP_TARGET_PANE=p1 \
                --env NAMEDROP_TARGET_TERMINAL=t1 --env NAMEDROP_TARGET_SESSION=s1 \
                --env HERDR_PLUGIN_STATE_DIR=/state --env HERDR_SOCKET_PATH=/tmp/herdr.sock --focus";
    for socket in ["socket-a", ""] {
        let mux = HerdrMux::new(
            Scripted {
                calls: Mutex::new(Vec::new()),
                reply: Box::new(|_| Ok("{}".into())),
            },
            socket,
        );
        assert_eq!(mux.endpoint().key, socket);
        mux.apply(&tour_popup()).unwrap();
        assert_eq!(calls(&mux), [argv]);
    }
}

#[test]
fn input_runs_today_s_command_for_each_kind() {
    let mux = herdr(|_| Ok("{}".into()));
    let pane = PaneId::new("p1");
    let expect = target().agent.unwrap();
    let request = PromptRequest {
        expect: &expect,
        text: "hello there",
    };
    for kind in [
        InputKind::Prompt(request),
        InputKind::Line("namedrop terminal-execute"),
        InputKind::Keys {
            recipient: Recipient::Agent,
            keys: &[Key::CtrlU, Key::CtrlU],
        },
        InputKind::Keys {
            recipient: Recipient::Pane,
            keys: &[Key::Escape, Key::Enter, Key::Up, Key::Down, Key::CtrlC],
        },
    ] {
        assert_eq!(mux.send(&pane, kind), InputOutcome::Delivered);
    }
    assert_eq!(
        calls(&mux),
        [
            "agent prompt p1 hello there",
            "pane run p1 namedrop terminal-execute",
            "agent send-keys p1 ctrl+u ctrl+u",
            "pane send-keys p1 esc enter up down ctrl+c",
        ]
    );
}

#[test]
fn herdr_error_codes_decide_the_input_outcome() {
    for (code, outcome) in [
        (
            "agent_not_found",
            InputOutcome::Declined(Retry::OnEvent, error("agent_not_found")),
        ),
        (
            "pane_not_found",
            InputOutcome::Declined(Retry::OnEvent, error("pane_not_found")),
        ),
        (
            "agent_busy",
            InputOutcome::Declined(Retry::OnEvent, error("agent_busy")),
        ),
        (
            "agent_blocked",
            InputOutcome::Declined(Retry::OnEvent, error("agent_blocked")),
        ),
        (
            "agent_not_ready",
            InputOutcome::Declined(Retry::AfterDelay, error("agent_not_ready")),
        ),
        (
            "conflict",
            InputOutcome::Declined(Retry::AfterDelay, error("conflict")),
        ),
        (
            "server_busy",
            InputOutcome::Declined(Retry::AfterDelay, error("server_busy")),
        ),
        (
            "invalid_request",
            InputOutcome::Refused(error("invalid_request")),
        ),
        ("invalid_args", InputOutcome::Refused(error("invalid_args"))),
        (
            "invalid_input",
            InputOutcome::Refused(error("invalid_input")),
        ),
        ("unsupported", InputOutcome::Refused(error("unsupported"))),
        (
            "unknown_command",
            InputOutcome::Refused(error("unknown_command")),
        ),
        ("internal", InputOutcome::Ambiguous(error("internal"))),
    ] {
        let reply = error(code);
        let mux = herdr(move |_| Err(reply.clone()));
        assert_eq!(
            mux.send(&PaneId::new("p1"), InputKind::Line("ls")),
            outcome,
            "{code}"
        );
    }
    let timed_out = herdr(|_| Err("Herdr command timed out after 10000 ms".into()));
    assert!(matches!(
        timed_out.send(&PaneId::new("p1"), InputKind::Line("ls")),
        InputOutcome::Ambiguous(_)
    ));
}

#[test]
fn timeout_and_unknown_prompt_results_never_authorize_an_automatic_resend() {
    for error in [
        "Herdr command timed out after 10000 ms",
        "connection closed after writing prompt",
        r#"{"error":{"code":"connection_refused"}}"#,
        r#"{"error":{"code":"unexpected"}}"#,
        // Both can only be reported after Herdr accepted the submission, so
        // the recipient may already have the prompt.
        r#"{"error":{"code":"agent_prompt_stalled"}}"#,
        r#"{"error":{"code":"timeout"}}"#,
    ] {
        assert_eq!(
            input_failure(error.to_owned()),
            InputOutcome::Ambiguous(error.to_owned()),
            "{error}"
        );
    }
    // Herdr rejects both before any input is sent. The `agent_blocked` payload
    // is the verbatim stderr of `herdr agent prompt` 0.8.2 against a blocked
    // recipient, exit code 1.
    for error in [
        r#"{"error":{"code":"agent_busy"}}"#,
        r#"{"error":{"code":"agent_blocked","message":"agent w1:p1 is blocked and requires interactive input"},"id":"cli:agent:prompt"}"#,
    ] {
        assert_eq!(
            input_failure(error.to_owned()),
            InputOutcome::Declined(Retry::OnEvent, error.to_owned()),
            "{error}"
        );
    }
}

#[test]
fn temporary_prompt_failures_only_request_polling_without_a_guaranteed_event() {
    for (codes, expected) in [
        (
            &["server_busy", "conflict", "agent_not_ready"][..],
            Retry::AfterDelay,
        ),
        (
            &[
                "agent_busy",
                "agent_blocked",
                "agent_not_found",
                "pane_not_found",
            ][..],
            Retry::OnEvent,
        ),
    ] {
        for code in codes {
            let error = serde_json::json!({"error": {"code": code}}).to_string();
            assert_eq!(
                input_failure(error.clone()),
                InputOutcome::Declined(expected, error),
                "{code}"
            );
        }
    }
    let refused = serde_json::json!({"error": {"code": "invalid_input"}}).to_string();
    assert_eq!(
        input_failure(refused.clone()),
        InputOutcome::Refused(refused)
    );
}

/// Herdr as `tests/inherited_pane.rs` saw it through the executor port: the
/// pane's shell is whatever pid the test says it is. These tests moved here
/// because each pins `pane process-info`'s argv and reply.
fn shell(pid: Option<u64>) -> HerdrMux<Scripted> {
    herdr(move |args| {
        assert_eq!(args, ["pane", "process-info", "--pane", "wH:p1"]);
        let shell_pid = pid.ok_or_else(|| "pane not found".to_owned())?;
        Ok(format!(
            r#"{{"result":{{"process_info":{{"pane_id":"wH:p1","shell_pid":{shell_pid},"foreground_process_group_id":{shell_pid},"foreground_processes":[]}}}}}}"#
        ))
    })
}

/// Herdr names the pane's shell but no foreground process group.
fn shell_without_group(pid: u64) -> HerdrMux<Scripted> {
    herdr(move |args| {
        assert_eq!(args, ["pane", "process-info", "--pane", "wH:p1"]);
        Ok(format!(
            r#"{{"result":{{"process_info":{{"pane_id":"wH:p1","shell_pid":{pid},"foreground_process_group_id":null,"foreground_processes":[]}}}}}}"#
        ))
    })
}

/// A live process that is certainly not an ancestor of this one.
fn unrelated_process() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap()
}

/// Herdr's own `pane list` output, captured from a live pane: many more
/// fields than the fixture's, with the session under `agent_session`.
fn live_pane_list() -> HerdrMux<Scripted> {
    herdr(|args| {
        match args {
        ["pane", "process-info", ..] => Ok(r#"{"id":"cli:pane:process_info","result":{"process_info":{"foreground_process_group_id":1147346,"foreground_processes":[{"argv":["claude"],"cmdline":"claude","cwd":"/repo","name":"claude","pid":1147346}],"pane_id":"wG:p21","shell_pid":4000000000},"type":"pane_process_info"}}"#.to_owned()),
        ["pane", "list"] => Ok(r#"{"id":"cli:pane:list","result":{"panes":[{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"sess-21"},"agent_status":"working","cwd":"/repo","focused":false,"foreground_cwd":"/repo","pane_id":"wG:p21","revision":20,"scroll":{"max_offset_from_bottom":0,"offset_from_bottom":0,"viewport_rows":40},"state_labels":{"working":"@a working"},"tab_id":"wG:tP","terminal_id":"term_1","terminal_title":"t","title":"@a","tokens":{"x":"0"},"workspace_id":"wG"},{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"sess-1"},"agent_status":"idle","cwd":"/repo","pane_id":"wG:p1","tab_id":"wG:tP","terminal_id":"term_2","workspace_id":"wG"},{"agent_status":"unknown","pane_id":"w9:p1","tab_id":"w9:t1","terminal_id":"term_3","workspace_id":"w9"}],"type":"pane_list"}}"#.to_owned()),
        other => Err(format!("unexpected call {other:?}")),
    }
    })
}

#[test]
fn an_inherited_pane_whose_shell_is_an_ancestor_is_accepted() {
    let parent = u64::from(std::os::unix::process::parent_id());
    assert_eq!(
        crate::verify_inherited_pane(&shell(Some(parent)), "wH:p1", &[]),
        Ok(())
    );
}

#[test]
fn an_inherited_pane_whose_shell_is_not_an_ancestor_is_refused_with_the_remedy() {
    let mut other = unrelated_process();
    let error = crate::verify_inherited_pane(&shell(Some(u64::from(other.id()))), "wH:p1", &[])
        .unwrap_err();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(error.contains("HERDR_PANE_ID=wH:p1"), "{error}");
    assert!(error.contains("not running inside pane wH:p1"), "{error}");
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn an_inherited_pane_herdr_cannot_describe_is_refused_with_herdr_s_own_reason() {
    let error = crate::verify_inherited_pane(&shell(None), "wH:p1", &[]).unwrap_err();
    assert!(error.contains("HERDR_PANE_ID=wH:p1"), "{error}");
    assert!(error.contains("pane not found"), "{error}");
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn an_inherited_pane_herdr_names_no_foreground_group_for_is_refused() {
    let parent = u64::from(std::os::unix::process::parent_id());
    let error =
        crate::verify_inherited_pane(&shell_without_group(parent), "wH:p1", &[]).unwrap_err();
    assert!(
        error.contains("Herdr reports no shell for pane wH:p1"),
        "{error}"
    );
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn the_live_pane_list_shape_vouches_for_the_session_s_own_pane_only() {
    assert_eq!(
        crate::verify_inherited_pane(&live_pane_list(), "wG:p21", &["sess-21"]),
        Ok(())
    );
    let error = crate::verify_inherited_pane(&live_pane_list(), "wG:p21", &["sess-1"]).unwrap_err();
    assert!(error.contains("--herdr-pane"), "{error}");
    let error = crate::verify_inherited_pane(&live_pane_list(), "wG:p21", &["nobody"]).unwrap_err();
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn a_refusal_says_how_many_agent_sessions_it_checked() {
    let none = crate::verify_inherited_pane(&live_pane_list(), "wG:p21", &[]).unwrap_err();
    assert!(
        none.contains("There is no CODEX_SESSION_ID or CLAUDE_CODE_SESSION_ID in the environment"),
        "{none}"
    );
    let elsewhere =
        crate::verify_inherited_pane(&live_pane_list(), "wG:p21", &["sess-1"]).unwrap_err();
    assert!(
        elsewhere
            .contains("None of the 1 agent session ID(s) in the environment runs in pane wG:p21"),
        "{elsewhere}"
    );
}

/// Health's `plugin list` must answer within 2 s and 64 KiB however long
/// the connection's own budget is.
#[test]
fn health_reads_the_integration_root_within_two_seconds_and_64_kib() {
    let mux = crate::mux::fake::FakeMux::world().build();
    assert_eq!(crate::health::registered_plugin_root(&mux), None);
    assert_eq!(
        mux.budgeted_reads(),
        [(
            crate::mux::fake::Read::IntegrationRoot,
            vec![Budget {
                timeout: Duration::from_secs(2),
                max_output_bytes: 64 * 1024,
            }]
        )]
    );
}

#[test]
fn a_scripted_transport_refuses_a_cancelled_command_before_it_starts() {
    let mux = herdr(|_| Ok(json!({"result": {"label": "build"}}).to_string()));
    let cancelled = AtomicBool::new(true);
    let within = mux.within(Allowance::each(|| Ok(Budget::ADAPTER_DEFAULT), &cancelled));
    assert_eq!(
        within.tab_label(&TabId::new("tab1")),
        Err(MuxError::Unavailable("Herdr read cancelled".to_owned()))
    );
    assert!(calls(&mux).is_empty());
}

#[test]
fn a_scripted_connection_within_a_budget_runs_the_same_command() {
    let mux = herdr(|_| Ok(json!({"result": {"label": "build"}}).to_string()));
    let tab = TabId::new("tab1");
    let within = mux.within(Allowance::fixed(Budget {
        timeout: Duration::from_secs(2),
        max_output_bytes: 4096,
    }));
    assert_eq!(within.tab_label(&tab).unwrap().as_deref(), Some("build"));
    assert_eq!(within.endpoint(), mux.endpoint());
    assert_eq!(calls(&mux), ["tab get tab1"]);
}

/// A command, its timeout and output cap, and the address of its flag.
type Run = (String, Duration, usize, Option<usize>);

/// Records the budget and the flag each command runs within, and answers a
/// tab label.
#[derive(Default)]
struct Budgeted {
    runs: Mutex<Vec<Run>>,
}

impl Transport for Budgeted {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.run_within(args, Budget::ADAPTER_DEFAULT, None)
    }

    fn run_within(
        &self,
        args: &[&str],
        budget: Budget,
        cancelled: Option<&AtomicBool>,
    ) -> Result<String, String> {
        self.runs.lock().unwrap().push((
            args.join(" "),
            budget.timeout,
            budget.max_output_bytes,
            cancelled.map(address),
        ));
        Ok(json!({"result": {"label": "build"}}).to_string())
    }
}

fn address(flag: &AtomicBool) -> usize {
    flag as *const AtomicBool as usize
}

#[test]
fn a_budget_within_a_budget_keeps_the_tighter_limit_of_each_and_the_narrowest_flag() {
    let mux = HerdrMux::new(Budgeted::default(), "socket-a");
    let wide = AtomicBool::new(false);
    let reads = mux.within(Allowance::each(
        || {
            Ok(Budget {
                timeout: Duration::from_millis(700),
                max_output_bytes: 1 << 20,
            })
        },
        &wide,
    ));
    let tab = TabId::new("tab1");
    let inner = reads.within(Allowance::fixed(Budget {
        timeout: Duration::from_secs(2),
        max_output_bytes: 4096,
    }));
    inner.tab_label(&tab).unwrap();
    let narrow = AtomicBool::new(false);
    let narrowest = inner.within(Allowance::each(
        || {
            Ok(Budget {
                timeout: Duration::from_millis(300),
                max_output_bytes: 1 << 30,
            })
        },
        &narrow,
    ));
    narrowest.tab_label(&tab).unwrap();
    let ran = |timeout_ms, flag| {
        (
            "tab get tab1".to_owned(),
            Duration::from_millis(timeout_ms),
            4096,
            Some(address(flag)),
        )
    };
    assert_eq!(
        *mux.transport.runs.lock().unwrap(),
        [ran(700, &wide), ran(300, &narrow)]
    );
    // A command watches one flag, so a wider flag that is already set
    // refuses the command before it starts.
    wide.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        narrowest.tab_label(&tab),
        Err(MuxError::Unavailable("Herdr read cancelled".to_owned()))
    );
    assert_eq!(mux.transport.runs.lock().unwrap().len(), 2);
}

fn pane_event(pane: &str, workspace: &str, tab: &str) -> serde_json::Value {
    json!({
        "pane_id": pane, "workspace_id": workspace, "terminal_id": "t1", "tab_id": tab,
        "focused": true, "agent_status": "idle", "revision": 1,
    })
}

fn slot(space: &str, pane: &str, tab: Option<&str>) -> PaneSlot {
    PaneSlot {
        space: SpaceId::new(space),
        pane: PaneId::new(pane),
        tab: tab.map(TabId::new),
    }
}

#[test]
fn plugin_events_decode_to_id_only_mux_events() {
    let lifecycle = "event.pane-lifecycle";
    let status = "event.agent-status-changed";
    for (hook, operation, data, expected) in [
        (
            "pane.created",
            lifecycle,
            json!({"type": "pane_created", "pane": pane_event("p1", "w1", "tab1")}),
            MuxEvent::PaneAppeared(slot("w1", "p1", Some("tab1"))),
        ),
        (
            "pane.closed",
            lifecycle,
            json!({"type": "pane_closed", "pane_id": "p1", "workspace_id": "w1"}),
            MuxEvent::PaneClosed(slot("w1", "p1", None)),
        ),
        (
            "pane.exited",
            lifecycle,
            json!({"type": "pane_exited", "pane_id": "p1", "workspace_id": "w1"}),
            MuxEvent::PaneClosed(slot("w1", "p1", None)),
        ),
        (
            "tab.closed",
            lifecycle,
            json!({"type": "tab_closed", "tab_id": "tab1", "workspace_id": "w1"}),
            MuxEvent::TabClosed {
                space: SpaceId::new("w1"),
                tab: TabId::new("tab1"),
            },
        ),
        (
            "pane.moved",
            lifecycle,
            json!({"type": "pane_moved", "previous_pane_id": "p1",
                   "previous_workspace_id": "w1", "previous_tab_id": "tab1",
                   "pane": pane_event("p2", "w2", "tab2"), "closed_tab_id": "tab1"}),
            MuxEvent::PaneMoved {
                from: slot("w1", "p1", Some("tab1")),
                to: slot("w2", "p2", Some("tab2")),
            },
        ),
        (
            "pane.agent_status_changed",
            status,
            json!({"type": "pane_agent_status_changed", "pane_id": "p1",
                   "workspace_id": "w1", "agent_status": "working"}),
            MuxEvent::AgentStatusChanged(slot("w1", "p1", None)),
        ),
        (
            "pane.agent_detected",
            status,
            json!({"type": "pane_agent_detected", "pane_id": "p1", "workspace_id": "w1",
                   "agent": "codex", "released": false}),
            MuxEvent::AgentDetected(slot("w1", "p1", None)),
        ),
    ] {
        let event = data["type"].as_str().unwrap().to_owned();
        let envelope = json!({"event": event, "data": data}).to_string();
        let raw = plugin_event_data(operation, &envelope, hook);
        assert_eq!(raw.as_ref(), Ok(&data), "{hook}");
        assert_eq!(
            decode_operation_input(operation, raw.unwrap()),
            Ok(expected),
            "{hook}"
        );
    }
}

#[test]
fn a_plugin_event_must_match_its_hook_and_operation_and_schema() {
    let data = json!({"type": "pane_closed", "pane_id": "p1", "workspace_id": "w1"});
    let envelope = json!({"event": "pane_closed", "data": data}).to_string();
    assert!(plugin_event_data("event.pane-lifecycle", &envelope, "pane.created").is_err());
    assert!(plugin_event_data("event.agent-status-changed", &envelope, "pane.closed").is_err());
    let unknown = json!({"event": "pane_resized", "data": {}}).to_string();
    assert_eq!(
        plugin_event_data("event.pane-lifecycle", &unknown, "pane.resized"),
        Err("unsupported Herdr plugin event: pane_resized".into())
    );
    let extra = json!({"type": "pane_closed", "pane_id": "p1", "workspace_id": "w1", "new": 1});
    assert!(decode_operation_input("event.pane-lifecycle", extra).is_err());
}
