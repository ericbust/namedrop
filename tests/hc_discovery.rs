use namedrop::{HcHost, HerdrCommandExecutor, HerdrCommandOutput, Host, PaneScope, SpaceLocator};
use serde_json::{Value, json};
use std::{cell::RefCell, path::Path};

struct Fixture {
    calls: RefCell<Vec<Vec<String>>>,
    error: &'static str,
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals":[{"address":"remote/t1","state":"live"},{"address":"t2","state":"live"},{"address":"t1","state":"live"}]}).to_string(),
            ["get", "remote/t1", "--json"] => panic!("remote discovery must not issue a get"),
            ["get", "t2", "--json"] => return Err(self.error.into()),
            ["get", "t1", "--json"] => include_str!("fixtures/hc/get-labeled.stdout").into(),
            _ => panic!("unexpected argv {args:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}
#[test]
fn remote_and_broken_records_do_not_hide_local_discovery() {
    for error in [
        r#"{"error":{"kind":"unavailable","message":"timed out"}}"#,
        r#"{"error":{"kind":"machine_unknown","message":"gone"}}"#,
        "unreadable",
    ] {
        let fixture = Fixture {
            calls: RefCell::default(),
            error,
        };
        let host = HcHost::new(&fixture);
        let warnings = host.discovery_warnings().unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].terminal, "t2");
        assert!(!warnings[0].message.is_empty());
        assert_eq!(host.panes(PaneScope::All).unwrap().len(), 1);
        assert_eq!(host.spaces().unwrap().len(), 1);
        assert_eq!(
            host.space(SpaceLocator::Workspace("fixture-space"), None)
                .unwrap()
                .panes
                .len(),
            1
        );
    }
}
#[test]
fn explicit_remote_target_is_rejected_before_transport() {
    let fixture = Fixture {
        calls: RefCell::default(),
        error: "unused",
    };
    let host = HcHost::new(&fixture);
    assert!(host.read("remote/t1", 1, None).is_err());
    assert!(host.prompt("remote/t1", "hello").is_err());
    assert!(host.run_in_pane("remote/t1", "true").is_err());
    assert!(host.interrupt("remote/t1").is_err());
    assert!(
        host.set_labels("remote/t1", &[("namedrop.name", "name")])
            .is_err()
    );
    assert!(fixture.calls.borrow().is_empty());
}
#[test]
fn history_read_caps_rows_and_preserves_trailing_newlines() {
    struct Read(RefCell<Vec<Vec<String>>>, String);
    impl HerdrCommandExecutor for Read {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            self.0
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            let stdout = if args[0] == "get" {
                include_str!("fixtures/hc/get-labeled.stdout").into()
            } else {
                json!({"text":self.1}).to_string()
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    for trailing in ["", "\n"] {
        let text = (0..300)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + trailing;
        for lines in [0, 40, 120, 121, 200, 500] {
            let fixture = Read(RefCell::default(), text.clone());
            let actual = HcHost::new(&fixture).read("t1", lines, None).unwrap();
            let expected: String = text
                .split_inclusive('\n')
                .skip(300usize.saturating_sub(lines))
                .collect();
            assert_eq!(actual, expected, "lines={lines} trailing={trailing:?}");
            let calls = fixture.0.borrow();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0], ["get", "t1", "--json"]);
            assert_eq!(calls[1].contains(&"--lines".into()), lines > 120);
        }
    }
}
#[test]
fn checkout_resolves_each_distinct_cwd_once_per_call() {
    if std::env::var_os("NAMEDROP_HC_GIT_COUNTER").is_none() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!("hc-git-cache-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("git");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$2\" >> \"$NAMEDROP_HC_GIT_COUNTER\"\ncase \"$2\" in /missing) exit 1;; esac\nprintf '/checkout\\n'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "checkout_resolves_each_distinct_cwd_once_per_call",
                "--nocapture",
            ])
            .env("PATH", &directory)
            .env("NAMEDROP_HC_GIT_COUNTER", directory.join("calls"))
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(directory);
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    struct Many;
    impl HerdrCommandExecutor for Many {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = if args[0] == "ls" {
                json!({"terminals":(0..10).map(|n| json!({"address":format!("t{n}"),"state":"live"})).collect::<Vec<_>>()}).to_string()
            } else {
                let mut record: Value =
                    serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
                record["address"] = json!(args[1]);
                record["foreground"]["cwd"] = json!(if args[1] == "t9" {
                    "/missing"
                } else {
                    "/checkout"
                });
                record.to_string()
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    let host = HcHost::new(Many);
    let counter = std::env::var_os("NAMEDROP_HC_GIT_COUNTER").unwrap();
    for at in [
        SpaceLocator::Checkout(Path::new("/checkout")),
        SpaceLocator::Workspace("fixture-space"),
    ] {
        std::fs::write(&counter, "").unwrap();
        assert!(!host.space(at, None).unwrap().worktree_attached);
        let mut paths: Vec<_> = std::fs::read_to_string(&counter)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        paths.sort();
        assert_eq!(paths, ["/checkout", "/missing"]);
    }
}
