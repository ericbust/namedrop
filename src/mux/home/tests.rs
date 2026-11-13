use super::*;
use std::{collections::HashMap, sync::Mutex};

const REX: &str = "/tmp/rex/s.sock";
const HERDR: &str = "/tmp/herdr/h.sock";
const SERVER: u32 = 300;
const HERDR_SERVER: u32 = 200;
const BLOCK: &str = "block:0f6c0d4e-1b5a-4f8e-9d2c-3a7b8c9d0e1f";
const SESSION: &str = "session:5b1d2c3e-4f60-4718-8a9b-0c1d2e3f4a5b";
const CANONICAL: &str = "rex:/private/tmp/rex/s.sock";

/// A process with scripted variables, ancestors, and socket answers. A
/// socket it has no answer for fails the test when probed.
#[derive(Default)]
struct Scripted {
    vars: HashMap<String, String>,
    id: u32,
    ancestors: Vec<u32>,
    peers: HashMap<PathBuf, Result<u32, i32>>,
    claimed: Option<Result<Claimed, String>>,
    probed: Mutex<Vec<PathBuf>>,
    incarnations: HashMap<u32, String>,
}

impl Scripted {
    fn var(mut self, name: &str, value: &str) -> Self {
        self.vars.insert(name.to_owned(), value.to_owned());
        self
    }

    /// The Rex claim of `BLOCK` in `SESSION`, on the server `REX_SERVER` names.
    fn in_block(self) -> Self {
        self.var("REX_BLOCK", BLOCK)
            .var("REX_SESSION", SESSION)
            .var("REX_SERVER", &format!("unix://{REX}"))
    }

    fn id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }

    fn ancestors(mut self, ancestors: &[u32]) -> Self {
        self.ancestors = ancestors.to_vec();
        self
    }

    fn peer(mut self, socket: &str, answer: Result<u32, i32>) -> Self {
        self.peers.insert(PathBuf::from(socket), answer);
        self
    }

    /// The server's answer about the block: its canonical endpoint, its pid,
    /// and the block's child.
    fn child(mut self, server: u32, child: u32) -> Self {
        self.claimed = Some(Ok(Claimed {
            endpoint: Endpoint::parse(CANONICAL),
            server,
            child,
            caller: Caller::default(),
        }));
        self
    }

    /// `pid` is alive and started at `start`.
    fn started(mut self, pid: u32, start: u64) -> Self {
        self.incarnations.insert(pid, format!("{pid}@{start}"));
        self
    }

    fn probed(&self) -> Vec<PathBuf> {
        self.probed.lock().unwrap().clone()
    }
}

impl Inherited for Scripted {
    fn var(&self, name: &str) -> Option<OsString> {
        self.vars.get(name).map(OsString::from)
    }

    fn id(&self) -> u32 {
        self.id
    }

    fn ancestors(&self) -> Vec<u32> {
        self.ancestors.clone()
    }

    fn peer(&self, socket: &Path) -> io::Result<u32> {
        self.probed.lock().unwrap().push(socket.to_owned());
        match self.peers.get(socket) {
            Some(Ok(pid)) => Ok(*pid),
            Some(Err(errno)) => Err(io::Error::from_raw_os_error(*errno)),
            None => panic!("{} was probed and has no answer", socket.display()),
        }
    }

    fn claimed(&self, socket: &Path, session: &str, block: &str) -> Result<Claimed, String> {
        assert_eq!(
            (socket, session, block),
            (Path::new(REX), SESSION, BLOCK),
            "the claim is verified on the socket that proved the ancestor"
        );
        self.claimed
            .clone()
            .unwrap_or_else(|| panic!("the block was read with no scripted answer"))
    }

    fn incarnation(&self, pid: u32) -> Option<String> {
        self.incarnations.get(&pid).cloned()
    }
}

/// The place of `BLOCK`, verified for a process whose ancestors are a
/// plain block's.
fn rex_place() -> Home {
    rex_place_under(&[110, SERVER, 1])
}

fn rex_place_under(ancestors: &[u32]) -> Home {
    Home::Rex(Place {
        endpoint: Endpoint::parse(CANONICAL),
        space: Some(SpaceId::new(SESSION)),
        pane: Some(PaneId::new(BLOCK)),
        caller: Some(Box::new(Caller::default().of(ancestors.to_vec()))),
    })
}

