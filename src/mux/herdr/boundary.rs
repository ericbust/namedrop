//! Herdr's arguments belong to this adapter. A source check fails when code
//! elsewhere under `src/` builds a Herdr command or starts the `herdr`
//! binary, so a new caller goes through the port instead.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Herdr commands by their leading arguments: every command the adapter
/// runs, and the observer and install commands named below. A verb no other
/// program uses stands alone, so a noun held in a variable, as in
/// `[owner, "send-keys", pane]`, still counts.
const HERDR_COMMANDS: &[&[&str]] = &[
    &["agent", "prompt"],
    &["integration", "install"],
    &["notification", "show"],
    &["pane", "list"],
    &["pane", "read"],
    &["pane", "run"],
    &["plugin", "list"],
    &["plugin", "pane"],
    &["process-info"],
    &["reload-config"],
    &["report-metadata"],
    &["send-keys"],
    &["send-text"],
    &["status", "server"],
    &["tab", "get"],
    &["tab", "rename"],
    &["terminal", "session"],
    &["workspace", "get"],
    &["workspace", "list"],
    &["worktree", "list"],
];

/// Text that starts the `herdr` binary. The variable's name alone counts,
/// so reading it through a constant or `vars_os()` still does.
const HERDR_BINARY: &[&str] = &[r#""HERDR_BIN_PATH""#, r#"Command::new("herdr""#];

/// What a file outside the adapter may name, by file under `src/`.
const ALLOWED: &[(&str, &str)] = &[
    // The live-peek observer starts `herdr terminal session observe` itself
    // until step 4 moves it into the adapter. The dashboard check in
    // `pane_effects.rs` pins its arguments.
    ("dashboard/observer_guard.rs", r#""terminal","session""#),
    ("dashboard/observer_guard.rs", r#""HERDR_BIN_PATH""#),
    // The dashboard check names the variable to forbid it outside the
    // observer guard.
    ("pane_effects.rs", r#""HERDR_BIN_PATH""#),
    // A command the person is told to run, never run here.
    ("install.rs", r#""integration","install""#),
    // git's `worktree list`, not Herdr's.
    ("context/checkout.rs", r#""worktree","list""#),
    // The fake's own tests ask its Herdr wire rendering for replies. It
    // answers arguments for script shims and never starts Herdr.
    ("mux/fake/tests.rs", r#""pane","list""#),
    ("mux/fake/tests.rs", r#""pane","read""#),
    ("mux/fake/tests.rs", r#""reload-config""#),
    ("mux/fake/tests.rs", r#""workspace","get""#),
];

/// The call at the start of `text`, with its argument list when it has
/// one.
pub(crate) fn call_at<'a>(text: &'a str, call: &str) -> &'a str {
    let mut depth = 0;
    let name = call.trim_end_matches('(').len();
    for (at, character) in text.char_indices().skip(name) {
        match character {
            '(' => depth += 1,
            ')' if depth == 1 => return &text[..=at],
            ')' => depth -= 1,
            _ if depth == 0 => return &text[..at],
            _ => {}
        }
    }
    text
}

/// Source with whitespace and string conversions removed, so
/// `"pane" .to_owned(), String::from("run")` reads as `"pane","run"`.
pub(crate) fn compact(source: &str) -> String {
    let mut text = [".to_owned()", ".to_string()", ".into()"].iter().fold(
        source.split_whitespace().collect::<String>(),
        |text, conversion| text.replace(conversion, ""),
    );
    while let Some(at) = text.find("String::from(") {
        let call = call_at(&text[at..], "String::from(");
        let argument = call["String::from(".len()..call.len() - 1].to_owned();
        text.replace_range(at..at + call.len(), &argument);
    }
    text
}

pub(crate) fn rust_sources(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// `source` without its comment lines and its `r#"…"#` literals. A comment
/// may name a command, and a raw string holds source a check test plants.
/// Real arguments need neither.
fn code(source: &str) -> String {
    let mut rest = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut text = String::new();
    while let Some(at) = rest.find("r#") {
        let hashes = rest[at + 1..].chars().take_while(|&c| c == '#').count();
        let open = at + 1 + hashes;
        if !rest[open..].starts_with('"') {
            text.push_str(&rest[..open]);
            rest.replace_range(..open, "");
            continue;
        }
        text.push_str(&rest[..at]);
        let close = format!("\"{}", "#".repeat(hashes));
        let end = rest[open + 1..]
            .find(&close)
            .map_or(rest.len(), |end| open + 1 + end + close.len());
        rest.replace_range(..end, "");
    }
    text.push_str(&rest);
    compact(&text).replace(").arg(", ",")
}

/// `words` as string literals, joined by `separator`.
fn quoted(words: &[&str], separator: &str) -> String {
    words
        .iter()
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(separator)
}

/// What `source` names that only the adapter may: a Herdr command outside a
/// slice pattern, or the `herdr` binary. A pattern such as a fake's
/// `["pane","list"] =>` matches arguments; it cannot run them.
fn herdr_names(source: &str) -> Vec<String> {
    let text = code(source);
    let commands = HERDR_COMMANDS.iter().filter_map(|words| {
        let command = quoted(words, ",");
        text.match_indices(&command)
            .any(|(at, _)| {
                let after = &text[at + command.len()..];
                !after.find(']').is_some_and(|end| {
                    after[end + 1..].starts_with("=>") || after[end + 1..].starts_with('|')
                })
            })
            .then_some(command)
    });
    let binary = HERDR_BINARY
        .iter()
        .filter(|binary| text.contains(*binary))
        .map(|binary| (*binary).to_owned());
    commands.chain(binary).collect()
}

/// Each Herdr command or start of the binary under `src` outside
/// `mux/herdr.rs`, `mux/herdr/`, and `ALLOWED`.
fn herdr_outside_the_adapter(src: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_sources(src, &mut files);
    files.sort();
    files
        .iter()
        .filter(|file| {
            *file != &src.join("mux/herdr.rs") && !file.starts_with(src.join("mux/herdr"))
        })
        .flat_map(|file| {
            let name = file.strip_prefix(src).unwrap().to_str().unwrap();
            herdr_names(&fs::read_to_string(file).unwrap())
                .into_iter()
                .filter(move |token| !ALLOWED.contains(&(name, token.as_str())))
                .map(move |token| format!("{name} names {token}"))
        })
        .collect()
}

#[test]
fn herdr_arguments_and_the_herdr_binary_appear_only_in_the_adapter() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = herdr_outside_the_adapter(&src);
    assert!(
        found.is_empty(),
        "Only src/mux/herdr/ may build Herdr arguments or start herdr. Reach Herdr through the `Mux` port (`mux::herdr::connect`):\n{}",
        found.join("\n")
    );
    for (file, token) in ALLOWED {
        assert!(
            herdr_names(&fs::read_to_string(src.join(file)).unwrap()).contains(&token.to_string()),
            "{file} no longer names {token}, so its allowance goes"
        );
    }
}

#[test]
fn the_boundary_check_catches_each_way_to_name_herdr() {
    let src = std::env::temp_dir().join(format!(
        "namedrop-herdr-boundary-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for directory in ["mux/herdr", "new/module", "dashboard"] {
        fs::create_dir_all(src.join(directory)).unwrap();
    }
    let bypass = src.join("new/module/reads.rs");
    let caught = |source: &str| {
        fs::write(&bypass, source).unwrap();
        let found = herdr_outside_the_adapter(&src);
        fs::remove_file(&bypass).unwrap();
        found
    };
    for words in HERDR_COMMANDS {
        let each = |form: &str| {
            words
                .iter()
                .map(|word| form.replace("WORD", word))
                .collect::<Vec<_>>()
        };
        for source in [
            format!("mux.execute ( & [ {} , pane ] )", quoted(words, " ,\n ")),
            format!(
                "vec![owner, {}, pane]",
                each("\"WORD\".to_owned()").join(",\n ")
            ),
            format!(
                "Command::new(bin){}.arg(pane)",
                each(".arg(\"WORD\")\n ").concat()
            ),
            format!("vec![{}, pane]", each("String::from(\"WORD\")").join(", ")),
            format!("let argv = [{}]; // run it", quoted(words, ", ")),
        ] {
            for owner in ["mux/herdr.rs", "mux/herdr/wire.rs"] {
                fs::write(src.join(owner), &source).unwrap();
            }
            assert_eq!(
                caught(&source),
                [format!("new/module/reads.rs names {}", quoted(words, ","))],
                "{source}"
            );
        }
    }
    for (source, token) in [
        (
            r#"Command::new("herdr").arg(pane)"#,
            r#"Command::new("herdr""#,
        ),
        (
            r#"Command::new(env::var("HERDR_BIN_PATH").unwrap())"#,
            r#""HERDR_BIN_PATH""#,
        ),
        (
            r#"std::env::var_os( "HERDR_BIN_PATH" )"#,
            r#""HERDR_BIN_PATH""#,
        ),
        (
            r#"const BIN: &str = "HERDR_BIN_PATH"; Command::new(env::var(BIN).unwrap())"#,
            r#""HERDR_BIN_PATH""#,
        ),
        (
            r#"env::vars_os().find(|(key, _)| key == "HERDR_BIN_PATH")"#,
            r#""HERDR_BIN_PATH""#,
        ),
    ] {
        assert_eq!(
            caught(source),
            [format!("new/module/reads.rs names {token}")],
            "{source}"
        );
    }
    for harmless in [
        "match args { [\"pane\", \"list\"] => {} [\"pane\", \"run\", ..] | [\"tab\", \"get\", ..] => {} _ => {} }",
        "/// Runs `[\"pane\", \"list\"]` through the port.\nmux.survey(Scope::All, Depth::Panes)",
        "// \"agent\", \"prompt\"\nlet ok = 1;",
        "let planted = r#\"Command::new(\"herdr\").args([\"pane\", \"send-keys\"])\"#;",
        "let words = [\"pane\", \"agent\", \"status\"];",
    ] {
        assert_eq!(caught(harmless), Vec::<String>::new(), "{harmless}");
    }
    let observer = r#"Command::new(env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into())).args(["terminal", "session", "observe", terminal])"#;
    fs::write(src.join("dashboard/observer_guard.rs"), observer).unwrap();
    assert_eq!(herdr_outside_the_adapter(&src), Vec::<String>::new());
    assert_eq!(
        caught(observer),
        [
            r#"new/module/reads.rs names "terminal","session""#,
            r#"new/module/reads.rs names "HERDR_BIN_PATH""#,
        ],
        "an allowance holds for its own file only"
    );
    fs::remove_dir_all(&src).unwrap();
}
