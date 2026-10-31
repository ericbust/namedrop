use crate::{HerdrCommandExecutor, HerdrCommandOutput, environment::EnvironmentSnapshot};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const FRAME_LIMIT: usize = 8 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(super) struct SubscriberRpc<'a> {
    endpoint: PathBuf,
    environment: &'a EnvironmentSnapshot,
    state: &'a Path,
    stop: &'a AtomicBool,
    caller: String,
}

struct Budget<'a> {
    deadline: Instant,
    stop: &'a AtomicBool,
    cancelled: &'a AtomicBool,
}

impl Budget<'_> {
    fn check(&self) -> Result<(), String> {
        if self.stop.load(Ordering::Relaxed) || self.cancelled.load(Ordering::Relaxed) {
            return Err("Subscriber RPC cancelled".into());
        }
        if Instant::now() >= self.deadline {
            return Err("Subscriber RPC timed out".into());
        }
        Ok(())
    }

    fn wait(&self, stream: &UnixStream, events: i16) -> Result<(), String> {
        self.check()?;
        let duration = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_INTERVAL);
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut fd, 1, duration.as_millis().max(1) as i32) };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().to_string());
        }
        self.check()
    }
}

pub(super) struct EventStream {
    socket: UnixStream,
    buffer: Vec<u8>,
}

