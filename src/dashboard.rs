pub use crate::store::dashboard as rows;
mod channel_delivery;
mod channel_service;
mod http;
mod hub;
mod server_files;
pub mod snapshot;
mod thread_controls;

use crate::{Clock, HerdrCommandExecutor, IdGenerator, LifecycleRuntime, StateNamespace};
use serde_json::{Value, json};
use server_files::{ServerFiles, ServerInfo, Token, UiSource};
use snapshot::SpaceKey;
use std::{
    fs::File,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::Path,
    process::{Command, ExitCode, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const DEFAULT_IDLE: Duration = Duration::from_secs(10 * 60);

pub fn open(
    runtime: &LifecycleRuntime<'_, impl HerdrCommandExecutor, impl Clock, impl IdGenerator>,
    open_browser: bool,
    ui_directory: Option<&Path>,
) -> Result<Value, String> {
    let ui = UiSource::from_directory(ui_directory)?;
    let workspace_id = match runtime.workspace_id {
        Some(id) => id.to_owned(),
        None => crate::discover_worktree(runtime.herdr, runtime.cwd)?.workspace_id,
    };
    let key = SpaceKey::of(&StateNamespace::for_space(
        runtime.session_socket_id,
        &workspace_id,
    ));
    let files = ServerFiles::at(runtime.state_directory)?;
    if let Some(label) = workspace_label(runtime.herdr, &workspace_id)
        && let Err(error) = files.record_label(key.as_str(), &label)
    {
        eprintln!("Could not save the workspace label: {error}");
    }
    let (server, info) = ensure(&files, &files.token()?, &ui)?;
    let url = format!(
        "http://127.0.0.1:{}/w/{}?n={}",
        info.port,
        key.as_str(),
        files.mint_entry()?
    );
    let opened = open_browser && launch_browser(&url);
    Ok(json!({
        "url": url,
        "space_key": key.as_str(),
        "workspace_id": workspace_id,
        "server": server,
        "opened": opened,
    }))
}

/// Idempotent: converges on the one live server whether it was already
/// running, is starting, or two callers race to start it.
fn ensure(
    files: &ServerFiles,
    token: &Token,
    ui: &UiSource,
) -> Result<(&'static str, ServerInfo), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut spawned = false;
    let mut waiting_for_matching_handoff = false;
    loop {
        let handoff_reader = files.handoff_reader()?;
        if handoff_reader.is_none() {
            let active = files.info();
            if let Some(info) = &active {
                if &info.ui == ui {
                    waiting_for_matching_handoff = true;
                } else if !waiting_for_matching_handoff || Instant::now() >= deadline {
                    return Err(format!(
                        "the dashboard server is running with {} and is restarting; retry when it finishes before requesting {}",
                        info.ui.setting(),
                        ui.setting()
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Err("the dashboard server is restarting; retry in a moment".to_owned());
            }
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        waiting_for_matching_handoff = false;
        if files.claimed()? {
            if let Some(info) = files.info()
                && let Some(phase) = answers(&info, token)
            {
                if &info.ui != ui {
                    if phase == ServerPhase::Restarting {
                        return Err(format!(
                            "the dashboard server is running with {} and is restarting; retry when it finishes before requesting {}",
                            info.ui.setting(),
                            ui.setting()
                        ));
                    }
                    return Err(format!(
                        "the dashboard server is running with {}; stop it with `kill {}` before requesting {}",
                        info.ui.setting(),
                        info.pid,
                        ui.setting()
                    ));
                }
                return Ok((if spawned { "started" } else { "running" }, info));
            }
        } else if !spawned {
            files.spawn(ui)?;
            spawned = true;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the dashboard server did not start; see {}",
                files.state().join("dashboard/server.log").display()
            ));
        }
        drop(handoff_reader);
        thread::sleep(Duration::from_millis(50));
    }
}

#[derive(PartialEq, Eq)]
enum ServerPhase {
    Running,
    Restarting,
}

/// Liveness is the claim plus this instance answering on its port; a stale
/// `server.json` from a crashed server never matches.
fn answers(info: &ServerInfo, token: &Token) -> Option<ServerPhase> {
    let probe = || -> std::io::Result<String> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, info.port))?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        write!(
            stream,
            "GET /healthz?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            token.as_str(),
            info.port
        )?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response)
    };
    let response = probe().ok()?;
    if !response.starts_with("HTTP/1.1 200") {
        return None;
    }
    let (_, body) = response.split_once("\r\n\r\n")?;
    let health: Value = serde_json::from_str(body).ok()?;
    if health["instance"].as_str() != Some(&info.instance) {
        return None;
    }
    Some(if health["restarting"] == true {
        ServerPhase::Restarting
    } else {
        ServerPhase::Running
    })
}

