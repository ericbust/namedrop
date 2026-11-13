//! The only code that decides whether a Rex server runs on this machine.

use super::control::{Connection, Fault, Timed};
use crate::mux::{Endpoint, REX_SCHEME};
use serde_json::Value;
use std::{io, os::unix::net::UnixStream, path::Path};

pub(super) struct LocalServer {
    /// `rex:` and the server's primary Unix socket, however it was dialed.
    pub endpoint: Endpoint,
    pub pid: u32,
    pub host_id: Option<String>,
}

/// Other systems name the peer by calls this code does not make, so no
/// server is proved local there.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn peer_pid(_stream: &UnixStream) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this system's Unix sockets name no peer pid that Namedrop reads",
    ))
}

/// The process at the other end of `stream`. A forwarder's peer is the
/// forwarder, not the server behind it. On macOS that is the last process
/// to use the server's end, and on Linux the process that called `listen`,
/// which for `rex server run` are both the server.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "macos")]
    let (level, option, mut pid) = (libc::SOL_LOCAL, libc::LOCAL_PEERPID, 0 as libc::pid_t);
    #[cfg(target_os = "linux")]
    let (level, option, mut pid) = (
        libc::SOL_SOCKET,
        libc::SO_PEERCRED,
        libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        },
    );
    let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
    // SAFETY: `pid` is a writable value of `length` bytes, the type this
    // option fills, and the descriptor stays open while `stream` is borrowed.
    let read = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            level,
            option,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    };
    if read != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    let pid = pid.pid;
    u32::try_from(pid).map_err(|_| io::Error::other(format!("the peer pid {pid} is not a pid")))
}

/// Proves that the connection to `dial`, whose peer is process `peer`, is
/// served by a Rex server on this machine: the server's own pid is the peer,
/// and its primary listener is a Unix socket, whose address names the server.
/// Matching `dial` to that listener by device and inode is deferred, because
/// the peer pid already refuses every forwarder measured.
pub(super) fn prove_local<S: Timed>(
    dial: &Path,
    control: &mut Connection<'_, S>,
    peer: u32,
) -> Result<LocalServer, Fault> {
    let status = control.request("server.status", None)?.payload;
    let refuse = |why: String| {
        Fault::NotLocal(format!(
            "{} is not a Rex server on this machine: {why}",
            dial.display()
        ))
    };
    let pid = status["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| refuse(format!("its status names no pid: {status}")))?;
    if pid != peer {
        return Err(refuse(format!(
            "the server is process {pid}, and the connection's peer is process {peer}"
        )));
    }
    let primary = status["listeners"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|listener| listener["primary"] == true);
    let address = primary
        .filter(|listener| listener["transport"] == "unix")
        .and_then(|listener| listener["addresses"][0].as_str())
        .filter(|address| Path::new(address).is_absolute())
        .ok_or_else(|| {
            refuse(format!(
                "its primary listener is not a Unix socket: {}",
                primary.unwrap_or(&Value::Null)
            ))
        })?;
    Ok(LocalServer {
        endpoint: Endpoint::parse(&format!("{REX_SCHEME}{address}")),
        pid,
        host_id: status["host_id"].as_str().map(str::to_owned),
    })
}

#[cfg(test)]
pub(super) mod tests;
