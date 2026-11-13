//! The `herdr` process, the adapter's only way to run Herdr. Each command is a
//! child in its own process group, with its output captured in private files
//! so the size cap holds while it runs.

use super::transport::{CANCELLED, Transport};
use crate::{
    mux::{Budget, narrowest},
    runtime::output_bounded,
};
use std::{
    env,
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const DEFAULT_HERDR_TIMEOUT_MS: u64 = 10_000;
const MAX_HERDR_TIMEOUT_MS: u64 = 60_000;

/// A connection's transport: each command runs within the connection's
/// budget and stops once its flag is set.
pub(super) struct Process {
    binary: String,
    /// `NAMEDROP_HERDR_TIMEOUT_MS`, which caps every command's timeout.
    timeout: Duration,
    /// `HERDR_SOCKET_PATH` for each command, or `None` to inherit it.
    socket: Option<String>,
    budget: Budget,
    cancelled: Arc<AtomicBool>,
}

impl Process {
    /// Reads `HERDR_BIN_PATH` and `NAMEDROP_HERDR_TIMEOUT_MS` now, once per
    /// connection.
    pub(super) fn new(socket: Option<&str>, budget: Budget, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            binary: env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_owned()),
            timeout: Duration::from_millis(
                env::var("NAMEDROP_HERDR_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|milliseconds| *milliseconds > 0)
                    .unwrap_or(DEFAULT_HERDR_TIMEOUT_MS)
                    .min(MAX_HERDR_TIMEOUT_MS),
            ),
            socket: socket.map(str::to_owned),
            budget,
            cancelled,
        }
    }
}

impl Transport for Process {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.run_within(args, self.budget, None)
    }

    fn run_within(
        &self,
        args: &[&str],
        budget: Budget,
        cancelled: Option<&AtomicBool>,
    ) -> Result<String, String> {
        let budget = self.budget.tighter(budget);
        let cancelled = narrowest(cancelled, Some(&self.cancelled))
            .map_err(|_| CANCELLED.to_owned())?
            .unwrap_or(&self.cancelled);
        let mut command = Command::new(&self.binary);
        command.args(args);
        if let Some(socket) = &self.socket {
            command.env("HERDR_SOCKET_PATH", socket);
        }
        let output = output_bounded(
            command,
            budget.timeout.min(self.timeout),
            cancelled,
            budget.max_output_bytes,
        )?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        String::from_utf8(output.stdout).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::{
        Allowance, Endpoint, MuxError, MuxKind, MuxRead, PaneId, ScreenRequest, Style, TabId, Wrap,
        herdr::HerdrMux,
    };
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
        thread,
        time::Instant,
    };

    /// Logs each command with the socket it was given, then sleeps when the
    /// mode file says `sleep`, prints that many bytes when it holds a number,
    /// and otherwise answers a tab label.
    const HERDR: &str = r#"#!/bin/sh