impl EventStream {
    fn connect(endpoint: &Path, budget: &Budget<'_>) -> Result<Self, String> {
        budget.check()?;
        let bytes = endpoint.as_os_str().as_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
            return Err("Invalid subscriber socket path".into());
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
            *target = *byte as libc::c_char;
        }
        let address_len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            address.sun_len = address_len as u8;
        }
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let socket = unsafe { UnixStream::from_raw_fd(fd) };
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        socket
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let result = unsafe {
            libc::connect(
                fd,
                &address as *const _ as *const libc::sockaddr,
                address_len as libc::socklen_t,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error.to_string());
            }
            loop {
                budget.wait(&socket, libc::POLLOUT)?;
                if let Some(error) = socket.take_error().map_err(|e| e.to_string())? {
                    return Err(error.to_string());
                }
                if socket.peer_addr().is_ok() {
                    break;
                }
            }
        }
        Ok(Self {
            socket,
            buffer: Vec::new(),
        })
    }

    fn request(&mut self, method: &str, params: Value, budget: &Budget<'_>) -> Result<(), String> {
        let mut frame =
            serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
                .map_err(|e| e.to_string())?;
        frame.push(b'\n');
        if frame.len() > FRAME_LIMIT {
            return Err("Subscriber RPC request exceeded frame limit".into());
        }
        let mut remaining = frame.as_slice();
        while !remaining.is_empty() {
            budget.check()?;
            match self.socket.write(remaining) {
                Ok(0) => return Err("Subscriber RPC connection closed while writing".into()),
                Ok(n) => remaining = &remaining[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    budget.wait(&self.socket, libc::POLLOUT)?
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    fn line(&mut self, budget: &Budget<'_>, limit: usize) -> Result<Option<String>, String> {
        loop {
            if budget.stop.load(Ordering::Relaxed) || budget.cancelled.load(Ordering::Relaxed) {
                return Err("Subscriber RPC cancelled".into());
            }
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                if end + 1 > limit {
                    return Err("Subscriber RPC exceeded frame limit".into());
                }
                let line = self.buffer.drain(..=end).collect::<Vec<_>>();
                return String::from_utf8(line).map(Some).map_err(|e| e.to_string());
            }
            if self.buffer.len() > limit {
                return Err("Subscriber RPC exceeded frame limit".into());
            }
            let mut bytes = [0u8; 8192];
            match self.socket.read(&mut bytes) {
                Ok(0) => return Err("Subscriber RPC connection closed".into()),
                Ok(n) => self.buffer.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= budget.deadline {
                        return Ok(None);
                    }
                    match budget.wait(&self.socket, libc::POLLIN) {
                        Err(_) if Instant::now() >= budget.deadline => return Ok(None),
                        result => result?,
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => (),
                Err(e) => return Err(e.to_string()),
            }
            if Instant::now() >= budget.deadline {
                return Ok(None);
            }
        }
    }

    pub(super) fn next(
        &mut self,
        deadline: Instant,
        stop: &AtomicBool,
    ) -> Result<Option<String>, String> {
        self.line(
            &Budget {
                deadline,
                stop,
                cancelled: stop,
            },
            FRAME_LIMIT,
        )
    }

    fn reply(&mut self, budget: &Budget<'_>, limit: usize) -> Result<Value, String> {
        budget.check()?;
        let line = self
            .line(budget, limit)?
            .ok_or("Subscriber RPC timed out")?;
        budget.check()?;
        let reply: Value = serde_json::from_str(&line)
            .map_err(|e| format!("Invalid subscriber RPC reply: {e}"))?;
        if reply.get("jsonrpc") != Some(&json!("2.0"))
            || reply.get("id") != Some(&json!(1))
            || reply.get("result").is_some() == reply.get("error").is_some()
        {
            return Err("Invalid subscriber RPC reply envelope".into());
        }
        if let Some(error) = reply.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .ok_or("Invalid subscriber RPC error")?;
            let kind = error["data"]["kind"]
                .as_str()
                .ok_or("Invalid subscriber RPC error kind")?;
            let hint = error["data"]["hint"].as_str().unwrap_or_default();
            return Err(
                json!({"error": {"message":message, "kind":kind, "hint":hint}}).to_string(),
            );
        }
        Ok(reply["result"].clone())
    }
}

impl<'a> SubscriberRpc<'a> {
    pub(super) fn new(
        environment: &'a EnvironmentSnapshot,
        state: &'a Path,
        stop: &'a AtomicBool,
    ) -> Result<Self, String> {
        let endpoint = environment.hc_endpoint()?.to_owned();
        let caller = format!("ppid:{}", std::process::id());
        Ok(Self {
            endpoint,
            environment,
            state,
            stop,
            caller,
        })
    }

    fn command(&self, args: &[&str]) -> Result<(&'static str, Value), String> {
        let terminal_params = |terminal: &str| json!({"terminal":terminal,"caller":self.caller});
        Ok(match args {
            ["server", "status", "--json"] => ("server.status", json!({})),
            ["ls", "--json"] => ("terminal.list", json!({"all":false,"labels":{}})),
            ["get", terminal, "--json"] => ("terminal.get", terminal_params(terminal)),
            ["read", terminal, rest @ ..] => {
                let lines = match rest {
                    ["--json"] => 0,
                    ["--lines", n, "--json"] => n
                        .parse::<usize>()
                        .map_err(|_| "Invalid subscriber read lines")?,
                    _ => return Err("Unsupported subscriber read arguments".into()),
                };
                let mut params = terminal_params(terminal);
                params["lines"] = json!(lines);
                ("terminal.read", params)
            }
            ["label", terminal, labels @ .., "--json"] if !labels.is_empty() => {
                let mut values = serde_json::Map::new();
                for label in labels {
                    let (key, value) = label
                        .split_once('=')
                        .filter(|(key, _)| !key.is_empty())
                        .ok_or("Invalid subscriber label")?;
                    values.insert(key.into(), json!(value));
                }
                (
                    "terminal.label",
                    json!({"terminal":terminal,"labels":values}),
                )
            }
            ["send", terminal, text, rest @ ..] => {
                let condition = match rest {
                    ["--json"] => None,
                    ["--if", "idle", "--json"] => Some("idle"),
                    _ => return Err("Unsupported subscriber send arguments".into()),
                };
                (
                    "terminal.send",
                    json!({"terminal":terminal,"text":text,"force":false,"if":condition,"wait":false,"timeout":null,"caller":self.caller}),
                )
            }
            ["interrupt", terminal, "--timeout", "2", "--json"] => (
                "terminal.interrupt",
                json!({"terminal":terminal,"timeout":2.0}),
            ),
            _ => return Err("Unsupported subscriber command arguments".into()),
        })
    }

    pub(super) fn subscribe(&self, deadline: Instant) -> Result<(EventStream, String), String> {
        let budget = Budget {
            deadline,
            stop: self.stop,
            cancelled: self.stop,
        };
        let mut stream = EventStream::connect(&self.endpoint, &budget)?;
        stream.request(
            "events.subscribe",
            json!({"terminal":null,"children_of":null,"labels":{}}),
            &budget,
        )?;
        let snapshot = stream.reply(&budget, FRAME_LIMIT)?;
        match serde_json::from_value::<super::StreamLine>(snapshot.clone()) {
            Ok(
                super::StreamLine::Snapshot { .. } | super::StreamLine::SnapshotWithoutTerminals(_),
            ) => Ok((stream, snapshot.to_string())),
            _ => Err("Invalid subscriber RPC snapshot".into()),
        }
    }
}

impl HerdrCommandExecutor for SubscriberRpc<'_> {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let timeout = if args.first() == Some(&"send") {
            Duration::from_secs(20)
        } else {
            Duration::from_secs(2)
        };
        self.execute_bounded(args, timeout, self.stop, FRAME_LIMIT)
    }
    fn execute_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        max_output_bytes: usize,
    ) -> Result<HerdrCommandOutput, String> {
        let (method, params) = self.command(args)?;
        let budget = Budget {
            deadline: Instant::now() + timeout.min(Duration::from_secs(60)),
            stop: self.stop,
            cancelled,
        };
        let mut stream = EventStream::connect(&self.endpoint, &budget)?;
        stream.request(method, params, &budget)?;
        let stdout = stream
            .reply(&budget, max_output_bytes.min(FRAME_LIMIT))?
            .to_string();
        if stdout.len() > max_output_bytes {
            return Err("Subscriber RPC exceeded output limit".into());
        }
        Ok(HerdrCommandOutput { stdout })
    }
    fn environment_snapshot(&self) -> Option<&EnvironmentSnapshot> {
        Some(self.environment)
    }
    fn state_directory(&self) -> Option<&Path> {
        Some(self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
    };

    fn fake(reply: &'static str) -> (EnvironmentSnapshot, std::thread::JoinHandle<Value>) {
        let root = crate::runtime::hc_socket_fixture::fixture::short_socket_root();
        let path = root.join("core.sock");
        crate::runtime::hc_socket_fixture::fixture::assert_socket_path(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let environment = EnvironmentSnapshot::from_variables([(
            "HC_SOCKET".into(),
            path.clone().into_os_string(),
        )]);
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
            std::fs::remove_dir_all(root).unwrap();
            serde_json::from_str(&line).unwrap()
        });
        (environment, thread)
    }

    #[test]
    fn commands_preserve_wire_params_and_results() {
        let cases = [
            (
                vec!["server", "status", "--json"],
                "server.status",
                json!({}),
            ),
            (
                vec!["ls", "--json"],
                "terminal.list",
                json!({"all":false,"labels":{}}),
            ),
            (
                vec!["get", "%1", "--json"],
                "terminal.get",
                json!({"terminal":"%1","caller":format!("ppid:{}",std::process::id())}),
            ),
            (
                vec!["read", "%1", "--json"],
                "terminal.read",
                json!({"terminal":"%1","caller":format!("ppid:{}",std::process::id()),"lines":0}),
            ),
            (
                vec!["send", "%1", "hello", "--json"],
                "terminal.send",
                json!({"terminal":"%1","text":"hello","force":false,"if":null,"wait":false,"timeout":null,"caller":format!("ppid:{}",std::process::id())}),
            ),
            (
                vec!["read", "%1", "--lines", "7", "--json"],
                "terminal.read",
                json!({"terminal":"%1","caller":format!("ppid:{}",std::process::id()),"lines":7}),
            ),
            (
                vec!["label", "%1", "x=old", "x=", "a=b=c", "--json"],
                "terminal.label",
                json!({"terminal":"%1","labels":{"x":"","a":"b=c"}}),
            ),
            (
                vec!["send", "%1", "hello", "--if", "idle", "--json"],
                "terminal.send",
                json!({"terminal":"%1","text":"hello","force":false,"if":"idle","wait":false,"timeout":null,"caller":format!("ppid:{}",std::process::id())}),
            ),
            (
                vec!["interrupt", "%1", "--timeout", "2", "--json"],
                "terminal.interrupt",
                json!({"terminal":"%1","timeout":2.0}),
            ),
        ];
        for (args, method, params) in cases {
            let (environment, server) =
                fake("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n");
            let stop = AtomicBool::new(false);
            let rpc = SubscriberRpc::new(&environment, Path::new("/tmp"), &stop).unwrap();
            assert_eq!(rpc.execute(&args).unwrap().stdout, "{\"ok\":true}");
            assert_eq!(
                server.join().unwrap(),
                json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
            );
            assert!(
                rpc.execute(&["--version"])
                    .unwrap_err()
                    .contains("Unsupported")
            );
        }
    }

    #[test]
    fn inherited_terminal_tokens_cannot_change_subscriber_caller() {
        for bound_to_core in [true, false] {
            let (environment, server) = fake("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n");
            let endpoint = environment.hc_endpoint().unwrap();
            let environment = EnvironmentSnapshot::from_variables([
                ("HC_SOCKET".into(), endpoint.as_os_str().to_owned()),
                ("HC_TERMINAL_ID".into(), "%9".into()),
                (
                    "HC_TERMINAL_SOCKET".into(),
                    if bound_to_core {
                        endpoint.as_os_str().to_owned()
                    } else {
                        "/different-core.sock".into()
                    },
                ),
            ]);
            let stop = AtomicBool::new(false);
            let rpc = SubscriberRpc::new(&environment, Path::new("/tmp"), &stop).unwrap();
            rpc.execute(&["get", "%1", "--json"]).unwrap();
            let caller = format!("ppid:{}", std::process::id());
            assert_eq!(server.join().unwrap()["params"]["caller"], caller);
        }
    }

    #[test]
    fn notification_cannot_stand_in_for_initial_snapshot() {
        let (environment, server) = fake(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"method\":\"heartbeat\",\"params\":{}}}\n",
        );
        let stop = AtomicBool::new(false);
        let rpc = SubscriberRpc::new(&environment, Path::new("/tmp"), &stop).unwrap();
        assert!(
            rpc.subscribe(Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        server.join().unwrap();
    }

    #[test]
    fn rpc_refusal_preserves_flat_kind_and_hint() {
        let (environment, server) = fake(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-1,\"message\":\"busy\",\"data\":{\"kind\":\"not_idle\",\"hint\":\"wait\"}}}\n",
        );
        let stop = AtomicBool::new(false);
        let rpc = SubscriberRpc::new(&environment, Path::new("/tmp"), &stop).unwrap();
        let error = rpc.execute(&["send", "%1", "hi", "--json"]).unwrap_err();
        assert_eq!(
            serde_json::from_str::<Value>(&error).unwrap(),
            json!({"error":{"kind":"not_idle","message":"busy","hint":"wait"}})
        );
        server.join().unwrap();
    }

    #[test]
    fn lost_send_reply_is_ambiguous_and_not_retried() {
        let (environment, server) = fake("");
        let stop = AtomicBool::new(false);
        let rpc = SubscriberRpc::new(&environment, Path::new("/tmp"), &stop).unwrap();
        let error = rpc.execute(&["send", "%1", "hi", "--json"]).unwrap_err();
        assert_eq!(
            rpc.classify_prompt_failure(&error),
            crate::PromptFailureKind::AmbiguousInvocation
        );
        assert_eq!(server.join().unwrap()["method"], "terminal.send");
    }

    #[test]
    fn subscription_keeps_notification_buffered_with_snapshot() {
        let (environment, server) = fake(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"terminals\":[],\"seq\":1}}\n{\"method\":\"heartbeat\",\"params\":{\"seq\":2}}\n",
        );
        let stop = AtomicBool::new(false);
        let rpc = SubscriberRpc::new(&environment, Path::new("/tmp"), &stop).unwrap();
        let (mut stream, snapshot) = rpc
            .subscribe(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&snapshot).unwrap()["seq"], 1);
        assert_eq!(
            serde_json::from_str::<Value>(&stream.next(Instant::now(), &stop).unwrap().unwrap())
                .unwrap()["params"]["seq"],
            2
        );
        assert_eq!(
            server.join().unwrap()["params"],
            json!({"terminal":null,"children_of":null,"labels":{}})
        );
    }

    #[test]
    fn partial_frames_survive_polling_and_stop_cancels() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut stream = EventStream {
            socket,
            buffer: Vec::new(),
        };
        let stop = AtomicBool::new(false);
        peer.write_all(b"part").unwrap();
        assert!(
            stream
                .next(Instant::now() + Duration::from_millis(20), &stop)
                .unwrap()
                .is_none()
        );
        peer.write_all(b"ial\n").unwrap();
        assert_eq!(
            stream
                .next(Instant::now() + Duration::from_secs(1), &stop)
                .unwrap()
                .unwrap(),
            "partial\n"
        );
        stop.store(true, Ordering::Relaxed);
        assert!(
            stream
                .next(Instant::now() + Duration::from_secs(1), &stop)
                .unwrap_err()
                .contains("cancelled")
        );
    }

    #[test]
    fn expired_reply_budget_rejects_buffered_success() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut stream = EventStream {
            socket,
            buffer: b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":true}\n".to_vec(),
        };
        let stop = AtomicBool::new(false);
        let budget = Budget {
            deadline: Instant::now(),
            stop: &stop,
            cancelled: &stop,
        };
        assert!(
            stream
                .reply(&budget, FRAME_LIMIT)
                .unwrap_err()
                .contains("timed out")
        );
        assert!(stream.next(Instant::now(), &stop).unwrap().is_some());
    }

    #[test]
    fn caller_cancellation_interrupts_a_stalled_reply() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut stream = EventStream {
            socket,
            buffer: Vec::new(),
        };
        let stop = AtomicBool::new(false);
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(30));
                cancelled.store(true, Ordering::Relaxed);
            });
            let started = Instant::now();
            let budget = Budget {
                deadline: started + Duration::from_secs(2),
                stop: &stop,
                cancelled: &cancelled,
            };
            assert!(
                stream
                    .reply(&budget, FRAME_LIMIT)
                    .unwrap_err()
                    .contains("cancelled")
            );
            assert!(started.elapsed() < Duration::from_millis(500));
        });
    }

    #[test]
    fn stalled_reply_and_frame_limit_are_bounded() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut stream = EventStream {
            socket,
            buffer: Vec::new(),
        };
        let stop = AtomicBool::new(false);
        let budget = Budget {
            deadline: Instant::now() + Duration::from_millis(20),
            stop: &stop,
            cancelled: &stop,
        };
        assert!(stream.reply(&budget, 10).unwrap_err().contains("timed out"));
        peer.write_all(b"01234567890").unwrap();
        let budget = Budget {
            deadline: Instant::now() + Duration::from_secs(1),
            stop: &stop,
            cancelled: &stop,
        };
        assert!(
            stream
                .reply(&budget, 10)
                .unwrap_err()
                .contains("frame limit")
        );
    }
}
