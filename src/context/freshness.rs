use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::PathBuf,
};

pub(crate) fn review_freshness(checkout: &CheckoutAccess, snapshot: &Value) -> Value {
    let Ok(worktree) = checkout.root() else {
        return match checkout {
            CheckoutAccess::Unavailable(reason, _) => {
                json!({"status":"unknown","reason":"checkout_unavailable","checkout_reason":reason.as_str()})
            }
            CheckoutAccess::Legacy(None) => {
                json!({"status":"unknown","reason":"no_attached_worktree"})
            }
            _ => json!({"status":"unknown","reason":"no_captured_git_state"}),
        };
    };
    let git_state = &snapshot["git_state"];
    let Some(pin) = git_state["pinned_commit"]
        .as_str()
        .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return json!({"status":"unknown","reason":"no_captured_git_state"});
    };
    match compare(worktree, pin, snapshot) {
        Ok(result) => result,
        Err(_) => json!({"status":"unknown","reason":"git_pin_or_worktree_unavailable"}),
    }
}

fn compare(worktree: &Path, pin: &str, snapshot: &Value) -> Result<Value, String> {
    let root = git_output(worktree, &["rev-parse", "--show-toplevel"])?;
    let root = Path::new(&root);
    // A missing pin must never be interpreted as an empty reviewed tree.
    git_output(root, &["cat-file", "-e", &format!("{pin}^{{commit}}")])?;
    let paths = snapshot["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["kind"] == "diff")
        .filter_map(|p| p["locator"].as_str())
        .map(|path| {
            if path.is_empty() {
                return String::new();
            }
            let normalized = Path::new(path)
                .components()
                .filter(|part| !matches!(part, Component::CurDir))
                .collect::<PathBuf>();
            if normalized.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                normalized.to_string_lossy().into_owned()
            }
        })
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(json!({"status":"unknown","reason":"no_explicit_diff_paths"}));
    }
    let allowed = paths
        .iter()
        .map(String::as_str)
        .filter(|path| {
            *path != "[BLOCKED PATH]"
                && validate_single_path(path).is_ok()
                && blocked_path_reason(root, path).is_none()
        })
        .collect::<Vec<_>>();
    if allowed.is_empty() {
        return Ok(json!({"status":"unknown","reason":"protected_or_invalid_paths"}));
    }
    let mut tree_args = vec![
        "--literal-pathspecs",
        "ls-tree",
        "-rz",
        "--full-tree",
        pin,
        "--",
    ];
    tree_args.extend(&allowed);
    let tree = git_bytes(root, &tree_args, None)?;
    let mut pinned = BTreeMap::new();
    for entry in tree.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|e| e.to_string())?;
        let (metadata, path) = entry.split_once('\t').ok_or("invalid Git tree entry")?;
        let mut fields = metadata.split_whitespace();
        let mode = fields.next().ok_or("missing Git mode")?;
        let kind = fields.next().ok_or("missing Git kind")?;
        let hash = fields.next().ok_or("missing Git hash")?;
        pinned.insert(
            path.to_owned(),
            (mode.to_owned(), kind.to_owned(), hash.to_owned()),
        );
    }
    let mut file_args = vec![
        "--literal-pathspecs",
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
    ];
    file_args.extend(&allowed);
    let files = git_bytes(root, &file_args, None)?;
    let mut candidates = pinned.keys().cloned().collect::<BTreeSet<_>>();
    for path in files.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        candidates.insert(
            std::str::from_utf8(path)
                .map_err(|e| e.to_string())?
                .to_owned(),
        );
    }
    let mut selected = BTreeSet::new();
    let mut unavailable = Vec::new();
    for path in paths {
        let path = path.as_str();
        if path == "[BLOCKED PATH]"
            || validate_single_path(path).is_err()
            || blocked_path_reason(root, path).is_some()
        {
            unavailable.push(json!({"path":"[BLOCKED PATH]","reason":"protected_or_invalid_path"}));
            continue;
        }
        let in_scope = |candidate: &str| {
            path == "." || candidate == path || candidate.starts_with(&format!("{path}/"))
        };
        // An absent entry is not a baseline for an explicit path: ignored
        // evidence may have existed at capture without ever entering the pin.
        if !pinned.keys().any(|candidate| {
            in_scope(candidate) && lexical_path_block(Path::new(candidate)).is_none()
        }) {
            unavailable.push(json!({"path":path,"reason":"path_not_captured_in_git_pin"}));
            continue;
        }
        for candidate in candidates.iter().filter(|candidate| in_scope(candidate)) {
            if lexical_path_block(Path::new(candidate)).is_some() {
                continue;
            }
            if blocked_path_reason(root, candidate).is_some() {
                unavailable
                    .push(json!({"path":"[BLOCKED PATH]","reason":"protected_or_invalid_path"}));
                continue;
            }
            selected.insert(candidate.clone());
        }
    }
    let mut changed = Vec::new();
    let mut checked = Vec::new();
    for path in selected {
        let original = pinned.get(&path);
        match compare_file(root, &path, original) {
            Ok(true) => {
                checked.push(path);
            }
            Ok(false) => {
                checked.push(path.clone());
                changed.push(path);
            }
            Err(_) => unavailable
                .push(json!({"path":path,"reason":"file_unavailable_unsupported_or_over_1_mib"})),
        }
    }
    Ok(
        json!({"status":if !changed.is_empty(){"stale"}else if !unavailable.is_empty(){"unknown"}else{"fresh"},
        "checked_paths":checked,"changed_paths":changed,"unavailable_paths":unavailable}),
    )
}

