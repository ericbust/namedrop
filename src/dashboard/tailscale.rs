use super::server_files::{ServerFiles, private_file};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnedHandler {
    pub dns_name: String,
    pub https_port: u16,
    pub owner_login: String,
    pub backend_port: u16,
}

impl OwnedHandler {
    pub fn authority(&self) -> String {
        if self.https_port == 443 {
            self.dns_name.clone()
        } else {
            self.serve_key()
        }
    }

    fn serve_key(&self) -> String {
        format!("{}:{}", self.dns_name, self.https_port)
    }

    pub fn origin(&self) -> String {
        format!("https://{}", self.authority())
    }

    fn target(&self) -> String {
        format!("http://127.0.0.1:{}", self.backend_port)
    }

    fn validate(&self) -> Result<(), String> {
        if !valid_dns(&self.dns_name)
            || !valid_login(&self.owner_login)
            || self.https_port == 0
            || self.backend_port == 0
        {
            return Err("invalid Channels Tailscale ownership record".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "state",
    content = "handler",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum Ownership {
    Installing(OwnedHandler),
    Enabled(OwnedHandler),
    Removing(OwnedHandler),
}

impl Ownership {
    pub fn handler(&self) -> &OwnedHandler {
        match self {
            Self::Installing(handler) | Self::Enabled(handler) | Self::Removing(handler) => handler,
        }
    }
}

pub(super) fn read(state: &Path) -> Result<Option<Ownership>, String> {
    let bytes = match fs::read(state.join("dashboard/tailscale.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read Channels Tailscale state: {error}")),
    };
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("malformed Channels Tailscale state: {error}"))?;
    if value.get("version") != Some(&json!(1)) {
        return Err("unsupported Channels Tailscale state version".to_owned());
    }
    value
        .as_object_mut()
        .ok_or("malformed Channels Tailscale state")?
        .remove("version");
    let ownership: Ownership = serde_json::from_value(value)
        .map_err(|error| format!("malformed Channels Tailscale state: {error}"))?;
    ownership.handler().validate()?;
    Ok(Some(ownership))
}

pub(super) fn operation_lock(files: &ServerFiles) -> Result<File, String> {
    let file = private_file(&files.state().join("dashboard/tailscale.lock"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("could not lock Channels Tailscale setup: {error}")),
        }
    }
}

fn write(files: &ServerFiles, ownership: &Ownership) -> Result<(), String> {
    let mut value = serde_json::to_value(ownership).map_err(|error| error.to_string())?;
    value
        .as_object_mut()
        .expect("ownership object")
        .insert("version".to_owned(), json!(1));
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    files.write_atomic("tailscale.json", &bytes)?;
    sync_directory(files)
}

fn sync_directory(files: &ServerFiles) -> Result<(), String> {
    File::open(files.state().join("dashboard"))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("could not persist Channels Tailscale state: {error}"))
}

fn forget(files: &ServerFiles) -> Result<(), String> {
    match fs::remove_file(files.state().join("dashboard/tailscale.json")) {
        Ok(()) => sync_directory(files),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

struct Cli<'a>(&'a std::ffi::OsStr);

impl Cli<'_> {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new(self.0)
            .args(args)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                format!("could not run tailscale; install its CLI and sign in: {error}")
            })?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let collect = |stream: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                stream
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map(|_| bytes)
            })
        };
        let stdout = collect(Box::new(stdout));
        let stderr = collect(Box::new(stderr));
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                Ok(None) => break Err("tailscale command timed out after 15 seconds".to_owned()),
                Err(error) => break Err(error.to_string()),
            }
        };
        if status.is_err() {
            let _ = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
        }
        let output = stdout
            .join()
            .map_err(|_| "tailscale output reader failed")?
            .map_err(|error| error.to_string())?;
        let errors = stderr
            .join()
            .map_err(|_| "tailscale error reader failed")?
            .map_err(|error| error.to_string())?;
        let status = status?;
        if output.len() > 1024 * 1024 || errors.len() > 1024 * 1024 {
            return Err("tailscale output exceeded 1 MiB".to_owned());
        }
        if !status.success() {
            return Err(format!(
                "tailscale {} failed ({status}): {}",
                args.join(" "),
                String::from_utf8_lossy(&errors).trim()
            ));
        }
        String::from_utf8(output).map_err(|error| format!("invalid tailscale output: {error}"))
    }

    fn json(&self, args: &[&str]) -> Result<Value, String> {
        serde_json::from_str(&self.run(args)?)
            .map_err(|error| format!("invalid tailscale JSON: {error}"))
    }

    fn status(&self) -> Result<ServeStatus, String> {
        ServeStatus::parse(self.json(&["serve", "status", "--json"])?)
    }

    fn change(&self, handler: &OwnedHandler, off: bool) -> Result<(), String> {
        let port = format!("--https={}", handler.https_port);
        let target = handler.target();
        let mut args = vec!["serve", "--bg", &port, "--set-path=/", &target];
        if off {
            args.push("off");
        }
        self.run(&args).map(|_| ())
    }
}

