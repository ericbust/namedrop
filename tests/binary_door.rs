//! Every test reaches Namedrop's binary through `tests/support/binary.rs`,
//! which keeps a Rex block's claim from every process a test starts. This
//! reads every file under `tests/` and fails when the binary's Cargo name
//! appears anywhere else, and when a Rust file starts a program with
//! `Command::new` that is not a plain utility named by a literal, because
//! such a program, or the path it was given, may run the binary.
//!
//! It cannot see a helper that runs the binary without `Command::new`, or a
//! program the door starts that hands the binary's path on to one that
//! inherits a claim of its own. A run of the whole suite from a scratch Rex
//! block, with the server's event log showing no client from the suite,
//! checks those.

#[path = "support/binary.rs"]
mod binary;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The binary's Cargo name, spelled so that this file does not hold it.
const TOKEN: &str = concat!("CARGO_BIN_EXE_", "namedrop");
const DOOR: &str = "support/binary.rs";
/// The spawn call, spelled so that this file's planted sources do not read
/// as its own spawns.
const NEW: &str = concat!("Command::", "new(");
/// A renamed import of `Command`, compacted, which would hide a spawn from
/// `NEW`. Spelled so that this file does not hold it.
const RENAMED: &str = concat!("Command", "as");

/// Programs a test may start with `Command::new` by name, because none of
/// them runs another program.
const UTILITIES: &[&str] = &[
    "git", "ps", "kill", "pkill", "pgrep", "cc", "sqlite3", "tar", "touch", "sleep", "true",
];

/// The only other `Command::new` calls under `tests/`: the door itself, and
/// the file publisher, whose shell runs only `cp`, `cat`, `chmod`, and `mv`.
const SPAWNS: &[(&str, &str)] = &[(DOOR, "program"), ("support/executable.rs", "\"/bin/sh\"")];

fn files(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// Each file under `root` other than the door that names the binary.
fn breaches(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    files(root, &mut found);
    found.sort();
    found
        .into_iter()
        .filter(|path| path.strip_prefix(root).unwrap() != Path::new(DOOR))
        .filter(|path| {
            fs::read(path).is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(TOKEN))
        })
        .map(|path| path.strip_prefix(root).unwrap().display().to_string())
        .collect()
}

/// Each `Command::new` under `root` whose program is neither a utility
/// named by a literal nor listed in `SPAWNS`, as `file: program`, and each
/// file that renames `Command`.
fn spawns(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    files(root, &mut found);
    found.sort();
    let mut breaches = Vec::new();
    for path in found {
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let name = path.strip_prefix(root).unwrap().display().to_string();
        let text: String = fs::read_to_string(&path)
            .unwrap()
            .split_whitespace()
            .collect();
        let identifier = |c: char| c.is_alphanumeric() || c == '_';
        for (at, _) in text.match_indices(RENAMED) {
            if !text[..at].ends_with(|c: char| identifier(c) || c == '"') {
                breaches.push(format!("{name} renames Command"));
            }
        }
        // `SlashCommand::new` is another type.
        for (at, _) in text
            .match_indices(NEW)
            .filter(|(at, _)| !text[..*at].ends_with(identifier))
        {
            let mut depth = 1;
            let start = at + NEW.len();
            let end = text[start..]
                .char_indices()
                .find_map(|(offset, character)| {
                    match character {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    (depth == 0).then_some(start + offset)
                })
                .unwrap_or(text.len());
            let program = text[start..end].trim_end_matches(',');
            let utility = program
                .strip_prefix('"')
                .and_then(|program| program.strip_suffix('"'))
                .is_some_and(|program| UTILITIES.contains(&program));
            if !utility && !SPAWNS.contains(&(name.as_str(), program)) {
                breaches.push(format!("{name}: {program}"));
            }
        }
    }
    breaches
}

fn removed(command: &Command) -> BTreeMap<String, bool> {
    command
        .get_envs()
        .map(|(key, value)| (key.to_string_lossy().into_owned(), value.is_none()))
        .collect()
}

#[test]
fn every_door_removes_the_rex_claim() {
    let claim = BTreeMap::from(
        ["REX_BLOCK", "REX_CLIENT", "REX_SERVER", "REX_SESSION"]
            .map(|name| (name.to_owned(), true)),
    );
    assert_eq!(removed(&binary::namedrop()), claim);
    assert_eq!(removed(&binary::command("/bin/sh")), claim);
    let mut built = Command::new("true");
    assert_eq!(removed(binary::without_rex(&mut built)), claim);
    let output = binary::command("/bin/sh")
        .args(["-c", "echo ${REX_BLOCK-unset} ${REX_SERVER-unset}"])
        .env("REX_BIN_DIR", "/kept")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "unset unset\n");
}

