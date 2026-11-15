use super::{CONFIG_LINE, MANAGED_LINE, registration};
use crate::{
    AgentRuntime, Clock, InheritedDirectories, SystemClock,
    installed_runtime::{self, PackageLease, Writer},
    mux::{Endpoint, home::Inherited},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Line {
    Inserted,
    Present,
    Printed,
    Removed,
}
impl Line {
    fn word(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Present => "present",
            Self::Printed => "printed",
            Self::Removed => "removed",
        }
    }
}

pub(super) fn has_line(bytes: &[u8]) -> bool {
    bytes
        .split(|b| *b == b'\n')
        .any(|line| std::str::from_utf8(line).is_ok_and(|line| line.trim() == CONFIG_LINE))
}

fn line_state(bytes: &[u8], previous: Option<&str>, regular: bool, print: bool) -> Line {
    if has_line(bytes) {
        return if previous == Some("inserted") {
            Line::Inserted
        } else {
            Line::Present
        };
    }
    if matches!(previous, Some("inserted" | "removed")) {
        return Line::Removed;
    }
    if print || !regular {
        Line::Printed
    } else {
        Line::Inserted
    }
}

pub fn confirm_remove(input: &Value) -> Result<(), String> {
    if input["confirmed"] != true {
        return Err("removal requires confirmation: namedrop install --remove --input-json '{\"confirmed\":true}' removes the registered Rex module and inserted init.lua line; a standalone install also removes its shared agent packages, permissions, Codex configuration, shell command and install record, and preserves its state directory.".into());
    }
    Ok(())
}

pub fn install(
    state: &Path,
    directories: &InheritedDirectories,
    inherited: &dyn Inherited,
    print: bool,
    input: Value,
) -> Result<Value, String> {
    let home = directories
        .home()
        .ok_or("Namedrop install needs an absolute HOME")?;
    let Server {
        endpoint,
        keymap,
        config,
        module,
    } = server(inherited, |error| {
        format!(
            "Start Rex, or set REX_SERVER to its Unix socket, then run namedrop install again.{}",
            error.map(|e| format!(" {e}")).unwrap_or_default()
        )
    })?;
    let launcher = crate::runtime_executable()?;
    if input["confirmed"] != true {
        let writes = targets(home, directories.agent_config(), &config, &module);
        let writes = writes
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                format!(
                    "{}: {}",
                    t["what"].as_str().unwrap_or_default(),
                    t["path"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "installation requires confirmation: state_directory={}, launcher={}, first line of the config {}, and {writes}. Run namedrop install --input-json '{{\"confirmed\":true}}'.",
            state.display(),
            launcher.display(),
            CONFIG_LINE,
        ));
    }
    managed_module(&module)?;
    let baseline = rpc(&endpoint, "server.config_reload", None)?;
    if crate::mux::rex::config_errors(&baseline) {
        return Err(format!(
            "Rex's unedited configuration failed: {baseline}. Add {CONFIG_LINE} to {} after repairing it. No config was edited.",
            config.display()
        ));
    }
    let lease = installed_runtime::package_guard(Some(home), state, Writer::Activation)?;
    lease.require_write()?;
    managed_module(&module)?;
    let mut record = registration(home)?.unwrap_or_else(|| json!({}));
    let metadata = fs::symlink_metadata(&config);
    let regular = match &metadata {
        Ok(m) => m.is_file(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => keymap["config"]["exists"] == false,
        Err(e) => return Err(e.to_string()),
    };
    let bytes = match fs::read(&config) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) if !regular => {
            let _ = e;
            Vec::new()
        }
        Err(e) => return Err(e.to_string()),
    };
    let previous = (record["config"] == json!(config))
        .then(|| record["line"].as_str())
        .flatten();
    let line = line_state(&bytes, previous, regular, print);
    let shell = install_packages(&lease, home, state, &launcher, directories.agent_config())?;
    if let Some(parent) = module.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::install::replace_if_changed(
        &lease,
        &module,
        &super::module_text(&launcher, state)?,
        Some(0o600),
    )?;
    let edit = if !print && line == Line::Inserted && !has_line(&bytes) {
        let newline = if bytes.windows(2).any(|s| s == b"\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut after = format!("{CONFIG_LINE}{newline}").into_bytes();
        after.extend_from_slice(&bytes);
        Some(Edit::apply(
            &lease,
            &config,
            &bytes,
            after,
            metadata.as_ref().ok().map(|m| m.permissions().mode()),
        )?)
    } else {
        None
    };
    record["server"] = json!(endpoint.key);
    record["config"] = json!(config);
    record["module"] = json!(module);
    record["launcher"] = json!(launcher);
    record["state"] = json!(state);
    record["line"] = json!(line.word());
    save(&lease, home, &record)?;
    drop(lease);
    let reload = match rpc(&endpoint, "server.config_reload", None) {
        Ok(reply) => reply,
        Err(error) => {
            return Err(restore(
                home,
                state,
                edit,
                &endpoint,
                &json!({"error":error}),
                false,
            )?);
        }
    };
    if crate::mux::rex::config_errors(&reload) {
        return Err(restore(home, state, edit, &endpoint, &reload, false)?);
    }
    let report = rpc(&endpoint, "client.keymap", Some(&record))?;
    if !matches!(line, Line::Printed | Line::Removed) && report["confirmed"] != true {
        return Err(format!("Namedrop's module is not confirmed: {report}"));
    }
    Ok(
        json!({"state_directory":state,"module":module,"config":config,"line":line.word(),"config_line":CONFIG_LINE,"manual_config":matches!(line,Line::Printed|Line::Removed),"instruction":format!("Put {CONFIG_LINE} on its own line in {}.", config.display()),"keys":report["keys"],"confirmed":report["confirmed"],"integration_packages":package_report(home),"shell_command":shell,"agent_integration_hook_activation":crate::user_hook_activation_guidance(Some(home),directories.agent_config()),"targets":targets(home,directories.agent_config(),&config,&module)}),
    )
}

