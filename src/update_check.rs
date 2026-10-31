use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
    process::Stdio,
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
}

fn enabled() -> bool {
    true
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

pub(crate) fn settings(path: Option<&Path>) -> Result<bool, String> {
    let path =
        path.ok_or("Namedrop cannot locate its update settings without HOME or XDG_CONFIG_HOME")?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(true);
    }
    read_json::<Settings>(path).map(|settings| settings.check_for_updates)
}

fn version(value: &str) -> Option<[u64; 3]> {
    if value.len() > 32 {
        return None;
    }
    let parts = value
        .split('.')
        .map(|part| {
            (!part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0')))
            .then(|| part.parse().ok())
            .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    parts.try_into().ok()
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
}

pub(crate) fn available(state: &Path, settings_path: Option<&Path>) -> Option<AvailableUpdate> {
    settings(settings_path).ok().filter(|enabled| *enabled)?;
    let cache = cache(state)?;
    let remote = cache.remote_version?;
    (version(&remote)? > version(env!("CARGO_PKG_VERSION"))?).then_some(AvailableUpdate {
        version: remote,
        checked_at: cache.checked_at,
        last_success_at: cache.last_success_at,
        last_error: cache.last_error,
    })
}

/// Called only by startup reconciliation and pane lifecycle events. The child
/// owns the lock and all subprocess/network work, so pane handling never waits.
#[doc(hidden)]
pub fn start(state: &Path, now: u64) {
    start_with_environment(
        state,
        now,
        &crate::environment::EnvironmentSnapshot::capture(),
    );
}

#[doc(hidden)]
pub fn start_with_environment(
    state: &Path,
    now: u64,
    environment: &crate::environment::EnvironmentSnapshot,
) {
    let settings_path = environment
        .agent_config()
        .update_settings(environment.home());
    let Some(path) = settings_path.as_deref() else {
        return;
    };
    if settings(Some(path)) != Ok(true)
        || cache(state)
            .is_some_and(|cache| now >= cache.checked_at && now - cache.checked_at < DAY_MS)
    {
        return;
    }
    let _ = environment
        .command("python3")
        .args(["-c", include_str!("../scripts/check-update.py")])
        .arg(state)
        .arg(path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
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
pub fn session_notice(input: &str, state: &Path, now: u64, output: Option<Value>) -> Option<Value> {
    let environment = crate::environment::EnvironmentSnapshot::capture();
    let path = environment
        .agent_config()
        .update_settings(environment.home());
    session_notice_at(input, state, now, output, path.as_deref())
}

pub fn session_notice_at(
    input: &str,
    state: &Path,
    now: u64,
    mut output: Option<Value>,
    settings_path: Option<&Path>,
) -> Option<Value> {
    let input: Value = serde_json::from_str(input).ok()?;
    if input["hook_event_name"] != "SessionStart"
        || matches!(input["source"].as_str(), Some("clear" | "compact"))
    {
        return output;
    }
    let Some(update) = available(state, settings_path) else {
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
        "Namedrop {} is available (you have {}). Update: {UPDATE_COMMAND}",
        update.version,
        env!("CARGO_PKG_VERSION")
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
    fn version_order_follows_numeric_semver_components() {
        assert!(super::version("1.10.0") > super::version("1.9.0"));
        assert!(super::version("2.0.0") > super::version("1.99.99"));
        assert!(super::version("1.0.10") > super::version("1.0.9"));
    }
}
