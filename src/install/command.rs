use super::{conflict, replace_if_changed, shell_single_quote};
use serde_json::{Value, json};
use std::{env, fs, io::ErrorKind, os::unix::fs::PermissionsExt, path::Path};

const OWNER: &str = "#!/bin/sh\n# Managed by Namedrop: shell command.\n";
const PATH_SETUP: &str = "# Namedrop CLI\ncase \":$PATH:\" in\n  *:\"$HOME/.local/bin\":*) ;;\n  *) export PATH=\"$HOME/.local/bin:$PATH\" ;;\nesac\n";

pub(crate) fn install_shell_command(
    home: &Path,
    executable: &Path,
    state: &Path,
    hc: bool,
) -> Result<Value, String> {
    if !home.is_absolute() || !executable.is_absolute() || !state.is_absolute() {
        return Err(
            "shell command installation requires absolute home, executable and state paths".into(),
        );
    }
    let bin = home.join(".local/bin");
    let launcher = bin.join(if hc { "namedrop-app" } else { "namedrop" });
    let existing = read_target(home, &launcher)?;
    if existing
        .as_ref()
        .is_some_and(|(text, _)| !text.starts_with(OWNER))
    {
        return Err(conflict(
            &launcher,
            "already exists and is not owned by Namedrop; move it before activating",
        ));
    }

    if hc {
        fs::create_dir_all(&bin).map_err(|e| conflict(&bin, &e.to_string()))?;
        replace_if_changed(&launcher, &hc_script(executable)?, Some(0o755))?;
        return Ok(json!({"path": launcher, "shell_startup_files": [], "path_configured": false}));
    }
    let shell = env::var("SHELL").unwrap_or_default();
    let mut startup_files = Vec::new();
    match Path::new(&shell).file_name().and_then(|name| name.to_str()) {
        Some("zsh") => {
            let directory = env::var_os("ZDOTDIR")
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.to_path_buf());
            // A custom startup directory outside HOME needs manual PATH setup.
            // Never write into another profile from an isolated activation.
            if directory.is_absolute() && directory.starts_with(home) {
                startup_files.push(directory.join(".zshrc"));
            }
        }
        Some("bash") => {
            startup_files.push(home.join(".bashrc"));
            let login = [".bash_profile", ".bash_login", ".profile"]
                .into_iter()
                .find(|name| home.join(name).exists())
                .unwrap_or(".profile");
            startup_files.push(home.join(login));
        }
        _ => {}
    }
    // Validate every target before changing either the command or shell files.
    let updates = startup_files
        .iter()
        .map(|path| {
            let (mut text, mode) =
                read_target(home, path)?.unwrap_or_else(|| (String::new(), 0o644));
            if !text.contains(PATH_SETUP) {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(PATH_SETUP);
            }
            Ok((path, text, mode))
        })
        .collect::<Result<Vec<_>, String>>()?;
    fs::create_dir_all(&bin).map_err(|e| conflict(&bin, &e.to_string()))?;
    let script = format!(
        "{OWNER}exec {} --plugin-state {} \"$@\"\n",
        shell_single_quote(executable.to_str().ok_or("executable path is not UTF-8")?),
        shell_single_quote(state.to_str().ok_or("state path is not UTF-8")?)
    );
    replace_if_changed(&launcher, &script, Some(0o755))?;
    for (path, text, mode) in &updates {
        fs::create_dir_all(path.parent().expect("startup file has a parent"))
            .map_err(|e| conflict(path, &e.to_string()))?;
        replace_if_changed(path, text, Some(*mode))?;
    }
    Ok(json!({
        "path": launcher,
        "shell_startup_files": startup_files,
        "path_configured": !startup_files.is_empty(),
        "current_shell": "export PATH=\"$HOME/.local/bin:$PATH\"",
        "next_step": if startup_files.is_empty() {
            "Add ~/.local/bin to your shell's PATH; automatic PATH setup supports zsh and bash startup files inside HOME."
        } else {
            "Open a new terminal, or run the current_shell command in this terminal."
        }
    }))
}

