//! Where setup writes shortcuts. This binary holds a single test because it
//! changes the process environment.
use std::fs;

#[test]
fn xdg_config_home_applies_only_to_this_users_own_home() {
    let root =
        std::env::temp_dir().join(format!("namedrop-shortcut-config-{}", std::process::id()));
    let (home, xdg) = (root.join("home"), root.join("xdg"));
    let default = home.join(".config/herdr/config.toml");
    fs::create_dir_all(&home).unwrap();
    // SAFETY: this is the only test in the binary, so nothing reads the
    // environment concurrently.
    unsafe {
        std::env::remove_var("HERDR_CONFIG_PATH");
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
        std::env::set_var("HOME", root.join("someone-else"));
    }
    // Activating for another home (a test or an isolated profile) never uses
    // this user's XDG directory.
    let report = namedrop::install_shortcuts(None, Some(&home)).unwrap();
    assert_eq!(report["path"], default.to_str().unwrap());
    assert!(!xdg.exists());

    // The process's own home follows XDG_CONFIG_HOME, as Herdr does.
    unsafe { std::env::set_var("HOME", &home) };
    fs::remove_file(&default).unwrap();
    let report = namedrop::install_shortcuts(None, Some(&home)).unwrap();
    assert_eq!(
        report["path"],
        xdg.join("herdr/config.toml").to_str().unwrap()
    );
    assert!(!default.exists());

    // A relative XDG_CONFIG_HOME is invalid under the XDG spec and ignored.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", "relative/config") };
    let report = namedrop::install_shortcuts(None, Some(&home)).unwrap();
    assert_eq!(report["path"], default.to_str().unwrap());
    let _ = fs::remove_dir_all(&root);
}