fn resolved(process: &Scripted) -> Result<Home, String> {
    resolve(&Flags::default(), process)
}

/// What a Rex block's process sees: the block's child 110, under the Rex
/// server `SERVER`, and nothing else claimed.
fn plain_block() -> Scripted {
    Scripted::default()
        .in_block()
        .ancestors(&[110, SERVER, 1])
        .peer(REX, Ok(SERVER))
        .child(SERVER, 110)
}

#[test]
fn every_row_of_the_home_table_picks_its_home() {
    let refused = libc::ECONNREFUSED;
    let rows: Vec<(&str, Scripted, Result<Home, &str>)> = vec![
        ("a plain Rex block", plain_block(), Ok(rex_place())),
        (
            "a shell in a Herdr pane inside a Rex block",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .var("HERDR_WORKSPACE_ID", "w1")
                .var("HERDR_PANE_ID", "w1:p1")
                .ancestors(&[120, HERDR_SERVER, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Ok(HERDR_SERVER)),
            Ok(Home::Herdr),
        ),
        (
            "a Herdr workspace action, with no pane",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .var("HERDR_WORKSPACE_ID", "w1")
                .ancestors(&[HERDR_SERVER, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Ok(HERDR_SERVER))
                .child(SERVER, HERDR_SERVER),
            Ok(Home::Herdr),
        ),
        (
            "a block of a Rex server started from a Herdr pane",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .var("HERDR_PANE_ID", "w1:p1")
                .ancestors(&[110, SERVER, 120, HERDR_SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Ok(HERDR_SERVER))
                .child(SERVER, 110),
            Ok(rex_place_under(&[110, SERVER, 120, HERDR_SERVER, 1])),
        ),
        (
            "a Rex block whose HERDR_SOCKET_PATH names a live Herdr elsewhere",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .var("HERDR_PANE_ID", "elsewhere")
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Ok(HERDR_SERVER))
                .child(SERVER, 110),
            Ok(rex_place()),
        ),
        (
            "a Rex block whose HERDR_SOCKET_PATH names no socket",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Err(libc::ENOENT))
                .child(SERVER, 110),
            Ok(rex_place()),
        ),
        (
            "Herdr's default socket missing, which proves nothing",
            Scripted::default()
                .in_block()
                .var("HOME", "/home/u")
                .var("HERDR_WORKSPACE_ID", "w1")
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer("/home/u/.config/herdr/herdr.sock", Err(libc::ENOENT)),
            Ok(Home::Herdr),
        ),
        (
            "a live Herdr on its default socket that is not an ancestor",
            Scripted::default()
                .in_block()
                .var("XDG_CONFIG_HOME", "/home/u/config")
                .var("HERDR_PANE_ID", "w1:p1")
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer("/home/u/config/herdr/herdr.sock", Ok(HERDR_SERVER)),
            Ok(Home::Herdr),
        ),
        (
            "Herdr refuses, as a full listener does",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Err(refused)),
            Ok(Home::Herdr),
        ),
        (
            "Herdr's socket denied",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Err(libc::EPERM)),
            Ok(Home::Herdr),
        ),
        (
            "Rex's socket refused, with a Herdr name",
            Scripted::default()
                .in_block()
                .var("HERDR_PANE_ID", "w1:p1")
                .var("HERDR_SOCKET_PATH", HERDR)
                .ancestors(&[120, HERDR_SERVER, 1])
                .peer(REX, Err(refused))
                .peer(HERDR, Ok(HERDR_SERVER)),
            Ok(Home::Herdr),
        ),
        (
            "Rex's server live and not an ancestor, with a Herdr name",
            Scripted::default()
                .in_block()
                .var("HERDR_SOCKET_PATH", HERDR)
                .ancestors(&[120, HERDR_SERVER, 1])
                .peer(REX, Ok(SERVER))
                .peer(HERDR, Ok(HERDR_SERVER)),
            Ok(Home::Herdr),
        ),
        (
            "Rex's socket denied, with a Herdr name",
            Scripted::default()
                .in_block()
                .var("HERDR_WORKSPACE_ID", "w1")
                .var("HERDR_SOCKET_PATH", HERDR)
                .ancestors(&[HERDR_SERVER, 1])
                .peer(REX, Err(libc::EPERM))
                .peer(HERDR, Ok(HERDR_SERVER)),
            Ok(Home::Herdr),
        ),
        (
            "Rex's socket refused, with no Herdr name",
            Scripted::default()
                .in_block()
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Err(refused)),
            Err(
                "REX_BLOCK=block:0f6c0d4e-1b5a-4f8e-9d2c-3a7b8c9d0e1f is on a Rex server this process cannot find (REX_SERVER=unix:///tmp/rex/s.sock); start that server with REX_SERVER set",
            ),
        ),
        (
            "Rex's socket missing, with no Herdr name",
            Scripted::default()
                .in_block()
                .ancestors(&[110, 1])
                .peer(REX, Err(libc::ENOENT)),
            Err(
                "REX_BLOCK=block:0f6c0d4e-1b5a-4f8e-9d2c-3a7b8c9d0e1f is on a Rex server this process cannot find (REX_SERVER=unix:///tmp/rex/s.sock); start that server with REX_SERVER set",
            ),
        ),
        (
            "a live Rex server that is not an ancestor, with no Herdr name",
            Scripted::default()
                .in_block()
                .ancestors(&[110, 1])
                .peer(REX, Ok(SERVER)),
            Err(
                "REX_BLOCK=block:0f6c0d4e-1b5a-4f8e-9d2c-3a7b8c9d0e1f is on a Rex server this process cannot find (REX_SERVER=unix:///tmp/rex/s.sock); start that server with REX_SERVER set",
            ),
        ),
        (
            "Rex's socket denied, with no Herdr name",
            Scripted::default()
                .in_block()
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Err(libc::EACCES)),
            Err(
                "the agent's sandbox denies the Rex socket that REX_SERVER=unix:///tmp/rex/s.sock names, and no Namedrop hook has bound REX_BLOCK=block:0f6c0d4e-1b5a-4f8e-9d2c-3a7b8c9d0e1f yet",
            ),
        ),
        (
            "a block whose child is not an ancestor",
            Scripted::default()
                .in_block()
                .id(100)
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .child(SERVER, 111),
            Err(
                "REX_BLOCK=block:0f6c0d4e-1b5a-4f8e-9d2c-3a7b8c9d0e1f does not run this process: its child 111 is neither this process nor an ancestor",
            ),
        ),
        (
            "a block whose child is this process, as after a launcher's exec",
            Scripted::default()
                .in_block()
                .id(110)
                .ancestors(&[SERVER, 1])
                .peer(REX, Ok(SERVER))
                .child(SERVER, 110),
            Ok(rex_place_under(&[SERVER, 1])),
        ),
        (
            "a claim answered by a server that is not the ancestor probed",
            Scripted::default()
                .in_block()
                .ancestors(&[110, SERVER, 1])
                .peer(REX, Ok(SERVER))
                .child(301, 110),
            Err(
                "the Rex server at /tmp/rex/s.sock is process 301, not this process's ancestor 300",
            ),
        ),
    ];
    for (name, process, expected) in rows {
        assert_eq!(
            resolved(&process),
            expected.map_err(str::to_owned),
            "{name}"
        );
    }
}

