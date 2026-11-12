#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/delivered.rs"]
mod delivered;
#[path = "support/executable.rs"]
mod executable;
#[path = "support/xws.rs"]
mod xws;

use namedrop::{StateNamespace, dashboard::snapshot::SpaceKey};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use xws::Fixture;

const UNREACHABLE: &str = "Herdr isn't reachable. Check that Herdr is running, then try again.";

/// A Herdr that keeps its workspaces and panes in a JSON file, so creating
/// and closing them is observable, and logs every call it gets.
const FAKE_HERDR: &str = r#"#!/usr/bin/python3
import json, os, sys
args = sys.argv[1:]
root = os.path.dirname(os.path.abspath(__file__))
path = os.path.join(root, 'herdr-state.json')
with open(os.path.join(root, 'herdr-calls.jsonl'), 'a') as log:
    log.write(json.dumps({'args': args, 'socket': os.environ.get('HERDR_SOCKET_PATH')}) + '\n')
with open(path) as source:
    state = json.load(source)
def save():
    with open(path + '.next', 'w') as out:
        json.dump(state, out)
    os.replace(path + '.next', path)
def failing(flag):
    return os.path.exists(os.path.join(root, 'fail-' + flag))
def option(name):
    return args[args.index(name) + 1]
def new_pane(workspace, tab, cwd):
    pane = {'pane_id': workspace + ':p%d' % state['next'], 'tab_id': tab, 'workspace_id': workspace,
            'terminal_id': 'term-%d' % state['next'], 'cwd': cwd, 'agent': None, 'agent_status': 'unknown'}
    state['panes'].append(pane)
    return pane
if args == ['workspace', 'list']:
    if failing('workspace-list'):
        sys.stderr.write('{"error":{"code":"server_not_running"}}')
        sys.exit(1)
    rows = []
    for number, workspace in enumerate(state['workspaces']):
        row = {'workspace_id': workspace['id'], 'label': workspace['label'], 'number': number + 1}
        if workspace.get('checkout'):
            row['worktree'] = {'checkout_path': workspace['checkout']}
        rows.append(row)
    print(json.dumps({'result': {'type': 'workspace_list', 'workspaces': rows}}))
elif args[:2] == ['workspace', 'create']:
    state['next'] += 1
    workspace = 'w%d' % state['next']
    state['workspaces'].append({'id': workspace, 'label': option('--label')})
    pane = new_pane(workspace, workspace + ':t1', option('--cwd'))
    save()
    print(json.dumps({'result': {'type': 'workspace_created',
        'workspace': {'workspace_id': workspace, 'label': option('--label')},
        'tab': {'tab_id': workspace + ':t1'}, 'root_pane': pane}}))
elif args[:2] == ['tab', 'create']:
    if failing('tab-create'):
        sys.stderr.write('tab create failed')
        sys.exit(1)
    state['next'] += 1
    workspace = option('--workspace')
    tab = workspace + ':t%d' % state['next']
    pane = new_pane(workspace, tab, option('--cwd'))
    save()
    print(json.dumps({'result': {'type': 'tab_created', 'tab': {'tab_id': tab}, 'root_pane': pane}}))
elif args[:2] == ['workspace', 'close']:
    state['workspaces'] = [w for w in state['workspaces'] if w['id'] != args[2]]
    state['panes'] = [p for p in state['panes'] if p['workspace_id'] != args[2]]
    save()
    print('{}')
elif args[:2] == ['tab', 'close']:
    state['panes'] = [p for p in state['panes'] if p['tab_id'] != args[2]]
    save()
    print('{}')
elif args[:2] == ['pane', 'list']:
    panes = state['panes']
    if '--workspace' in args:
        panes = [p for p in panes if p['workspace_id'] == option('--workspace')]
    print(json.dumps({'result': {'panes': panes}}))
elif args[:2] == ['worktree', 'list']:
    if failing('worktree-list'):
        sys.stderr.write('worktree list failed')
        sys.exit(1)
    workspace = [w for w in state['workspaces'] if w['id'] == args[3]]
    if workspace and workspace[0].get('checkout'):
        print(json.dumps({'result': {'worktrees': [{'path': workspace[0]['checkout'], 'open_workspace_id': args[3]}]}}))
    else:
        sys.stderr.write('{"error":{"code":"not_git_worktree"}}')
        sys.exit(1)
