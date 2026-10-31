#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;
use namedrop::{cli::RuntimeOptions, runtime::HostKind};
use std::{fs, process::Command};

#[test]
fn runtime_roots_stay_at_the_construction_environment() {
    if std::env::var_os("NAMEDROP_SNAPSHOT_CHILD").is_none() {
        let root = std::env::temp_dir().join(format!("namedrop-snapshot-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime_roots_stay_at_the_construction_environment", "--nocapture"])
            .isolated_home(&root)
            .env_remove("NAMEDROP_STATE_DIR")
            .env("HC_SESSION", "default")
            .env("NAMEDROP_SNAPSHOT_CHILD", "1")
            .output().unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let original = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let options = RuntimeOptions { host: Some(HostKind::Hc), ..Default::default() };
    // This child runs exactly one test and starts no threads.
    unsafe {
        for key in ["HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR"] {
            std::env::set_var(key, original.join("late").join(key));
        }
    }
    assert_eq!(options.state_directory().unwrap(), original.join(".local/state/namedrop/hc/default"));
}
