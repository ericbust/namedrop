//! `namedrop dashboard`: a loopback page that shows one Herdr space's agents,
//! mailboxes and conversation as it happens.
pub use crate::store::dashboard as rows;
mod http;
mod hub;
mod server_files;
pub mod snapshot;

use server_files::{ServerFiles, ServerInfo};
use std::{
    fs::File,
    net::{Ipv4Addr, TcpListener},
    path::Path,
    process::ExitCode,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DEFAULT_IDLE: Duration = Duration::from_secs(10 * 60);

/// Runs the singleton server for `state` until it idles out or hands off to
/// a newer release. A second server for the same state exits at once.
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
    let Some(claim) = files.claim(Duration::from_secs(5))? else {
        return Ok(());
    };
    // Holding the shared command gate for a long-lived process would make
    // every update time out waiting for it to drain.
    drop(command_gate);
    let listener = bind(files.info().map(|info| info.port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let instance = server_files::random_hex(16)?;
    files.publish(
        &claim,
        &ServerInfo {
            pid: std::process::id(),
            port,
            instance: instance.clone(),
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        },
    )?;
    let (hub, commands) = mpsc::channel();
    let front = Arc::new(http::Front {
        port,
        token: files.token()?,
        instance: instance.clone(),
        hub,
    });
    thread::spawn(move || http::accept_loop(listener, front));
    eprintln!("dashboard: serving 127.0.0.1:{port}");
    let verdict = hub::run(&files, &instance, idle(), commands);
    eprintln!("dashboard: stopping: {}", verdict.reason());
    if let hub::Verdict::Handoff(_) = verdict {
        drop(claim);
        files.spawn()?;
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
