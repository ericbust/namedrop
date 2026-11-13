//! The only code that decides whether a Rex server runs on this machine.

use super::control::{Control, Fault};
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

/// The peer pid of one non-blocking connect to `socket`, which sends
/// nothing and closes at once. A listener whose queue is full refuses or
/// would block at once, and a connect that has not completed `within` fails
/// `TimedOut`.
pub(super) fn peer_within(socket: &Path, within: std::time::Duration) -> io::Result<u32> {
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    };
    // SAFETY: an all-zero sockaddr_un is a valid empty address.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let path = socket.as_os_str().as_bytes();
    if path.len() >= address.sun_path.len() || path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} cannot name a Unix socket", socket.display()),
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(path) {
        *slot = *byte as libc::c_char;
    }
    // SAFETY: socket takes no pointers, and a non-negative result is a new
    // descriptor that `OwnedFd` then closes.
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `descriptor` is open and owned by nothing else.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let fd = descriptor.as_raw_fd();
    // SAFETY: fcntl reads and sets flags of a descriptor this function owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: as above.
    if flags < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `address` is a filled sockaddr_un of the length given.
    let connected = unsafe {
        libc::connect(
            fd,
            (&raw const address).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if connected != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let wait = libc::c_int::try_from(within.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: `poll` is one valid pollfd.
        match unsafe { libc::poll(&raw mut poll, 1, wait) } {
            0 => return Err(io::ErrorKind::TimedOut.into()),
            ready if ready < 0 => return Err(io::Error::last_os_error()),
            _ => {}
        }
        let mut pending: libc::c_int = 0;
        let mut length = std::mem::size_of_val(&pending) as libc::socklen_t;
        // SAFETY: SO_ERROR fills one c_int of `length` bytes.
        let read = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&raw mut pending).cast(),
                &raw mut length,
            )
        };
        if read != 0 {
            return Err(io::Error::last_os_error());
        }
        if pending != 0 {
            return Err(io::Error::from_raw_os_error(pending));
        }
    }
    peer_pid(&UnixStream::from(descriptor))
}

/// Proves that the connection to `dial`, whose peer is process `peer`, is
/// served by a Rex server on this machine: the server's own pid is the peer,
/// and its primary listener is a Unix socket, whose address names the server.
/// Matching `dial` to that listener by device and inode is deferred, because
/// the peer pid already refuses every forwarder measured.
pub(super) fn prove_local(
    dial: &Path,
    control: &mut dyn Control,
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
