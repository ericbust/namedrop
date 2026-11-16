use super::{
    install::{install_packages, save},
    registration,
};
use crate::{
    AgentConfigDirectories,
    installed_runtime::{
        self, Retirement, Writer,
        packages::{Owner, owner, same},
    },
    mux::{
        Endpoint,
        processes::{LocalProcesses, LocalRead, ThisMachine},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Agent {
    endpoint: String,
    block: String,
    agent: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Predecessor {
    state: PathBuf,
    sealed: bool,
    agents: Vec<Agent>,
}

fn predecessors(record: &Value) -> Vec<Predecessor> {
    record["predecessors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect()
}

pub(crate) fn handoff(
    home: Option<&Path>,
    state: &Path,
    config: AgentConfigDirectories<'_>,
) -> Result<Vec<Value>, String> {
    let Some(home) = home else {
        return Ok(Vec::new());
    };
    if installed_runtime::standalone(state)? {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    if let Some(old) = crate::locator::read_install_record(&crate::locator::install_record(home))
        && !same(&old, state)
        && owner(&old)? == Some(Owner::Standalone)
        && old.join("runtime/namedrop").is_file()
    {
        candidates.push(old);
    }
    // An unreadable rex.json stops only a handoff that must write its entry.
    // Otherwise the package refresh reports it, and activation goes on.
    let record = match registration(home) {
        Ok(record) => record,
        Err(_) if candidates.is_empty() => None,
        Err(error) => return Err(error),
    };
    if let Some(record) = record {
        for entry in predecessors(&record) {
            if !entry.sealed
                && installed_runtime::retirement(&entry.state)?
                    .is_some_and(|marker| marker.hands_to(state))
                && !candidates.contains(&entry.state)
            {
                candidates.push(entry.state);
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    installed_runtime::packages::preflight_handoff(home)?;
    let mut reports = Vec::new();
    for old in candidates {
        let gate = installed_runtime::retirement::exclusive(
            &old,
            Duration::from_secs(60),
            "Activate Namedrop",
        )?;
        let mut invalid = Vec::new();
        match installed_runtime::retirement(&old)? {
            None => {
                let lease = installed_runtime::package_guard(Some(home), state, Writer::Handoff)?;
                let current =
                    crate::locator::read_install_record(&crate::locator::install_record(home));
                if current.as_deref().is_none_or(|r| !same(r, &old)) {
                    drop(lease);
                    drop(gate);
                    continue;
                }
                let (agents, unreadable) = bindings(&old);
                invalid = unreadable;
                let entry = Predecessor {
                    state: old.clone(),
                    sealed: false,
                    agents,
                };
                let mut record = registration(home)?.unwrap_or_else(|| json!({}));
                let entries = record
                    .as_object_mut()
                    .ok_or("Rex registration is not an object")?
                    .entry("predecessors")
                    .or_insert_with(|| json!([]));
                let entries = entries
                    .as_array_mut()
                    .ok_or("Rex predecessors is not an array")?;
                entries.retain(|e| {
                    e["state"]
                        .as_str()
                        .is_none_or(|s| !same(Path::new(s), &old))
                });
                entries.push(serde_json::to_value(entry).map_err(|e| e.to_string())?);
                save(&lease, home, &record)?;
                drop(lease);
                installed_runtime::retirement::write(
                    &old,
                    &Retirement::HerdrHandoff {
                        successor: state.to_owned(),
                        at: installed_runtime::retirement::utc_now(),
                    },
                )?;
            }
            Some(marker) if marker.hands_to(state) => {}
            Some(_) => {
                drop(gate);
                continue;
            }
        }
        drop(gate);
        installed_runtime::helpers::stop(&old)?;
        let lease = installed_runtime::package_guard(Some(home), state, Writer::Activation)?;
        let launcher = crate::runtime_executable()?;
        install_packages(&lease, home, state, &launcher, config)?;
        // A module that cannot be rewritten still names the old launcher, so
        // the entry stays unsealed and the next activation tries again.
        let rewritten = super::refresh(&lease, home, &launcher, state);
        drop(lease);
        let confirmation = match rewritten {
            Ok(()) => confirm(home, state, &old),
            Err(error) => json!({"status":"not_confirmed","error":error,"sealed":false}),
        };
        reports.push(json!({"state":old,"notice":format!("The names made on Rex stay in {} and were not carried over. In each Rex session, run Activate Namedrop again and name the panes again.",old.display()),"invalid_bindings":invalid,"module":confirmation}));
    }
    Ok(reports)
}

fn bindings(state: &Path) -> (Vec<Agent>, Vec<PathBuf>) {
    let mut agents = Vec::new();
    let mut invalid = Vec::new();
    let Ok(entries) = fs::read_dir(state.join("rex/sessions")) else {
        return (agents, invalid);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_file())
            || path.extension().is_none_or(|x| x != "json")
        {
            continue;
        }
        match fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Agent>(&bytes).ok())
        {
            Some(agent)
                if agent.agent.split_once('@').is_some_and(|(pid, start)| {
                    pid.parse::<u32>().is_ok_and(|p| p > 1) && !start.is_empty()
                }) && agent.endpoint.starts_with("rex:")
                    && !agent.block.is_empty() =>
            {
                agents.push(agent)
            }
            _ => invalid.push(path),
        }
    }
    agents
        .sort_by(|a, b| (&a.endpoint, &a.block, &a.agent).cmp(&(&b.endpoint, &b.block, &b.agent)));
    (agents, invalid)
}

fn confirm(home: &Path, state: &Path, old: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = (|| {
        let Some(record) = registration(home)? else {
            return Ok(json!({"status":"removed"}));
        };
        if !predecessors(&record)
            .iter()
            .any(|entry| same(&entry.state, old))
        {
            return Ok(json!({"status":"removed"}));
        }
        let seal_without_call = matches!(record["line"].as_str(), Some("printed" | "removed"))
            || record["server"].as_str().is_none();
        if !seal_without_call {
            let endpoint = Endpoint::parse(
                record["server"]
                    .as_str()
                    .ok_or("Rex registration has no server")?,
            );
            let socket = endpoint
                .unix_socket()
                .ok_or("Rex registration names no Unix socket")?;
            let down = match crate::mux::rex::peer_within(socket, Duration::from_millis(20)) {
                Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ECONNREFUSED)) => {
                    true
                }
                Err(e) => return Err(e.to_string()),
                Ok(_) => false,
            };
            if !down {
                let report = crate::mux::rex::configuration(
                    &endpoint,
                    "server.config_reload",
                    Some(&record),
                    deadline,
                )?;
                if report["confirmed"] != true {
                    return Err(format!("The module is not confirmed: {report}"));
                }
            }
        }
        seal(home, state, old)
    })();
    result.unwrap_or_else(|error| json!({"status":"not_confirmed","error":error,"sealed":false}))
}

/// Marks `old`'s entry sealed under the lease, reading `rex.json` again. An
/// entry or a file removed meanwhile stays removed, and is reported so.
fn seal(home: &Path, state: &Path, old: &Path) -> Result<Value, String> {
    let lease = installed_runtime::package_guard(Some(home), state, Writer::Handoff)?;
    let Some(mut record) = registration(home)? else {
        return Ok(json!({"status":"removed"}));
    };
    let mut sealed = false;
    for entry in record["predecessors"].as_array_mut().into_iter().flatten() {
        if entry["state"]
            .as_str()
            .is_some_and(|s| same(Path::new(s), old))
        {
            entry["sealed"] = json!(true);
            sealed = true;
        }
    }
    if !sealed {
        return Ok(json!({"status":"removed"}));
    }
    save(&lease, home, &record)?;
    Ok(json!({"status":"confirmed","sealed":true}))
}

/// One warning for each retired predecessor with an agent that still runs
/// and has not run this state's hooks. Every SessionStart and
/// UserPromptSubmit hook binds its agent in its own state with no activation
/// needed (`mux::home::context_hook`), so a binding of that incarnation here
/// proves the agent runs the new hooks (the U13 review's F1).
pub(crate) fn predecessor_warnings(home: &Path, state: &Path) -> Vec<String> {
    let Ok(Some(record)) = registration(home) else {
        return Vec::new();
    };
    let local = ThisMachine;
    let (bound, _) = bindings(state);
    predecessors(&record).into_iter().filter_map(|entry| {
        if !installed_runtime::retirement(&entry.state).ok().flatten().is_some_and(|marker| marker.hands_to(state)) { return None; }
        let agents: Vec<_> = entry.agents.into_iter().filter(|a| {
            a.agent.split_once('@').and_then(|(pid,_)| pid.parse::<u32>().ok()).is_some_and(|pid| matches!(local.read(pid), LocalRead::Alive(p) if p.incarnation.0 == a.agent))
                && !bound.iter().any(|b| b.agent == a.agent)
        }).collect();
        if agents.is_empty() { return None; }
        let count = if agents.len() == 1 { "1 agent on Rex has".to_owned() } else { format!("{} agents on Rex have",agents.len()) };
        let blocks = agents.iter().map(|a| format!("{} on {}",a.block,a.endpoint)).collect::<Vec<_>>().join("\n");
        Some(format!("Namedrop on Rex moved to Herdr's install in {}. {count} not run its hooks since the move from {}, and the names made there were not carried over. In each Rex session, run Activate Namedrop again and name the panes again. An agent runs the new hooks at its next turn. Restart one that shows Hook failed or does not answer a mention. This warning clears as each of them takes a turn or ends.\n{blocks}",state.display(),entry.state.display()))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("nd-u7-confirm-{}-{tag}", std::process::id()));
        let home = root.join("home");
        let state = root.join("herdr");
        fs::create_dir_all(home.join(".config/namedrop")).unwrap();
        fs::create_dir_all(&state).unwrap();
        (root, home, state)
    }
    fn record(home: &Path, state: &Path, line: &str, server: Value) {
        fs::write(crate::locator::rex_registration(home),json!({"server":server,"line":line,"module":"/cfg/namedrop.lua","launcher":"/herdr/runtime/namedrop","state":state,"predecessors":[{"state":"/old","sealed":false,"agents":[]}]}).to_string()).unwrap();
    }
    #[test]
    fn confirmation_seals_without_a_module_or_server_and_never_recreates_a_removed_record() {
        let (root, home, state) = fixture("offline");
        // A server listens on the recorded socket, so a call would connect.
        let socket = root.join("listening.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listening = json!(format!("rex:{}", socket.display()));
        for (line, server) in [
            ("printed", listening.clone()),
            ("removed", listening),
            ("inserted", Value::Null),
            (
                "inserted",
                json!(format!("rex:{}", root.join("missing.sock").display())),
            ),
        ] {
            record(&home, &state, line, server);
            let result = confirm(&home, &state, Path::new("/old"));
            assert_eq!(result["sealed"], true, "{line}: {result}");
            let saved = registration(&home).unwrap().unwrap();
            assert_eq!(saved["predecessors"][0]["sealed"], true);
            assert_eq!(
                listener.accept().map(|_| ()).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "{line}: step 6 connected to the server"
            );
        }
        fs::remove_file(crate::locator::rex_registration(&home)).unwrap();
        assert_eq!(
            confirm(&home, &state, Path::new("/old"))["status"],
            "removed"
        );
        assert!(registration(&home).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_seal_never_reports_sealed_for_a_record_or_entry_removed_meanwhile() {
        let (root, home, state) = fixture("vanished");
        assert_eq!(
            seal(&home, &state, Path::new("/old")).unwrap(),
            json!({"status":"removed"})
        );
        assert!(registration(&home).unwrap().is_none());
        let path = crate::locator::rex_registration(&home);
        fs::write(
            &path,
            json!({"predecessors":[{"state":"/other","sealed":false}]}).to_string(),
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(
            seal(&home, &state, Path::new("/old")).unwrap(),
            json!({"status":"removed"})
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn confirmation_caps_a_hung_server_at_five_seconds_even_with_a_sixty_second_request_limit() {
        if std::env::var_os("NAMEDROP_TEST_CONFIRM_CHILD").is_none() {
            let result=std::process::Command::new(std::env::current_exe().unwrap()).env_clear().env("NAMEDROP_TEST_CONFIRM_CHILD","1").env("NAMEDROP_REX_TIMEOUT_MS","60000")
                .args(["--exact","rex_ui::handoff::tests::confirmation_caps_a_hung_server_at_five_seconds_even_with_a_sixty_second_request_limit","--nocapture"]).output().unwrap();
            assert!(
                result.status.success(),
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let (root, home, state) = fixture("hung");
        let socket = root.join("r.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let server = thread::spawn(move || {
            let mut connections = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok((s, _)) = listener.accept() {
                    connections.push(s);
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        record(
            &home,
            &state,
            "inserted",
            json!(format!("rex:{}", socket.display())),
        );
        let started = Instant::now();
        let result = confirm(&home, &state, Path::new("/old"));
        let elapsed = started.elapsed();
        done.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert!(elapsed < Duration::from_secs(6), "{elapsed:?}");
        assert_eq!(result["status"], "not_confirmed");
        assert_eq!(
            registration(&home).unwrap().unwrap()["predecessors"][0]["sealed"],
            false
        );
        fs::remove_dir_all(root).unwrap();
    }
}