fn valid_dns(name: &str) -> bool {
    name.len() <= 253
        && name.ends_with(".ts.net")
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.is_ascii()
        && !login
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
}

fn identity(status: &Value) -> Result<(String, String), String> {
    if status["BackendState"] != "Running" {
        return Err(
            "Tailscale is not Running; sign in and connect before opening Channels".to_owned(),
        );
    }
    let myself = &status["Self"];
    if myself["Tags"]
        .as_array()
        .is_some_and(|tags| !tags.is_empty())
    {
        return Err(
            "Channels requires a user-owned Tailscale device, not a tagged device".to_owned(),
        );
    }
    let reported_dns = myself["DNSName"].as_str().unwrap_or("");
    let dns = reported_dns
        .strip_suffix('.')
        .unwrap_or(reported_dns)
        .to_owned();
    let user = myself["UserID"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or("Tailscale did not report the device owner's user ID")?;
    let login = status["User"][user.to_string()]["LoginName"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if !valid_dns(&dns) || !valid_login(&login) {
        return Err("Tailscale did not report a valid MagicDNS name and owner login".to_owned());
    }
    Ok((dns, login))
}

struct ServeStatus {
    raw: Value,
    occupied: BTreeSet<u16>,
    funnel: BTreeSet<u16>,
}

fn port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| format!("malformed Tailscale Serve port: {value}"))
}

fn host_port(value: &str) -> Result<u16, String> {
    let (host, value) = value
        .rsplit_once(':')
        .ok_or("malformed Tailscale Serve host:port")?;
    if host.is_empty() {
        return Err("malformed Tailscale Serve host:port".to_owned());
    }
    port(value)
}

impl ServeStatus {
    fn parse(mut raw: Value) -> Result<Self, String> {
        if raw.is_null() {
            raw = json!({});
        }
        let mut parsed = Self {
            raw,
            occupied: BTreeSet::new(),
            funnel: BTreeSet::new(),
        };
        Self::scan(&parsed.raw, &mut parsed.occupied, &mut parsed.funnel)?;
        Ok(parsed)
    }