#[test]
fn herdr_is_probed_only_when_a_herdr_location_name_is_set() {
    let block = plain_block().var("HERDR_PLUGIN_STATE_DIR", "/s");
    assert_eq!(resolved(&block), Ok(rex_place()));
    assert_eq!(block.probed(), [PathBuf::from(REX)]);
    let empty = plain_block()
        .var("HERDR_PANE_ID", "")
        .peer("/home/u/.config/herdr/herdr.sock", Ok(HERDR_SERVER))
        .var("HOME", "/home/u");
    assert_eq!(
        resolved(&empty),
        Ok(Home::Herdr),
        "an empty Herdr name is still a Herdr name, and its guessed socket is doubt"
    );
}

#[test]
fn rex_s_socket_is_rex_server_as_rex_reads_it_or_rex_s_default() {
    let encoded = Scripted::default()
        .var("REX_BLOCK", BLOCK)
        .var("REX_SESSION", SESSION)
        .var("REX_SERVER", "unix:///tmp/a%20b/s.sock")
        .ancestors(&[110, 1])
        .peer("/tmp/a b/s.sock", Err(libc::ECONNREFUSED));
    assert!(resolved(&encoded).is_err());
    assert_eq!(encoded.probed(), [PathBuf::from("/tmp/a b/s.sock")]);

    let bare = plain_block().var("REX_SERVER", REX);
    assert_eq!(
        resolved(&bare),
        Ok(rex_place()),
        "rex reads a bare absolute path as a Unix socket"
    );
    assert_eq!(bare.probed(), [PathBuf::from(REX)]);
    let literal = Scripted::default()
        .var("REX_BLOCK", BLOCK)
        .var("REX_SESSION", SESSION)
        .var("REX_SERVER", "/tmp/a%20b/s.sock")
        .ancestors(&[110, 1])
        .peer("/tmp/a%20b/s.sock", Err(libc::ECONNREFUSED));
    assert!(resolved(&literal).is_err());
    assert_eq!(
        literal.probed(),
        [PathBuf::from("/tmp/a%20b/s.sock")],
        "rex takes a bare path as written"
    );

    #[cfg(target_os = "macos")]
    let (variable, value, default) = (
        "HOME",
        "/Users/u",
        "/Users/u/Library/Application Support/rex/server.sock",
    );
    #[cfg(not(target_os = "macos"))]
    let (variable, value, default) = (
        "XDG_RUNTIME_DIR",
        "/run/user/7",
        "/run/user/7/rex/server.sock",
    );
    let defaulted = Scripted::default()
        .var("REX_BLOCK", BLOCK)
        .var("REX_SESSION", SESSION)
        .var(variable, value)
        .var("HERDR_SOCKET_PATH", HERDR)
        .ancestors(&[110, 1])
        .peer(default, Err(libc::ENOENT))
        .peer(HERDR, Err(libc::ENOENT));
    assert_eq!(
        resolved(&defaulted),
        Ok(Home::Herdr),
        "a missing default is doubt, not a missing server"
    );
    assert_eq!(
        defaulted.probed(),
        [PathBuf::from(default), PathBuf::from(HERDR)]
    );

    for server in [
        "devbox:7297",
        "127.0.0.1:7297",
        "devbox",
        "rex/s.sock",
        "unix:/tmp/rex/s.sock",
    ] {
        let elsewhere = Scripted::default()
            .var("REX_BLOCK", BLOCK)
            .var("REX_SESSION", SESSION)
            .var("REX_SERVER", server)
            .ancestors(&[110, 1]);
        assert!(
            resolved(&elsewhere).is_err(),
            "{server}: a server that is not a Unix socket is never an ancestor"
        );
        assert_eq!(elsewhere.probed(), Vec::<PathBuf>::new(), "{server}");
    }
}