#[test]
fn only_the_door_names_the_binary() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    assert!(
        fs::read_to_string(tests.join(DOOR))
            .unwrap()
            .contains(TOKEN),
        "the door must name the binary"
    );
    assert_eq!(
        breaches(&tests),
        Vec::<String>::new(),
        "Spawn the binary with `crate::binary::namedrop()`, name its path with `crate::binary::namedrop_path()`, and start any program that may run it with `crate::binary::command()`, so it inherits no REX_BLOCK, REX_SESSION, REX_CLIENT, or REX_SERVER"
    );
}

#[test]
fn every_test_spawn_is_a_utility_or_the_door() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    assert_eq!(
        spawns(&tests),
        Vec::<String>::new(),
        "Start a program that may run the binary with `crate::binary::command()`, so it inherits no REX_BLOCK, REX_SESSION, REX_CLIENT, or REX_SERVER. `Command::new` is for the utilities in `UTILITIES`"
    );
}

#[test]
fn a_spawn_of_another_program_is_caught() {
    let root = std::env::temp_dir().join(format!("namedrop-test-spawns-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("support")).unwrap();
    fs::write(
        root.join(DOOR),
        format!("pub fn command(program: &str) -> Command {{ {NEW}program) }}"),
    )
    .unwrap();
    fs::write(
        root.join("quiet.rs"),
        format!(
            "{NEW}\"git\").arg(\"status\"); {NEW}\n    \"sleep\",\n).arg(\"1\"); crate::binary::command(\"/bin/sh\"); Slash{NEW}text); let note = \"Command as typed\";"
        ),
    )
    .unwrap();
    fs::write(root.join("script.py"), format!("{NEW}\"sh\")")).unwrap();
    assert_eq!(spawns(&root), Vec::<String>::new());
    for (source, caught) in [
        (
            format!("{NEW}crate::binary::namedrop_path()).output()"),
            "planted.rs: crate::binary::namedrop_path()",
        ),
        (
            format!("{NEW}\"sh\").args([\"-c\", \"true\"])"),
            "planted.rs: \"sh\"",
        ),
        (
            format!("std::process::{NEW}&script).status()"),
            "planted.rs: &script",
        ),
        (
            format!(
                "use std::process::{} Spawn; let child = Spawn::new(script);",
                concat!("Command", " as")
            ),
            "planted.rs renames Command",
        ),
    ] {
        fs::write(root.join("planted.rs"), &source).unwrap();
        assert_eq!(spawns(&root), [caught], "{source}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_spawn_outside_the_door_is_caught() {
    let root = std::env::temp_dir().join(format!("namedrop-binary-door-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("support")).unwrap();
    fs::write(root.join(DOOR), format!("env!(\"{TOKEN}\")")).unwrap();
    fs::write(root.join("quiet.rs"), "crate::binary::namedrop().output()").unwrap();
    assert_eq!(breaches(&root), Vec::<String>::new());
    for (file, source) in [
        ("planted.rs", format!("{NEW}env!(\"{TOKEN}\")).output()")),
        (
            "support/helper.rs",
            format!("let path = env!(\"{TOKEN}\");"),
        ),
        ("script.py", format!("os.environ[\"{TOKEN}\"]")),
    ] {
        fs::write(root.join(file), source).unwrap();
        assert!(
            breaches(&root).contains(&file.to_owned()),
            "{file} was not caught"
        );
        fs::remove_file(root.join(file)).unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}
