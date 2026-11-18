pub trait Clock {
    fn now_unix_ms(&self) -> u64;

    fn wait_ms(&self, duration_ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(duration_ms));
    }
}

pub trait IdGenerator {
    fn next_id(&self, kind: &str) -> String;
}
