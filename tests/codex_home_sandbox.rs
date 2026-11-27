//! The Codex worker under the real `sandbox-exec`, with the profile the
//! worker builds, around a stand-in for Codex that does what Codex does
//! before it asks a model: it resolves a `CODEX_HOME` it was given, stops
//! with Codex's own two lines when it cannot, and reads its credential file
//! by the resolved path. The file holds no credential, and nothing reaches
//! a provider or the network. The stand-in then tries to list each
//! directory in `STAND_IN_PRIVATE`, which the profile lets it look up but
//! never list, and says in its answer whether it was handed a home.
#![cfg(target_os = "macos")]

use namedrop::suggest_agent_persona;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

/// A state directory no test creates. Codex does not read its settings.
const STATE: &str = "/nonexistent/namedrop-test-state";

const STAND_IN: &str = r#"
#include <dirent.h>
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {
    const char *given = getenv("CODEX_HOME");
    char home[PATH_MAX], path[PATH_MAX], buffer[4096];
    if (given) {
        if (!realpath(given, home)) {
            int code = errno;
            fprintf(stderr, "WARNING: proceeding, even though we could not create PATH aliases: "
                "failed to canonicalize CODEX_HOME \"%s\": %s (os error %d)\n", given, strerror(code), code);
            fprintf(stderr, "Error finding codex home: failed to canonicalize CODEX_HOME \"%s\": %s (os error %d)\n",
                given, strerror(code), code);
            return 1;
        }
    } else {
        snprintf(home, sizeof home, "%s/.codex", getenv("HOME"));
    }
    snprintf(path, sizeof path, "%s/auth.json", home);
    FILE *auth = fopen(path, "r");
    if (!auth) {
        fprintf(stderr, "Error reading %s: %s\n", path, strerror(errno));
        return 1;
    }
    fclose(auth);
    const char *private_dirs = getenv("STAND_IN_PRIVATE");
    char list[4 * PATH_MAX];
    snprintf(list, sizeof list, "%s", private_dirs ? private_dirs : "");
    for (char *dir = strtok(list, ":"); dir; dir = strtok(NULL, ":")) {
        DIR *listing = opendir(dir);
        if (listing && readdir(listing)) {
            fprintf(stderr, "Listed %s\n", dir);
            return 1;
        }
        if (listing) closedir(listing);
    }
    while (fread(buffer, 1, sizeof buffer, stdin) > 0) {}
    const char *output = NULL;
    for (int i = 1; i + 1 < argc; i++) {
        if (!strcmp(argv[i], "-o")) output = argv[i + 1];
    }
    FILE *result = output ? fopen(output, "w") : NULL;
    if (!result) {
        fprintf(stderr, "Error writing the last message: %s\n", strerror(errno));
        return 1;
    }
    fputs(given ? "{\"description\": \"Reviews the parser changes with a given home.\"}"
                : "{\"description\": \"Reviews the parser changes.\"}", result);
    fclose(result);
    return 0;
}
"#;

/// A home and the `CODEX_HOME` a person's environment gives the worker, or
/// none, where Codex's files really are, and directories above them that
/// Codex may look up but not list.
struct Layout {
    name: &'static str,
    home: PathBuf,
    given: Option<PathBuf>,
    files: PathBuf,
    private: Vec<PathBuf>,
}