/// The Rex server `install` edits, found as `rex_claim` finds it, and the
/// config path it reports.
struct Server {
    endpoint: Endpoint,
    keymap: Value,
    config: PathBuf,
    module: PathBuf,
}

fn server(
    inherited: &dyn Inherited,
    unreachable: impl Fn(Option<&str>) -> String,
) -> Result<Server, String> {
    let socket = crate::mux::home::rex_socket(inherited).ok_or_else(|| unreachable(None))?;
    let endpoint = Endpoint::parse(&format!("rex:{}", socket.display()));
    let keymap = rpc(&endpoint, "client.keymap", None).map_err(|e| unreachable(Some(&e)))?;
    let config = keymap["config"]["path"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("Rex did not report an absolute config.path")?;
    let module = config
        .parent()
        .ok_or("Rex config has no directory")?
        .join("namedrop.lua");
    Ok(Server {
        endpoint,
        keymap,
        config,
        module,
    })
}

/// Everything a confirmed `install` writes outside the state directory, in
/// the order `install.sh` lists it.
fn targets(
    home: &Path,
    config_directories: crate::AgentConfigDirectories<'_>,
    config: &Path,
    module: &Path,
) -> Value {
    let mut targets = vec![
        ("Rex module", module.to_owned()),
        (
            "Rex config line, with a backup beside it",
            config.to_owned(),
        ),
    ];
    targets.extend(crate::install::package_targets(home, config_directories));
    Value::Array(
        targets
            .into_iter()
            .map(|(what, path)| json!({"what": what, "path": path}))
            .collect(),
    )
}

/// `install.sh`'s read-only look before it asks: the Rex server `install`
/// would use, by the same rule, and what `install` would write. It writes
/// nothing and takes no lock.
pub fn install_preview(
    directories: &InheritedDirectories,
    inherited: &dyn Inherited,
) -> Result<Value, String> {
    let home = directories
        .home()
        .ok_or("Namedrop install needs an absolute HOME.")?;
    let found = server(inherited, |error| {
        match error {
        Some(error) => format!(
            "Namedrop found no running Rex server: {}. Start Rex, or set REX_SERVER to its Unix socket, then run sh scripts/install.sh again.",
            super::error_line(error).trim_end_matches('.')
        ),
        _ => "Namedrop cannot tell where Rex's server is. Start Rex, or set REX_SERVER to its Unix socket, then run sh scripts/install.sh again.".into(),
    }
    })?;
    Ok(json!({
        "server": found.endpoint.key,
        "config": found.config,
        "module": found.module,
        "targets": targets(home, directories.agent_config(), &found.config, &found.module),
    }))
}

pub(super) fn install_packages(
    lease: &PackageLease,
    home: &Path,
    state: &Path,
    launcher: &Path,
    config: crate::AgentConfigDirectories<'_>,
) -> Result<Value, String> {
    let shell = crate::install::install_shell_command(lease, home, launcher, state)?;
    crate::install::codex_sandbox::activate(lease, &config.codex(home), state)?;
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        crate::install::install_integration_package_with_config(
            lease, home, agent, launcher, state, config,
        )?;
    }
    Ok(shell)
}

fn package_report(home: &Path) -> Value {
    let mut report = json!({});
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        report[agent.as_str()] =
            json!({"installed":true,"path":crate::package_directory(home,agent)});
    }
    report
}

