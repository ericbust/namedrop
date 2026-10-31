use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub struct AgentProcess(Child);
impl AgentProcess {
    pub fn new() -> Self {
        Self::with_program("codex")
    }
    pub fn with_program(program: &str) -> Self {
        let agent = Self(
            Command::new("/bin/sleep")
                .arg0(program)
                .arg("600")
                .spawn()
                .unwrap(),
        );
        #[cfg(target_os = "linux")]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !std::fs::read(format!("/proc/{}/cmdline", agent.pid()))
                .is_ok_and(|args| args.starts_with(format!("{program}\0").as_bytes()))
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "fake agent did not exec"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        agent
    }
    pub fn pid(&self) -> u32 {
        self.0.id()
    }
}
impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn in_agent_tree(test: &str) -> bool {
    in_process_tree(test, "codex")
}

pub fn in_process_tree(test: &str, program: &str) -> bool {
    if std::env::var("NAMEDROP_HC_AGENT_TEST").as_deref() == Ok(test) {
        return false;
    }
    let output = Command::new("/bin/bash")
        .arg0(program)
        .args(["-c", "\"$1\" --exact \"$2\" --nocapture; exit $?", "agent"])
        .arg(std::env::current_exe().unwrap())
        .arg(test)
        .env("NAMEDROP_HC_AGENT_TEST", test)
        .env("NAMEDROP_HC_TERMINAL_PID", std::process::id().to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