fn compare_file(
    root: &Path,
    path: &str,
    original: Option<&(String, String, String)>,
) -> Result<bool, String> {
    if original.is_some_and(|(_, kind, _)| kind != "blob") {
        return Err("unsupported Git entry".to_owned());
    }
    let metadata = match fs::symlink_metadata(root.join(path)) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(original.is_none()),
        Err(e) => return Err(e.to_string()),
    };
    let (mode, bytes, filter) = if metadata.file_type().is_symlink() {
        (
            "120000",
            fs::read_link(root.join(path))
                .map_err(|e| e.to_string())?
                .as_os_str()
                .as_bytes()
                .to_vec(),
            false,
        )
    } else if metadata.is_file() {
        if blocked_path_reason(root, path).is_some() {
            return Err("protected path".to_owned());
        }
        let bytes =
            read_evidence_bytes(fs::File::open(root.join(path)).map_err(|e| e.to_string())?)?;
        (
            if metadata.permissions().mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            },
            bytes,
            true,
        )
    } else {
        return Err("not a file".to_owned());
    };
    let mut args = vec!["hash-object", "--stdin"];
    let filter_path = format!("--path={path}");
    if filter {
        let attributes = git_bytes(root, &["check-attr", "-z", "filter", "--", path], None)?;
        let value = attributes
            .split(|b| *b == 0)
            .nth(2)
            .ok_or("missing filter attribute")?;
        if value != b"unspecified" && value != b"unset" {
            return Err("custom content filters are not executed by a freshness read".to_owned());
        }
        args.push(&filter_path);
    }
    let hash = git_bytes(root, &args, Some(&bytes))?;
    let hash = std::str::from_utf8(&hash)
        .map_err(|e| e.to_string())?
        .trim();
    Ok(original.is_some_and(|(old_mode, _, old_hash)| old_mode == mode && old_hash == hash))
}

fn git_bytes(root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(input) = input {
        let write = child.stdin.take().expect("piped stdin").write_all(input);
        if let Err(error) = write {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
    }
    let bytes = read_evidence_bytes(child.stdout.take().expect("piped stdout"));
    if bytes.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Git evidence unavailable".to_owned());
    }
    bytes
}