elif args[:2] == ['pane', 'process-info']:
    print(json.dumps({'result': {'process_info': {'shell_pid': 42, 'foreground_process_group_id': 42, 'foreground_processes': []}}}))
else:
    print('{}')
"#;

struct Server {
    child: Child,
    state: PathBuf,
    port: u16,
    cookie: String,
    csrf: String,
}

struct Response {
    status: u16,
    body: String,
}

impl Response {
    fn error(&self) -> String {
        serde_json::from_str::<Value>(&self.body).unwrap_or_else(|_| panic!("{}", self.body))
            ["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{}", self.body))
            .to_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("{}", self.body))
    }
}

/// The Herdr this test runs: w1 is a plain folder with two agents, w2 is
/// paused, w3 is deactivated and sits on a checkout.
fn herdr_state(f: &Fixture) -> Value {
    let pane = |workspace: &str, pane: &str, tab: &str, cwd: &Path| {
        json!({"pane_id": pane, "tab_id": tab, "workspace_id": workspace,
               "terminal_id": format!("term-{pane}"), "cwd": cwd, "agent": "codex",
               "agent_status": "idle", "agent_session": {"value": xws::session(pane)}})
    };
    let repo = f.root.join("repo");
    let checkout = f.root.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    json!({
        "next": 3,
        "workspaces": [
            {"id": "w1", "label": "ws1"},
            {"id": "w2", "label": "ws2"},
            {"id": "w3", "label": "ws3", "checkout": checkout},
        ],
        "panes": [
            pane("w1", "pane-1-lead", "w1:t1", &repo),
            pane("w1", "pane-1-planner", "w1:t2", &repo),
            pane("w2", "pane-2-lead", "w2:t1", &repo),
            pane("w3", "pane-3-lead", "w3:t1", &f.root.join("elsewhere")),
        ],
    })
}

fn fixture() -> Fixture {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("home/project")).unwrap();
    fs::write(f.root.join("home/notes.txt"), "").unwrap();
    f.sql(
        "UPDATE worktrees SET paused = 1 WHERE worktree_id = 'w2';
           UPDATE worktrees SET deactivated = 1 WHERE worktree_id = 'w3';",
    );
    write_state(&f, &herdr_state(&f));
    f
}

fn write_state(f: &Fixture, state: &Value) {
    let next = f.root.join("herdr-state.json.test");
    fs::write(&next, state.to_string()).unwrap();
    fs::rename(next, f.root.join("herdr-state.json")).unwrap();
}

fn read_state(f: &Fixture) -> Value {
    serde_json::from_str(&fs::read_to_string(f.root.join("herdr-state.json")).unwrap()).unwrap()
}

fn calls(f: &Fixture) -> Vec<Vec<String>> {
    fs::read_to_string(f.root.join("herdr-calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let call: Value = serde_json::from_str(line).unwrap();
            assert_eq!(call["socket"], "socket-a", "{line}");
            serde_json::from_value(call["args"].clone()).unwrap()
        })
        .collect()
}

fn creations(f: &Fixture) -> Vec<Vec<String>> {
    calls(f)
        .into_iter()
        .filter(|args| {
            matches!(
                args.iter().map(String::as_str).take(2).collect::<Vec<_>>()[..],
                ["workspace" | "tab", "create" | "close"] | ["pane", "run"]
            )
        })
        .collect()
}

fn fail(f: &Fixture, what: &str, failing: bool) {
    let flag = f.root.join(format!("fail-{what}"));
    if failing {
        fs::write(flag, "").unwrap();
    } else {
        let _ = fs::remove_file(flag);
    }
}

