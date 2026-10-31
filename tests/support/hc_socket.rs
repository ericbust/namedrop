#[path = "socket_fixture.rs"]
pub mod fixture;

use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct HcSocket {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl HcSocket {
    #[allow(dead_code)]
    pub fn bind(path: &Path) -> io::Result<Self> {
        Self::listen(path, None)
    }

    #[allow(dead_code)]
    pub fn rpc(path: &Path, core_id: &str) -> io::Result<Self> {
        Self::listen(path, Some(core_id.to_owned()))
    }

    fn listen(path: &Path, core_id: Option<String>) -> io::Result<Self> {
        fixture::assert_socket_path(path);
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let root = path.parent().unwrap().to_owned();
        let worker = thread::spawn(move || {
            let mut clients = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if let Some(core_id) = &core_id {
                            let root = root.clone();
                            let core_id = core_id.clone();
                            let stopped = Arc::clone(&stopped);
                            clients.push(thread::spawn(move || {
                                serve(stream, &root, &core_id, &stopped)
                            }));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("fake hc listener failed: {error}"),
                }
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for HcSocket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn serve(mut stream: UnixStream, root: &Path, core_id: &str, stopped: &AtomicBool) {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    while !stopped.load(Ordering::Relaxed) {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return,
        }
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["jsonrpc"], "2.0");
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("rpc.jsonl"))
            .unwrap();
        log.write_all(format!("{request}\n").as_bytes()).unwrap();
        let method = request["method"].as_str().unwrap();
        if method == "events.subscribe" && root.join("eof").exists() {
            return;
        }
        let expected_params = match method {
            "server.status" => serde_json::json!({}),
            "terminal.list" => serde_json::json!({"all": false, "labels": {}}),
            "events.subscribe" => {
                serde_json::json!({"terminal": null, "children_of": null, "labels": {}})
            }
            _ => panic!("unexpected fake core request: {request}"),
        };
        assert_eq!(request["params"], expected_params);
        let result = match method {
            "server.status" => serde_json::json!({"core":{"id":core_id}}),
            "terminal.list" => serde_json::json!({"terminals":[]}),
            "events.subscribe" => serde_json::json!({"channel":null,"seq":0,"terminals":[]}),
            _ => panic!("unexpected fake core request: {request}"),
        };
        if writeln!(
            stream,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result})
        )
        .is_err()
        {
            return;
        }
        if method != "events.subscribe" {
            continue;
        }
        loop {
            if stopped.load(Ordering::Relaxed) {
                return;
            }
            if root.join("disconnect").exists() {
                std::fs::write(root.join("stream-ended"), "").unwrap();
                return;
            }
            let method = if root.join("missed").exists() {
                "missed"
            } else {
                "heartbeat"
            };
            if writeln!(
                stream,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","method":method,"params":{}})
            )
            .is_err()
            {
                return;
            }
            if root.join("repeat").exists() && subscriptions(root) < 6 {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[allow(dead_code)]
pub fn subscriptions(root: &Path) -> usize {
    std::fs::read_to_string(root.join("rpc.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["method"] == "events.subscribe"
        })
        .count()
}
