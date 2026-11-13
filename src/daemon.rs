//! How every background Namedrop process starts, so that none of them takes
//! an identity from the process that started it. A daemon outlives the hook,
//! pane, or agent tool that started it, so the names that say where that
//! process ran must not become the daemon's.

use crate::AGENT_SESSION_MARKERS;
use crate::SpaceId;
use crate::locator::{Locator, Name};
use crate::mux::Endpoint;
use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

/// The location a background process serves, which is all of its location
/// that it gets.
#[derive(Clone, Copy, Debug)]
pub enum Scope<'a> {
    /// The deferred worker and executor, and the reminder clock.
    Endpoint(&'a Endpoint),
    /// The status worker and the dashboard's mailbox wake.
    Space(&'a Endpoint, &'a SpaceId),
    /// The prompt hook's mailbox wake when its home named a space and no
    /// endpoint, as a Herdr pane on Herdr's default socket. The space keeps
    /// the set of names the hook read it under.
    SpaceAlone(Name, &'a SpaceId),
    /// The dashboard server.
    None,
}

/// Who starts a daemon, which decides whether it keeps the multiplexer
/// timeouts, `NAMEDROP_HERDR_TIMEOUT_MS` and `NAMEDROP_REX_TIMEOUT_MS`, that
/// its starter holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Starter {
    /// Any process but the Stop hook's front end. A timeout it holds is one a
    /// person set, so the daemon keeps it.
    Caller,
    /// The Stop hook's front end, whose wrapper caps every request at 250 ms
    /// for the hook's own deadline. Its daemons drop both timeouts.
    StopHook,
}

/// What a daemon never inherits: the Rex claim, which says which block the
/// starting process ran in, and the agent session markers, which say which
/// agent ran it. `REX_BIN_DIR`, `REX_HOME`, and `REX_RUNTIME_DIR` stay,
/// because they say how to run `rex`, not where this process runs.
const SCRUBBED: [&str; 5] = [
    "REX_BLOCK",
    "REX_SESSION",
    "REX_CLIENT",
    "REX_SERVER",
    "CLAUDE_SESSION_ID",
];

/// The multiplexer timeouts, which a daemon of [`Starter::StopHook`] drops.
const TIMEOUTS: [&str; 2] = ["NAMEDROP_HERDR_TIMEOUT_MS", "NAMEDROP_REX_TIMEOUT_MS"];

/// A detached background Namedrop process: its own process group, null stdio,
/// the scrub, and `scope` passed under the names of its endpoint's kind. Each
/// location name the scope leaves out is cleared under both sets.
pub fn command(executable: &Path, scope: Scope<'_>, starter: Starter) -> Command {
    let mut command = Command::new(executable);
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    scrub(&mut command, starter);
    let (endpoint, space) = match scope {
        Scope::Endpoint(endpoint) => (Some(endpoint), None),
        Scope::Space(endpoint, space) => (Some(endpoint), Some((Name::of(endpoint.kind), space))),
        Scope::SpaceAlone(set, space) => (None, Some((set, space))),
        Scope::None => (None, None),
    };
    match endpoint {
        Some(endpoint) => {
            Locator::Endpoint.pass(&mut command, Name::of(endpoint.kind), &endpoint.key)
        }
        None => Locator::Endpoint.clear(&mut command),
    }
    match space {
        Some((set, space)) => Locator::Space.pass(&mut command, set, space.as_str()),
        None => Locator::Space.clear(&mut command),
    }
    Locator::Pane.clear(&mut command);
    command
}