fn compile(directory: &Path) -> PathBuf {
    let source = directory.join("codex.c");
    fs::write(&source, STAND_IN).unwrap();
    let binary = directory.join("codex");
    let output = Command::new("cc")
        .args(["-O1", "-o"])
        .arg(&binary)
        .arg(&source)
        .output()
        .expect("the C compiler");
    assert!(
        output.status.success(),
        "the stand-in for Codex did not build: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

#[test]
fn the_worker_resolves_the_codex_executable_and_credential_home() {
    // By its real path, so the one link below is the only one Codex meets.
    let root = env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("nd-codex-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("bin")).unwrap();
    let codex = compile(&root.join("bin"));
    // The testbed's profiles, and a wrapper that sets `CODEX_HOME` under
    // `~/.config`, put Codex's home beside `HOME` or deep below it, not
    // directly under a two-level `HOME`.
    let profile = |name: &str| root.join("profiles").join(name);
    std::os::unix::fs::symlink(profile("linked"), root.join("link")).unwrap();
    fs::create_dir_all(profile("home-link")).unwrap();
    std::os::unix::fs::symlink(profile("home-link"), root.join("home-link")).unwrap();
    // A dotfiles manager may keep `~/.codex` elsewhere behind a link.
    fs::create_dir_all(profile("dot-link").join("home")).unwrap();
    std::os::unix::fs::symlink(
        profile("dot-link").join("dotfiles/codex"),
        profile("dot-link").join("home/.codex"),
    )
    .unwrap();
    let layouts = [
        Layout {
            name: "the testbed's profile",
            home: profile("testbed").join("home"),
            given: Some(profile("testbed").join("codex")),
            files: profile("testbed").join("codex"),
            private: vec![root.join("profiles")],
        },
        Layout {
            name: "a profile reached through a link",
            home: profile("linked").join("home"),
            given: Some(root.join("link").join("codex")),
            files: profile("linked").join("codex"),
            private: vec![root.join("profiles")],
        },
        Layout {
            name: "a home under ~/.config",
            home: profile("config").join("home"),
            given: Some(profile("config").join("home/.config/agents/codex")),
            files: profile("config").join("home/.config/agents/codex"),
            private: vec![
                profile("config").join("home/.config"),
                profile("config").join("home/.config/agents"),
                root.join("profiles"),
            ],
        },
        Layout {
            name: "the default under HOME",
            home: profile("default").join("home"),
            given: None,
            files: profile("default").join("home/.codex"),
            private: vec![root.join("profiles")],
        },
        // As a HOME under macOS's /tmp is: Codex reads its default home by
        // the path through the link, which it does not resolve.
        Layout {
            name: "the default under a HOME reached through a link",
            home: root.join("home-link/users/me"),
            given: None,
            files: profile("home-link").join("users/me/.codex"),
            private: vec![root.join("profiles")],
        },
        // Codex reads `~/.codex` by the path through the link, which the
        // sandbox checks at the link's own path (round B's review, F4).
        Layout {
            name: "the default home a link",
            home: profile("dot-link").join("home"),
            given: None,
            files: profile("dot-link").join("dotfiles/codex"),
            private: vec![profile("dot-link").join("dotfiles"), root.join("profiles")],
        },
    ];
    let mut failures = Vec::new();
    for layout in &layouts {
        fs::create_dir_all(&layout.home).unwrap();
        fs::create_dir_all(&layout.files).unwrap();
        fs::write(layout.files.join("auth.json"), "{}").unwrap();
        // Exercise the native installation's symlink without a usable PATH.
        let local_bin = layout.home.join(".local/bin");
        fs::create_dir_all(&local_bin).unwrap();
        std::os::unix::fs::symlink(&codex, local_bin.join("codex")).unwrap();
        // Safety: this file holds one test, so no other thread reads the
        // environment while it changes.
        unsafe {
            env::set_var("HOME", &layout.home);
            env::set_var("PATH", &root);
            env::remove_var("NAMEDROP_AGENT_STATUS_CODEX_BIN");
            match &layout.given {
                Some(given) => env::set_var("CODEX_HOME", given),
                None => env::remove_var("CODEX_HOME"),
            }
            env::remove_var("NAMEDROP_AGENT_STATUS_SANDBOX_BIN");
            env::set_var(
                "STAND_IN_PRIVATE",
                env::join_paths(&layout.private).unwrap(),
            );
        }
        // Codex is handed a home only when the person's environment gave it
        // one, so with none it finds its own default as it always did.
        let expected = if layout.given.is_some() {
            "Reviews the parser changes with a given home."
        } else {
            "Reviews the parser changes."
        };
        match suggest_agent_persona(
            Path::new(STATE),
            "reviewer",
            "",
            "codex",
            &AtomicBool::new(false),
        ) {
            Ok(suggestion) if suggestion == expected => {}
            outcome => failures.push(format!("{}: {outcome:?}", layout.name)),
        }
    }
    let _ = fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "the sandboxed worker failed:\n{}",
        failures.join("\n")
    );
}
