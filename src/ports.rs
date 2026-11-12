#[derive(Debug, PartialEq)]
pub struct HerdrCommandOutput {
    pub stdout: String,
}

pub trait HerdrCommandExecutor {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String>;

    /// A bounded read for latency-sensitive UI. Process-backed executors must
    /// enforce the deadline and cancellation while their child is running.
    fn execute_bounded(
        &self,
        args: &[&str],
        _timeout: std::time::Duration,
        cancelled: &std::sync::atomic::AtomicBool,
        max_output_bytes: usize,
    ) -> Result<HerdrCommandOutput, String> {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Herdr read cancelled".to_owned());
        }
        let output = self.execute(args)?;
        if output.stdout.len() > max_output_bytes {
            return Err("Herdr read exceeded its output limit".to_owned());
        }
        Ok(output)
    }

    /// What a failed input command means for its input. Anything an executor
    /// does not recognize may have reached the pane, so it is never resent.
    fn input_failure(&self, error: String) -> crate::mux::InputOutcome {
        crate::mux::InputOutcome::Ambiguous(error)
    }
}

pub trait Clock {
    fn now_unix_ms(&self) -> u64;

    fn wait_ms(&self, duration_ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(duration_ms));
    }
}

pub trait IdGenerator {
    fn next_id(&self, kind: &str) -> String;
}