#[test]
fn a_rex_home_takes_its_place_from_the_claim_and_no_location_name() {
    let block = plain_block()
        .var("NAMEDROP_ENDPOINT", "rex:/tmp/other.sock")
        .var("NAMEDROP_SPACE_ID", "session:other")
        .var("NAMEDROP_PANE_ID", "block:other")
        .var("HERDR_SOCKET_PATH", HERDR)
        .var("HERDR_PANE_ID", "w9:p9")
        .peer(HERDR, Err(libc::ENOENT));
    assert_eq!(resolved(&block), Ok(rex_place()));
}

#[test]
fn an_explicit_endpoint_decides_by_its_kind_with_no_probe() {
    let block = Scripted::default().in_block();
    let flags = |endpoint| Flags {
        endpoint: Some(endpoint),
        space: Some("session:s"),
        pane: Some("block:b"),
        plugin_state: None,
    };
    for endpoint in ["", "default", "/tmp/h.sock"] {
        assert_eq!(
            resolve(&flags(endpoint), &block),
            Ok(Home::Herdr),
            "{endpoint:?}"
        );
    }
    assert_eq!(
        resolve(&flags("rex:/tmp/r.sock"), &block),
        Ok(Home::Rex(Place {
            endpoint: Endpoint::parse("rex:/tmp/r.sock"),
            space: Some(SpaceId::new("session:s")),
            pane: Some(PaneId::new("block:b")),
            caller: None,
        }))
    );
    assert_eq!(block.probed(), Vec::<PathBuf>::new());
}

