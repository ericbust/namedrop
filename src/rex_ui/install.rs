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
        // A line the person added back after removing it is the inserted
        // line again, as the refresh and Doctor read it.
        return if matches!(previous, Some("inserted" | "removed")) {
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
        let writes = targets(home, directories.agent_config(), &config, &module)
            .iter()
            .map(|(what, path)| format!("{what}: {}", path.display()))
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
    // A lease that refuses refuses before the baseline, so a refused install
    // never reloads the person's config. The lease is not held across the
    // reload, and is taken again for the writes.
    installed_runtime::package_guard(Some(home), state, Writer::Activation)?.require_write()?;
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
        Err(e) => {
            return Err(format!(
                "Namedrop could not read {}: {e}. Nothing was changed. Make the file readable, or put {CONFIG_LINE} on its own line in it, then run namedrop install again.",
                config.display()
            ));
        }
    };
    let previous = (record["config"] == json!(config))
        .then(|| record["line"].as_str())
        .flatten();
    let mut line = line_state(&bytes, previous, regular, print);
    let mut reason = match line {
        Line::Printed if print => {
            Some("namedrop install --print-config edits no Rex config file.".to_owned())
        }
        Line::Printed => Some(format!(
            "{} is {}, and Namedrop edits only a regular file.",
            config.display(),
            if metadata.as_ref().is_ok_and(fs::Metadata::is_symlink) {
                "a symbolic link"
            } else {
                "not a regular file"
            }
        )),
        Line::Removed => Some(format!(
            "The line Namedrop inserted was removed from {}, and Namedrop never adds it again.",
            config.display()
        )),
        Line::Inserted | Line::Present => None,
    };
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
        // An edit that fails changes nothing, so the install goes on as
        // --print-config does and says why.
        match Edit::apply(
            &lease,
            &config,
            &bytes,
            after,
            metadata.as_ref().ok().map(|m| m.permissions().mode()),
        ) {
            Ok(edit) => Some(edit),
            Err(error) => {
                line = Line::Printed;
                reason = Some(format!(
                    "Namedrop could not edit {}: {error}",
                    config.display()
                ));
                None
            }
        }
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
    if let Some(edit) = &edit {
        edit.keep_only_its_backup();
    }
    let report = rpc(&endpoint, "client.keymap", Some(&record))?;
    if !matches!(line, Line::Printed | Line::Removed) && report["confirmed"] != true {
        return Err(format!("Namedrop's module is not confirmed: {report}"));
    }
    Ok(
        json!({"state_directory":state,"module":module,"config":config,"line":line.word(),"config_line":CONFIG_LINE,"manual_config":matches!(line,Line::Printed|Line::Removed),"instruction":format!("Put {CONFIG_LINE} on its own line in {}.", config.display()),"reason":reason,"keys":report["keys"],"confirmed":report["confirmed"],"integration_packages":package_report(home),"shell_command":shell,"agent_integration_hook_activation":crate::user_hook_activation_guidance(Some(home),directories.agent_config(),crate::mux::MuxKind::Rex),"targets":listed(targets(home,directories.agent_config(),&config,&module))}),
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
) -> Vec<(&'static str, PathBuf)> {
    let mut targets = vec![
        ("Rex module", module.to_owned()),
        (
            "Rex config line, with a backup beside it",
            config.to_owned(),
        ),
    ];
    targets.extend(crate::install::package_targets(home, config_directories));
    targets
}