    fn scan(
        config: &Value,
        occupied: &mut BTreeSet<u16>,
        funnel: &mut BTreeSet<u16>,
    ) -> Result<(), String> {
        let object = config
            .as_object()
            .ok_or("malformed Tailscale Serve config")?;
        for name in ["TCP", "Web", "AllowFunnel"] {
            let Some(value) = object.get(name).filter(|value| !value.is_null()) else {
                continue;
            };
            let entries = value
                .as_object()
                .ok_or_else(|| format!("malformed Tailscale Serve {name}"))?;
            for (key, value) in entries {
                let number = if name == "TCP" {
                    port(key)?
                } else {
                    host_port(key)?
                };
                if name == "AllowFunnel" {
                    occupied.insert(number);
                    if value
                        .as_bool()
                        .ok_or("malformed Tailscale AllowFunnel value")?
                    {
                        funnel.insert(number);
                        occupied.insert(number);
                    }
                } else {
                    if !value.is_object() {
                        return Err(format!("malformed Tailscale Serve {name} handler"));
                    }
                    if name == "TCP" {
                        for field in ["HTTPS", "HTTP"] {
                            if value.get(field).is_some_and(|value| !value.is_boolean()) {
                                return Err(format!("malformed Tailscale Serve TCP {field}"));
                            }
                        }
                        for field in ["TCPForward", "TerminateTLS"] {
                            if value.get(field).is_some_and(|value| !value.is_string()) {
                                return Err(format!("malformed Tailscale Serve TCP {field}"));
                            }
                        }
                    } else {
                        let handlers = value
                            .get("Handlers")
                            .and_then(Value::as_object)
                            .ok_or("malformed Tailscale Serve Web handlers")?;
                        for (path, handler) in handlers {
                            if !path.starts_with('/') || !handler.is_object() {
                                return Err("malformed Tailscale Serve Web path handler".to_owned());
                            }
                            for field in ["Proxy", "Path", "Text", "Redirect"] {
                                if handler.get(field).is_some_and(|value| !value.is_string()) {
                                    return Err(format!("malformed Tailscale Serve Web {field}"));
                                }
                            }
                        }
                    }
                    occupied.insert(number);
                }
            }
        }
        for name in ["Foreground", "Services"] {
            let Some(value) = object.get(name).filter(|value| !value.is_null()) else {
                continue;
            };
            for config in value
                .as_object()
                .ok_or_else(|| format!("malformed Tailscale Serve {name}"))?
                .values()
            {
                Self::scan(config, occupied, funnel)?;
            }
        }
        Ok(())
    }

    fn exact(&self, handler: &OwnedHandler) -> bool {
        if self.funnel.contains(&handler.https_port)
            || self.raw["TCP"][handler.https_port.to_string()] != json!({"HTTPS":true})
            || self.raw["Web"][handler.serve_key()]
                != json!({"Handlers":{"/":{"Proxy":handler.target()}}})
        {
            return false;
        }
        let mut remaining = self.raw.clone();
        Self::strip_owned(&mut remaining, handler);
        Self::parse(remaining).is_ok_and(|status| !status.occupied.contains(&handler.https_port))
    }

    fn strip_owned(raw: &mut Value, handler: &OwnedHandler) {
        for (name, key) in [
            ("TCP", handler.https_port.to_string()),
            ("Web", handler.serve_key()),
            ("AllowFunnel", handler.serve_key()),
        ] {
            if let Some(entries) = raw.get_mut(name).and_then(Value::as_object_mut) {
                entries.remove(&key);
            }
        }
    }

    fn foreign(&self, handler: &OwnedHandler) -> Value {
        let mut raw = self.raw.clone();
        Self::strip_owned(&mut raw, handler);
        // Go omits empty maps when updating Serve config.
        fn normalize(raw: &mut Value) {
            if let Some(object) = raw.as_object_mut() {
                for name in ["TCP", "Web", "AllowFunnel", "Foreground", "Services"] {
                    if let Some(value) = object.get_mut(name) {
                        if (name == "Foreground" || name == "Services")
                            && let Some(entries) = value.as_object_mut()
                        {
                            for value in entries.values_mut() {
                                normalize(value);
                            }
                        }
                        if value.is_null()
                            || value.as_object().is_some_and(|entries| entries.is_empty())
                        {
                            object.remove(name);
                        }
                    }
                }
            }
        }
        normalize(&mut raw);
        raw
    }

    fn choose_port(&self) -> Result<u16, String> {
        std::iter::once(443)
            .chain(8443..=8474)
            .find(|port| !self.occupied.contains(port))
            .ok_or_else(|| {
                "no free Tailscale HTTPS port (443 or 8443–8474); existing handlers were preserved"
                    .to_owned()
            })
    }
}

pub(super) fn enable(files: &ServerFiles, backend_port: u16) -> Result<OwnedHandler, String> {
    enable_with(files, backend_port, &Cli(std::ffi::OsStr::new("tailscale")))
}

