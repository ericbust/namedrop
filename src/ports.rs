#[derive(Debug, PartialEq)]
pub struct HerdrCommandOutput {
    pub stdout: String,
}

/// Why a prompt submission failed, judged by one question only: **could the
/// recipient already have the input?** Dispatch decides whether to requeue,
/// fail or hold from this answer, so a code may only be classified as anything
/// other than [`PromptFailureKind::AmbiguousInvocation`] when the host
/// documents which side of that line it falls on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptFailureKind {
    /// The host refused the submission *before sending any input*, and the
    /// refusal may pass. Nothing reached the recipient, so returning the
    /// delivery to the queue cannot duplicate an invocation.
    TemporaryPreInvocation,
    /// No input was sent; only a newer settled observation permits another attempt.
    DeferredUntilSettle { observed_status_ms: Option<u64> },
    /// The host rejected the call itself — an identical retry cannot succeed.
    DefinitiveNonRetryable,
    /// It cannot be established that the input never arrived. The conservative
    /// default, and the only safe answer for anything unrecognized: such work
    /// is held for a human and is never resent automatically.
    AmbiguousInvocation,
}

pub trait HerdrCommandExecutor {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String>;

    fn environment_snapshot(&self) -> Option<&crate::environment::EnvironmentSnapshot> {
        None
    }

    fn state_directory(&self) -> Option<&std::path::Path> {
        None
    }

    fn open_hc_popup(&self, _args: &[&str]) -> Result<HerdrCommandOutput, String> {
        Err("HC popups are unavailable with this executor".to_owned())
    }

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

    fn classify_prompt_failure(&self, _error: &str) -> PromptFailureKind {
        PromptFailureKind::AmbiguousInvocation
    }
}

impl<T: HerdrCommandExecutor + ?Sized> HerdrCommandExecutor for &T {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        (**self).execute(args)
    }

    fn environment_snapshot(&self) -> Option<&crate::environment::EnvironmentSnapshot> {
        (**self).environment_snapshot()
    }

    fn state_directory(&self) -> Option<&std::path::Path> {
        (**self).state_directory()
    }

    fn open_hc_popup(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        (**self).open_hc_popup(args)
    }

    fn execute_bounded(
        &self,
        args: &[&str],
        timeout: std::time::Duration,
        cancelled: &std::sync::atomic::AtomicBool,
        max_output_bytes: usize,
    ) -> Result<HerdrCommandOutput, String> {
        (**self).execute_bounded(args, timeout, cancelled, max_output_bytes)
    }

    fn classify_prompt_failure(&self, error: &str) -> PromptFailureKind {
        (**self).classify_prompt_failure(error)
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
