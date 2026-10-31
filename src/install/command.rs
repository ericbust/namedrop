use super::{conflict, replace_if_changed, shell_single_quote};
use serde_json::{Value, json};
use std::{env, fs, io::ErrorKind, os::unix::fs::PermissionsExt, path::Path};

const OWNER: &str = "#!/bin/sh\n# Managed by Namedrop: shell command.\n";
const PATH_SETUP: &str = "# Namedrop CLI\ncase \":$PATH:\" in\n  *:\"$HOME/.local/bin\":*) ;;\n  *) export PATH=\"$HOME/.local/bin:$PATH\" ;;\nesac\n";

/// The launcher supplies installation state while preserving the invoking pane's
/// Herdr context. Runtime permissions continue to use the real executable.
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
    let launcher = bin.join("namedrop");
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
    let executable = shell_single_quote(executable.to_str().ok_or("executable path is not UTF-8")?);
    let tui = if hc {
        existing.as_ref().map(|(text, _)| text.split_once("# TUI command\n").map_or_else(|| text.strip_prefix(OWNER).unwrap_or(text), |(_, command)| command).to_owned()).unwrap_or_else(|| format!("exec {executable} --host tui --plugin-state \"${{HERDR_PLUGIN_STATE_DIR:-${{XDG_STATE_HOME:-$HOME/.local/state}}/herdr/plugins/namedrop}}\" \"$@\"\n"))
    } else {
        format!(
            "exec {executable} --plugin-state {} \"$@\"\n",
            shell_single_quote(state.to_str().ok_or("state path is not UTF-8")?)
        )
    };
    let script = format!(
        r#"{OWNER}namedrop_explicit_host=
namedrop_transport_host=
namedrop_previous_arg=
for namedrop_arg do
  if [ "$namedrop_previous_arg" = --host ]; then
    namedrop_explicit_host=$namedrop_arg
    break
  fi
  case "$namedrop_arg" in
    --host=*) namedrop_explicit_host=${{namedrop_arg#--host=}}; break ;;
    --herdr-socket|--herdr-socket=*|--herdr-workspace|--herdr-workspace=*|--herdr-pane|--herdr-pane=*) namedrop_transport_host=tui ;;
    --hc-terminal|--hc-terminal=*|--hc-space|--hc-space=*)
      [ "$namedrop_transport_host" = tui ] || namedrop_transport_host=hc ;;

  esac
  namedrop_previous_arg=$namedrop_arg
done
namedrop_selected_host=${{namedrop_explicit_host:-$namedrop_transport_host}}
if [ "$namedrop_selected_host" = hc ] || {{ [ -z "$namedrop_selected_host" ] && [ -z "${{HERDR_SOCKET_PATH-}}${{HERDR_PANE_ID-}}" ] && [ -n "${{HC_TERMINAL_ID-}}" ]; }}; then
  if [ -z "$namedrop_explicit_host" ]; then set -- --host hc "$@"; fi
  exec {executable} "$@"
fi
# TUI command
{tui}"#
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
    fn hc_activation_preserves_tui_launcher_and_is_idempotent() {
        let root =
            std::env::temp_dir().join(format!("namedrop-launcher-hosts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let tui = root.join("tui-binary");
        let hc = root.join("hc-binary");
        for (path, tag) in [(&tui, "tui"), (&hc, "hc")] {
            fs::write(path, format!("#!/bin/sh\nprintf '%s\\n' {tag} \"$@\"\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let state = root.join("old-tui-state");
        install_shell_command(&root, &tui, &state, false).unwrap();
        install_shell_command(&root, &hc, &root.join("hc-state"), true).unwrap();
        let launcher = root.join(".local/bin/namedrop");
        let first = fs::read(&launcher).unwrap();
        install_shell_command(&root, &hc, &root.join("other-hc-state"), true).unwrap();
        assert_eq!(fs::read(&launcher).unwrap(), first);
        let tui_output = Command::new(&launcher)
            .arg("doctor")
            .env("HERDR_SOCKET_PATH", "tui-socket")
            .env_remove("NAMEDROP_HOST")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(tui_output.stdout).unwrap(),
            format!("tui\n--plugin-state\n{}\ndoctor\n", state.display())
        );
        let hc_output = Command::new(&launcher)
            .arg("doctor")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env("HC_TERMINAL_ID", "t1")
            .env_remove("NAMEDROP_HOST")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(hc_output.stdout).unwrap(),
            "hc\n--host\nhc\ndoctor\n"
        );
        for flags in [vec!["--host", "hc"], vec!["--host=hc"]] {
            let output = Command::new(&launcher)
                .args(&flags)
                .arg("doctor")
                .env("HERDR_SOCKET_PATH", "tui")
                .env_remove("NAMEDROP_HOST")
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("hc\n{}\ndoctor\n", flags.join("\n"))
            );
        }
        let output = Command::new(&launcher)
            .args(["--host", "tui", "doctor"])
            .env("HC_TERMINAL_ID", "t1")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("NAMEDROP_HOST")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "tui\n--plugin-state\n{}\n--host\ntui\ndoctor\n",
                state.display()
            )
        );
        for flag in ["--herdr-socket", "--herdr-workspace", "--herdr-pane"] {
            for args in [
                vec![flag.to_owned(), "value".to_owned()],
                vec![format!("{flag}=value")],
            ] {
                let output = Command::new(&launcher)
                    .args(&args)
                    .arg("doctor")
                    .env_remove("HERDR_SOCKET_PATH")
                    .env_remove("HERDR_PANE_ID")
                    .env("HC_TERMINAL_ID", "t1")
                    .env_remove("NAMEDROP_HOST")
                    .output()
                    .unwrap();
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    format!(
                        "tui\n--plugin-state\n{}\n{}\ndoctor\n",
                        state.display(),
                        args.join("\n")
                    )
                );
            }
        }
        for flag in ["--hc-terminal", "--hc-space"] {
            for args in [
                vec![flag.to_owned(), "value".to_owned()],
                vec![format!("{flag}=value")],
            ] {
                let output = Command::new(&launcher)
                    .args(&args)
                    .arg("doctor")
                    .env("HERDR_SOCKET_PATH", "tui")
                    .env_remove("HC_TERMINAL_ID")
                    .env_remove("NAMEDROP_HOST")
                    .output()
                    .unwrap();
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    format!("hc\n--host\nhc\n{}\ndoctor\n", args.join("\n"))
                );
            }
        }
        for tui_environment in [true, false] {
            let mut command = Command::new(&launcher);
            command.arg("doctor").env("NAMEDROP_HOST", "hc");
            if tui_environment {
                command
                    .env("HERDR_SOCKET_PATH", "tui")
                    .env("HERDR_PANE_ID", "p1")
                    .env("HC_TERMINAL_ID", "t1");
            } else {
                command
                    .env_remove("HERDR_SOCKET_PATH")
                    .env_remove("HERDR_PANE_ID")
                    .env_remove("HC_TERMINAL_ID");
            }
            let output = command.output().unwrap();
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("tui\n--plugin-state\n{}\ndoctor\n", state.display()),
                "NAMEDROP_HOST is not a host selector"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