fn enable_with(
    files: &ServerFiles,
    backend_port: u16,
    cli: &Cli<'_>,
) -> Result<OwnedHandler, String> {
    let previous = read(files.state())?;
    if matches!(previous, Some(Ownership::Removing(_))) {
        return Err(
            "Tailscale removal is incomplete; run `namedrop channels --tailscale off` again first"
                .to_owned(),
        );
    }
    let (dns_name, owner_login) = identity(&cli.json(&["status", "--json"])?)?;
    let before = cli.status()?;
    let handler = match previous {
        Some(ownership) => {
            let handler = ownership.handler().clone();
            if handler.dns_name != dns_name
                || handler.owner_login != owner_login
                || handler.backend_port != backend_port
            {
                return Err("Tailscale identity or Channels port changed; turn off the recorded handler before enabling again".to_owned());
            }
            if before.exact(&handler) {
                write(files, &Ownership::Enabled(handler.clone()))?;
                return Ok(handler);
            }
            if before.occupied.contains(&handler.https_port) {
                return Err("the recorded Tailscale port contains a foreign handler or Funnel; it was preserved".to_owned());
            }
            handler
        }
        None => OwnedHandler {
            dns_name,
            https_port: before.choose_port()?,
            owner_login,
            backend_port,
        },
    };
    write(files, &Ownership::Installing(handler.clone()))?;
    cli.change(&handler, false)?;
    let after = cli.status()?;
    if !after.exact(&handler) || before.foreign(&handler) != after.foreign(&handler) {
        return Err("Tailscale Serve readback did not match the owned handler and unchanged foreign configuration; Channels tailnet access remains disabled".to_owned());
    }
    write(files, &Ownership::Enabled(handler.clone()))?;
    Ok(handler)
}

pub(super) fn disable(files: &ServerFiles) -> Result<(), String> {
    disable_with(files, &Cli(std::ffi::OsStr::new("tailscale")))
}

fn disable_with(files: &ServerFiles, cli: &Cli<'_>) -> Result<(), String> {
    let Some(ownership) = read(files.state())? else {
        return Ok(());
    };
    let handler = ownership.handler().clone();
    write(files, &Ownership::Removing(handler.clone()))?;
    let before = cli.status()?;
    if !before.occupied.contains(&handler.https_port) {
        return forget(files);
    }
    if !before.exact(&handler) {
        return Err("the recorded Tailscale port contains a foreign handler or Funnel; it was preserved and Channels tailnet access is disabled".to_owned());
    }
    // The CLI derives the host it removes from the current device identity.
    let (dns, _) = identity(&cli.json(&["status", "--json"])?)?;
    if dns != handler.dns_name {
        return Err("Tailscale device DNS name changed; the recorded handler was preserved and tailnet access is disabled".to_owned());
    }
    cli.change(&handler, true)?;
    let after = cli.status()?;
    if after.occupied.contains(&handler.https_port)
        || before.foreign(&handler) != after.foreign(&handler)
    {
        return Err("Tailscale removal readback failed; ownership was retained and tailnet access is disabled".to_owned());
    }
    forget(files)
}

pub(crate) struct Diagnostic {
    pub url: Option<String>,
    pub problem: Option<String>,
}

pub(crate) fn diagnose(state: &Path) -> Option<Diagnostic> {
    diagnose_with(state, &Cli(std::ffi::OsStr::new("tailscale")))
}

