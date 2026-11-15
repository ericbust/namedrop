use super::*;
use crate::cli::UiMode;
use std::{
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nd-u6-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".config/namedrop")).unwrap();
        Self(root)
    }
    fn lease(&self) -> PackageLease {
        crate::installed_runtime::package_guard(
            Some(&self.0),
            &self.0.join("state"),
            crate::installed_runtime::Writer::Activation,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn module_matches_the_pinned_lua() {
    assert_eq!(
        module_text(
            Path::new("/fixture/runtime/namedrop"),
            Path::new("/fixture/state")
        )
        .unwrap(),
        include_str!("../../tests/fixtures/rex_ui.lua")
    );
}

#[test]
fn module_quotes_paths_as_lua_51_bytes() {
    let text = module_text(Path::new("/a/\"\\\n雪9"), Path::new("/state")).unwrap();
    assert!(
        text.contains(r#"local NAMEDROP = "/a/\"\\\010\233\155\1709""#),
        "{text}"
    );
    assert!(module_text(Path::new("relative"), Path::new("/state")).is_err());
    let text = module_text(Path::new("/__STATE__"), Path::new("/__LAUNCHER__")).unwrap();
    assert!(text.contains("local NAMEDROP = \"/__STATE__\""));
    assert!(text.contains("local STATE = \"/__LAUNCHER__\""));
}

#[test]
fn palette_names_select_the_existing_operations() {
    for (name, expected) in [
        ("activate", Action::Activate),
        ("name", Action::Popup(UiMode::Name)),
        ("manage", Action::Popup(UiMode::Manage)),
        ("doctor", Action::Popup(UiMode::Doctor)),
        ("status", Action::Popup(UiMode::Status)),
        ("tour", Action::Popup(UiMode::Tour)),
        ("channels", Action::Channels),
        ("pause", Action::Pause),
        ("resume", Action::Resume),
        ("deactivate", Action::Deactivate),
    ] {
        assert_eq!(action(name).unwrap(), expected, "{name}");
    }
    assert_eq!(
        action("future").unwrap_err(),
        "This Namedrop has no action future. Run sh scripts/install.sh again."
    );
}

#[test]
fn palette_failure_decodes_the_envelope_and_stays_on_one_line() {
    assert_eq!(
        error_line(r#"{"status":"rejected","error":{"message":"First line.\nRepair here."}}"#),
        "First line. Repair here."
    );
    assert_eq!(error_line("plain\r\nrepair"), "plain repair");
}

#[test]
fn refresh_follows_registration_and_preserves_the_persons_config() {
    let f = Fixture::new();
    let module = f.0.join("namedrop.lua");
    let config = f.0.join("custom.lua");
    fs::write(&config, "-- untouched\n").unwrap();
    let predecessors = json!([{"state":"/old", "sealed":true, "agents":[]}]);
    let path = crate::locator::rex_registration(&f.0);
    fs::write(&path, json!({"socket":"/tmp/fixture.sock", "config":config, "module":module, "line":"removed", "launcher":"/old/runtime/namedrop", "state":"/old", "predecessors":predecessors, "future":42}).to_string()).unwrap();
    let state = f.0.join("state");
    let lease = f.lease();
    refresh(&lease, &f.0, Path::new("/new/runtime/namedrop"), &state).unwrap();
    assert_eq!(
        fs::read_to_string(&module).unwrap(),
        module_text(Path::new("/new/runtime/namedrop"), &state).unwrap()
    );
    let saved = registration(&f.0).unwrap().unwrap();
    assert_eq!(saved["launcher"], "/new/runtime/namedrop");
    assert_eq!(saved["state"], json!(state));
    assert_eq!(saved["predecessors"], predecessors);
    assert_eq!(saved["future"], 42);
    assert_eq!(saved["line"], "removed");
    assert_eq!(fs::read_to_string(config).unwrap(), "-- untouched\n");
    let before = fs::read(&path).unwrap();
    refresh(&lease, &f.0, Path::new("/new/runtime/namedrop"), &state).unwrap();
    assert_eq!(before, fs::read(path).unwrap());
}

#[test]
fn absent_registration_keeps_refresh_and_inspection_inert() {
    let f = Fixture::new();
    let lease = f.lease();
    refresh(&lease, &f.0, Path::new("/launcher"), &f.0.join("state")).unwrap();
    assert!(!f.0.join("namedrop.lua").exists());
    assert!(inspect(Some(&f.0), &Endpoint::parse("rex:/tmp/no-such-u6.sock")).is_none());
    assert_eq!(
        shortcuts(Some(&f.0), &Endpoint::parse("rex:/tmp/no-such-u6.sock")),
        json!({"status":"no_module", "repair":"Run sh scripts/install.sh."})
    );
    fs::write(
        crate::locator::rex_registration(&f.0),
        r#"{"predecessors":[]}"#,
    )
    .unwrap();
    refresh(&lease, &f.0, Path::new("/launcher"), &f.0.join("state")).unwrap();
    assert!(inspect(Some(&f.0), &Endpoint::parse("rex:/tmp/no-such-u6.sock")).is_none());
}

#[test]
fn refresh_refuses_an_unowned_module_and_malformed_registration() {
    let f = Fixture::new();
    let module = f.0.join("namedrop.lua");
    let path = crate::locator::rex_registration(&f.0);
    fs::write(&module, "-- mine\n").unwrap();
    fs::write(&path, json!({"module":module,"state":"/old"}).to_string()).unwrap();
    let lease = f.lease();
    assert!(
        refresh(&lease, &f.0, Path::new("/launcher"), Path::new("/state"))
            .unwrap_err()
            .contains("not managed")
    );
    assert_eq!(fs::read_to_string(&module).unwrap(), "-- mine\n");
    assert_eq!(registration(&f.0).unwrap().unwrap()["state"], "/old");
    fs::write(&path, "[]").unwrap();
    assert!(registration(&f.0).unwrap_err().contains("not an object"));
    fs::write(&path, "{").unwrap();
    assert!(registration(&f.0).is_err());
}

#[test]
fn load_start_guard_accepts_only_the_server_parent() {
    use crate::mux::home::{Claimed, Inherited};
    use std::{cell::RefCell, ffi::OsString, io, path::PathBuf};
    struct Start {
        server: Option<String>,
        peer: Option<u32>,
        calls: RefCell<Vec<PathBuf>>,
    }
    impl Inherited for Start {
        fn var(&self, name: &str) -> Option<OsString> {
            (name == "REX_SERVER")
                .then(|| self.server.clone())
                .flatten()
                .map(Into::into)
        }
        fn id(&self) -> u32 {
            panic!("no process survey")
        }
        fn ancestors(&self) -> Vec<u32> {
            panic!("no process survey")
        }
        fn peer(&self, socket: &Path) -> io::Result<u32> {
            self.calls.borrow_mut().push(socket.into());
            self.peer.ok_or_else(|| io::ErrorKind::NotFound.into())
        }
        fn claimed(&self, _: &Path, _: &str, _: &str) -> Result<Claimed, String> {
            panic!("no host request")
        }
        fn incarnation(&self, _: u32) -> Option<String> {
            panic!("no process survey")
        }
    }
    let mut start = Start {
        server: Some("unix:///tmp/rex%20guard.sock".into()),
        peer: Some(123),
        calls: RefCell::default(),
    };
    // The server names itself by its listener, which the bridge is keyed by.
    let proved = RefCell::new(Vec::new());
    let prove = |socket: &Path| {
        proved.borrow_mut().push(socket.to_owned());
        Some(Endpoint::parse("rex:/private/tmp/rex guard.sock"))
    };
    assert_eq!(
        server_start_endpoint(&start, 123, prove).unwrap().key,
        "rex:/private/tmp/rex guard.sock"
    );
    assert_eq!(
        proved.borrow().as_slice(),
        [Path::new("/tmp/rex guard.sock")]
    );
    assert!(server_start_endpoint(&start, 123, |_| None).is_none());
    assert!(server_start_endpoint(&start, 456, prove).is_none());
    assert_eq!(
        start.calls.borrow().as_slice(),
        [
            Path::new("/tmp/rex guard.sock"),
            Path::new("/tmp/rex guard.sock"),
            Path::new("/tmp/rex guard.sock")
        ]
    );
    start.peer = None;
    assert!(server_start_endpoint(&start, 123, prove).is_none());
    start.server = Some("tcp://remote:7000".into());
    start.calls.borrow_mut().clear();
    assert!(server_start_endpoint(&start, 123, prove).is_none());
    assert!(start.calls.borrow().is_empty());
    start.server = None;
    assert!(server_start_endpoint(&start, 123, prove).is_none());
    assert!(start.calls.borrow().is_empty());
    // A refused start makes no protocol request.
    assert_eq!(proved.borrow().len(), 1);
}

#[test]
fn refresh_remembers_a_removed_managed_line_without_restoring_it() {
    let f = Fixture::new();
    let config = f.0.join("init.lua");
    let module = f.0.join("namedrop.lua");
    fs::write(&config, "-- the person removed the line\n").unwrap();
    fs::write(crate::locator::rex_registration(&f.0),json!({"config":config,"module":module,"line":"inserted","predecessors":[{"state":"/old"}]}).to_string()).unwrap();
    refresh(
        &f.lease(),
        &f.0,
        Path::new("/runtime/namedrop"),
        &f.0.join("state"),
    )
    .unwrap();
    let saved = registration(&f.0).unwrap().unwrap();
    assert_eq!(saved["line"], "removed");
    assert_eq!(saved["predecessors"], json!([{"state":"/old"}]));
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "-- the person removed the line\n"
    );
}

/// The U7 review's F1: only a read that finds the file without the line, or
/// finds no file, counts as removed, and a line the person adds back, as
/// Doctor says, clears `removed`.
#[test]
fn the_line_state_is_read_from_init_lua_each_time_and_a_failed_read_changes_nothing() {
    let f = Fixture::new();
    let config = f.0.join("init.lua");
    let module = f.0.join("namedrop.lua");
    let path = crate::locator::rex_registration(&f.0);
    let state = f.0.join("state");
    let refresh_with = |line: &str| {
        fs::write(
            &path,
            json!({"config":config,"module":module,"line":line}).to_string(),
        )
        .unwrap();
        refresh(&f.lease(), &f.0, Path::new("/runtime/namedrop"), &state).unwrap();
        registration(&f.0).unwrap().unwrap()["line"].clone()
    };
    let confirmed =
        |record: &Value| Ok::<_, String>(json!({"confirmed":true,"line":record["line"]}));
    // A file the refresh cannot read keeps the state, whatever it holds.
    fs::create_dir(&config).unwrap();
    assert_eq!(refresh_with("inserted"), "inserted");
    assert_eq!(
        inspect_with(Some(&f.0), confirmed).unwrap().unwrap()["confirmed"],
        true
    );
    assert_eq!(refresh_with("removed"), "removed");
    fs::remove_dir(&config).unwrap();
    fs::write(&config, format!("{CONFIG_LINE}\n-- mine\n")).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&config).is_err() {
        assert_eq!(refresh_with("inserted"), "inserted");
    }
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    // The person adds the removed line back: refresh and Doctor read it.
    assert_eq!(refresh_with("removed"), "inserted");
    fs::write(
        &path,
        json!({"config":config,"module":module,"line":"removed"}).to_string(),
    )
    .unwrap();
    let report = inspect_with(Some(&f.0), confirmed).unwrap().unwrap();
    assert_eq!(report["confirmed"], true, "{report}");
    assert_eq!(report["line"], "inserted");
    // A read that finds the line gone, or the file gone, is a removal.
    fs::write(&config, "-- mine\n").unwrap();
    assert_eq!(
        inspect_with(Some(&f.0), confirmed).unwrap().unwrap()["confirmed"],
        false
    );
    assert_eq!(refresh_with("inserted"), "removed");
    fs::remove_file(&config).unwrap();
    assert_eq!(refresh_with("inserted"), "removed");
    // A line the person owns, or one printed, is never re-read.
    for line in ["present", "printed"] {
        assert_eq!(refresh_with(line), line);
    }
    assert!(!config.exists());
}

/// The U6 review's F3 and F7: activation names the keys only for a confirmed
/// module, and a module the server loaded before a refresh rewrote it gets
/// a reload, not a reinstall.
#[test]
fn shortcuts_name_keys_only_for_a_confirmed_module_and_a_reload_for_a_stale_one() {
    let f = Fixture::new();
    let module = f.0.join("namedrop.lua");
    let config = f.0.join("init.lua");
    let (launcher, state) = (Path::new("/new/runtime/namedrop"), Path::new("/new"));
    fs::write(&config, format!("{CONFIG_LINE}\n")).unwrap();
    fs::write(&module, module_text(launcher, state).unwrap()).unwrap();
    fs::write(
        crate::locator::rex_registration(&f.0),
        json!({"module":module,"config":config,"line":"inserted","launcher":launcher,"state":state})
            .to_string(),
    )
    .unwrap();
    // The server's report: About names `loaded` as the module that answered,
    // baked to the old launcher.
    let report = |confirmed: bool, loaded: &Path| {
        let about = json!({"module":format!("@{}", loaded.display()),"launcher":"/old/runtime/namedrop","state":"/old"});
        move |record: &Value| {
            Ok::<_, String>(
                json!({"confirmed":confirmed,"module":record["module"],"keys":["k"],"actions_complete":true,"about":about}),
            )
        }
    };
    let shortcuts = |confirmed, loaded: &Path| {
        shortcut_report(inspect_with(Some(&f.0), report(confirmed, loaded)))
    };
    assert_eq!(
        shortcuts(true, &module),
        json!({"status":"rex_module","path":module,"keys":["k"]})
    );
    let reload = json!({"status":"stale_module","path":module,"repair":RELOAD_REPAIR});
    assert_eq!(shortcuts(false, &module), reload);
    let reinstall = json!({"status":"no_module", "repair":"Run sh scripts/install.sh."});
    // Another module answered, so a reload would not help.
    assert_eq!(shortcuts(false, &f.0.join("other/namedrop.lua")), reinstall);
    // The file is not the text a reload would load.
    fs::write(
        &module,
        module_text(Path::new("/elsewhere"), state).unwrap(),
    )
    .unwrap();
    assert_eq!(shortcuts(false, &module), reinstall);
    fs::write(&module, module_text(launcher, state).unwrap()).unwrap();
    assert_eq!(shortcuts(false, &module), reload);
    // With the line gone, a reload loads no module at all.
    fs::write(&config, "-- mine\n").unwrap();
    assert_eq!(shortcuts(true, &module), reinstall);
    assert_eq!(shortcuts(false, &module), reinstall);
}