#[test]
fn a_flag_names_its_own_value_in_a_claimed_place() {
    let flags = Flags {
        endpoint: None,
        space: None,
        pane: Some("block:flag"),
        plugin_state: None,
    };
    assert_eq!(
        resolve(&flags, &plain_block()),
        Ok(Home::Rex(Place {
            endpoint: Endpoint::parse(CANONICAL),
            space: Some(SpaceId::new(SESSION)),
            pane: Some(PaneId::new("block:flag")),
            caller: Some(Box::new(Caller::default().of(vec![110, SERVER, 1]))),
        }))
    );
}

#[test]
fn without_a_rex_claim_the_set_of_names_decides() {
    let neutral = |endpoint: &str| {
        Scripted::default()
            .var("NAMEDROP_ENDPOINT", endpoint)
            .var("NAMEDROP_SPACE_ID", "session:s")
            .var("NAMEDROP_PANE_ID", "")
    };
    assert_eq!(
        resolved(&neutral("rex:/tmp/r.sock")),
        Ok(Home::Rex(Place {
            endpoint: Endpoint::parse("rex:/tmp/r.sock"),
            space: Some(SpaceId::new("session:s")),
            pane: None,
            caller: None,
        }))
    );
    assert_eq!(resolved(&neutral("/tmp/h.sock")), Ok(Home::Herdr));
    assert_eq!(
        resolved(&neutral("rex:/tmp/r.sock").var("HERDR_WORKSPACE_ID", "w1")),
        Ok(Home::Herdr),
        "a Herdr name hides every neutral one"
    );
    assert_eq!(
        resolved(&neutral("rex:/tmp/r.sock").var("REX_BLOCK", BLOCK)),
        Ok(Home::Rex(Place {
            endpoint: Endpoint::parse("rex:/tmp/r.sock"),
            space: Some(SpaceId::new("session:s")),
            pane: None,
            caller: None,
        })),
        "a claim needs both REX_BLOCK and REX_SESSION"
    );
    assert_eq!(resolved(&Scripted::default()), Ok(Home::Herdr));
}

#[test]
fn every_home_gives_its_location_from_one_set_of_names() {
    let herdr = Scripted::default()
        .var("HERDR_SOCKET_PATH", "/h.sock")
        .var("HERDR_WORKSPACE_ID", "w1")
        .var("NAMEDROP_PANE_ID", "n1:p1")
        .var("NAMEDROP_ENDPOINT", "rex:/tmp/r.sock");
    assert_eq!(
        Home::Herdr.location(&herdr),
        Some(Location {
            endpoint: Some(Endpoint::parse("/h.sock")),
            space: Some(SpaceId::new("w1")),
            pane: None,
            names: Name::Herdr,
        }),
        "a Herdr name hides every neutral one"
    );
    let neutral = Scripted::default()
        .var("NAMEDROP_ENDPOINT", "/n.sock")
        .var("NAMEDROP_PANE_ID", "n1:p1");
    assert_eq!(
        Home::Herdr.location(&neutral),
        Some(Location {
            endpoint: Some(Endpoint::parse("/n.sock")),
            space: None,
            pane: Some(PaneId::new("n1:p1")),
            names: Name::Neutral,
        })
    );
    let Home::Rex(place) = rex_place() else {
        unreachable!()
    };
    assert_eq!(
        Home::Rex(place.clone()).location(&herdr),
        Some(Location {
            endpoint: Some(place.endpoint.clone()),
            space: place.space.clone(),
            pane: place.pane.clone(),
            names: Name::Neutral,
        }),
        "a Rex place reads no name"
    );
    let deferral = Deferral {
        endpoint: place.endpoint,
        space: SpaceId::new(SESSION),
        pane: PaneId::new(BLOCK),
    };
    assert_eq!(Home::RexDeferred(deferral).location(&herdr), None);
}

/// `ThisProcess` reads the real environment and sockets, so only `fn main`
/// builds it and only `mux::home` defines it. A test that named it would
/// choose its home from the developer's own terminal.
mod this_process_boundary {
    use std::{fs, path::Path};

