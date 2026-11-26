//! A test that writes a script with `fs::write` and then makes it executable
//! can fail on Linux with ETXTBSY ("Text file busy"): a process that another
//! test thread spawns meanwhile holds a copy of the write descriptor until it
//! execs, and the kernel refuses to run the file. `tests/support/executable.rs`
//! writes executables in a child process instead. This fails when a Rust file
//! under `src/` or `tests/` writes a `#!` script with `fs::write` and then
//! gives it an execute bit.
//!
//! It cannot see a script body that is not a literal in the `fs::write` call,
//! a writer other than `fs::write`, or hook wrappers that the installer under
//! test writes in the test process and a test then runs.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Spelled so that this file does not hold the patterns it looks for.
const WRITE: &str = concat!("fs::", "write(");
const SHEBANG: &str = concat!("#", "!/");
const CHMOD: &str = concat!("set_", "permissions(");
const MODE: &str = concat!("from_", "mode(0o");

/// Each `line: text` in `source` where a script written with `fs::write`
/// is then given an execute bit.
fn breaches(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    for (start, line) in lines.iter().enumerate() {
        if !line.contains(WRITE) {
            continue;
        }
        let end = (start..lines.len())
            .find(|&index| lines[index].contains(';'))
            .unwrap_or(lines.len() - 1);
        if !lines[start..=end].iter().any(|line| line.contains(SHEBANG)) {
            continue;
        }
        let after = lines[end + 1..lines.len().min(end + 9)].join("\n");
        let Some(chmod) = after.find(CHMOD) else {
            continue;
        };
        let executable = after[chmod..]
            .find(MODE)
            .map(|mode| &after[chmod + mode + MODE.len()..])
            .is_some_and(|digits| {
                digits
                    .chars()
                    .take(3)
                    .filter_map(|digit| digit.to_digit(8))
                    .any(|digit| digit & 1 == 1)
            });
        if executable {
            found.push(format!("{}: {}", start + 1, line.trim()));
        }
    }
    found
}

fn rust_sources(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn no_test_makes_a_script_executable_after_writing_it_in_process() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_sources(&root.join("src"), &mut files);
    rust_sources(&root.join("tests"), &mut files);
    files.sort();
    let found: Vec<String> = files
        .iter()
        .flat_map(|file| {
            let name = file.strip_prefix(root).unwrap().display().to_string();
            breaches(&fs::read_to_string(file).unwrap())
                .into_iter()
                .map(move |breach| format!("{name}:{breach}"))
        })
        .collect();
    assert!(
        found.is_empty(),
        "write these with executable::write_executable (crate::executable in src/), \
         then set a narrower mode if needed:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_check_finds_a_script_made_executable_after_a_plain_write() {
    let planted = format!(
        "fn f() {{\n    {WRITE}\n        &path,\n        \"{SHEBANG}bin/sh\\nexit 0\\n\",\n    )\n    .unwrap();\n    fs::{CHMOD}\n        &path,\n        fs::Permissions::{MODE}700),\n    )\n    .unwrap();\n}}\n"
    );
    assert_eq!(breaches(&planted).len(), 1, "{planted}");
    let not_executable = planted.replace("0o700", "0o600");
    assert_eq!(breaches(&not_executable), Vec::<String>::new());
    let no_script = planted.replace(SHEBANG, "plain text ");
    assert_eq!(breaches(&no_script), Vec::<String>::new());
}