/// The scrub alone, for the Stop worker, which stays in its hook's process
/// group and takes its location from what the hook hands it.
pub fn scrub(command: &mut Command, starter: Starter) {
    let timeouts = match starter {
        Starter::Caller => &[][..],
        Starter::StopHook => &TIMEOUTS[..],
    };
    for name in SCRUBBED
        .into_iter()
        .chain(AGENT_SESSION_MARKERS)
        .chain(timeouts.iter().copied())
    {
        command.env_remove(name);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mux::herdr::boundary::{call_at, compact, rust_sources};
    use std::{collections::BTreeMap, fs};

    fn child_environment(command: &Command) -> BTreeMap<String, Option<String>> {
        command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    /// The scrub's names for a starter other than the Stop hook, each removed.
    fn scrubbed() -> BTreeMap<String, Option<String>> {
        SCRUBBED
            .into_iter()
            .chain(AGENT_SESSION_MARKERS)
            .map(|name| (name.to_owned(), None))
            .collect()
    }

    fn with(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        let mut environment = scrubbed();
        environment.extend(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned))),
        );
        environment
    }

    const TIMEOUT_NAMES: [&str; 2] = ["NAMEDROP_HERDR_TIMEOUT_MS", "NAMEDROP_REX_TIMEOUT_MS"];

    /// The scrub drops the Rex claim and the agent markers, and keeps the
    /// timeouts a caller holds, which a person set. Only a daemon of the Stop
    /// hook drops them, because its wrapper's 250 ms is for the hook alone.
    #[test]
    fn the_scrub_drops_the_rex_claim_and_the_agent_markers_and_the_timeouts_only_for_the_stop_hook()
    {
        for (starter, timeouts) in [(Starter::Caller, false), (Starter::StopHook, true)] {
            let mut command = Command::new("true");
            scrub(&mut command, starter);
            let environment = child_environment(&command);
            for name in [
                "REX_BLOCK",
                "REX_SESSION",
                "REX_CLIENT",
                "REX_SERVER",
                "CLAUDE_CODE_SESSION_ID",
                "CODEX_SESSION_ID",
                "CODEX_THREAD_ID",
                "CLAUDE_SESSION_ID",
            ] {
                assert_eq!(environment.get(name), Some(&None), "{starter:?} {name}");
            }
            for name in TIMEOUT_NAMES {
                assert_eq!(
                    environment.get(name),
                    timeouts.then_some(&None),
                    "{starter:?} {name}"
                );
            }
            for kept in [
                "REX_BIN_DIR",
                "REX_HOME",
                "REX_RUNTIME_DIR",
                "HERDR_SOCKET_PATH",
                "HERDR_WORKSPACE_ID",
                "HERDR_PANE_ID",
                "HERDR_PLUGIN_STATE_DIR",
            ] {
                assert!(!environment.contains_key(kept), "{kept}");
            }
            assert_eq!(
                environment.len(),
                if timeouts { 20 } else { 18 },
                "{environment:?}"
            );
        }
    }

    /// Whatever its scope, a daemon keeps a caller's timeouts and drops the
    /// Stop hook's.
    #[test]
    fn a_daemon_keeps_its_caller_s_timeouts_and_drops_the_stop_hook_s() {
        let endpoint = Endpoint::parse("/run/h.sock");
        let space = SpaceId::new("w1");
        for scope in [
            Scope::Space(&endpoint, &space),
            Scope::Endpoint(&endpoint),
            Scope::SpaceAlone(Name::Herdr, &space),
            Scope::None,
        ] {
            let caller =
                child_environment(&command(Path::new("/bin/namedrop"), scope, Starter::Caller));
            let stop = child_environment(&command(
                Path::new("/bin/namedrop"),
                scope,
                Starter::StopHook,
            ));
            for name in TIMEOUT_NAMES {
                assert!(!caller.contains_key(name), "{scope:?} {name}");
                assert_eq!(stop.get(name), Some(&None), "{scope:?} {name}");
            }
        }
    }

    /// Under Herdr a daemon gets today's Herdr names for its scope, the neutral
    /// ones cleared, and every location name its scope leaves out cleared
    /// under both sets.
    #[test]
    fn a_herdr_daemon_gets_its_scope_under_herdr_s_names() {
        let endpoint = Endpoint::parse("/run/h.sock");
        let space = SpaceId::new("w1");
        let pane = [("HERDR_PANE_ID", None), ("NAMEDROP_PANE_ID", None)];
        let cases = [
            (
                Scope::Space(&endpoint, &space),
                [
                    ("HERDR_SOCKET_PATH", Some("/run/h.sock")),
                    ("NAMEDROP_ENDPOINT", None),
                    ("HERDR_WORKSPACE_ID", Some("w1")),
                    ("NAMEDROP_SPACE_ID", None),
                ],
            ),
            (
                Scope::Endpoint(&endpoint),
                [
                    ("HERDR_SOCKET_PATH", Some("/run/h.sock")),
                    ("NAMEDROP_ENDPOINT", None),
                    ("HERDR_WORKSPACE_ID", None),
                    ("NAMEDROP_SPACE_ID", None),
                ],
            ),
            (
                Scope::None,
                [
                    ("HERDR_SOCKET_PATH", None),
                    ("NAMEDROP_ENDPOINT", None),
                    ("HERDR_WORKSPACE_ID", None),
                    ("NAMEDROP_SPACE_ID", None),
                ],
            ),
        ];
        for (scope, location) in cases {
            let command = command(Path::new("/bin/namedrop"), scope, Starter::Caller);
            assert_eq!(
                child_environment(&command),
                with(&[&location[..], &pane[..]].concat()),
                "{scope:?}"
            );
        }
    }

    /// A hook whose home named a space and no endpoint, as a Herdr pane on
    /// Herdr's default socket, gives its wake that space under the set of
    /// names that held it and no endpoint, as the wake the wrapper backgrounded
    /// inherited them.
    #[test]
    fn a_space_without_an_endpoint_keeps_the_set_of_names_that_held_it() {
        let space = SpaceId::new("w1");
        let pane = [("HERDR_PANE_ID", None), ("NAMEDROP_PANE_ID", None)];
        let cases = [
            (
                Name::Herdr,
                [
                    ("HERDR_SOCKET_PATH", None),
                    ("NAMEDROP_ENDPOINT", None),
                    ("HERDR_WORKSPACE_ID", Some("w1")),
                    ("NAMEDROP_SPACE_ID", None),
                ],
            ),
            (
                Name::Neutral,
                [
                    ("HERDR_SOCKET_PATH", None),
                    ("NAMEDROP_ENDPOINT", None),
                    ("HERDR_WORKSPACE_ID", None),
                    ("NAMEDROP_SPACE_ID", Some("w1")),
                ],
            ),
        ];
        for (set, location) in cases {
            let command = command(
                Path::new("/bin/namedrop"),
                Scope::SpaceAlone(set, &space),
                Starter::Caller,
            );
            assert_eq!(
                child_environment(&command),
                with(&[&location[..], &pane[..]].concat()),
                "{set:?}"
            );
        }
    }

    /// Under Rex a daemon gets its scope under the neutral names and no Herdr
    /// location name at all, so a stale outer Herdr cannot redirect it.
    #[test]
    fn a_rex_daemon_gets_its_scope_under_the_neutral_names() {
        let endpoint = Endpoint::parse("rex:/tmp/r.sock");
        let space = SpaceId::new("s1");
        let pane = [("HERDR_PANE_ID", None), ("NAMEDROP_PANE_ID", None)];
        let cases = [
            (
                Scope::Space(&endpoint, &space),
                [
                    ("HERDR_SOCKET_PATH", None),
                    ("NAMEDROP_ENDPOINT", Some("rex:/tmp/r.sock")),
                    ("HERDR_WORKSPACE_ID", None),
                    ("NAMEDROP_SPACE_ID", Some("s1")),
                ],
            ),
            (
                Scope::Endpoint(&endpoint),
                [
                    ("HERDR_SOCKET_PATH", None),
                    ("NAMEDROP_ENDPOINT", Some("rex:/tmp/r.sock")),
                    ("HERDR_WORKSPACE_ID", None),
                    ("NAMEDROP_SPACE_ID", None),
                ],
            ),
        ];
        for (scope, location) in cases {
            let command = command(Path::new("/bin/namedrop"), scope, Starter::Caller);
            assert_eq!(
                child_environment(&command),
                with(&[&location[..], &pane[..]].concat()),
                "{scope:?}"
            );
        }
    }

    /// A real child runs in its own process group, so a signal to its
    /// starter's group misses it, has `/dev/null` for its stdin, stdout, and
    /// stderr, and sees neither the Rex claim it inherited nor a pane, only
    /// its scope.
    #[test]
    fn a_daemon_runs_in_its_own_group_with_null_stdio_and_without_the_rex_claim() {
        struct Removed(std::path::PathBuf);
        impl Drop for Removed {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let directory =
            std::env::temp_dir().join(format!("namedrop-daemon-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let _removed = Removed(directory.clone());
        let script = directory.join("record");
        fs::write(
            &script,
            "#!/bin/sh\nfor fd in 0 1 2; do if [ /dev/fd/$fd -ef /dev/null ]; then stdio=\"$stdio null\"; else stdio=\"$stdio open\"; fi; done\n{ ps -o pgid= -p $$; echo \"stdio<${stdio# }> rex<${REX_BLOCK-unset}> pane<${HERDR_PANE_ID-unset}> socket<${HERDR_SOCKET_PATH-unset}>\"; } > \"$0.out\"\n",
        )
        .unwrap();
        let endpoint = Endpoint::parse("/run/h.sock");
        // `/bin/sh` reads the script, so no exec of a file just written can
        // race another test's fork into ETXTBSY.
        let status = command(
            Path::new("/bin/sh"),
            Scope::Endpoint(&endpoint),
            Starter::Caller,
        )
        .arg(&script)
        .env("PATH", "/usr/bin:/bin")
        .status()
        .unwrap();
        assert!(status.success());
        let recorded = fs::read_to_string(directory.join("record.out")).unwrap();
        let (group, names) = recorded.split_once('\n').unwrap();
        assert_ne!(
            group.trim().parse::<i32>().unwrap(),
            unsafe { libc::getpgrp() },
            "{recorded}"
        );
        assert_eq!(
            names.trim(),
            "stdio<null null null> rex<unset> pane<unset> socket</run/h.sock>"
        );
    }

    /// What a production spawn of a program named by an expression is for.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Spawn {
        /// `daemon::command` itself.
        Door,
        /// The Stop worker, scrubbed of the Stop hook's timeouts by the next
        /// statement.
        Scrubbed,
        /// Namedrop's own executable on purpose, outside the door.
        Exception(&'static str),
        /// Another program.
        Other(&'static str),
    }

    /// Programs a production spawn may name by a literal with no entry in
    /// `SPAWNS`, because none of them starts another program.
    const UTILITIES: &[&str] = &["/bin/ps", "/bin/stty", "git"];

    /// Every production `Command::new` whose program is not a string literal
    /// naming one of `UTILITIES`, by file and call. A Namedrop self-spawn goes through `daemon::command`
    /// or `daemon::scrub`; the four exceptions keep their identity on purpose.
    const SPAWNS: &[(&str, &str, Spawn)] = &[
        ("daemon.rs", "Command::new(executable)", Spawn::Door),
        ("stop_hook.rs", "Command::new(executable)", Spawn::Scrubbed),
        (
            "main.rs",
            "Command::new(launcher)",
            Spawn::Exception("the runtime re-exec replaces this process and keeps its identity"),
        ),
        (
            "installed_runtime.rs",
            "Command::new(launcher)",
            Spawn::Exception("the runtime re-exec replaces this process and keeps its identity"),
        ),
        (
            "reminder/clock.rs",
            "Command::new(selected)",
            Spawn::Exception("the clock's re-exec into a replaced runtime keeps its identity"),
        ),
        (
            "dashboard/server_files.rs",
            "Command::new(executable)",
            Spawn::Exception("the `--help` capability probe exits at once and reads no location"),
        ),
        (
            "dashboard/live_peek.rs",
            "Command::new(std::env::current_exe()?)",
            Spawn::Exception("the observer guard is Herdr adapter plumbing, refused on Rex"),
        ),
        (
            "agent_status_model.rs",
            "Command::new(sandbox_binary())",
            Spawn::Other("the summary worker's sandbox"),
        ),
        (
            "agent_status_model.rs",
            "Command::new(claude_binary())",
            Spawn::Other("the Claude Code summary worker"),
        ),
        (
            "dashboard.rs",
            "Command::new(opener)",
            Spawn::Other("the browser"),
        ),
        (
            "dashboard/observer_guard.rs",
            "Command::new(env::var_os(\"HERDR_BIN_PATH\").unwrap_or_else(||\"herdr\"))",
            Spawn::Other("Herdr"),
        ),
        (
            "dashboard/tailscale.rs",
            "Command::new(self.0)",
            Spawn::Other("tailscale"),
        ),
        (
            "linux_sandbox.rs",
            "Command::new(&binary)",
            Spawn::Other("the Codex summary worker under Landlock"),
        ),
        (
            "mux/herdr/process.rs",
            "Command::new(&self.binary)",
            Spawn::Other("Herdr"),
        ),
        (
            "terminal.rs",
            "Command::new(\"/bin/sh\")",
            Spawn::Other("a terminal recipient's command, as a person sent it"),
        ),
        (
            "update_check.rs",
            "Command::new(\"python3\")",
            Spawn::Other("the update check's script"),
        ),
    ];

    /// `source` with comments removed, char literals emptied, and each string
    /// literal cut to its letters, digits, and `_./-`, so text a literal holds
    /// is never read as code and a program it names still reads as one.
    fn code(source: &str) -> String {
        let plain = |text: &[char]| -> String {
            text.iter()
                .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
                .collect()
        };
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
                let start = at;
                while at < chars.len() && !chars[at..].starts_with(&close) {
                    at += 1;
                }
                code.push_str(&format!(
                    "\"{}\"",
                    plain(&chars[start..at.min(chars.len())])
                ));
                at += close.len();
            } else if rest[0] == '"' {
                at += 1;
                let start = at;
                while at < chars.len() && chars[at] != '"' {
                    at += if chars[at] == '\\' { 2 } else { 1 };
                }
                code.push_str(&format!(
                    "\"{}\"",
                    plain(&chars[start..at.min(chars.len())])
                ));
                at += 1;
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
        code
    }

    /// Compacted code with every `#[cfg(test)]` item cut out, and the names
    /// of the modules those items declared in files of their own.
    fn production(source: &str) -> (String, Vec<String>) {
        let mut text = code(source);
        let mut test_modules = Vec::new();
        while let Some(at) = text.find("#[cfg(test)]") {
            let item = &text[at..];
            let end = match (item.find('{'), item.find(';')) {
                (Some(open), semicolon) if semicolon.is_none_or(|semicolon| open < semicolon) => {
                    let mut depth = 0;
                    let mut close = open;
                    for (offset, character) in item[open..].char_indices() {
                        match character {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    close = open + offset;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    close + 1
                }
                (_, Some(semicolon)) => {
                    let words: Vec<&str> = item[..semicolon].split_whitespace().collect();
                    if words.contains(&"mod") && !item[..semicolon].contains("#[path") {
                        test_modules.extend(words.last().map(|name| (*name).to_owned()));
                    }
                    semicolon + 1
                }
                _ => item.len(),
            };
            text.replace_range(at..at + end, "");
        }
        (compact(&text), test_modules)
    }

    /// The files under `src` that build into production, each with its
    /// compacted production code.
    pub(crate) fn production_files(src: &Path) -> Vec<(String, String)> {
        let mut files = Vec::new();
        rust_sources(src, &mut files);
        files.sort();
        let mut test_only = Vec::new();
        let mut found = Vec::new();
        for file in &files {
            let (text, modules) = production(&fs::read_to_string(file).unwrap());
            let directory = match file.file_stem().unwrap().to_str().unwrap() {
                "lib" | "main" | "mod" => file.parent().unwrap().to_path_buf(),
                stem => file.parent().unwrap().join(stem),
            };
            for module in modules {
                test_only.push(directory.join(format!("{module}.rs")));
                test_only.push(directory.join(&module));
            }
            found.push((file.clone(), text));
        }
        found
            .into_iter()
            .filter(|(file, _)| {
                let name = file.file_name().unwrap().to_str().unwrap();
                name != "tests.rs"
                    && !name.ends_with("_tests.rs")
                    && !test_only
                        .iter()
                        .any(|test| file == test || file.starts_with(test))
            })
            .map(|(file, text)| {
                (
                    file.strip_prefix(src).unwrap().to_str().unwrap().to_owned(),
                    text,
                )
            })
            .collect()
    }

    /// Each production spawn the registry does not list as it is, each listed
    /// spawn not found, and each way of starting a `Command` the scan cannot
    /// follow: a renamed import, a type alias, and `Command::new` taken as a
    /// function. Only a spawn of one of `UTILITIES` by its name needs no
    /// entry, so a shell, `python3`, or `namedrop` named by a literal is
    /// listed or caught.
    fn self_spawn_breaches(src: &Path) -> Vec<String> {
        let mut listed = SPAWNS.to_vec();
        let mut breaches = Vec::new();
        for (name, text) in production_files(src) {
            let identifier = |c: char| c.is_alphanumeric() || c == '_';
            for (at, _) in text.match_indices("Commandas") {
                if !text[..at].ends_with(|c: char| identifier(c) || c == '"') {
                    breaches.push(format!("{name} renames Command"));
                }
            }
            for statement in text.split([';', '{', '}']) {
                let statement = ["pub(crate)", "pub(super)", "pub"]
                    .iter()
                    .find_map(|visibility| statement.strip_prefix(visibility))
                    .unwrap_or(statement);
                if let Some((_, aliased)) = statement
                    .strip_prefix("type")
                    .and_then(|alias| alias.split_once('='))
                    && (aliased == "Command" || aliased.ends_with("::Command"))
                {
                    breaches.push(format!("{name} aliases Command"));
                }
            }
            // `SlashCommand::new` and `PaneCommand::new_line` are other types.
            let starts = |at: usize| !text[..at].ends_with(identifier);
            for (at, _) in text.match_indices("Command::new") {
                if starts(at) && !text[at + "Command::new".len()..].starts_with('(') {
                    breaches.push(format!("{name} takes Command::new as a function"));
                }
            }
            for (at, _) in text
                .match_indices("Command::new(")
                .filter(|(at, _)| starts(*at))
            {
                let call = call_at(&text[at..], "Command::new(");
                if call
                    .strip_prefix("Command::new(\"")
                    .and_then(|program| program.strip_suffix("\")"))
                    .is_some_and(|program| UTILITIES.contains(&program))
                {
                    continue;
                }
                match listed
                    .iter()
                    .position(|&(file, spawn, _)| file == name && spawn == call)
                {
                    Some(index) => {
                        let (_, _, kind) = listed.swap_remove(index);
                        let next = text[at + call.len()..]
                            .split_once(';')
                            .map(|(_, next)| next);
                        if kind == Spawn::Scrubbed
                            && !next.is_some_and(|next| {
                                next.starts_with(
                                    "namedrop::daemon::scrub(&mutcommand,namedrop::daemon::Starter::StopHook)",
                                )
                            })
                        {
                            breaches.push(format!("{name} does not scrub {call}"));
                        }
                    }
                    None => breaches.push(format!("{name} starts {call}")),
                }
            }
        }
        breaches.extend(listed.iter().map(|(file, call, kind)| {
            let what = match kind {
                Spawn::Door => "the door",
                Spawn::Scrubbed => "a scrubbed self-spawn",
                Spawn::Exception(why) | Spawn::Other(why) => why,
            };
            format!("{file} no longer starts {call} ({what})")
        }));
        breaches
    }

    #[test]
    fn every_self_spawn_goes_through_the_daemon_door() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let breaches = self_spawn_breaches(&src);
        assert!(
            breaches.is_empty(),
            "Start a background Namedrop process with `daemon::command`, or `daemon::scrub` for one that stays in its hook's group, so it takes no identity from its starter. Another program gets an entry in `SPAWNS`:\n{}",
            breaches.join("\n")
        );
    }

    /// Each production file outside this one that names `StopHook`, with how
    /// often, and whether `main.rs` names it only for the Stop front end's
    /// clock.
    fn stop_hook_starters(src: &Path) -> (Vec<(String, usize)>, bool) {
        let files = production_files(src);
        let front_end = files.iter().any(|(name, text)| {
            name == "main.rs"
                && text.contains(
                    "ifletSome(state_directory)=cli.stop_hook_state_directory(){let_=namedrop::reminder::ensure_clock(state_directory,None,Starter::StopHook);stop_hook::run(state_directory,inherited);",
                )
        });
        let sites = files
            .into_iter()
            .filter(|(name, _)| name != "daemon.rs")
            .map(|(name, text)| (name, text.matches("StopHook").count()))
            .filter(|(_, count)| *count > 0)
            .collect();
        (sites, front_end)
    }

    /// Only the Stop hook's front end starts a daemon as `Starter::StopHook`:
    /// its clock in `main.rs` and its worker in `stop_hook.rs`. Any other
    /// daemon started that way would lose a timeout a person set.
    #[test]
    fn only_the_stop_hook_s_front_end_passes_the_stop_hook_starter() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert_eq!(
            stop_hook_starters(&src),
            (
                vec![("main.rs".to_owned(), 1), ("stop_hook.rs".to_owned(), 1)],
                true
            )
        );
    }

    #[test]
    fn the_self_spawn_check_catches_a_spawn_outside_the_door() {
        let root = std::env::temp_dir().join(format!("namedrop-spawns-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for (name, _) in production_files(&src) {
            let target = root.join(&name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(src.join(&name), target).unwrap();
        }
        assert_eq!(self_spawn_breaches(&root), Vec::<String>::new());
        let planted = root.join("planted.rs");
        for (source, caught) in [
            (
                "fn wake() { Command::new(crate::runtime_executable().unwrap()).spawn(); }",
                "planted.rs starts Command::new(crate::runtime_executable().unwrap())",
            ),
            (
                "fn wake() { let e = std::env::current_exe().unwrap(); Command::new(e).spawn(); }",
                "planted.rs starts Command::new(e)",
            ),
            (
                "#[cfg(test)] fn helper() {} fn wake() { std::process::Command::new(launcher(state)).spawn(); }",
                "planted.rs starts Command::new(launcher(state))",
            ),
            (
                "use std::process::Command as Spawn; fn wake() { Spawn::new(crate::runtime_executable().unwrap()).spawn(); }",
                "planted.rs renames Command",
            ),
            (
                "use std::process::{Command as Spawn, Stdio};",
                "planted.rs renames Command",
            ),
            (
                "pub(crate) type Spawn = std::process::Command; fn wake() { Spawn::new(e).spawn(); }",
                "planted.rs aliases Command",
            ),
            (
                "fn wake() { let new = Command::new; new(e).spawn(); }",
                "planted.rs takes Command::new as a function",
            ),
            (
                "fn wake() { Some(e).map(std::process::Command::new); }",
                "planted.rs takes Command::new as a function",
            ),
            (
                "fn wake() { Command::new(\"namedrop\").arg(\"hook\").spawn(); }",
                "planted.rs starts Command::new(\"namedrop\")",
            ),
            (
                "fn wake() { Command::new(\"/usr/local/bin/namedrop\").spawn(); }",
                "planted.rs starts Command::new(\"/usr/local/bin/namedrop\")",
            ),
            (
                "fn wake() { let exe = crate::runtime_executable().unwrap(); Command::new(\"sh\").arg(\"-c\").arg(format!(\"{} hook mailbox-wake\", exe.display())).spawn(); }",
                "planted.rs starts Command::new(\"sh\")",
            ),
        ] {
            fs::write(&planted, source).unwrap();
            let found = self_spawn_breaches(&root);
            assert!(found.contains(&caught.to_owned()), "{source}: {found:?}");
        }
        fs::remove_file(&planted).unwrap();
        fs::write(&planted, "// Command::new(launcher)\nconst TEXT: [&str; 2] = [\"Command::new(launcher)\", \"Command as typed\"];\nfn status() { Command::new(\"git\").arg(\"status\").status(); }\nfn other() { SlashCommand::new(launcher); PaneCommand::new_line(launcher); }\n#[cfg(test)]\nmod tests { fn spawn() { Command::new(launcher); Command::new(\"namedrop\"); } }\n").unwrap();
        assert_eq!(
            self_spawn_breaches(&root),
            Vec::<String>::new(),
            "comments, strings, utilities, and tests are not breaches"
        );
        fs::remove_file(&planted).unwrap();
        let stop = root.join("stop_hook.rs");
        let source = fs::read_to_string(&stop).unwrap();
        let scrub = "namedrop::daemon::scrub(&mut command, namedrop::daemon::Starter::StopHook);";
        assert!(source.contains(scrub));
        for (unscrubbed, what) in [
            ("", "an unscrubbed Stop worker"),
            (
                "namedrop::daemon::scrub(&mut command, namedrop::daemon::Starter::Caller);",
                "a Stop worker that keeps the Stop hook's timeouts",
            ),
        ] {
            fs::write(&stop, source.replace(scrub, unscrubbed)).unwrap();
            assert!(
                self_spawn_breaches(&root)
                    .contains(&"stop_hook.rs does not scrub Command::new(executable)".to_owned()),
                "{what} was not caught"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