    const NAME: &str = concat!("This", "Process");

    /// Code without comments and with every string and char literal emptied.
    fn code(source: &str) -> String {
        let chars: Vec<char> = source.chars().collect();
        let mut code = String::new();
        let mut at = 0;
        while at < chars.len() {
            let rest = &chars[at..];
            if rest.starts_with(&['/', '/']) {
                while at < chars.len() && chars[at] != '\n' {
                    at += 1;
                }
            } else if rest.starts_with(&['/', '*']) {
                let mut depth = 0;
                while at < chars.len() {
                    if chars[at..].starts_with(&['/', '*']) {
                        depth += 1;
                        at += 2;
                    } else if chars[at..].starts_with(&['*', '/']) {
                        depth -= 1;
                        at += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        at += 1;
                    }
                }
            } else if rest[0] == 'r'
                && !chars
                    .get(at.wrapping_sub(1))
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                && rest[1..].iter().take_while(|c| **c == '#').count() + 1 < rest.len()
                && rest[1 + rest[1..].iter().take_while(|c| **c == '#').count()] == '"'
            {
                let hashes = rest[1..].iter().take_while(|c| **c == '#').count();
                let close: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                at += 2 + hashes;
                while at < chars.len() && !chars[at..].starts_with(&close) {
                    at += 1;
                }
                at += close.len();
                code.push_str("\"\"");
            } else if rest[0] == '"' {
                at += 1;
                while at < chars.len() && chars[at] != '"' {
                    at += if chars[at] == '\\' { 2 } else { 1 };
                }
                at += 1;
                code.push_str("\"\"");
            } else if rest[0] == '\'' && (rest.get(1) == Some(&'\\') || rest.get(2) == Some(&'\''))
            {
                at += 1;
                while at < chars.len() && chars[at] != '\'' {
                    at += if chars[at] == '\\' { 2 } else { 1 };
                }
                at += 1;
                code.push_str("''");
            } else {
                code.push(rest[0]);
                at += 1;
            }
        }
        code.split_whitespace().collect()
    }

    /// Where the name stands in `code`, as whole words.
    fn uses(code: &str) -> Vec<usize> {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        code.match_indices(NAME)
            .map(|(at, _)| at)
            .filter(|at| !code[..*at].ends_with(word) && !code[at + NAME.len()..].starts_with(word))
            .collect()
    }

