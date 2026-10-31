#[path = "socket_fixture.rs"]
pub mod fixture;

use std::{
    io,
    os::unix::net::UnixListener,
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
    pub fn bind(path: &Path) -> io::Result<Self> {
        fixture::assert_socket_path(path);
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => drop(stream),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("fake hc listener failed: {error}"),
                }
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
