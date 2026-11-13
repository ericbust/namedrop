//! Every test reaches Namedrop's binary through `tests/support/binary.rs`,
//! which keeps a Rex block's claim from every process a test starts. This
//! reads every file under `tests/` and fails when the binary's Cargo name
//! appears anywhere else.
//!
//! It cannot see a helper that builds its own `Command` from the path, a
//! program handed the path as an argument, or a copied or script-run binary,
//! because none names the binary where it runs. A run of the whole suite from
//! a scratch Rex block, with the server's event log showing no client from
//! the suite, checks those.

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
    let mut built = Command::new("/bin/sh");
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
fn a_spawn_outside_the_door_is_caught() {
    let root = std::env::temp_dir().join(format!("namedrop-binary-door-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("support")).unwrap();
    fs::write(root.join(DOOR), format!("env!(\"{TOKEN}\")")).unwrap();
    fs::write(root.join("quiet.rs"), "crate::binary::namedrop().output()").unwrap();
    assert_eq!(breaches(&root), Vec::<String>::new());
    for (file, source) in [
        (
            "planted.rs",
            format!("Command::new(env!(\"{TOKEN}\")).output()"),
        ),
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