    /// The body of `fn main` in compacted code, as a byte range.
    fn main_body(code: &str) -> std::ops::Range<usize> {
        let start = code.find("fnmain()").expect("main.rs has fn main");
        let open = start + code[start..].find('{').unwrap();
        let mut depth = 0;
        for (offset, character) in code[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return open..open + offset;
                    }
                }
                _ => {}
            }
        }
        open..code.len()
    }

    fn files(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }

    /// Each use of the name outside its definition and impl in
    /// `src/mux/home.rs` and the one construction in `fn main`.
    fn breaches(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for directory in ["src", "tests"] {
            files(&root.join(directory), &mut found);
        }
        found.sort();
        let mut breaches = Vec::new();
        for file in found {
            let relative = file
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let code = code(&fs::read_to_string(&file).unwrap());
            let mut uses = uses(&code);
            match relative.as_str() {
                "src/mux/home.rs" => {
                    let after = |at: usize| &code[at + NAME.len()..];
                    uses.retain(|at| {
                        let definition =
                            code[..*at].ends_with("pubstruct") && after(*at).starts_with(';');
                        let implementation = code[..*at].ends_with("implInheritedfor")
                            && after(*at).starts_with('{');
                        !(definition || implementation)
                    });
                }
                "src/main.rs" => {
                    let body = main_body(&code);
                    let constructions: Vec<usize> = uses
                        .iter()
                        .copied()
                        .filter(|at| body.contains(at))
                        .collect();
                    if constructions.len() == 1 {
                        uses.retain(|at| !body.contains(at));
                    }
                }
                _ => {}
            }
            breaches.extend(uses.iter().map(|at| {
                let start = at.saturating_sub(40);
                format!(
                    "{relative}: …{}…",
                    &code[start..(*at + NAME.len() + 20).min(code.len())]
                )
            }));
        }
        breaches
    }

    #[test]
    fn only_fn_main_builds_this_process() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(breaches(root), Vec::<String>::new());
    }

    #[test]
    fn the_boundary_catches_a_library_or_test_that_names_it() {
        let root = std::env::temp_dir().join(format!("namedrop-home-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        let definition = format!(
            "pub struct {NAME};\nimpl Inherited for {NAME} {{\n    fn var(&self) {{}}\n}}\n"
        );
        write("src/mux/home.rs", &definition);
        write(
            "src/main.rs",
            &format!(
                "fn main() {{\n    let inherited: &dyn Inherited = &namedrop::mux::home::{NAME};\n}}\n// {NAME}\nconst TEXT: &str = \"{NAME}\";\n"
            ),
        );
        write("tests/clean.rs", "fn a() {}\n");
        assert_eq!(breaches(&root), Vec::<String>::new());

        write(
            "src/agent_status.rs",
            &format!(
                "fn observing() {{ let home = crate::mux::home::resolve(&flags, &crate::mux::home::{NAME}); }}\n"
            ),
        );
        write(
            "tests/home_probe.rs",
            &format!("#[cfg(test)]\nmod tests {{\n    use namedrop::mux::home::{NAME};\n}}\n"),
        );
        write(
            "src/main.rs",
            &format!(
                "fn main() {{\n    let inherited: &dyn Inherited = &namedrop::mux::home::{NAME};\n}}\nfn other() {{ let _ = {NAME}; }}\n"
            ),
        );
        let breached = breaches(&root);
        assert_eq!(breached.len(), 3, "{breached:#?}");
        for file in ["src/agent_status.rs", "tests/home_probe.rs", "src/main.rs"] {
            assert!(
                breached.iter().any(|breach| breach.starts_with(file)),
                "{file}: {breached:#?}"
            );
        }
        fs::remove_dir_all(&root).unwrap();
    }
}

/// Only `mux::home` and `locator` read where a process runs. Every other
/// production reader of the endpoint, space, or pane takes it from the home,
/// so a Rex block never reads a stale Herdr name and a Herdr pane never reads
/// a neutral one. `Locator` has no method that reads the environment, so the
/// compiler holds that form. This scan is a tripwire for a literal read of one
/// of the six names; it cannot see a name passed as a value, such as
/// `env::var(Locator::Pane.herdr_var())`, or a search of `env::vars()`.
mod identity_boundary {
    use std::{fs, path::Path};

    const READERS: [&str; 2] = ["mux/home.rs", "locator.rs"];
    /// Live peek's observer reads the socket live peek gave it, and live peek
    /// is refused on Rex.
    const EXEMPT: [(&str, &str); 1] = [(
        "dashboard/observer_guard.rs",
        "var_os(\"HERDR_SOCKET_PATH\"",
    )];

    /// Each read of a location name in compacted production code outside
    /// the readers, as `file: read`.
    fn breaches(src: &Path) -> Vec<String> {
        let mut reads = Vec::new();
        for name in [
            "HERDR_SOCKET_PATH",
            "HERDR_WORKSPACE_ID",
            "HERDR_PANE_ID",
            "NAMEDROP_ENDPOINT",
            "NAMEDROP_SPACE_ID",
            "NAMEDROP_PANE_ID",
        ] {
            reads.push(format!("var(\"{name}\""));
            reads.push(format!("var_os(\"{name}\""));
        }
        let mut breaches = Vec::new();
        for (file, text) in crate::daemon::tests::production_files(src) {
            if READERS.contains(&file.as_str()) {
                continue;
            }
            for read in &reads {
                if text.contains(read.as_str()) && !EXEMPT.contains(&(file.as_str(), read.as_str()))
                {
                    breaches.push(format!("{file}: {read}"));
                }
            }
        }
        breaches
    }