fn key(workspace: &str) -> String {
    SpaceKey::of(&StateNamespace::for_space("socket-a", workspace))
        .as_str()
        .to_owned()
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

impl Server {
    fn start(f: &Fixture) -> Self {
        let shim = f.root.join("fake-herdr");
        executable::write_executable(&shim, FAKE_HERDR);
        let home = f.root.join("home");
        let stderr = fs::File::create(f.root.join("dashboard.stderr")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .arg("--plugin-state")
            .arg(&f.state)
            .arg("dashboard-server")
            .env("HERDR_BIN_PATH", &shim)
            .env("TMPDIR", &f.root)
            .env("HOME", &home)
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("HERDR_CONFIG_PATH", home.join("herdr.toml"))
            .env("CODEX_HOME", home.join(".codex"))
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("NAMEDROP_CHANNEL_ROUTER", "off")
            .env_remove("ZDOTDIR")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let info = loop {
            if let Ok(bytes) = fs::read(f.state.join("dashboard/server.json"))
                && let Ok(info) = serde_json::from_slice::<Value>(&bytes)
                && info["pid"] == child.id()
            {
                break info;
            }
            assert!(Instant::now() < deadline, "dashboard did not start");
            thread::sleep(Duration::from_millis(20));
        };
        let port = info["port"].as_u64().unwrap() as u16;
        let session = fs::read_to_string(f.state.join("dashboard/session")).unwrap();
        let mut server = Self {
            child,
            state: f.state.clone(),
            port,
            cookie: format!("namedrop_dashboard_{port}={session}"),
            csrf: String::new(),
        };
        let response = server.request("GET", "/api/session", &[("Cookie", &server.cookie)], "");
        assert_eq!(response.status, 200, "{}", response.body);
        server.csrf = response.json()["csrf_token"].as_str().unwrap().to_owned();
        server
    }

    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
        let mut request = format!("{method} {path} HTTP/1.1\r\n");
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("host"))
        {
            request.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
        }
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        if method == "POST" {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        request.push_str(body);
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        Response {
            status: head.split(' ').nth(1).unwrap().parse().unwrap(),
            body: body.to_owned(),
        }
    }

    fn post_with(&self, path: &str, body: &Value, extra: &[(&str, &str)]) -> Response {
        let mut headers = vec![
            ("Cookie", self.cookie.as_str()),
            ("X-Namedrop-Csrf", self.csrf.as_str()),
            ("Content-Type", "application/json"),
        ];
        headers.extend_from_slice(extra);
        self.request("POST", path, &headers, &body.to_string())
    }

    fn create_channel(&self, workspace: &str, name: &str, folder: &str) -> Response {
        self.post_with(
            &format!("/w/{}/channels", key(workspace)),
            &json!({"name": name, "folder": folder}),
            &[],
        )
    }

    fn add_agent(&self, workspace: &str, body: Value) -> Response {
        self.post_with(&format!("/w/{}/agents", key(workspace)), &body, &[])
    }

    fn folders(&self, workspace: &str) -> Response {
        self.request(
            "GET",
            &format!("/w/{}/folders", key(workspace)),
            &[("Cookie", &self.cookie)],
            "",
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        xws::stop_workers(&self.state);
    }
}

#[test]
fn every_new_channel_and_agent_request_needs_the_session_csrf_and_same_origin() {
    let f = fixture();
    let server = Server::start(&f);
    let channel = json!({"name": "Proto", "folder": "~/project"});
    let agent = json!({"kind": "claude", "name": "coder", "persona": null});
    let origin = format!("http://127.0.0.1:{}", server.port);
    for (path, body) in [
        (format!("/w/{}/channels", key("w1")), &channel),
        (format!("/w/{}/agents", key("w1")), &agent),
    ] {
        let body = body.to_string();
        let json_type = ("Content-Type", "application/json");
        let cookie = ("Cookie", server.cookie.as_str());
        let csrf = ("X-Namedrop-Csrf", server.csrf.as_str());
        for (headers, status) in [
            (vec![json_type, csrf], 401),
            (vec![json_type, cookie], 403),
            (
                vec![
                    json_type,
                    cookie,
                    ("X-Namedrop-Csrf", "0".repeat(64).leak()),
                ],
                403,
            ),
            (
                vec![json_type, cookie, csrf, ("Origin", "https://evil.example")],
                403,
            ),
            (
                vec![json_type, cookie, csrf, ("Sec-Fetch-Site", "cross-site")],
                403,
            ),
            (
                vec![json_type, cookie, csrf, ("Sec-Fetch-Site", "same-site")],
                403,
            ),
            (vec![json_type, cookie, csrf, ("Host", "evil.example")], 403),
            (vec![json_type, cookie, csrf, ("Host", "localhost:1")], 403),
        ] {
            assert_eq!(
                server.request("POST", &path, &headers, &body).status,
                status,
                "{path} {headers:?}"
            );
        }
        let unknown = path.replacen(&key("w1"), &key("w9"), 1);
        let response = server.post_with(&unknown, &serde_json::from_str(&body).unwrap(), &[]);
        assert_eq!(response.status, 404, "{}", response.body);
        assert_eq!(
            response.error(),
            "Namedrop doesn't know this channel. Reload the page, then try again."
        );
        let same_origin = server.post_with(
            &unknown,
            &serde_json::from_str(&body).unwrap(),
            &[("Origin", &origin), ("Sec-Fetch-Site", "same-origin")],
        );
        assert_eq!(same_origin.status, 404, "the page's own headers pass");
    }
    assert_eq!(creations(&f), Vec::<Vec<String>>::new());
    let folders = format!("/w/{}/folders", key("w1"));
    assert_eq!(server.request("GET", &folders, &[], "").status, 401);
    for header in [
        ("Origin", "https://evil.example"),
        ("Sec-Fetch-Site", "same-site"),
    ] {
        assert_eq!(
            server
                .request("GET", &folders, &[("Cookie", &server.cookie), header], "")
                .status,
            403
        );
    }
    assert_eq!(
        server
            .request(
                "GET",
                &folders,
                &[
                    ("Cookie", &server.cookie),
                    ("Sec-Fetch-Site", "same-origin")
                ],
                ""
            )
            .status,
        200
    );
    assert_eq!(server.folders("w9").status, 404);
}

#[test]
fn recent_folders_list_each_workspace_folder_newest_first_and_need_herdr() {
    let f = fixture();
    let server = Server::start(&f);
    let response = server.folders("w1");
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        response.json(),
        json!({"folders": [f.root.join("checkout"), f.root.join("repo")]}),
        "w3's checkout wins over its pane, and w2 shares w1's folder"
    );
    fail(&f, "workspace-list", true);
    let response = server.folders("w1");
    assert_eq!(response.status, 503);
    assert_eq!(response.error(), UNREACHABLE);
}

#[test]
fn a_new_channel_is_a_new_workspace_with_namedrop_active_and_in_the_index() {
    let f = fixture();
    let server = Server::start(&f);
    let response = server.create_channel("w1", "  Proto Chan ", "~/project/");
    assert_eq!(response.status, 201, "{}", response.body);
    let folder = fs::canonicalize(f.root.join("home/project")).unwrap();
    assert_eq!(
        response.json(),
        json!({"key": key("w4"), "workspace_id": "w4", "label": "Proto Chan"})
    );
    let calls = calls(&f);
    assert_eq!(
        calls[..2],
        [
            args(&["workspace", "list"]),
            args(&[
                "workspace",
                "create",
                "--cwd",
                folder.to_str().unwrap(),
                "--label",
                "Proto Chan",
                "--no-focus",
            ]),
        ]
    );
    assert!(
        calls[2..].contains(&args(&["pane", "list", "--workspace", "w4"])),
        "activation discovered the new workspace: {calls:?}"
    );
    assert!(!calls.iter().any(|call| call[1] == "close"), "{calls:?}");
    assert_eq!(
        f.count(
            "SELECT COUNT(*) FROM worktrees WHERE session_socket_id = 'socket-a' \
             AND worktree_id = 'w4' AND activated = 1 AND deactivated = 0"
        ),
        1
    );
    let spaces = server.request("GET", "/api/spaces", &[("Cookie", &server.cookie)], "");
    assert!(
        spaces
            .json()
            .as_array()
            .unwrap()
            .iter()
            .any(|space| space["key"] == key("w4")),
        "{}",
        spaces.body
    );
}

#[test]
fn a_refused_channel_reads_plainly_and_creates_no_workspace() {
    let f = fixture();
    let server = Server::start(&f);
    let home = f.root.join("home");
    let cases = [
        (
            "Proto",
            "~/missing",
            400,
            "That folder doesn't exist. Namedrop never creates folders.",
        ),
        (
            "Proto",
            "~/notes.txt",
            400,
            "That path is a file, not a folder.",
        ),
        (
            "Proto",
            "home/project",
            400,
            "Type the folder's full path, starting with / or ~.",
        ),
        (
            "Proto",
            "~nobody/project",
            400,
            "Type the folder's full path, starting with / or ~.",
        ),
        ("   ", "~/project", 400, "Give the channel a name."),
        (
            &"n".repeat(65),
            "~/project",
            400,
            "A channel name can be up to 64 characters.",
        ),
        (
            " ALL ",
            "~/project",
            400,
            "A channel name needs a letter or a digit, and it can't be \"all\".",
        ),
        (
            "--",
            "~/project",
            400,
            "A channel name needs a letter or a digit, and it can't be \"all\".",
        ),
        (
            "WS1",
            "~/project",
            409,
            "Another channel is already named #ws1. Pick a different name.",
        ),
        (
            " Ws2 ",
            &home.join("project").display().to_string(),
            409,
            "Another channel is already named #ws2. Pick a different name.",
        ),
    ];
    for (name, folder, status, sentence) in cases {
        let response = server.create_channel("w1", name, folder);
        assert_eq!(
            (response.status, response.error()),
            (status, sentence.to_owned()),
            "{name} {folder}"
        );
    }
    fail(&f, "workspace-list", true);
    let response = server.create_channel("w1", "Proto", "~/project");
    assert_eq!(
        (response.status, response.error()),
        (503, UNREACHABLE.to_owned())
    );
    assert_eq!(creations(&f), Vec::<Vec<String>>::new());
    let malformed = server.post_with(
        &format!("/w/{}/channels", key("w1")),
        &json!({"name": "Proto", "folder": "~/project", "extra": 1}),
        &[],
    );
    assert_eq!(malformed.status, 400);
}

#[test]
fn a_channel_whose_activation_fails_is_closed_again() {
    let f = fixture();
    let server = Server::start(&f);
    fail(&f, "worktree-list", true);
    let response = server.create_channel("w1", "Proto", "~/project");
    assert_eq!(response.status, 500, "{}", response.body);
    assert_eq!(
        response.error(),
        "Namedrop couldn't activate in the new workspace, so it was closed again. Try again."
    );
    let folder = fs::canonicalize(f.root.join("home/project")).unwrap();
    assert_eq!(
        creations(&f),
        [
            args(&[
                "workspace",
                "create",
                "--cwd",
                folder.to_str().unwrap(),
                "--label",
                "Proto",
                "--no-focus"
            ]),
            args(&["workspace", "close", "w4"]),
        ]
    );
    assert!(
        !read_state(&f)["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["id"] == "w4")
    );
}

#[test]
fn a_new_agent_gets_its_own_tab_its_name_and_only_the_plain_command() {
    let f = fixture();
    let server = Server::start(&f);
    let repo = f.root.join("repo").display().to_string();
    for (kind, name, persona, pane, command) in [
        (
            "claude",
            "@coder",
            Some("$(touch pwned); `id` && echo"),
            "w1:p4",
            Some("claude"),
        ),
        ("codex", "helper", None, "w1:p5", Some("codex")),
        ("terminal", "shell", Some("  "), "w1:p6", None),
    ] {
        let before = creations(&f).len();
        let response = server.add_agent(
            "w1",
            json!({"kind": kind, "name": name, "persona": persona}),
        );
        assert_eq!(response.status, 201, "{}", response.body);
        let alias = name.trim_start_matches('@');
        let address_id = f
            .text(&format!(
                "SELECT address_id FROM pane_bindings WHERE worktree_id = 'w1' \
                 AND pane_id = '{pane}' AND active = 1"
            ))
            .unwrap_or_else(|| panic!("{pane} is not bound"));
        assert_eq!(
            response.json(),
            json!({"address_id": address_id, "alias": alias})
        );
        let mut expected = vec![args(&[
            "tab",
            "create",
            "--workspace",
            "w1",
            "--cwd",
            &repo,
            "--label",
            &format!("@{alias}"),
            "--no-focus",
        ])];
        if let Some(command) = command {
            expected.push(args(&["pane", "run", pane, command]));
        }
        assert_eq!(creations(&f)[before..], expected[..], "{kind}");
        assert_eq!(
            f.text(&format!(
                "SELECT pane_type || ':' || COALESCE(persona, '') FROM pane_addresses \
                 WHERE worktree_id = 'w1' AND address_id = '{address_id}'"
            )),
            Some(format!(
                "{}:{}",
                if kind == "terminal" {
                    "terminal"
                } else {
                    "agent"
                },
                persona.map(str::trim).unwrap_or_default()
            )),
            "{kind}"
        );
    }
    assert!(
        calls(&f)
            .iter()
            .filter(|call| call[..2] == ["pane", "run"])
            .all(|call| call.len() == 4 && matches!(call[3].as_str(), "claude" | "codex")),
        "no request text reaches a command"
    );
}

#[test]
fn a_refused_agent_reads_plainly_and_opens_no_tab() {
    let f = fixture();
    let server = Server::start(&f);
    let agent = |name: &str| json!({"kind": "claude", "name": name, "persona": null});
    for (workspace, body, status, sentence) in [
        (
            "w1",
            agent("Code Reviewer"),
            400,
            "A name uses lowercase letters, digits and single hyphens, like code-reviewer.",
        ),
        (
            "w1",
            json!({"kind": "bash", "name": "x", "persona": null}),
            400,
            "Pick Claude Code, Codex or a terminal.",
        ),
        (
            "w1",
            json!({"kind": "claude", "name": "x", "persona": "p".repeat(1_001)}),
            400,
            "A persona can be up to 1,000 characters.",
        ),
        (
            "w1",
            json!({"kind": "claude", "name": "x", "persona": "a\u{202E}b"}),
            400,
            "A persona can't contain control or invisible characters.",
        ),
        (
            "w1",
            agent("@lead"),
            409,
            "@lead is already in use in this channel. Pick a different name.",
        ),
        (
            "w2",
            agent("coder"),
            409,
            "Namedrop is paused in this channel. Resume it, then add the agent.",
        ),
        (
            "w3",
            agent("coder"),
            409,
            "Namedrop isn't active in this channel.",
        ),
    ] {
        let response = server.add_agent(workspace, body);
        assert_eq!(
            (response.status, response.error()),
            (status, sentence.to_owned()),
            "{workspace}"
        );
    }
    f.sql("UPDATE pane_addresses SET retired = 1 WHERE address_id = 'address-w1-planner'");
    f.rename("w1", "lead", "lead-two");
    for (name, sentence) in [
        (
            "planner",
            "@planner was retired in this channel and can't be used again. Pick a different name.",
        ),
        (
            "lead",
            "@lead was renamed to @lead-two in this channel and can't be used again. Pick a different name.",
        ),
    ] {
        let response = server.add_agent("w1", agent(name));
        assert_eq!(
            (response.status, response.error()),
            (409, sentence.to_owned())
        );
    }
    let mut state = herdr_state(&f);
    let gone = f.root.join("gone");
    state["panes"][0]["cwd"] = json!(gone);
    write_state(&f, &state);
    let response = server.add_agent("w1", agent("coder"));
    assert_eq!(
        (response.status, response.error()),
        (
            409,
            format!(
                "This channel's folder no longer exists: {}.",
                gone.display()
            )
        )
    );
    state["workspaces"] = json!([{"id": "w2", "label": "ws2"}]);
    write_state(&f, &state);
    let response = server.add_agent("w1", agent("coder"));
    assert_eq!(
        (response.status, response.error()),
        (404, "This channel's workspace is closed.".to_owned())
    );
    fail(&f, "workspace-list", true);
    let response = server.add_agent("w1", agent("coder"));
    assert_eq!(
        (response.status, response.error()),
        (503, UNREACHABLE.to_owned())
    );
    fail(&f, "workspace-list", false);
    write_state(&f, &herdr_state(&f));
    fail(&f, "tab-create", true);
    let response = server.add_agent("w1", agent("coder"));
    assert_eq!(
        (response.status, response.error()),
        (
            500,
            "Herdr couldn't open a tab for the agent. Try again.".to_owned()
        )
    );
    assert_eq!(
        creations(&f),
        [args(&[
            "tab",
            "create",
            "--workspace",
            "w1",
            "--cwd",
            &f.root.join("repo").display().to_string(),
            "--label",
            "@coder",
            "--no-focus"
        ])],
        "only the failed create reached Herdr"
    );
}

#[test]
fn an_agent_that_cannot_be_named_has_its_tab_closed_again() {
    let f = fixture();
    let server = Server::start(&f);
    fail(&f, "worktree-list", true);
    let response = server.add_agent(
        "w1",
        json!({"kind": "claude", "name": "coder", "persona": null}),
    );
    assert_eq!(response.status, 500, "{}", response.body);
    assert_eq!(
        response.error(),
        "Namedrop couldn't name the new pane, so its tab was closed again. Try again."
    );
    let created = creations(&f);
    assert_eq!(created.len(), 2, "{created:?}");
    assert_eq!(created[0][..2], ["tab", "create"]);
    assert_eq!(created[1], args(&["tab", "close", "w1:t4"]));
    assert!(
        !read_state(&f)["panes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pane| pane["tab_id"] == "w1:t4")
    );
}

fn snapshots(server: &Server, workspace: &str) -> std::sync::mpsc::Receiver<Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    write!(
        stream,
        "GET /w/{}/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
        key(workspace),
        server.port,
        server.cookie
    )
    .unwrap();
    let (frames, received) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut buffered = String::new();
        let mut chunk = [0; 65536];
        loop {
            let Ok(read) = stream.read(&mut chunk) else {
                return;
            };
            if read == 0 {
                return;
            }
            buffered.push_str(&String::from_utf8_lossy(&chunk[..read]));
            while let Some(end) = buffered.find("\n\n") {
                let event: String = buffered.drain(..end + 2).collect();
                if let Some(data) = event.lines().find_map(|line| line.strip_prefix("data: "))
                    && let Ok(frame) = serde_json::from_str::<Value>(data)
                    && frame.get("attention").is_some()
                    && frames.send(frame).is_err()
                {
                    return;
                }
            }
        }
    });
    received
}