fn workspace_label(herdr: &impl HerdrCommandExecutor, workspace_id: &str) -> Option<String> {
    let output = herdr.execute(&["workspace", "get", workspace_id]).ok()?;
    let value: Value = serde_json::from_str(&output.stdout).ok()?;
    value["result"]["workspace"]["label"]
        .as_str()
        .filter(|label| !label.trim().is_empty())
        .map(str::to_owned)
}

fn launch_browser(url: &str) -> bool {
    use std::os::unix::process::CommandExt;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    {
        "xdg-open"
    } else {
        return false;
    };
    Command::new(opener)
        .arg(url)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

pub fn serve(
    state: &Path,
    command_gate: Option<File>,
    ui_directory: Option<&Path>,
    inherit_claim: bool,
) -> ExitCode {
    match UiSource::from_directory(ui_directory)
        .and_then(|ui| run_server(state, command_gate, ui, inherit_claim))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dashboard: {error}");
            ExitCode::from(69)
        }
    }
}

fn run_server(
    state: &Path,
    command_gate: Option<File>,
    ui: UiSource,
    inherit_claim: bool,
) -> Result<(), String> {
    let files = ServerFiles::at(state)?;
    let claim = if inherit_claim {
        files.inherit_claim()?
    } else {
        let Some(claim) = files.claim()? else {
            return Ok(());
        };
        claim
    };
    // Holding the shared command gate for a long-lived process would make
    // every update time out waiting for it to drain.
    drop(command_gate);
    let assets = http::Assets::new(&ui);
    // Before server.json, so whoever sees this server can already enter it.
    let session = files.session()?;
    let listener = bind(files.info().map(|info| info.port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let instance = server_files::random_hex(16)?;
    let mut channel_service = channel_service::ChannelService::start(state)?;
    files.publish(
        &claim,
        &ServerInfo {
            pid: std::process::id(),
            port,
            instance: instance.clone(),
            started_at_ms: hub::now_ms(),
            ui: ui.clone(),
        },
    )?;
    let (hub, commands) = mpsc::channel();
    let mutations = Arc::new(std::sync::Mutex::new(()));
    let restarting = Arc::new(AtomicBool::new(false));
    let front = Arc::new(http::Front {
        port,
        files: ServerFiles::at(state)?,
        token: files.token()?,
        session,
        instance: instance.clone(),
        hub,
        assets,
        restarting: restarting.clone(),
        mutations: mutations.clone(),
        channel_wake: channel_service.wake(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let accept_stop = stop.clone();
    let acceptor = thread::spawn(move || http::accept_loop(listener, front, accept_stop));
    eprintln!("dashboard: serving 127.0.0.1:{port}");
    let verdict = hub::run(&files, &instance, idle(), commands);
    eprintln!("dashboard: stopping: {}", verdict.reason());
    restarting.store(true, Ordering::Release);
    {
        let _admitted = mutations
            .lock()
            .map_err(|_| "dashboard mutation lock poisoned".to_owned())?;
    }
    channel_service.shutdown()?;
    match verdict {
        hub::Verdict::Handoff(_) => {
            restarting.store(true, Ordering::Release);
            files.handoff(&ui, claim, || {
                stop.store(true, Ordering::Release);
                TcpStream::connect_timeout(
                    &(Ipv4Addr::LOCALHOST, port).into(),
                    Duration::from_secs(1),
                )
                .map_err(|e| format!("could not retire dashboard listener: {e}"))?;
                acceptor
                    .join()
                    .map_err(|_| "dashboard listener panicked".to_owned())
            })?;
        }
        // Still holding the claim, so no successor reads the old session.
        hub::Verdict::Idle => files.rotate_session()?,
        hub::Verdict::Exit(_) => {}
    }
    Ok(())
}

/// Reuses the last port so open tabs and bookmarks survive a restart. A
/// predecessor may still hold it for a moment during a handoff.
fn bind(preferred: Option<u16>) -> Result<TcpListener, String> {
    if let Some(port) = preferred {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Ok(listener) => return Ok(listener),
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
    }
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("could not listen: {e}"))
}

fn idle() -> Duration {
    std::env::var("NAMEDROP_DASHBOARD_IDLE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map_or(DEFAULT_IDLE, Duration::from_millis)
}