fn hc_script(executable: &Path) -> Result<String, String> {
    let executable = shell_single_quote(executable.to_str().ok_or("executable path is not UTF-8")?);
    Ok(format!("{OWNER}exec {executable} \"$@\"\n"))
}

pub(super) fn inspect_hc_shell_command(home: &Path) -> Result<(), String> {
    let launcher = home.join(".local/bin/namedrop-app");
    let (text, mode) = read_target(home, &launcher)?
        .ok_or_else(|| conflict(&launcher, "is missing; activate again to install it"))?;
    let executable = text
        .strip_prefix(OWNER)
        .and_then(|body| body.strip_prefix("exec '"))
        .and_then(|body| body.strip_suffix("' \"$@\"\n"))
        .map(|path| path.replace(r#"'"'"'"#, "'"))
        .ok_or_else(|| conflict(&launcher, "is not an owned Namedrop bootstrap"))?;
    let executable = Path::new(&executable);
    if !executable.is_absolute() || text != hc_script(executable)? {
        return Err(conflict(&launcher, "has an invalid bootstrap command"));
    }
    if mode != 0o755 {
        return Err(conflict(
            &launcher,
            "has incorrect permissions; activate again to repair them",
        ));
    }
    if !fs::metadata(executable)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    {
        return Err(conflict(
            &launcher,
            "references an unavailable executable; activate again to refresh it",
        ));
    }
    Ok(())
}

fn read_target(home: &Path, target: &Path) -> Result<Option<(String, u32)>, String> {
    let relative = target
        .strip_prefix(home)
        .map_err(|_| conflict(target, "is outside HOME"))?;
    let mut path = home.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(conflict(target, "contains a non-local path component"));
        }
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(conflict(
                    &path,
                    "is a symbolic link; configure PATH manually or use a regular file",
                ));
            }
            Ok(metadata) if path == target && metadata.is_file() => {
                return fs::read_to_string(&path)
                    .map(|text| Some((text, metadata.permissions().mode() & 0o777)))
                    .map_err(|e| conflict(&path, &e.to_string()));
            }
            Ok(metadata) if path != target && metadata.is_dir() => {}
            Ok(_) => return Err(conflict(&path, "has an unexpected file type")),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(conflict(&path, &e.to_string())),
        }
    }
    Err(conflict(target, "is not a file path"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn tui_launcher_matches_base_bytes_with_quoted_paths() {
        let root =
            std::env::temp_dir().join(format!("namedrop-launcher-bytes-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let executable = Path::new("/opt/name'drop\n/bin");
        let state = Path::new("/state/it's here\n");
        install_shell_command(&root, executable, state, false).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".local/bin/namedrop")).unwrap(),
            "#!/bin/sh\n# Managed by Namedrop: shell command.\nexec '/opt/name'\"'\"'drop\n/bin' --plugin-state '/state/it'\"'\"'s here\n' \"$@\"\n"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn hc_bootstrap_updates_development_fallback_without_touching_tui() {
        let root = std::env::temp_dir().join(format!(
            "namedrop-bootstrap-fallback-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        for name in ["first", "second"] {
            let executable = root.join(name);
            fs::write(
                &executable,
                format!("#!/bin/sh\nprintf '%s\\n' {name} \"$@\"\n"),
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            install_shell_command(
                &root,
                &executable,
                &root.join(name).with_extension("state"),
                true,
            )
            .unwrap();
            let output = Command::new(root.join(".local/bin/namedrop-app"))
                .args(["doctor", "--host", "hc"])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("{name}\ndoctor\n--host\nhc\n")
            );
            assert!(!root.join(".local/bin/namedrop").exists());
            assert!(!root.join(".bashrc").exists());
            assert!(!root.join(".profile").exists());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