fn starting_card(frame: &Value) -> Option<Value> {
    frame["attention"]
        .as_array()?
        .iter()
        .find(|card| card["source_kind"] == "agent_start")
        .cloned()
}

#[test]
fn an_agent_that_has_not_connected_needs_the_person_until_its_session_is_named() {
    let f = fixture();
    let server = Server::start(&f);
    let frames = snapshots(&server, "w1");
    let started = Instant::now();
    let response = server.add_agent(
        "w1",
        json!({"kind": "claude", "name": "coder", "persona": null}),
    );
    assert_eq!(response.status, 201, "{}", response.body);
    let address_id = response.json()["address_id"].as_str().unwrap().to_owned();
    let card = loop {
        let frame = frames
            .recv_timeout(Duration::from_secs(30))
            .expect("a snapshot with the card");
        if let Some(card) = starting_card(&frame) {
            assert_eq!(frame["health"]["level"], "needs_you", "{frame}");
            break card;
        }
    };
    assert!(
        started.elapsed() >= Duration::from_secs(20),
        "the card waits 20 seconds"
    );
    assert_eq!(card["attention_id"], format!("agent-start-{address_id}"));
    assert_eq!(card["source_id"], address_id);
    assert_eq!(
        card["response"],
        json!({"kind": "pane", "address_id": address_id})
    );
    assert_eq!(
        card["summary"]["text"],
        "@coder hasn't connected yet. Open its pane. It may be asking you to trust the folder or to log in. A new Codex connects after its first prompt."
    );
    let mut state = read_state(&f);
    let pane = state["panes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|pane| pane["pane_id"] == "w1:p4")
        .unwrap();
    pane["agent"] = json!("claude");
    pane["agent_status"] = json!("idle");
    pane["agent_session"] = json!({"value": "session-coder"});
    write_state(&f, &state);
    wait_for("the reported session to be named", || {
        f.count(&format!(
            "SELECT COUNT(*) FROM agent_session_observations WHERE worktree_id = 'w1' \
             AND address_id = '{address_id}' AND agent_session_id = 'session-coder'"
        )) > 0
    });
    loop {
        let frame = frames
            .recv_timeout(Duration::from_secs(15))
            .expect("a snapshot without the card");
        if starting_card(&frame).is_none() {
            break;
        }
    }
}