printf 'socket<%s> %s\n' "${HERDR_SOCKET_PATH-unset}" "$*" >> "$0.calls"
mode=$(cat "$0.mode" 2>/dev/null)
case "$mode" in
sleep) exec sleep 5 ;;
'') printf '%s' '{"result":{"label":"build"}}' ;;
*) head -c "$mode" /dev/zero | tr '\0' x ;;
esac
"#;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A script `herdr` in a directory of its own, removed on drop.
    struct Herdr(PathBuf);

    impl Herdr {
        fn new() -> Self {
            let directory = env::temp_dir().join(format!(
                "namedrop-process-transport-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let binary = directory.join("herdr");
            crate::executable::write_executable(&binary, HERDR);
            Self(binary)
        }

        /// A connection to this `herdr`, as `herdr::connect` makes one.
        fn connect(
            &self,
            socket: Option<&str>,
            budget: Budget,
            cancelled: &Arc<AtomicBool>,
        ) -> HerdrMux<Process> {
            HerdrMux {
                transport: Process {
                    binary: self.0.display().to_string(),
                    ..Process::new(socket, budget, Arc::clone(cancelled))
                },
                endpoint: Endpoint {
                    kind: MuxKind::Herdr,
                    key: socket.unwrap_or_default().to_owned(),
                },
            }
        }

        fn mode(&self, mode: &str) {
            fs::write(self.0.with_extension("mode"), mode).unwrap();
        }

        fn calls(&self) -> Vec<String> {
            fs::read_to_string(self.0.with_extension("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    impl Drop for Herdr {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    fn budget(timeout_ms: u64, max_output_bytes: usize) -> Budget {
        Budget {
            timeout: Duration::from_millis(timeout_ms),
            max_output_bytes,
        }
    }

    fn timed_out(ms: u64) -> Result<Option<String>, MuxError> {
        Err(MuxError::Timeout(format!(
            "Herdr command timed out after {ms} ms"
        )))
    }

    fn cancelled() -> Result<Option<String>, MuxError> {
        Err(MuxError::Unavailable("Herdr read cancelled".to_owned()))
    }

    /// The length of the screen text a read returns.
    fn screen(mux: &dyn MuxRead) -> Result<usize, MuxError> {
        let request = ScreenRequest {
            lines: 1,
            wrap: Wrap::AsDrawn,
            style: Style::Plain,
        };
        mux.screen(&PaneId::new("p1"), request)
            .map(|screen| screen.text.len())
    }

    /// Sets `flag` once `delay` has passed, on a thread of its own.
    fn set_after(flag: &Arc<AtomicBool>, delay: Duration) -> thread::JoinHandle<()> {
        let flag = Arc::clone(flag);
        thread::spawn(move || {
            thread::sleep(delay);
            flag.store(true, Ordering::SeqCst);
        })
    }

    #[test]
    fn bounded_reads_enforce_output_deadline_and_cancellation() {
        let shell = Process {
            binary: "/bin/sh".to_owned(),
            timeout: Duration::from_secs(2),
            ..Process::new(None, Budget::ADAPTER_DEFAULT, Arc::default())
        };
        let cancelled = AtomicBool::new(false);
        let limited = |timeout_ms, max_output_bytes| budget(timeout_ms, max_output_bytes);
        assert_eq!(
            shell
                .run_within(&["-c", "printf ok"], limited(80, 2), Some(&cancelled))
                .unwrap(),
            "ok"
        );
        assert!(
            shell
                .run_within(&["-c", "printf too-long"], limited(80, 2), Some(&cancelled))
                .unwrap_err()
                .contains("output limit")
        );
        let start = Instant::now();
        assert!(
            shell
                .run_within(&["-c", "sleep 10"], limited(80, 100), Some(&cancelled))
                .unwrap_err()
                .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(30));
                cancelled.store(true, Ordering::Relaxed);
            });
            assert!(
                shell
                    .run_within(&["-c", "sleep 10"], limited(1000, 100), Some(&cancelled))
                    .unwrap_err()
                    .contains("cancelled")
            );
        });
    }

    #[test]
    fn a_connection_gives_herdr_its_socket_or_leaves_the_inherited_one() {
        let herdr = Herdr::new();
        let flag = Arc::default();
        let tab = TabId::new("tab1");
        let named = herdr.connect(Some("/run/a.sock"), Budget::ADAPTER_DEFAULT, &flag);
        assert_eq!(named.tab_label(&tab).unwrap().as_deref(), Some("build"));
        let inherited = herdr.connect(None, Budget::ADAPTER_DEFAULT, &flag);
        assert_eq!(inherited.tab_label(&tab).unwrap().as_deref(), Some("build"));
        let socket = env::var("HERDR_SOCKET_PATH").unwrap_or_else(|_| "unset".to_owned());
        assert_eq!(
            herdr.calls(),
            [
                "socket</run/a.sock> tab get tab1".to_owned(),
                format!("socket<{socket}> tab get tab1"),
            ]
        );
    }

    /// The agent status panel and its identity recheck share one deadline,
    /// so each command must get what is left of it, and a budget that has
    /// run out is the read's error with no command run.
    #[test]
    fn each_command_within_a_shrinking_budget_gets_what_is_left_of_it() {
        let herdr = Herdr::new();
        herdr.mode("sleep");
        let flag = Arc::default();
        let mux = herdr.connect(Some("socket-a"), Budget::ADAPTER_DEFAULT, &flag);
        let caller = AtomicBool::new(false);
        let left = Mutex::new(vec![300, 450]);
        let reads = mux.within(Allowance::each(
            || {
                let timeout = left.lock().unwrap().pop().ok_or("the budget ran out")?;
                Ok(budget(timeout, 4096))
            },
            &caller,
        ));
        let tab = TabId::new("tab1");
        assert_eq!(reads.tab_label(&tab), timed_out(450));
        assert_eq!(reads.tab_label(&tab), timed_out(300));
        let ran = herdr.calls().len();
        assert_eq!(
            reads.tab_label(&tab),
            Err(MuxError::Unavailable("the budget ran out".to_owned()))
        );
        assert_eq!(herdr.calls().len(), ran, "no command ran");
    }

    #[test]
    fn a_command_keeps_the_tighter_of_each_limit() {
        let herdr = Herdr::new();
        let flag = Arc::default();
        let too_large = Err(MuxError::TooLarge(
            "Herdr read exceeded its output limit".to_owned(),
        ));
        let capped = herdr.connect(Some("socket-a"), budget(5000, 4096), &flag);
        let wider = capped.within(Allowance::fixed(budget(5000, 1 << 20)));
        let narrower = capped.within(Allowance::fixed(budget(5000, 100)));
        herdr.mode("4096");
        assert_eq!(screen(&*wider), Ok(4096));
        assert_eq!(screen(&*narrower), too_large);
        herdr.mode("4097");
        assert_eq!(screen(&*wider), too_large);

        herdr.mode("sleep");
        let slow = herdr.connect(Some("socket-a"), budget(400, usize::MAX), &flag);
        let tab = TabId::new("tab1");
        let wider = slow.within(Allowance::fixed(budget(2000, usize::MAX)));
        let narrower = slow.within(Allowance::fixed(budget(250, usize::MAX)));
        assert_eq!(wider.tab_label(&tab), timed_out(400));
        assert_eq!(narrower.tab_label(&tab), timed_out(250));
        assert_eq!(slow.tab_label(&tab), timed_out(400));
    }

    /// A command watches one flag: the caller's when a read names one, the
    /// connection's otherwise. The connection's flag, once set, still refuses
    /// a command before it starts.
    #[test]
    fn a_command_watches_the_narrowest_flag_and_a_set_wider_one_refuses_it() {
        let herdr = Herdr::new();
        herdr.mode("sleep");
        let connection = Arc::new(AtomicBool::new(false));
        let mux = herdr.connect(Some("socket-a"), budget(5000, 4096), &connection);
        let caller = Arc::new(AtomicBool::new(false));
        let reads = mux.within(Allowance::each(|| Ok(budget(5000, 4096)), &caller));
        let tab = TabId::new("tab1");

        let started = Instant::now();
        let setter = set_after(&caller, Duration::from_millis(50));
        assert_eq!(reads.tab_label(&tab), cancelled());
        setter.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));

        caller.store(false, Ordering::SeqCst);
        let started = Instant::now();
        let setter = set_after(&connection, Duration::from_millis(50));
        assert_eq!(mux.tab_label(&tab), cancelled());
        setter.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));

        let ran = herdr.calls().len();
        assert_eq!(reads.tab_label(&tab), cancelled());
        assert_eq!(herdr.calls().len(), ran, "a refused command never started");
    }
}
