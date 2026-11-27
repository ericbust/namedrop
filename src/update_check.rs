use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const DAY_MS: u64 = 86_400_000;
const MAX_JSON_BYTES: u64 = 65_536;
pub(crate) const UPDATE_COMMAND: &str = "herdr plugin action invoke update --plugin namedrop";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default = "enabled")]
    check_for_updates: bool,
    /// The program Namedrop runs for its own Claude Code calls. `null` is
    /// not a program, so it makes the file invalid rather than meaning
    /// `claude`.
    #[serde(default, deserialize_with = "program")]
    claude_command: Option<String>,
}

fn program<'de, D: serde::Deserializer<'de>>(value: D) -> Result<Option<String>, D::Error> {
    String::deserialize(value).map(Some)
}

fn enabled() -> bool {
    true
}

/// Whether the state's owner is a standalone install. Any other owner, an
/// unreadable one included, keeps Herdr's settings and notice, as every state
/// did before owners existed.
fn standalone(state: &Path) -> bool {
    crate::installed_runtime::standalone(state) == Ok(true)
}

/// Herdr's settings file, under its config directory.
const HERDR_SETTINGS: &str = "herdr/plugins/config/namedrop/settings.json";

/// The directories one process finds its settings under: `HOME` and an
/// absolute `XDG_CONFIG_HOME`.
pub(crate) struct ConfigRoots {
    home: Option<PathBuf>,
    config: Option<PathBuf>,
}

impl ConfigRoots {
    /// This process's own.
    pub(crate) fn environment() -> Self {
        Self::new(
            env::var_os("HOME").map(PathBuf::from),
            env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        )
    }

    pub(crate) fn new(home: Option<PathBuf>, config: Option<PathBuf>) -> Self {
        Self {
            home,
            config: config.filter(|path| path.is_absolute()),
        }
    }

    pub(crate) fn absolute_home(&self) -> Option<&Path> {
        self.home.as_deref().filter(|path| path.is_absolute())
    }
}

pub(crate) fn settings_path(state: &Path) -> Result<PathBuf, String> {
    settings_path_in(state, &ConfigRoots::environment())
}

pub(crate) fn settings_path_in(state: &Path, roots: &ConfigRoots) -> Result<PathBuf, String> {
    if standalone(state) {
        return roots
            .absolute_home()
            .map(|home| home.join(".config/namedrop/settings.json"))
            .ok_or(
                "Namedrop cannot locate its standalone update settings without an absolute HOME"
                    .into(),
            );
    }
    let directory = roots
        .config
        .clone()
        .or_else(|| roots.home.as_ref().map(|home| home.join(".config")))
        .ok_or("Namedrop cannot locate its update settings without HOME or XDG_CONFIG_HOME")?;
    Ok(directory.join(HERDR_SETTINGS))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut data = Vec::new();
    file.take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|error| error.to_string())?;
    if data.len() as u64 > MAX_JSON_BYTES {
        return Err("JSON exceeds size limit".into());
    }
    serde_json::from_slice(&data).map_err(|error| error.to_string())
}

fn read_settings(path: &Path) -> Result<Settings, String> {
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(Settings {
            check_for_updates: true,
            claude_command: None,
        });
    }
    read_json::<Settings>(path)
}

pub(crate) fn settings(state: &Path) -> Result<bool, String> {
    read_settings(&settings_path(state)?).map(|settings| settings.check_for_updates)
}

/// The `claude_command` setting, from the settings file this install reads.
pub(crate) struct ClaudeCommand {
    pub file: PathBuf,
    /// None when the file does not set it.
    pub command: Option<String>,
}

/// Why the Claude Code program cannot be told, and the file to fix, when
/// there is one.
pub(crate) struct SettingsFault {
    pub path: Option<PathBuf>,
    pub reason: String,
}

impl SettingsFault {
    fn at(path: &Path, reason: String) -> Self {
        Self {
            path: Some(path.to_owned()),
            reason,
        }
    }
}