pub(super) fn save(lease: &PackageLease, home: &Path, record: &Value) -> Result<(), String> {
    let _held = lease;
    let path = crate::locator::rex_registration(home);
    if registration(home)?.as_ref() == Some(record) {
        return Ok(());
    }
    installed_runtime::retirement::atomic_json(&path, record)
}

pub(super) fn managed_module(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if !m.is_file() => Err(format!(
            "{} is not a regular managed Namedrop module",
            path.display()
        )),
        Ok(_)
            if fs::read_to_string(path)
                .map_err(|e| e.to_string())?
                .lines()
                .next()
                != Some(MANAGED_LINE) =>
        {
            Err(format!("{} is not managed by Namedrop", path.display()))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub(super) fn rpc(
    endpoint: &Endpoint,
    method: &str,
    record: Option<&Value>,
) -> Result<Value, String> {
    crate::mux::rex::configuration(
        endpoint,
        method,
        record,
        Instant::now() + Duration::from_secs(25),
    )
}

struct Edit {
    path: PathBuf,
    backup: Option<PathBuf>,
    after: Vec<u8>,
}
impl Edit {
    fn apply(
        lease: &PackageLease,
        path: &Path,
        before: &[u8],
        after: Vec<u8>,
        mode: Option<u32>,
    ) -> Result<Self, String> {
        let backup = mode.map(|_| {
            path.with_file_name(format!(
                "{}.namedrop-{}.bak",
                path.file_name().unwrap_or_default().to_string_lossy(),
                installed_runtime::retirement::utc_now()
            ))
        });
        if let Some(backup) = &backup {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(backup)
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            file.write_all(before)
                .and_then(|_| {
                    file.set_permissions(fs::Permissions::from_mode(mode.unwrap() & 0o777))
                })
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
        let text = std::str::from_utf8(&after).map_err(|e| e.to_string())?;
        let _held = lease;
        let temporary = path.with_file_name(format!(
            ".namedrop-config-{}.tmp",
            SystemClock.now_unix_ms()
        ));
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.unwrap_or(0o600) & 0o777)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let result = file
            .write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
            .and_then(|_| fs::rename(&temporary, path))
            .and_then(|_| fs::File::open(path.parent().unwrap()).and_then(|f| f.sync_all()));
        let _ = fs::remove_file(temporary);
        result.map_err(|e| e.to_string())?;
        Ok(Self {
            path: path.to_owned(),
            backup,
            after,
        })
    }
}

fn restore(
    home: &Path,
    state: &Path,
    edit: Option<Edit>,
    endpoint: &Endpoint,
    diagnostic: &Value,
    removing: bool,
) -> Result<String, String> {
    let Some(edit) = edit else {
        return Ok(format!("Rex rejected its configuration: {diagnostic}"));
    };
    let lease = installed_runtime::package_guard(
        Some(home),
        state,
        if removing {
            Writer::Removal
        } else {
            Writer::Command
        },
    )?;
    if removing && installed_runtime::standalone(state)? {
        removal_marker(state)?;
    }
    let message = if fs::symlink_metadata(&edit.path).is_ok_and(|m| m.is_file())
        && fs::read(&edit.path).ok().as_ref() == Some(&edit.after)
    {
        if let Some(backup) = edit.backup {
            fs::rename(backup, &edit.path).map_err(|e| e.to_string())?;
        } else {
            fs::remove_file(&edit.path).map_err(|e| e.to_string())?;
        }
        if let Some(mut record) = registration(home)?
            && record["config"].as_str() == edit.path.to_str()
            && !fs::read(&edit.path).is_ok_and(|bytes| has_line(&bytes))
        {
            record["line"] = json!("printed");
            save(&lease, home, &record)?;
        }
        format!(
            "Rex rejected the edit: {diagnostic}. Restored {} byte for byte.",
            edit.path.display()
        )
    } else {
        format!(
            "Rex rejected the edit: {diagnostic}. {} changed meanwhile and was left alone; backup: {:?}.",
            edit.path.display(),
            edit.backup
        )
    };
    drop(lease);
    let _ = rpc(endpoint, "server.config_reload", None);
    Ok(message)
}

fn removal_marker(state: &Path) -> Result<(), String> {
    match installed_runtime::retirement(state)? {
        Some(installed_runtime::Retirement::Removed { .. }) => Ok(()),
        Some(marker) => Err(marker.message()),
        None => Err("Namedrop was installed again. Removal stopped.".into()),
    }
}

pub fn remove(
    state: &Path,
    directories: &InheritedDirectories,
    _inherited: &dyn Inherited,
) -> Result<Value, String> {
    let home = directories
        .home()
        .ok_or("Namedrop removal needs an absolute HOME")?;
    let standalone = installed_runtime::standalone(state)?;
    let lease = installed_runtime::package_guard(Some(home), state, Writer::Removal)?;
    if standalone {
        match installed_runtime::retirement(state)? {
            None => {
                return Ok(
                    json!({"removed":false,"message":"Namedrop was installed again. Removal stopped."}),
                );
            }
            Some(installed_runtime::Retirement::HerdrHandoff { successor, at }) => {
                return Err(
                    installed_runtime::Retirement::HerdrHandoff { successor, at }.message(),
                );
            }
            Some(installed_runtime::Retirement::Removed { .. }) => {}
        }
    }
    let record_owner = crate::locator::read_install_record(&crate::locator::install_record(home));
    let remove_packages = standalone
        && record_owner
            .as_deref()
            .is_none_or(|r| installed_runtime::packages::same(r, state));
    let record = registration(home)?;
    let mut edit = None;
    if let Some(record) = &record
        && let Some(config) = record["config"].as_str().map(PathBuf::from)
        && record["line"] == "inserted"
        && fs::symlink_metadata(&config).is_ok_and(|m| m.is_file())
    {
        let before = fs::read(&config).map_err(|e| e.to_string())?;
        let n = before
            .iter()
            .position(|b| *b == b'\n')
            .map_or(before.len(), |n| n + 1);
        if before.get(..n) == Some(format!("{CONFIG_LINE}\n").as_bytes())
            || before.get(..n) == Some(format!("{CONFIG_LINE}\r\n").as_bytes())
        {
            edit = Some(Edit::apply(
                &lease,
                &config,
                &before,
                before[n..].to_vec(),
                fs::metadata(&config).ok().map(|m| m.permissions().mode()),
            )?);
        }
    }
    drop(lease);
    if let Some(record) = &record
        && let Some(server) = record["server"].as_str()
    {
        let endpoint = Endpoint::parse(server);
        if let Ok(reload) = rpc(&endpoint, "server.config_reload", None)
            && crate::mux::rex::config_errors(&reload)
        {
            return Err(restore(home, state, edit, &endpoint, &reload, true)?);
        }
    }
    let lease = installed_runtime::package_guard(Some(home), state, Writer::Removal)?;
    if standalone {
        match installed_runtime::retirement(state)? {
            None => {
                return Ok(
                    json!({"removed":false,"message":"Namedrop was installed again. Removal stopped."}),
                );
            }
            Some(installed_runtime::Retirement::HerdrHandoff { successor, at }) => {
                return Err(
                    installed_runtime::Retirement::HerdrHandoff { successor, at }.message(),
                );
            }
            Some(installed_runtime::Retirement::Removed { .. }) => {}
        }
    }
    let owner = crate::locator::read_install_record(&crate::locator::install_record(home));
    let remove_packages = remove_packages
        && owner
            .as_deref()
            .is_none_or(|r| installed_runtime::packages::same(r, state));
    if remove_packages {
        crate::install::remove_all(&lease, home, state, directories.agent_config())?;
    }
    if let Some(record) = registration(home)? {
        if let Some(module) = record["module"].as_str().map(PathBuf::from)
            && fs::symlink_metadata(&module).is_ok_and(|m| m.is_file())
            && fs::read_to_string(&module)
                .is_ok_and(|text| text.lines().next() == Some(MANAGED_LINE))
        {
            fs::remove_file(module).map_err(|e| e.to_string())?;
        }
        fs::remove_file(crate::locator::rex_registration(home)).map_err(|e| e.to_string())?;
    }
    Ok(
        json!({"removed":true,"state_preserved":true,"packages_removed":remove_packages,"message":if remove_packages {"Namedrop was removed from Rex. Its state directory was preserved."} else {"Removed only the Rex configuration. The shared packages and install record belong to another owner."}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_lines_and_prior_ownership_decide_the_edit() {
        for bytes in [
            b"-- require(\"namedrop\")\n".as_slice(),
            b"local s = 'pcall(require, \"namedrop\");'\n",
        ] {
            assert_eq!(line_state(bytes, None, true, false), Line::Inserted);
        }
        assert_eq!(
            line_state(
                format!("  {CONFIG_LINE} \r\n").as_bytes(),
                None,
                true,
                false
            ),
            Line::Present
        );
        for prior in ["inserted", "removed"] {
            assert_eq!(line_state(b"", Some(prior), true, false), Line::Removed);
        }
        assert_eq!(
            line_state(b"", Some("printed"), true, false),
            Line::Inserted
        );
        assert_eq!(
            line_state(CONFIG_LINE.as_bytes(), Some("printed"), true, false),
            Line::Present
        );
        assert_eq!(line_state(b"", None, false, false), Line::Printed);
        assert_eq!(line_state(b"", None, true, true), Line::Printed);
    }
}