fn listed(targets: Vec<(&'static str, PathBuf)>) -> Value {
    targets
        .into_iter()
        .map(|(what, path)| json!({"what": what, "path": path}))
        .collect()
}

const START_REX: &str =
    "Start Rex, or set REX_SERVER to its Unix socket, then run sh scripts/install.sh again.";

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
    let found = server(inherited, |error| match error {
        Some(error) => format!(
            "Namedrop found no running Rex server: {}. {START_REX}",
            super::error_line(error).trim_end_matches('.')
        ),
        None => format!("Namedrop cannot tell where Rex's server is. {START_REX}"),
    })?;
    Ok(json!({
        "server": found.endpoint.key,
        "config": found.config,
        "module": found.module,
        "targets": listed(targets(home, directories.agent_config(), &found.config, &found.module)),
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
    /// Replaces `path`'s bytes `before` with `after`, as bytes, so a file
    /// that is not UTF-8 is edited too. It fails having changed nothing:
    /// `path` keeps its bytes, and the backup it made is gone.
    fn apply(
        lease: &PackageLease,
        path: &Path,
        before: &[u8],
        after: Vec<u8>,
        mode: Option<u32>,
    ) -> Result<Self, String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let _held = lease;
        let backup = mode.map(|_| {
            path.with_file_name(format!(
                "{}{}.bak",
                backup_prefix(path),
                installed_runtime::retirement::utc_now()
            ))
        });
        if let (Some(backup), Some(mode)) = (&backup, mode) {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(backup)
                .map_err(|e| e.to_string())?;
            let written = file
                .write_all(before)
                .and_then(|_| file.set_permissions(fs::Permissions::from_mode(mode & 0o777)))
                .and_then(|_| file.sync_all());
            if let Err(error) = written {
                let _ = fs::remove_file(backup);
                return Err(error.to_string());
            }
        }
        let temporary = path.with_file_name(format!(
            ".namedrop-config-{}.tmp",
            SystemClock.now_unix_ms()
        ));
        let replaced = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.unwrap_or(0o600) & 0o777)
            .open(&temporary)
            .and_then(|mut file| {
                let written = file
                    .write_all(&after)
                    .and_then(|_| file.sync_all())
                    .and_then(|_| fs::rename(&temporary, path));
                let _ = fs::remove_file(&temporary);
                written
            });
        if let Err(error) = replaced {
            if let Some(backup) = &backup {
                let _ = fs::remove_file(backup);
            }
            return Err(error.to_string());
        }
        // The edit stands once the rename is done. Syncing the folder only
        // makes it durable sooner.
        if let Some(folder) = path.parent() {
            let _ = fs::File::open(folder).and_then(|f| f.sync_all());
        }
        Ok(Self {
            path: path.to_owned(),
            backup,
            after,
        })
    }

    /// Once the edit stands, deletes the backups older than its own, so the
    /// folder keeps only the newest. A newer one, of an edit still running,
    /// stays.
    fn keep_only_its_backup(&self) {
        let (Some(folder), Some(name)) = (
            self.path.parent(),
            self.backup
                .as_ref()
                .and_then(|b| b.file_name())
                .and_then(|n| n.to_str()),
        ) else {
            return;
        };
        let prefix = backup_prefix(&self.path);
        for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
            let file = entry.file_name();
            if file.to_str().is_some_and(|file| {
                file.starts_with(&prefix) && file.ends_with(".bak") && file < name
            }) && entry.file_type().is_ok_and(|t| t.is_file())
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// `init.lua.namedrop-`: a backup's name is this and the UTC time, so names
/// sort in the order the backups were made.
fn backup_prefix(path: &Path) -> String {
    format!(
        "{}.namedrop-",
        path.file_name().unwrap_or_default().to_string_lossy()
    )
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

/// What a standalone removal returns once its `removed` marker is gone or
/// replaced, or `None` while the marker stands.
fn stopped(state: &Path) -> Option<Result<Value, String>> {
    match installed_runtime::retirement(state) {
        Err(error) => Some(Err(error)),
        Ok(None) => Some(Ok(
            json!({"removed":false,"message":"Namedrop was installed again. Removal stopped."}),
        )),
        Ok(Some(installed_runtime::Retirement::Removed { .. })) => None,
        Ok(Some(marker)) => Some(Err(marker.message())),
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
    // A removal that a reinstall already stopped asks Rex nothing.
    if standalone && let Some(stopped) = stopped(state) {
        return stopped;
    }
    // Rex's verdict on the file before the edit, so an error that was already
    // there is not taken for one the edit made. Read with no lease held.
    let failed_before = registration(home).ok().flatten().and_then(|record| {
        let (record, _) = planned_removal(record).ok()??;
        let reload = rpc(
            &Endpoint::parse(record["server"].as_str()?),
            "server.config_reload",
            None,
        )
        .ok()?;
        crate::mux::rex::config_errors(&reload).then_some(reload)
    });
    let lease = installed_runtime::package_guard(Some(home), state, Writer::Removal)?;
    if standalone && let Some(stopped) = stopped(state) {
        return stopped;
    }
    let record_owner = crate::locator::read_install_record(&crate::locator::install_record(home));
    let remove_packages = standalone
        && record_owner
            .as_deref()
            .is_none_or(|r| installed_runtime::packages::same(r, state));
    let record = registration(home)?;
    let planned = match record.clone() {
        Some(record) => planned_removal(record)?,
        None => None,
    };
    let edit = match planned {
        Some((record, before)) => {
            let config = PathBuf::from(record["config"].as_str().unwrap_or_default());
            let n = before
                .iter()
                .position(|b| *b == b'\n')
                .map_or(before.len(), |n| n + 1);
            Some(Edit::apply(
                &lease,
                &config,
                &before,
                before[n..].to_vec(),
                fs::metadata(&config).ok().map(|m| m.permissions().mode()),
            )?)
        }
        None => None,
    };
    drop(lease);
    if let Some(record) = &record
        && let Some(server) = record["server"].as_str()
    {
        let endpoint = Endpoint::parse(server);
        // Only an error the edit made fails removal: with no edit, or with a
        // file that already failed, removal goes on.
        if let Ok(reload) = rpc(&endpoint, "server.config_reload", None)
            && crate::mux::rex::config_errors(&reload)
            && edit.is_some()
            && failed_before.is_none()
        {
            return Err(restore(home, state, edit, &endpoint, &reload, true)?);
        }
    }
    if let Some(edit) = &edit {
        edit.keep_only_its_backup();
    }
    let lease = installed_runtime::package_guard(Some(home), state, Writer::Removal)?;
    if standalone && let Some(stopped) = stopped(state) {
        return stopped;
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
    let mut report = json!({"removed":true,"state_preserved":true,"packages_removed":remove_packages,"message":if remove_packages {"Namedrop was removed from Rex. Its state directory was preserved."} else {"Removed only the Rex configuration. The shared packages and install record belong to another owner."}});
    if let Some(diagnostic) = failed_before {
        report["config_error"] = json!(format!(
            "Rex's configuration failed before Namedrop removed its line, so that error is not Namedrop's: {diagnostic}"
        ));
    }
    Ok(report)
}

/// The record, its line state read again from `init.lua`, and the file's
/// bytes, when removal deletes the line: the line state is `inserted`, the
/// file is regular, and its first line is exactly the line. A file it cannot
/// read is an error, never a missing line.
fn planned_removal(mut record: Value) -> Result<Option<(Value, Vec<u8>)>, String> {
    super::reinspect_line(&mut record);
    let Some(config) = record["config"].as_str().map(PathBuf::from) else {
        return Ok(None);
    };
    if record["line"] != "inserted" || !fs::symlink_metadata(&config).is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    let before = fs::read(&config).map_err(|e| format!("{}: {e}", config.display()))?;
    let n = before
        .iter()
        .position(|b| *b == b'\n')
        .map_or(before.len(), |n| n + 1);
    Ok(
        (before.get(..n) == Some(format!("{CONFIG_LINE}\n").as_bytes())
            || before.get(..n) == Some(format!("{CONFIG_LINE}\r\n").as_bytes()))
        .then_some((record, before)),
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
            // The line the person added back, as Doctor said, is ours again.
            assert_eq!(
                line_state(CONFIG_LINE.as_bytes(), Some(prior), true, false),
                Line::Inserted
            );
        }
        // Only a line this install inserted stays out once removed. The
        // person asked to install, so their own line, or a printed one, that
        // is now gone is inserted.
        for prior in ["printed", "present"] {
            assert_eq!(line_state(b"", Some(prior), true, false), Line::Inserted);
        }
        assert_eq!(
            line_state(CONFIG_LINE.as_bytes(), Some("printed"), true, false),
            Line::Present
        );
        assert_eq!(line_state(b"", None, false, false), Line::Printed);
        assert_eq!(line_state(b"", None, true, true), Line::Printed);
    }
}