impl From<SettingsFault> for String {
    fn from(fault: SettingsFault) -> Self {
        fault.reason
    }
}

/// The `claude_command` setting this install reads, from the settings files
/// under `roots`.
///
/// It decides which Claude Code account Namedrop's own calls use, so where
/// the update check falls back to Herdr's file or to `true`, this fails: an
/// owner that cannot be read, a known settings file that exists but cannot
/// be read, including a link that leads nowhere, and a `claude_command` that
/// a file this install does not read sets differently are each an error,
/// never `claude`.
pub(crate) fn claude_command(
    state: &Path,
    roots: &ConfigRoots,
) -> Result<ClaudeCommand, SettingsFault> {
    crate::installed_runtime::standalone(state).map_err(|error| {
        SettingsFault::at(
            &state.join("runtime/owner"),
            format!("Namedrop cannot tell which settings.json this install reads: {error}"),
        )
    })?;
    let file =
        settings_path_in(state, roots).map_err(|reason| SettingsFault { path: None, reason })?;
    let command = match present(&file)? {
        false => None,
        true => {
            read_json::<Settings>(&file)
                .map_err(|error| {
                    SettingsFault::at(&file, format!("{} is invalid: {error}", file.display()))
                })?
                .claude_command
        }
    };
    for other in unread_settings_paths(&file, roots) {
        if !present(&other)? {
            continue;
        }
        let set = read_json::<Value>(&other)
            .map_err(|error| {
                SettingsFault::at(
                    &other,
                    format!(
                        "{} is invalid, so Namedrop cannot tell whether it sets claude_command: {error}",
                        other.display()
                    ),
                )
            })?
            .get("claude_command")
            .cloned();
        match (set, &command) {
            (Some(_), None) => {
                return Err(SettingsFault::at(
                    &other,
                    format!(
                        "claude_command is set in {}, but this install reads {}; move it to {}",
                        other.display(),
                        file.display(),
                        file.display()
                    ),
                ));
            }
            (Some(set), Some(command)) if set != *command => {
                return Err(SettingsFault::at(
                    &other,
                    format!(
                        "claude_command is set in {} and in {}, which this install reads, to different programs; make them match, or remove it from {}",
                        other.display(),
                        file.display(),
                        other.display()
                    ),
                ));
            }
            _ => {}
        }
    }
    Ok(ClaudeCommand { file, command })
}

/// Whether anything is at `path`, a link that leads nowhere included. A path
/// that is missing because a directory above it is such a link, as when
/// `~/.config/herdr` links to a dotfiles directory that is gone, cannot be
/// read either.
fn present(path: &Path) -> Result<bool, SettingsFault> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            for directory in path.ancestors().skip(1) {
                match fs::symlink_metadata(directory) {
                    Ok(meta) if meta.is_symlink() && fs::metadata(directory).is_err() => {
                        return Err(SettingsFault::at(
                            directory,
                            format!(
                                "{} is a link that leads nowhere, so Namedrop cannot read {}",
                                directory.display(),
                                path.display()
                            ),
                        ));
                    }
                    // The nearest directory that exists decides it.
                    Ok(_) => break,
                    Err(_) => continue,
                }
            }
            Ok(false)
        }
        Err(error) => Err(SettingsFault::at(
            path,
            format!("{} cannot be inspected: {error}", path.display()),
        )),
    }
}