fn diagnose_with(state: &Path, cli: &Cli<'_>) -> Option<Diagnostic> {
    let ownership = match read(state) {
        Ok(Some(ownership)) => ownership,
        Ok(None) => return None,
        Err(problem) => {
            return Some(Diagnostic {
                url: None,
                problem: Some(problem),
            });
        }
    };
    let handler = ownership.handler();
    let inspect = || {
        if !matches!(ownership, Ownership::Enabled(_)) {
            return Err(
                "Channels Tailscale setup or removal is incomplete; tailnet access is disabled"
                    .to_owned(),
            );
        }
        let server: super::server_files::ServerInfo =
            serde_json::from_slice(&fs::read(state.join("dashboard/server.json")).map_err(
                |error| format!("Channels server record is missing or unreadable: {error}"),
            )?)
            .map_err(|error| format!("Channels server record is malformed: {error}"))?;
        if server.port != handler.backend_port {
            return Err("Tailscale handler targets a stale Channels backend port".to_owned());
        }
        let status = cli.status()?;
        if status.funnel.contains(&handler.https_port) {
            return Err(
                "Funnel is enabled on the recorded Channels port; turn it off in Tailscale"
                    .to_owned(),
            );
        }
        if !status.occupied.contains(&handler.https_port) {
            return Err("the recorded Channels Tailscale handler is missing".to_owned());
        }
        if status.raw["Web"][handler.serve_key()]["Handlers"]["/"]["Proxy"]
            .as_str()
            .is_some_and(|target| target != handler.target())
        {
            return Err(
                "the recorded Tailscale handler has a stale Channels backend target".to_owned(),
            );
        }
        if !status.exact(handler) {
            return Err(
                "the recorded Channels Tailscale port contains a foreign or changed handler"
                    .to_owned(),
            );
        }
        Ok(())
    };
    Some(Diagnostic {
        url: Some(handler.origin()),
        problem: inspect().err(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    const DNS: &str = "device.tail123.ts.net";

    fn handler() -> OwnedHandler {
        OwnedHandler {
            dns_name: DNS.into(),
            https_port: 443,
            owner_login: "owner@example.com".into(),
            backend_port: 1234,
        }
    }

    fn own_config(handler: &OwnedHandler) -> Value {
        json!({"TCP":{handler.https_port.to_string():{"HTTPS":true}}, "Web":{handler.serve_key():{"Handlers":{"/":{"Proxy":handler.target()}}}}})
    }

    struct Fixture {
        root: PathBuf,
        files: ServerFiles,
        executable: PathBuf,
    }

    impl Fixture {
        fn new(config: Value) -> Self {
            let root = std::env::temp_dir().join(format!(
                "namedrop-tailscale-{}",
                super::super::server_files::random_hex(12).unwrap()
            ));
            fs::create_dir_all(&root).unwrap();
            let files = ServerFiles::at(&root.join("state")).unwrap();
            let executable = root.join("tailscale");
            fs::write(
                &executable,
                r#"#!/usr/bin/python3
import json, pathlib, sys
root = pathlib.Path(__file__).parent
args = sys.argv[1:]
if args == ['--probe']:
    sys.exit(0)
def read(name):
    return json.loads((root / name).read_text())
def write(name, value):
    (root / name).write_text(json.dumps(value))
if args == ['status', '--json']:
    print(json.dumps(read('identity.json')))
elif args == ['serve', 'status', '--json']:
    print(json.dumps(read('serve.json')))
else:
    state = read('state/dashboard/tailscale.json')
    with (root / 'changes.jsonl').open('a') as file:
        file.write(json.dumps({'args':args, 'state':state['state']}) + '\n')
    assert args[:2] == ['serve', '--bg']
    assert '--set-path=/' in args
    port = next(arg.split('=',1)[1] for arg in args if arg.startswith('--https='))
    target = next(arg for arg in args if arg.startswith('http://'))
    off = args[-1] == 'off'
    behavior = read('behavior.json')
    if behavior.get('fail_off' if off else 'fail_enable'):
        print('fixture command failed', file=sys.stderr)
        sys.exit(1)
    config = read('serve.json') or {}
    key = state['handler']['dns_name'] + ':' + port
    if off:
        config.get('TCP', {}).pop(port, None)
        config.get('Web', {}).pop(key, None)
        config.get('AllowFunnel', {}).pop(key, None)
    else:
        config.setdefault('TCP', {})[port] = {'HTTPS': True}
        config.setdefault('Web', {})[key] = {'Handlers': {'/': {'Proxy':target}}}
    if behavior.get('change_foreign'):
        config['unexpected'] = True
    write('serve.json', config)
"#,
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            // Another test's fork can hold the script's write descriptor until its own
            // exec, and running the script meanwhile fails with ETXTBSY.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match Command::new(&executable).arg("--probe").status() {
                    Err(error)
                        if error.raw_os_error() == Some(libc::ETXTBSY)
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    result => break assert!(result.unwrap().success()),
                }
            }
            let fixture = Self {
                root,
                files,
                executable,
            };
            fixture.put("serve.json", config);
            fixture.put("identity.json", json!({"BackendState":"Running", "Self":{"DNSName":format!("{DNS}."),"UserID":42},"User":{"42":{"LoginName":"owner@example.com"}}}));
            fixture.put("behavior.json", json!({}));
            fixture
        }

        fn cli(&self) -> Cli<'_> {
            Cli(self.executable.as_os_str())
        }
        fn put(&self, name: &str, value: Value) {
            fs::write(self.root.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        fn get(&self, name: &str) -> Value {
            serde_json::from_slice(&fs::read(self.root.join(name)).unwrap()).unwrap()
        }
        fn changes(&self) -> Vec<Value> {
            fs::read_to_string(self.root.join("changes.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn setup_preserves_foreign_handlers_and_is_idempotent() {
        let foreign = json!({"TCP":{"443":{"HTTPS":true}},"Web":{"other.tail123.ts.net:443":{"Handlers":{"/":{"Text":"foreign"}},"FutureOption":{"nested":[1,2]}}},"Foreground":{"session":{"TCP":{"8443":{"TCPForward":"127.0.0.1:8888"}}}},"AllowFunnel":{"another.tail123.ts.net:8444":true}});
        let fixture = Fixture::new(foreign.clone());
        let owned = enable_with(&fixture.files, 1234, &fixture.cli()).unwrap();
        assert_eq!(owned.https_port, 8445);
        assert_eq!(owned.origin(), format!("https://{DNS}:8445"));
        assert!(
            matches!(read(fixture.files.state()).unwrap(), Some(Ownership::Enabled(record)) if record == owned)
        );
        assert_eq!(
            fs::metadata(fixture.files.state().join("dashboard/tailscale.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(fixture.changes()[0]["state"], "installing");
        assert_eq!(
            fixture.changes()[0]["args"],
            json!([
                "serve",
                "--bg",
                "--https=8445",
                "--set-path=/",
                "http://127.0.0.1:1234"
            ])
        );
        assert_eq!(
            enable_with(&fixture.files, 1234, &fixture.cli()).unwrap(),
            owned
        );
        assert_eq!(fixture.changes().len(), 1);
        disable_with(&fixture.files, &fixture.cli()).unwrap();
        assert!(read(fixture.files.state()).unwrap().is_none());
        assert_eq!(fixture.changes()[1]["state"], "removing");
        assert_eq!(
            fixture.changes()[1]["args"],
            json!([
                "serve",
                "--bg",
                "--https=8445",
                "--set-path=/",
                "http://127.0.0.1:1234",
                "off"
            ])
        );
        assert_eq!(
            ServeStatus::parse(fixture.get("serve.json"))
                .unwrap()
                .foreign(&owned),
            ServeStatus::parse(foreign).unwrap().foreign(&owned)
        );
        disable_with(&fixture.files, &fixture.cli()).unwrap();
        assert_eq!(fixture.changes().len(), 2);
    }

    #[test]
    fn interrupted_install_recovers_and_never_adopts_unrecorded_handlers() {
        let fixture = Fixture::new(own_config(&handler()));
        let owned = enable_with(&fixture.files, 1234, &fixture.cli()).unwrap();
        assert_eq!(
            owned.https_port, 8443,
            "matching but unrecorded handler is foreign"
        );
        assert_eq!(
            fixture.get("serve.json")["Web"][format!("{DNS}:443")],
            own_config(&handler())["Web"][format!("{DNS}:443")]
        );
        write(&fixture.files, &Ownership::Installing(owned.clone())).unwrap();
        assert_eq!(
            enable_with(&fixture.files, 1234, &fixture.cli()).unwrap(),
            owned
        );
        assert_eq!(
            fixture.changes().len(),
            1,
            "recover readback without a second mutation"
        );
        assert!(enable_with(&fixture.files, 9999, &fixture.cli()).is_err());
    }

    #[test]
    fn failed_setup_and_off_retain_recoverable_ownership() {
        let fixture = Fixture::new(json!({}));
        fixture.put("behavior.json", json!({"fail_enable":true}));
        assert!(enable_with(&fixture.files, 1234, &fixture.cli()).is_err());
        assert!(matches!(
            read(fixture.files.state()).unwrap(),
            Some(Ownership::Installing(_))
        ));
        fixture.put("behavior.json", json!({}));
        let owned = enable_with(&fixture.files, 1234, &fixture.cli()).unwrap();
        fixture.put("behavior.json", json!({"fail_off":true}));
        assert!(disable_with(&fixture.files, &fixture.cli()).is_err());
        assert!(
            matches!(read(fixture.files.state()).unwrap(), Some(Ownership::Removing(record)) if record == owned)
        );
        assert!(
            enable_with(&fixture.files, 1234, &fixture.cli())
                .unwrap_err()
                .contains("removal is incomplete")
        );
        fixture.put("behavior.json", json!({}));
        disable_with(&fixture.files, &fixture.cli()).unwrap();
        assert!(read(fixture.files.state()).unwrap().is_none());
    }

    #[test]
    fn changed_handler_and_funnel_are_preserved_on_off() {
        for change in [
            json!({"extra_path":true}),
            json!({"funnel":true}),
            json!({"foreign_dns":true}),
            json!({"unknown":true}),
        ] {
            let fixture = Fixture::new(json!({}));
            let owned = enable_with(&fixture.files, 1234, &fixture.cli()).unwrap();
            let mut changed = fixture.get("serve.json");
            if change["extra_path"] == true {
                changed["Web"][owned.serve_key()]["Handlers"]["/extra"] = json!({"Text":"foreign"});
            }
            if change["funnel"] == true {
                changed["AllowFunnel"] = json!({owned.serve_key():true});
            }
            if change["foreign_dns"] == true {
                changed["Web"]["other.tail123.ts.net:443"] =
                    json!({"Handlers":{"/":{"Text":"foreign"}}});
            }
            if change["unknown"] == true {
                changed["TCP"]["443"]["FutureOption"] = json!(true);
            }
            fixture.put("serve.json", changed.clone());
            assert!(
                disable_with(&fixture.files, &fixture.cli()).is_err(),
                "{change}"
            );
            assert_eq!(fixture.get("serve.json"), changed);
            assert_eq!(fixture.changes().len(), 1);
            assert!(matches!(
                read(fixture.files.state()).unwrap(),
                Some(Ownership::Removing(_))
            ));
        }
    }

    #[test]
    fn readback_checks_foreign_configuration_before_allowing_access() {
        let fixture = Fixture::new(json!({}));
        fixture.put("behavior.json", json!({"change_foreign":true}));
        assert!(
            enable_with(&fixture.files, 1234, &fixture.cli())
                .unwrap_err()
                .contains("readback")
        );
        assert!(matches!(
            read(fixture.files.state()).unwrap(),
            Some(Ownership::Installing(_))
        ));
    }

    #[test]
    fn missing_handlers_can_be_removed_without_a_running_backend() {
        let fixture = Fixture::new(json!({}));
        write(&fixture.files, &Ownership::Installing(handler())).unwrap();
        disable_with(&fixture.files, &fixture.cli()).unwrap();
        assert!(read(fixture.files.state()).unwrap().is_none());
        assert!(fixture.changes().is_empty());
    }

    #[test]
    fn malformed_status_and_invalid_identities_fail_closed() {
        for value in [
            json!([]),
            json!({"TCP":[]}),
            json!({"TCP":{"0":{}}}),
            json!({"TCP":{"443":{"HTTPS":"true"}}}),
            json!({"Web":{"missing-port":{}}}),
            json!({"Web":{"name:443":{"Handlers":[]}}}),
            json!({"Web":{"name:443":{"Handlers":{"/":null}}}}),
            json!({"AllowFunnel":{"name:443":"true"}}),
            json!({"Foreground":{"session":{"TCP":true}}}),
        ] {
            assert!(ServeStatus::parse(value.clone()).is_err(), "{value}");
        }
        assert_eq!(
            ServeStatus::parse(Value::Null)
                .unwrap()
                .choose_port()
                .unwrap(),
            443
        );
        let fixture = Fixture::new(json!({}));
        let original = fixture.get("identity.json");
        for invalid in [
            json!({"BackendState":"Stopped"}),
            json!({"Self":{"UserID":0}}),
            json!({"Self":{"UserID":42,"DNSName":format!("{DNS}."),"Tags":["tag:server"]}}),
        ] {
            let mut status = original.clone();
            for (key, value) in invalid.as_object().unwrap() {
                status[key] = value.clone();
            }
            assert!(identity(&status).is_err(), "{status}");
        }
        assert_eq!(
            identity(&original).unwrap(),
            (DNS.to_owned(), "owner@example.com".to_owned())
        );
    }

    #[test]
    fn malformed_record_is_not_treated_as_disabled_and_diagnostics_do_not_create_files() {
        let fixture = Fixture::new(json!({}));
        fs::write(
            fixture.files.state().join("dashboard/tailscale.json"),
            b"{}",
        )
        .unwrap();
        assert!(read(fixture.files.state()).is_err());
        assert!(disable_with(&fixture.files, &fixture.cli()).is_err());
        assert!(fixture.changes().is_empty());
        let absent = fixture.root.join("absent");
        assert!(diagnose(&absent).is_none());
        assert!(!absent.exists());
    }
    #[test]
    fn diagnostics_report_url_incomplete_missing_funnel_foreign_and_stale_target() {
        let fixture = Fixture::new(own_config(&handler()));
        let owned = handler();
        let server = super::super::server_files::ServerInfo {
            pid: 1,
            port: 1234,
            instance: "test".into(),
            started_at_ms: 0,
            ui: super::super::server_files::UiSource::Embedded,
        };
        fixture.put(
            "state/dashboard/server.json",
            serde_json::to_value(&server).unwrap(),
        );
        write(&fixture.files, &Ownership::Enabled(owned.clone())).unwrap();
        let diagnostic = diagnose_with(fixture.files.state(), &fixture.cli()).unwrap();
        assert_eq!(
            diagnostic.url.as_deref(),
            Some("https://device.tail123.ts.net")
        );
        assert!(diagnostic.problem.is_none());
        for (config, expected) in [
            (json!({}), "missing"),
            (
                {
                    let mut config = own_config(&owned);
                    config["AllowFunnel"] = json!({owned.serve_key():true});
                    config
                },
                "Funnel",
            ),
            (
                {
                    let mut config = own_config(&owned);
                    config["TCP"]["443"]["FutureOption"] = json!(true);
                    config
                },
                "foreign",
            ),
            (
                {
                    let mut config = own_config(&owned);
                    config["Web"][owned.serve_key()]["Handlers"]["/"]["Proxy"] =
                        json!("http://127.0.0.1:9999");
                    config
                },
                "stale",
            ),
        ] {
            fixture.put("serve.json", config);
            let problem = diagnose_with(fixture.files.state(), &fixture.cli())
                .unwrap()
                .problem
                .unwrap();
            assert!(problem.contains(expected), "{problem}");
        }
        write(&fixture.files, &Ownership::Installing(owned)).unwrap();
        assert!(
            diagnose_with(fixture.files.state(), &fixture.cli())
                .unwrap()
                .problem
                .unwrap()
                .contains("incomplete")
        );
        assert!(
            fixture.changes().is_empty(),
            "Doctor only reads state and Serve status"
        );
    }

    #[test]
    fn all_existing_port_mentions_reserve_ports_and_exhaustion_never_mutates() {
        let mut config = json!({"AllowFunnel":{"other.tail123.ts.net:443":false}});
        assert_eq!(
            ServeStatus::parse(config.clone())
                .unwrap()
                .choose_port()
                .unwrap(),
            8443
        );
        for port in 8443..=8474 {
            config["AllowFunnel"][format!("other.tail123.ts.net:{port}")] = json!(false);
        }
        let fixture = Fixture::new(config.clone());
        assert!(
            enable_with(&fixture.files, 1234, &fixture.cli())
                .unwrap_err()
                .contains("no free")
        );
        assert_eq!(fixture.get("serve.json"), config);
        assert!(read(fixture.files.state()).unwrap().is_none());
        assert!(fixture.changes().is_empty());
    }
}
