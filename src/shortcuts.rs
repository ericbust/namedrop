//! Shared shortcut installation for activation and the explicit repair command.
use crate::HerdrCommandExecutor;
use std::{
    env, fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

/// (key, action, label, description). Every key is free in Herdr's default
/// keymap; `prefix+shift+d` is Herdr's close_workspace, so Doctor uses `h`.
/// `prefix+shift+t` is rename_tab, so Tour uses `o`.
const SHORTCUTS: [(&str, &str, &str, &str); 5] = [
    (
        "prefix+shift+s",
        "namedrop.agent-status",
        "Agent status",
        "show this agent's status",
    ),
    (
        "prefix+shift+i",
        "namedrop.name-agent",
        "Name this pane",
        "name this pane",
    ),
    (
        "prefix+shift+m",
        "namedrop.manage-addresses",
        "Manage names",
        "manage Namedrop names and agents",
    ),
    (
        "prefix+shift+h",
        "namedrop.doctor",
        "Doctor",
        "open Namedrop Doctor",
    ),
    (
        "prefix+shift+o",
        "namedrop.tour",
        "Tour",
        "open the Namedrop tour",
    ),
];

fn contains_key(item: &Item, key: &str) -> bool {
    if let Some(text) = item.as_str() {
        return text.eq_ignore_ascii_case(key);
    }
    if let Some(table) = item.as_table_like() {
        return table.iter().any(|(_, item)| contains_key(item, key));
    }
    item.as_array_of_tables().is_some_and(|tables| {
        tables
            .iter()
            .any(|table| table.iter().any(|(_, item)| contains_key(item, key)))
    })
}

/// Herdr reads `$XDG_CONFIG_HOME/herdr/config.toml` when that is an absolute
/// path, otherwise `~/.config/herdr/config.toml`. XDG_CONFIG_HOME belongs to
/// this process's own user, so it only applies when `home` is that user's
/// HOME; activating for another home (tests, isolated profiles) never touches
/// this user's configuration.
fn default_config(home: &Path) -> PathBuf {
    let own_home = env::var_os("HOME").is_some_and(|value| Path::new(&value) == home);
    env::var_os("XDG_CONFIG_HOME")
        .filter(|_| own_home)
        .map(PathBuf::from)
        .filter(|directory| directory.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("herdr/config.toml")
}

/// Add each Namedrop shortcut that is not yet bound. An existing binding for
/// the action is kept, including a custom key; a key already used by anything
/// else is left alone and reported. All additions land in one atomic write.
pub fn install_shortcuts(
    config: Option<&Path>,
    home: Option<&Path>,
) -> Result<serde_json::Value, String> {
    let path = config
        .map(Path::to_path_buf)
        .or_else(|| env::var_os("HERDR_CONFIG_PATH").map(PathBuf::from))
        .or_else(|| home.map(default_config))
        .ok_or("Could not locate Herdr configuration; use --config PATH")?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(metadata),
        Ok(_) => return Err("Herdr configuration must be a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let existed = metadata.is_some();
    let original = if existed {
        fs::read_to_string(&path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };
    let mut document = original.parse::<DocumentMut>().map_err(|e| e.to_string())?;

    let mut bindings = Vec::new();
    let mut additions = Vec::new();
    for (key, action, label, description) in SHORTCUTS {
        let bound = document
            .get("keys")
            .and_then(|keys| keys.get("command"))
            .and_then(Item::as_array_of_tables)
            .and_then(|commands| {
                commands.iter().find(|table| {
                    table.get("type").and_then(Item::as_str) == Some("plugin_action")
                        && table.get("command").and_then(Item::as_str) == Some(action)
                })
            })
            .map(|binding| binding.get("key").and_then(Item::as_str).map(str::to_owned));
        bindings.push(if let Some(bound) = bound {
            serde_json::json!({"action":action, "label":label, "key":bound, "status":"already_configured"})
        } else if document.get("keys").is_some_and(|keys| contains_key(keys, key)) {
            serde_json::json!({"action":action, "label":label, "key":key, "status":"conflict",
                "error":format!("{key} already has a binding; configuration unchanged. Bind {action} to another key in Herdr settings.")})
        } else {
            additions.push((key, action, description));
            serde_json::json!({"action":action, "label":label, "key":key, "status":"installed"})
        });
    }
    let status = if bindings
        .iter()
        .any(|binding| binding["status"] == "conflict")
    {
        "needs_attention"
    } else if additions.is_empty() {
        "already_configured"
    } else {
        "installed"
    };
    if additions.is_empty() {
        return Ok(serde_json::json!({"status":status, "path":path, "bindings":bindings}));
    }

    if document.get("keys").is_none() {
        document["keys"] = Item::Table(Table::new());
    }
    let keys = document["keys"]
        .as_table_like_mut()
        .ok_or("Herdr keys must be a table")?;
    if keys.get("command").is_none() {
        keys.insert("command", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let commands = keys
        .get_mut("command")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or("Herdr keys.command must be an array of tables")?;
    for (key, action, description) in additions {
        let mut binding = Table::new();
        binding["key"] = value(key);
        binding["type"] = value("plugin_action");
        binding["command"] = value(action);
        binding["description"] = value(description);
        commands.push(binding);
    }

    let temporary = path.with_extension(format!("namedrop-shortcuts-{}.tmp", std::process::id()));
    let backup = path.with_extension(format!("namedrop-shortcuts-{}.bak", std::process::id()));
    let write_private = |target: &Path, text: &str| -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_private(&temporary, &document.to_string())?;
    let result = (|| {
        if let Some(metadata) = metadata {
            if fs::read_to_string(&path).map_err(|e| e.to_string())? != original {
                return Err(
                    "Herdr configuration changed while preparing the shortcuts; try again"
                        .to_owned(),
                );
            }
            write_private(&backup, &original)?;
            fs::set_permissions(&temporary, metadata.permissions()).map_err(|e| e.to_string())?;
            fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        } else {
            // Publish a new config only if no file appeared while preparing it.
            fs::hard_link(&temporary, &path).map_err(|e| e.to_string())?;
        }
        Ok(
            serde_json::json!({"status":status, "path":path, "backup":existed.then_some(&backup), "next_step":"herdr server reload-config", "bindings":bindings}),
        )
    })();
    let _ = fs::remove_file(&temporary);
    result
}

pub(crate) fn activate_shortcuts(
    home: Option<&Path>,
    herdr: &impl HerdrCommandExecutor,
) -> serde_json::Value {
    match install_shortcuts(None, home) {
        Ok(mut report) => {
            match herdr.execute(&["server", "reload-config"]) {
                Ok(_) => {
                    report["reloaded"] = serde_json::json!(true);
                    report.as_object_mut().unwrap().remove("next_step");
                }
                Err(error) => {
                    report["reloaded"] = serde_json::json!(false);
                    report["error"] = serde_json::json!(format!(
                        "Shortcuts saved, but Herdr could not reload them: {error}. Run herdr server reload-config or reopen setup."
                    ));
                }
            }
            report
        }
        Err(error) => serde_json::json!({"status":"unavailable", "error":error}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn tour_shortcut_is_installed_with_the_others() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "namedrop-tour-shortcuts-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();

        for (name, tour_key, custom) in [
            ("fresh", "prefix+shift+o", false),
            ("custom", "prefix+alt+o", true),
        ] {
            let config = root.join(format!("{name}.toml"));
            if custom {
                fs::write(
                    &config,
                    "[[keys.command]]\nkey = \"prefix+alt+o\"\ntype = \"plugin_action\"\ncommand = \"namedrop.tour\"\n",
                )
                .unwrap();
            }

            let report = install_shortcuts(Some(&config), None).unwrap();
            assert_eq!(report["status"], "installed");
            let bindings = report["bindings"].as_array().unwrap();
            assert_eq!(bindings.len(), 5);
            let contents = fs::read_to_string(&config).unwrap();
            let document = contents.parse::<DocumentMut>().unwrap();
            let commands = document["keys"]["command"].as_array_of_tables().unwrap();
            assert_eq!(commands.len(), 5);

            for (key, action) in [
                ("prefix+shift+s", "namedrop.agent-status"),
                ("prefix+shift+i", "namedrop.name-agent"),
                ("prefix+shift+m", "namedrop.manage-addresses"),
                ("prefix+shift+h", "namedrop.doctor"),
                (tour_key, "namedrop.tour"),
            ] {
                let binding = bindings.iter().find(|b| b["action"] == action).unwrap();
                assert_eq!(binding["key"], key);
                assert_eq!(
                    binding["status"],
                    if custom && action == "namedrop.tour" {
                        "already_configured"
                    } else {
                        "installed"
                    }
                );
                let command = commands
                    .iter()
                    .find(|c| c["command"].as_str() == Some(action))
                    .unwrap();
                assert_eq!(command["key"].as_str(), Some(key));
                assert_eq!(command["type"].as_str(), Some("plugin_action"));
            }

            let repeated = install_shortcuts(Some(&config), None).unwrap();
            assert_eq!(repeated["status"], "already_configured");
            assert_eq!(fs::read_to_string(&config).unwrap(), contents);
        }

        fs::remove_dir_all(root).unwrap();
    }
}
