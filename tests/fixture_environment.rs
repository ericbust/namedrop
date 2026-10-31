#[path = "support/environment.rs"]
mod environment;

use environment::CommandHome;
use std::{fs, path::Path, process::Command};

#[test]
fn isolated_home_overrides_every_ambient_xdg_directory() {
    let home = std::env::temp_dir().join("namedrop-fixture-environment");
    let directories = [
        ("HOME", home.clone()),
        ("XDG_STATE_HOME", home.join(".local/state")),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_RUNTIME_DIR", home.join(".runtime")),
    ];
    let mut command = Command::new("sh");
    for (key, _) in &directories {
        command.env(key, "/ambient/host-directory");
    }
    let output = command
        .isolated_home(&home)
        .args([
            "-c",
            "printf '%s\\n' \"$HOME\" \"$XDG_STATE_HOME\" \"$XDG_CONFIG_HOME\" \"$XDG_DATA_HOME\" \"$XDG_RUNTIME_DIR\"",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    for ((key, expected), actual) in directories.iter().zip(output.lines()) {
        assert_eq!(
            actual,
            expected.to_str().unwrap(),
            "{key} leaked from the host"
        );
    }
    assert_eq!(output.lines().count(), directories.len());
}

fn assigns_home_directly(source: &str, _extension: &str) -> bool {
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(".env(\"HOME\",")
}

#[test]
fn home_assignment_guard_covers_rust_and_python_setters() {
    for (extension, source, expected) in [
        ("rs", r#"command.env("HOME", home)"#, true),
        ("rs", r#"command.envs([("HOME", home)])"#, true),
        ("rs", r#"command.isolated_home(home)"#, false),
        ("py", "env.update(HOME=str(home))", true),
        ("py", "env = {'HOME': str(home)}", true),
        ("py", "os.environ['HOME'] = str(home)", true),
        ("py", "env.update(XDG_STATE_HOME=str(state))", false),
        ("py", "env.update(isolated_home(home))", false),
    ] {
        assert_eq!(
            assigns_home_directly(source, extension),
            expected,
            "{source}"
        );
    }
}

#[test]
fn command_fixtures_use_the_shared_home_helper() {
    fn audit(directory: &Path, violations: &mut Vec<String>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                audit(&path, violations);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !path.ends_with("tests/support/environment.rs")
            {
                let source = fs::read_to_string(&path).unwrap();
                let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
                if compact.contains(".env(\"HOME\",") {
                    violations.push(path.display().to_string());
                }
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    audit(&root.join("tests"), &mut violations);
    audit(&root.join("src"), &mut violations);
    assert!(
        violations.is_empty(),
        "fixture HOME assignments must use CommandHome::isolated_home:\n{}",
        violations.join("\n")
    );
}
