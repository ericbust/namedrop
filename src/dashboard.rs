pub use crate::store::dashboard as rows;
mod http;
mod hub;
mod server_files;
pub mod snapshot;

use crate::{Clock, HerdrCommandExecutor, IdGenerator, LifecycleRuntime, StateNamespace};
use serde_json::{Value, json};
use server_files::{ServerFiles, ServerInfo, Token};
use snapshot::SpaceKey;
use std::{
    fs::File,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::Path,
    process::{Command, ExitCode, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_IDLE: Duration = Duration::from_secs(10 * 60);

pub fn open(
    runtime: &LifecycleRuntime<'_, impl HerdrCommandExecutor, impl Clock, impl IdGenerator>,
    open_browser: bool,
) -> Result<Value, String> {
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
    let (server, info) = ensure(&files, &files.token()?)?;
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
fn ensure(files: &ServerFiles, token: &Token) -> Result<(&'static str, ServerInfo), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut spawned = false;
    loop {
        if files.claimed()? {
            if let Some(info) = files.info()
                && answers(&info, token)
            {
                return Ok((if spawned { "started" } else { "running" }, info));
            }
        } else if !spawned {
            files.spawn()?;
            spawned = true;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the dashboard server did not start; see {}",
                files.state().join("dashboard/server.log").display()
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Liveness is the claim plus this instance answering on its port; a stale
/// `server.json` from a crashed server never matches.
fn answers(info: &ServerInfo, token: &Token) -> bool {
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
    probe().is_ok_and(|response| {
        response.starts_with("HTTP/1.1 200")
            && response.contains(&format!("\"instance\":\"{}\"", info.instance))
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

pub fn serve(state: &Path, command_gate: Option<File>) -> ExitCode {
    match run_server(state, command_gate) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dashboard: {error}");
            ExitCode::from(69)
        }
    }
}

fn run_server(state: &Path, command_gate: Option<File>) -> Result<(), String> {
    let files = ServerFiles::at(state)?;
    let Some(claim) = files.claim()? else {
        return Ok(());
    };
    // Holding the shared command gate for a long-lived process would make
    // every update time out waiting for it to drain.
    drop(command_gate);
    // Before server.json, so whoever sees this server can already enter it.
    let session = files.session()?;
    let listener = bind(files.info().map(|info| info.port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let instance = server_files::random_hex(16)?;
    files.publish(
        &claim,
        &ServerInfo {
            pid: std::process::id(),
            port,
            instance: instance.clone(),
            started_at_ms: hub::now_ms(),
        },
    )?;
    let (hub, commands) = mpsc::channel();
    let front = Arc::new(http::Front {
        port,
        files: ServerFiles::at(state)?,
        token: files.token()?,
        session,
        instance: instance.clone(),
        hub,
    });
    thread::spawn(move || http::accept_loop(listener, front));
    eprintln!("dashboard: serving 127.0.0.1:{port}");
    let verdict = hub::run(&files, &instance, idle(), commands);
    eprintln!("dashboard: stopping: {}", verdict.reason());
    match verdict {
        hub::Verdict::Handoff(_) => {
            drop(claim);
            files.spawn()?;
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