    #[test]
    fn only_the_home_and_the_locator_read_a_location_name() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert_eq!(breaches(&src), Vec::<String>::new());
    }

    #[test]
    fn the_boundary_catches_a_reader_and_skips_tests() {
        let root = std::env::temp_dir().join(format!("namedrop-identity-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("mux/herdr")).unwrap();
        fs::create_dir_all(root.join("dashboard")).unwrap();
        let write = |file: &str, text: &str| fs::write(root.join(file), text).unwrap();
        write(
            "locator.rs",
            "fn a() { std::env::var_os(\"HERDR_PANE_ID\"); }",
        );
        write(
            "dashboard/observer_guard.rs",
            "fn a() { env::var_os(\"HERDR_SOCKET_PATH\"); }",
        );
        write(
            "mux/herdr/process.rs",
            "// env::var(\"HERDR_PANE_ID\")\nconst TEXT: &str = \"env::var(\\\"HERDR_PANE_ID\\\")\";\n#[cfg(test)]\nmod tests {\n    fn t() { let socket = env::var(\"HERDR_SOCKET_PATH\"); }\n}\n",
        );
        write(
            "mux/herdr/tests.rs",
            "fn t() { env::var(\"HERDR_SOCKET_PATH\"); }",
        );
        assert_eq!(breaches(&root), Vec::<String>::new());
        for (source, caught) in [
            (
                "fn a() { let pane = std::env::var(\"HERDR_PANE_ID\"); }",
                "planted.rs: var(\"HERDR_PANE_ID\"",
            ),
            (
                "fn a() { env::var_os(\n    \"NAMEDROP_ENDPOINT\",\n); }",
                "planted.rs: var_os(\"NAMEDROP_ENDPOINT\"",
            ),
        ] {
            write("planted.rs", source);
            assert_eq!(breaches(&root), [caught], "{source}");
        }
        fs::remove_dir_all(&root).unwrap();
    }
}

/// A state directory holding `BLOCK`'s binding, whose agent is `agent`.
struct Bound(PathBuf);

impl Bound {
    fn agent(agent: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let state = std::env::temp_dir().join(format!(
            "nd-home-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let sessions = state.join("rex/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("{}.json", BLOCK.strip_prefix("block:").unwrap())),
            serde_json::json!({
                "block": BLOCK, "endpoint": CANONICAL, "space": SESSION,
                "foreground": agent, "agent": agent, "runtime": "codex",
                "session": "codex-session", "bound_at_ms": 1,
            })
            .to_string(),
        )
        .unwrap();
        Self(state)
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Codex tool under its default sandbox: the Rex socket is denied, and
/// its ancestors, the Codex it runs under among them, still read.
fn sandboxed_tool() -> Scripted {
    Scripted::default()
        .in_block()
        .ancestors(&[120, 110, SERVER, 1])
        .peer(REX, Err(libc::EPERM))
}

fn deferred() -> Home {
    Home::RexDeferred(Deferral {
        endpoint: Endpoint::parse(CANONICAL),
        space: SpaceId::new(SESSION),
        pane: PaneId::new(BLOCK),
    })
}

#[test]
fn a_denied_socket_defers_to_the_block_its_binding_proves() {
    let bound = Bound::agent("110@5000");
    let flag = Flags {
        plugin_state: Some(&bound.0),
        ..Flags::default()
    };
    assert_eq!(
        resolve(&flag, &sandboxed_tool().started(110, 5000)),
        Ok(deferred())
    );
    let named = sandboxed_tool()
        .started(110, 5000)
        .var("NAMEDROP_STATE_DIR", bound.0.to_str().unwrap());
    assert_eq!(
        resolved(&named),
        Ok(deferred()),
        "a Rex home's state directory holds the binding too"
    );
    assert_eq!(
        deferred().location(&named),
        None,
        "a deferral is no place to record a turn"
    );
}

#[test]
fn a_binding_that_does_not_prove_the_denied_caller_is_an_error() {
    let not_an_ancestor = Bound::agent("130@5000");
    let restarted = Bound::agent("110@4000");
    for (bound, process) in [
        (&not_an_ancestor, sandboxed_tool().started(130, 5000)),
        (&restarted, sandboxed_tool().started(110, 5000)),
        (&restarted, sandboxed_tool()),
    ] {
        let flags = Flags {
            plugin_state: Some(&bound.0),
            ..Flags::default()
        };
        let refused = resolve(&flags, &process).unwrap_err();
        assert!(
            refused.starts_with("the agent's sandbox denies the Rex socket")
                && refused.contains("is not an ancestor of this process"),
            "{refused}"
        );
    }
}
