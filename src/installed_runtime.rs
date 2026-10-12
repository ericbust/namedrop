//! Stable command paths and shared-package ownership across compatible updates.
use sha2::Digest;
use std::{fs, os::unix::fs::OpenOptionsExt, path::PathBuf};

/// Hold across the command, including dispatch and shared package writes. The
/// updater takes the exclusive lock while old commands drain and state migrates.
pub fn command_guard(state: &std::path::Path) -> Result<Option<fs::File>, String> {
    let runtime = state.join("runtime");
    if !runtime.is_dir() {
        return Ok(None);
    }
    let path = runtime.join("commands.lock");
    if fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("runtime command lock is not a regular file".into());
    }
    let guard = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| error.to_string())?;
    guard.lock_shared().map_err(|error| error.to_string())?;
    if fs::symlink_metadata(runtime.join("migration.json")).is_ok() {
        return Err("Namedrop activation was interrupted. Reopen Update Namedrop to finish; this command made no changes.".into());
    }
    Ok(Some(guard))
}

pub(crate) fn directory() -> Result<Option<PathBuf>, String> {
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| error.to_string())?;
    let Some(release) = executable.parent() else {
        return Ok(None);
    };
    let Some(releases) = release.parent() else {
        return Ok(None);
    };
    if executable.file_name().is_none_or(|name| name != "namedrop")
        || releases.file_name().is_none_or(|name| name != "releases")
        || !release
            .file_name()
            .is_some_and(|name| is_release_name(&name.to_string_lossy()))
    {
        return Ok(None);
    }
    let Some(runtime) = releases.parent() else {
        return Ok(None);
    };
    let contract: serde_json::Value = serde_json::from_slice(
        &fs::read(release.join("runtime.json")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if contract
        != serde_json::from_str::<serde_json::Value>(include_str!("../runtime.json"))
            .map_err(|error| error.to_string())?
    {
        return Err(
            "the running runtime snapshot has an inconsistent compatibility declaration".into(),
        );
    }
    Ok(Some(runtime.to_path_buf()))
}

fn is_release_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|c| c.is_ascii_hexdigit())
}

/// The release that `runtime/current` selects is not the executable the
/// registered plugin root staged at `bin/namedrop`.
pub(crate) struct StaleRelease {
    pub root: PathBuf,
    pub built: PathBuf,
    pub active: String,
}

/// Local checks gate the Herdr read, so dev runs and machines that were never
/// activated never ask for the plugin root. Only `bin/namedrop` counts: a Mac
/// dev `cargo build` writes the `target/` fallback, which must not look stale.
pub(crate) fn stale_release(
    state: &std::path::Path,
    plugin_root: impl FnOnce() -> Option<PathBuf>,
) -> Option<StaleRelease> {
    let runtime = state.join("runtime");
    if !fs::symlink_metadata(runtime.join("namedrop")).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let target = fs::read_link(runtime.join("current")).ok()?;
    let active = target.strip_prefix("releases").ok()?.to_str()?;
    if !is_release_name(active) {
        return None;
    }
    let active_len = fs::metadata(runtime.join("current/namedrop")).ok()?.len();
    let root = plugin_root()?;
    let built = root.join("bin/namedrop");
    let built_len = fs::symlink_metadata(&built)
        .ok()
        .filter(|m| m.is_file())?
        .len();
    // Activation names each release by its digest, so only the staged file is hashed.
    let same = built_len == active_len && sha256_hex(&built).ok()? == active;
    (!same).then(|| StaleRelease {
        root,
        built,
        active: active.to_owned(),
    })
}

fn sha256_hex(path: &std::path::Path) -> std::io::Result<String> {
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// The launcher remains a regular, stable path so cached hooks and exact command
/// permission rules survive replacement of the managed source checkout.
pub fn executable() -> Result<PathBuf, String> {
    match directory()? {
        Some(runtime) => {
            let launcher = runtime.join("namedrop");
            if !fs::symlink_metadata(&launcher).is_ok_and(|metadata| metadata.is_file()) {
                return Err(
                    "the stable runtime launcher is missing or is not a regular file".into(),
                );
            }
            Ok(launcher)
        }
        None => std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|error| error.to_string()),
    }
}

/// Serialize pointer publication with package refresh. A command already running
/// from an older compatible snapshot may finish its work, but cannot overwrite
/// the current runtime's shared integration packages afterwards.
pub(crate) fn package_guard() -> Result<(Option<fs::File>, bool), String> {
    let Some(runtime) = directory()? else {
        return Ok((None, true));
    };
    let path = runtime.join("packages.lock");
    if fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("runtime package lock is not a regular file".into());
    }
    let guard = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| error.to_string())?;
    guard.lock().map_err(|error| error.to_string())?;
    let active =
        fs::canonicalize(runtime.join("current/namedrop")).map_err(|error| error.to_string())?;
    let running = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| error.to_string())?;
    Ok((Some(guard), active == running))
}

/// A live update must keep every host entrypoint behind the stable launcher.
pub fn validate_update_manifest(path: &std::path::Path) -> Result<(), String> {
    let manifest = fs::read_to_string(path)
        .map_err(|error| error.to_string())?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| error.to_string())?;
    if manifest.get("id").and_then(toml_edit::Item::as_str) != Some("namedrop") {
        return Err("update manifest is not Namedrop".into());
    }
    for section in ["startup", "actions", "panes", "events"] {
        let entries = manifest
            .get(section)
            .and_then(toml_edit::Item::as_array_of_tables)
            .ok_or_else(|| format!("update manifest has no {section}"))?;
        for entry in entries {
            let args = entry
                .get("command")
                .and_then(toml_edit::Item::as_array)
                .ok_or("update entrypoint has no command")?
                .iter()
                .map(toml_edit::Value::as_str)
                .collect::<Option<Vec<_>>>()
                .ok_or("update command is not a string array")?;
            if !matches!(
                args.as_slice(),
                ["sh", "./scripts/run.sh", ..]
                    | ["sh", "./scripts/setup.sh", "activate", "--keep-open"]
                    | ["sh", "./scripts/update.sh"]
                    | ["sh", "./scripts/update.sh", "--open"]
            ) {
                return Err(format!(
                    "{section} entrypoint bypasses the stable runtime launcher"
                ));
            }
        }
    }
    Ok(())
}
