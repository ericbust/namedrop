use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub struct AgentProcess(Child);
impl AgentProcess {
    pub fn new() -> Self {
        Self(Command::new("/bin/sleep").arg0("codex").arg("600").spawn().unwrap())
    }
    pub fn pid(&self) -> u32 { self.0.id() }
}
impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn in_agent_tree(test: &str) -> bool {
    if std::env::var("NAMEDROP_HC_AGENT_TEST").as_deref() == Ok(test) { return false; }
    let output = Command::new("/bin/bash")
        .arg0("codex")
        .args(["-c", "\"$1\" --exact \"$2\" --nocapture; exit $?", "agent"])
        .arg(std::env::current_exe().unwrap()).arg(test)
        .env("NAMEDROP_HC_AGENT_TEST", test).output().unwrap();
    assert!(output.status.success(), "{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    true
}
