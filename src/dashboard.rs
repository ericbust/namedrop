pub use crate::store::dashboard as rows;
mod acknowledge;
mod channel_delivery;
mod channel_service;
mod coordination_review;
mod http;
mod hub;
mod observer_guard;
mod reminders;
mod server_files;
mod skills;
pub mod snapshot;
pub(crate) mod tailscale;
mod thread_controls;

pub(crate) use server_files::current_operator_session;

/// Internal child entry point. A private stdin pipe ties an observer to its
/// dashboard's lifetime, including a quiet terminal after dashboard SIGKILL.
#[doc(hidden)]
pub fn run_observer_guard(
    terminal: &str,
    rows: Option<u16>,
    cols: Option<u16>,
) -> std::process::ExitCode {
    observer_guard::run(terminal, rows, cols)
}

use crate::{
    Clock, IdGenerator, LifecycleRuntime, StateNamespace,
    mux::{Connector, Depth, MuxConnector, MuxRead, Scope},
};
use serde_json::{Value, json};
use server_files::{ServerFiles, ServerInfo, Token, UiSource};
use snapshot::{RepoRef, SpaceKey};
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
    runtime: &LifecycleRuntime<'_, impl MuxRead, impl Clock, impl IdGenerator>,
    open_browser: bool,
    ui_directory: Option<&Path>,
    tailscale: Option<bool>,
) -> Result<Value, String> {
    if tailscale == Some(false) {
        return disable_tailscale(runtime.state_directory);
    }
    let files = ServerFiles::at(runtime.state_directory)?;
    let _tailscale_operation = tailscale
        .map(|_| tailscale::operation_lock(&files))
        .transpose()?;
    let ui = UiSource::from_directory(ui_directory)?;
    let workspace_id = match runtime.workspace_id {
        Some(id) => id.to_owned(),
        None => crate::discover_worktree(runtime.mux, runtime.cwd)?.workspace_id,
    };
    let key = SpaceKey::of(&StateNamespace::for_space(
        runtime.session_socket_id,
        &workspace_id,
    ));
    let space = crate::SpaceId::new(&workspace_id);
    if let Some(workspace) = runtime
        .mux
        .survey(Scope::Space(&space), Depth::Spaces)
        .ok()
        .and_then(|survey| survey.spaces.into_iter().next())
    {
        if let Some(label) = workspace.label.filter(|label| !label.trim().is_empty())
            && let Err(error) = files.record_label(key.as_str(), &label)
        {
            eprintln!("Could not save the workspace label: {error}");
        }
        let repo = RepoRef::of_checkout(workspace.checkout.as_ref());
        if let Err(error) = files.record_repo(key.as_str(), repo.as_ref()) {
            eprintln!("Could not save the workspace repository: {error}");
        }
    }
    let (server, info) = ensure(&files, &files.token()?, &ui)?;
    let origin = if tailscale == Some(true) {
        tailscale::enable(&files, info.port)?.origin()
    } else {
        format!("http://127.0.0.1:{}", info.port)
    };
    let url = format!("{origin}/w/{}?n={}", key.as_str(), files.mint_entry()?);
    let opened = open_browser && launch_browser(&url);
    Ok(json!({
        "url": url,
        "space_key": key.as_str(),
        "workspace_id": workspace_id,
        "server": server,
        "opened": opened,
        "tailscale": tailscale == Some(true),
    }))
}

/// Disable tailnet access without workspace discovery or starting a server.
pub fn disable_tailscale(state: &Path) -> Result<Value, String> {
    let files = ServerFiles::at(state)?;
    let _operation = tailscale::operation_lock(&files)?;
    tailscale::disable(&files)?;
    Ok(json!({"tailscale": "off"}))
}

/// Idempotent: converges on the one live server whether it was already
/// running, is starting, or two callers race to start it.
fn ensure(
    files: &ServerFiles,
    token: &Token,
    ui: &UiSource,
) -> Result<(&'static str, ServerInfo), String> {
    let ownership = tailscale::read(files.state())?;
    let pinned_port = ownership
        .as_ref()
        .map(|record| record.handler().backend_port);
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
                if pinned_port.is_some_and(|port| port != info.port) {
                    return Err("Channels server port differs from its owned Tailscale target; turn Tailscale off before restarting it".to_owned());
                }
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
    let ownership = tailscale::read(state)?;
    let pinned = ownership
        .as_ref()
        .map(|record| record.handler().backend_port);
    let listener = bind(
        pinned.or_else(|| files.info().map(|info| info.port)),
        pinned.is_some(),
    )?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let instance = server_files::random_hex(16)?;
    let mux: Arc<dyn MuxConnector> = Arc::new(Connector);
    let mut channel_service = channel_service::ChannelService::start(state, mux.clone())?;
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
    let control_effects = thread_controls::ControlEffects::default();
    let front = Arc::new(http::Front {
        mux: mux.clone(),
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
        control_effects: control_effects.clone(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let accept_stop = stop.clone();
    let acceptor = thread::spawn(move || http::accept_loop(listener, front, accept_stop));
    eprintln!("dashboard: serving 127.0.0.1:{port}");
    let verdict = hub::run(&files, &instance, idle(), commands, mux);
    eprintln!("dashboard: stopping: {}", verdict.reason());
    restarting.store(true, Ordering::Release);
    {
        let _admitted = mutations
            .lock()
            .map_err(|_| "dashboard mutation lock poisoned".to_owned())?;
    }
    control_effects.wait();
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
fn bind(preferred: Option<u16>, pinned: bool) -> Result<TcpListener, String> {
    if let Some(port) = preferred {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Ok(listener) => return Ok(listener),
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                Err(error) if pinned => {
                    return Err(format!(
                        "could not bind the owned Tailscale backend port {port}: {error}; free the port or run `namedrop channels --tailscale off`"
                    ));
                }
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

#[cfg(test)]
mod tailscale_port_tests {
    use super::*;

    #[test]
    fn owned_backend_port_never_falls_back_to_another_listener() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let error = bind(Some(port), true).unwrap_err();
        assert!(
            error.contains(&format!("Tailscale backend port {port}")),
            "{error}"
        );
        let local = bind(Some(port), false).unwrap();
        assert_ne!(local.local_addr().unwrap().port(), port);
        drop(occupied);
        let restored = bind(Some(port), true).unwrap();
        assert_eq!(restored.local_addr().unwrap().port(), port);
    }
}