/// The settings files a person may have edited for this install that it
/// does not read: the standalone file, and Herdr's under `XDG_CONFIG_HOME`
/// and under `~/.config`.
fn unread_settings_paths(read: &Path, roots: &ConfigRoots) -> Vec<PathBuf> {
    let home = roots.absolute_home();
    let mut paths = Vec::new();
    for path in [
        home.map(|home| home.join(".config/namedrop/settings.json")),
        roots
            .config
            .as_ref()
            .map(|config| config.join(HERDR_SETTINGS)),
        home.map(|home| home.join(".config").join(HERDR_SETTINGS)),
    ]
    .into_iter()
    .flatten()
    {
        if path != read && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    core: [u64; 3],
    release: Release,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Release {
    Experimental(u64),
    Stable,
}

fn version(value: &str) -> Option<Version> {
    fn number(part: &str) -> Option<u64> {
        (!part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0')))
        .then(|| part.parse().ok())
        .flatten()
    }

    if value.len() > 32 {
        return None;
    }
    let (core, release) = match value.split_once("-exp.") {
        Some((core, serial)) => (core, Release::Experimental(number(serial)?)),
        None => (value, Release::Stable),
    };
    let parts = core.split('.').map(number).collect::<Option<Vec<_>>>()?;
    Some(Version {
        core: parts.try_into().ok()?,
        release,
    })
}

#[derive(Deserialize)]
struct Cache {
    schema_version: u32,
    checked_at: u64,
    remote_version: Option<String>,
    last_success_at: Option<u64>,
    last_error: Option<String>,
}

fn cache(state: &Path) -> Option<Cache> {
    let cache = read_json::<Cache>(&state.join("update-check.json")).ok()?;
    (cache.schema_version == 1).then_some(cache)
}

pub(crate) struct AvailableUpdate {
    pub version: String,
    pub checked_at: u64,
    pub last_success_at: Option<u64>,
    pub last_error: Option<String>,
    pub standalone: bool,
}

impl AvailableUpdate {
    pub fn instruction(&self) -> String {
        if self.standalone {
            format!(
                "download Namedrop {} and run sh scripts/install.sh",
                self.version
            )
        } else {
            UPDATE_COMMAND.to_owned()
        }
    }
}

pub(crate) fn available(state: &Path) -> Option<AvailableUpdate> {
    settings(state).ok().filter(|enabled| *enabled)?;
    let cache = cache(state)?;
    let remote = cache.remote_version?;
    (version(&remote)? > version(env!("CARGO_PKG_VERSION"))?).then_some(AvailableUpdate {
        version: remote,
        checked_at: cache.checked_at,
        last_success_at: cache.last_success_at,
        last_error: cache.last_error,
        standalone: standalone(state),
    })
}

/// Called by startup reconciliation and pane lifecycle events, and for a
/// standalone install by its Rex bridge. The child owns the lock and all
/// subprocess/network work, so pane handling never waits. Returns the child
/// it started, for a long-lived caller to reap.
#[doc(hidden)]
pub fn start(state: &Path, now: u64) -> Option<Child> {
    let path = due(state, now)?;
    Command::new("python3")
        .args(["-c", include_str!("../scripts/check-update.py")])
        .arg(state)
        .arg(path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// The settings file of a check that is due: the settings allow it and none
/// ran within the day.
fn due(state: &Path, now: u64) -> Option<PathBuf> {
    let path = settings_path(state).ok()?;
    (settings(state) == Ok(true)
        && !cache(state)
            .is_some_and(|cache| now >= cache.checked_at && now - cache.checked_at < DAY_MS))
    .then_some(path)
}

/// Starts the update check of a standalone install, from its Rex bridge,
/// which runs no `reconcile` or lifecycle event. A Herdr-owned state keeps
/// its own triggers. The check reads `runtime/source.json`, which a first
/// install writes after the bridge has started, so until it exists the
/// bridge's next start runs the check (the U10 review's F2). It asks about
/// Apple's stub only for a check that is due, so an opted-out or checked
/// install runs no `xcode-select` (round A's review, F4).
pub(crate) fn start_standalone(state: &Path, now: u64) -> Option<Child> {
    if !standalone(state)
        || !state.join("runtime/source.json").is_file()
        || due(state, now).is_none()
        || apple_python_stub()
    {
        return None;
    }
    start(state, now)
}

/// On a Mac without the developer tools, `/usr/bin/python3` is Apple's stub,
/// which fails and offers to install them. It writes no cache, so the bridge
/// would run it on every connection and every hour (the U10 review's F1).
/// The stub is the `python3` that `start` would run when it is the first on
/// PATH and `xcode-select -p`, found as the check finds it, fails.
fn apple_python_stub() -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !cfg!(target_os = "macos") {
        return false;
    }
    // Without PATH, Command searches the system default.
    let path = env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let first = env::split_paths(&path)
        .map(|directory| directory.join("python3"))
        .find(|candidate| {
            fs::metadata(candidate).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        });
    first.and_then(|python| fs::canonicalize(python).ok())
        == Some(PathBuf::from("/usr/bin/python3"))
        && !Command::new("xcode-select")
            .arg("-p")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

#[derive(Deserialize, Serialize)]
struct Notice {
    version: String,
    shown_at: u64,
}

static NEXT_NOTICE: AtomicU64 = AtomicU64::new(1);

fn save_notice(state: &Path, notice: &Notice) -> Option<()> {
    let temporary = state.join(format!(
        ".update-notice-{}-{}",
        std::process::id(),
        NEXT_NOTICE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .ok()?;
    let result = serde_json::to_writer(&mut file, notice)
        .ok()
        .and_then(|()| file.flush().ok())
        .and_then(|()| fs::rename(&temporary, state.join("update-notice.json")).ok());
    let _ = fs::remove_file(&temporary);
    result
}

/// Add only a person-visible field. This also works without a pane runtime,
/// such as Codex's shared app-server or an unnamed session's first hook.
#[doc(hidden)]
pub fn session_notice(
    input: &str,
    state: &Path,
    now: u64,
    mut output: Option<Value>,
) -> Option<Value> {
    let input: Value = serde_json::from_str(input).ok()?;
    if input["hook_event_name"] != "SessionStart"
        || matches!(input["source"].as_str(), Some("clear" | "compact"))
    {
        return output;
    }
    let Some(update) = available(state) else {
        return output;
    };
    if read_json::<Notice>(&state.join("update-notice.json")).is_ok_and(|notice| {
        notice.version == update.version && now.saturating_sub(notice.shown_at) < DAY_MS
    }) || save_notice(
        state,
        &Notice {
            version: update.version.clone(),
            shown_at: now,
        },
    )
    .is_none()
    {
        return output;
    }
    let message = format!(
        "Namedrop {} is available (you have {}). Update: {}",
        update.version,
        env!("CARGO_PKG_VERSION"),
        update.instruction()
    );
    let output = output.get_or_insert_with(|| serde_json::json!({}));
    output["systemMessage"] = match output["systemMessage"].as_str() {
        Some(existing) => format!("{existing}\n{message}"),
        None => message,
    }
    .into();
    Some(output.clone())
}

#[cfg(test)]
mod tests {
    #[test]
    fn experimental_versions_are_valid_and_ordered() {
        assert!(
            super::version("1.5.0-exp.1").is_some(),
            "experimental version must parse"
        );
        for (older, newer) in [
            ("1.5.0-exp.1", "1.5.0-exp.2"),
            ("1.5.0-exp.2", "1.5.0-exp.10"),
            ("1.5.0-exp.10", "1.5.0"),
            ("1.5.0", "1.5.1-exp.0"),
        ] {
            assert!(
                super::version(older).expect("valid older version")
                    < super::version(newer).expect("valid newer version"),
                "{older} < {newer}"
            );
        }
    }

    #[test]
    fn experimental_versions_reject_invalid_identifiers() {
        for value in [
            "1.5.0-exp.",
            "1.5.0-exp.01",
            "1.5.0-exp.-1",
            "1.5.0-exp.18446744073709551616",
            "1.5.0-exp.1.extra",
            "1.5.0-exp.1+build",
            "1.5.0-exp.1\n",
            "1.5.0-exp.$(id)",
        ] {
            assert!(super::version(value).is_none(), "{value}");
        }
    }

    #[test]
    fn version_order_follows_numeric_semver_components() {
        assert!(super::version("1.10.0") > super::version("1.9.0"));
        assert!(super::version("2.0.0") > super::version("1.99.99"));
        assert!(super::version("1.0.10") > super::version("1.0.9"));
    }
}
