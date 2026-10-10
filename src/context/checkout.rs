use super::{WorkingContextSnapshot, evidence_pointer_path_locator, redact_disclosure_text};
use crate::DiscoveredWorktree;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt, fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckoutInput {
    path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckoutSelector {
    AttachedSpace,
    Explicit(PathBuf),
}

impl CheckoutSelector {
    pub(crate) fn from_input(input: Option<CheckoutInput>) -> Result<Self, CheckoutRefusal> {
        let Some(input) = input else {
            return Ok(Self::AttachedSpace);
        };
        let path = PathBuf::from(input.path);
        if !path.is_absolute() {
            return Err(CheckoutRefusal::RelativePath);
        }
        Ok(Self::Explicit(path))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckoutRefusal {
    RelativePath,
    Missing,
    NotADirectory,
    NotAWorktreeRoot { toplevel: PathBuf },
    BareRepository,
    UnregisteredWorktree,
    DifferentRepository,
    NotAGitWorktree,
    SpaceWorktreeRequired,
    Unverifiable { detail: String },
}

impl fmt::Display for CheckoutRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativePath => formatter.write_str(
                "checkout.path must be an absolute path; it is never resolved against a working \
                 directory",
            ),
            Self::Missing => formatter.write_str("checkout.path does not exist"),
            Self::NotADirectory => formatter.write_str("checkout.path is not a directory"),
            Self::NotAWorktreeRoot { toplevel } => write!(
                formatter,
                "checkout.path is inside the worktree {}; select the worktree root itself",
                redact_disclosure_text(&toplevel.to_string_lossy())
            ),
            Self::BareRepository => formatter
                .write_str("checkout.path is a bare repository, which has no checked-out files"),
            Self::UnregisteredWorktree => formatter.write_str(
                "checkout.path is not a worktree registered with this Space's repository; it \
                 has a .git pointer that `git worktree list` does not report",
            ),
            Self::DifferentRepository => formatter.write_str(
                "checkout.path is a checkout of a different repository than this Space's worktree",
            ),
            Self::NotAGitWorktree => formatter.write_str("checkout.path is not a Git worktree"),
            Self::SpaceWorktreeRequired => formatter.write_str(
                "this Space has no attached Git Worktree, so it has no context root: file and Git \
                 context, and any explicit checkout, are resolved against the Space's own \
                 repository, never the invoking process's directory",
            ),
            Self::Unverifiable { detail } => {
                write!(
                    formatter,
                    "the checkout's identity could not be verified: {detail}"
                )
            }
        }
    }
}

impl From<CheckoutRefusal> for String {
    fn from(refusal: CheckoutRefusal) -> Self {
        refusal.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Timestamp {
    secs: u64,
    nanos: u32,
}

/// Device numbers are left out: the canonical path pins the mount, and btrfs
/// subvolume device numbers change across mounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryLifetime {
    inode: u64,
    birth: Option<Timestamp>,
}

enum LifetimeError {
    Missing,
    NotADirectory,
    Unreadable(io::Error),
}

impl DirectoryLifetime {
    fn read(path: &Path) -> Result<Self, LifetimeError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => LifetimeError::Missing,
            _ => LifetimeError::Unreadable(error),
        })?;
        if !metadata.is_dir() {
            return Err(LifetimeError::NotADirectory);
        }
        let birth = metadata
            .created()
            .ok()
            .and_then(|created| created.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|since| Timestamp {
                secs: since.as_secs(),
                nanos: since.subsec_nanos(),
            });
        Ok(Self {
            inode: metadata.ino(),
            birth,
        })
    }

    /// Inode only when either side lacks a birth time, so a kernel that starts
    /// reporting one does not strand older snapshots.
    fn matches(&self, other: &Self) -> bool {
        self.inode == other.inode
            && match (self.birth, other.birth) {
                (Some(saved), Some(now)) => saved == now,
                _ => true,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Selection {
    SpaceDefault,
    Explicit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckoutIdentity {
    root: PathBuf,
    common_dir: PathBuf,
    admin_dir: PathBuf,
    root_lifetime: DirectoryLifetime,
    admin_lifetime: DirectoryLifetime,
    selection: Selection,
}

impl CheckoutIdentity {
    fn summary(&self, state: &str) -> Value {
        json!({
            "state": state,
            "selection": self.selection,
            "root": redact_disclosure_text(&self.root.to_string_lossy()),
            "label": self
                .root
                .file_name()
                .map(|name| redact_disclosure_text(&name.to_string_lossy())),
            "layout": if self.admin_dir == self.common_dir { "main" } else { "linked" },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CheckoutBinding {
    Bound(CheckoutIdentity),
    Unrooted {},
}

impl CheckoutBinding {
    pub(crate) const FIELD: &'static str = "checkout_binding";
    const VERSION: u64 = 1;

    pub(crate) fn to_json(&self) -> Value {
        let mut record = serde_json::to_value(self).expect("proved checkout paths are UTF-8");
        record["version"] = json!(Self::VERSION);
        record
    }
}

pub(crate) fn bind_checkout(
    selector: CheckoutSelector,
    snapshot: &WorkingContextSnapshot,
    capture_git_state: bool,
    space: &DiscoveredWorktree,
) -> Result<CheckoutBinding, CheckoutRefusal> {
    let needs_checkout = capture_git_state
        || snapshot
            .evidence
            .iter()
            .any(|pointer| evidence_pointer_path_locator(pointer).is_some());
    if selector == CheckoutSelector::AttachedSpace && !needs_checkout {
        return Ok(CheckoutBinding::Unrooted {});
    }
    let space_root = space_root(space).ok_or(CheckoutRefusal::SpaceWorktreeRequired)?;
    let (candidate, selection) = match selector {
        CheckoutSelector::AttachedSpace => (space_root.clone(), Selection::SpaceDefault),
        CheckoutSelector::Explicit(path) => (path, Selection::Explicit),
    };
    prove_checkout(&space_root, &candidate, selection).map(CheckoutBinding::Bound)
}

fn prove_checkout(
    space_root: &Path,
    candidate: &Path,
    selection: Selection,
) -> Result<CheckoutIdentity, CheckoutRefusal> {
    let trusted = space_root.canonicalize().map_err(unverifiable)?;
    let root = candidate
        .canonicalize()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => CheckoutRefusal::Missing,
            _ => unverifiable(error),
        })?;
    if !fs::metadata(&root).map_err(unverifiable)?.is_dir() {
        return Err(CheckoutRefusal::NotADirectory);
    }
    if root.to_str().is_none() {
        return Err(unverifiable("the checkout path is not valid UTF-8"));
    }
    if root != trusted {
        let registered = registered_worktrees(&trusted)?;
        if !registered
            .iter()
            .any(|worktree| !worktree.bare && worktree.root == root)
        {
            return Err(classify_unregistered(&trusted, &root, &registered));
        }
    }
    let dirs = git_dirs(&root)?;
    if dirs.toplevel != root {
        return Err(CheckoutRefusal::NotAWorktreeRoot {
            toplevel: dirs.toplevel,
        });
    }
    if root != trusted && git_dirs(&trusted)?.common != dirs.common {
        return Err(CheckoutRefusal::DifferentRepository);
    }
    let lifetime = |path: &Path| {
        DirectoryLifetime::read(path).map_err(|error| match error {
            LifetimeError::Missing => unverifiable(format!("{} is missing", path.display())),
            LifetimeError::NotADirectory => {
                unverifiable(format!("{} is not a directory", path.display()))
            }
            LifetimeError::Unreadable(error) => unverifiable(error),
        })
    };
    if dirs.admin.to_str().is_none() || dirs.common.to_str().is_none() {
        return Err(unverifiable("the Git directories are not valid UTF-8"));
    }
    Ok(CheckoutIdentity {
        root_lifetime: lifetime(&root)?,
        admin_lifetime: lifetime(&dirs.admin)?,
        root,
        common_dir: dirs.common,
        admin_dir: dirs.admin,
        selection,
    })
}

fn unverifiable(detail: impl fmt::Display) -> CheckoutRefusal {
    CheckoutRefusal::Unverifiable {
        detail: detail.to_string(),
    }
}

fn classify_unregistered(
    trusted: &Path,
    root: &Path,
    registered: &[RegisteredWorktree],
) -> CheckoutRefusal {
    if registered
        .iter()
        .any(|worktree| worktree.bare && worktree.root == root)
    {
        return CheckoutRefusal::BareRepository;
    }
    let dot_git = root.join(".git");
    match fs::symlink_metadata(&dot_git) {
        Ok(metadata) if metadata.is_file() => {
            let pointer = fs::read_to_string(&dot_git).ok();
            let admin = pointer
                .as_deref()
                .and_then(|pointer| pointer.trim().strip_prefix("gitdir:"))
                .and_then(|admin| root.join(admin.trim()).canonicalize().ok());
            match (admin, git_dirs(trusted)) {
                (Some(admin), Ok(space)) if admin.starts_with(&space.common) => {
                    CheckoutRefusal::UnregisteredWorktree
                }
                _ => CheckoutRefusal::DifferentRepository,
            }
        }
        Ok(_) => CheckoutRefusal::DifferentRepository,
        Err(_) if root.join("HEAD").is_file() && root.join("objects").is_dir() => {
            CheckoutRefusal::BareRepository
        }
        Err(_) => registered
            .iter()
            .find(|worktree| root.starts_with(&worktree.root))
            .map_or(CheckoutRefusal::NotAGitWorktree, |worktree| {
                CheckoutRefusal::NotAWorktreeRoot {
                    toplevel: worktree.root.clone(),
                }
            }),
    }
}

struct RegisteredWorktree {
    root: PathBuf,
    bare: bool,
}

fn registered_worktrees(trusted: &Path) -> Result<Vec<RegisteredWorktree>, CheckoutRefusal> {
    let listing =
        identity_git(trusted, &["worktree", "list", "--porcelain", "-z"]).map_err(unverifiable)?;
    let mut worktrees = Vec::new();
    let mut current: Option<RegisteredWorktree> = None;
    for field in listing.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            current = Path::new(path)
                .canonicalize()
                .ok()
                .map(|root| RegisteredWorktree { root, bare: false });
        } else if field == "bare" {
            if let Some(worktree) = current.as_mut() {
                worktree.bare = true;
            }
        } else if field.is_empty() {
            worktrees.extend(current.take());
        }
    }
    worktrees.extend(current);
    Ok(worktrees)
}

struct GitDirs {
    toplevel: PathBuf,
    admin: PathBuf,
    common: PathBuf,
}

fn git_dirs(root: &Path) -> Result<GitDirs, CheckoutRefusal> {
    let output = identity_git(
        root,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
    )
    .map_err(unverifiable)?;
    let mut lines = output.lines().map(|line| root.join(line).canonicalize());
    let mut next = || {
        lines
            .next()
            .ok_or_else(|| unverifiable("git rev-parse returned too few paths"))?
            .map_err(unverifiable)
    };
    Ok(GitDirs {
        toplevel: next()?,
        admin: next()?,
        common: next()?,
    })
}

fn identity_git(directory: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckoutUnavailable {
    RootMissing,
    RootReplaced,
    AdminMissing,
    AdminReplaced,
    PointerRetargeted,
    RepositoryMismatch,
    IdentityUnverifiable,
    UnsupportedBindingVersion,
    InvalidBinding,
}

impl CheckoutUnavailable {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RootMissing => "root_missing",
            Self::RootReplaced => "root_replaced",
            Self::AdminMissing => "admin_missing",
            Self::AdminReplaced => "admin_replaced",
            Self::PointerRetargeted => "pointer_retargeted",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::IdentityUnverifiable => "identity_unverifiable",
            Self::UnsupportedBindingVersion => "unsupported_binding_version",
            Self::InvalidBinding => "invalid_binding",
        }
    }
}

fn revalidate(identity: &CheckoutIdentity) -> Result<(), CheckoutUnavailable> {
    match identity.root.canonicalize() {
        Ok(path) if path == identity.root => {}
        Ok(_) => return Err(CheckoutUnavailable::RootReplaced),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(CheckoutUnavailable::RootMissing);
        }
        Err(_) => return Err(CheckoutUnavailable::IdentityUnverifiable),
    }
    match DirectoryLifetime::read(&identity.root) {
        Ok(now) if identity.root_lifetime.matches(&now) => {}
        Ok(_) | Err(LifetimeError::NotADirectory) => {
            return Err(CheckoutUnavailable::RootReplaced);
        }
        Err(LifetimeError::Missing) => return Err(CheckoutUnavailable::RootMissing),
        Err(LifetimeError::Unreadable(_)) => {
            return Err(CheckoutUnavailable::IdentityUnverifiable);
        }
    }
    match DirectoryLifetime::read(&identity.admin_dir) {
        Ok(now) if identity.admin_lifetime.matches(&now) => {}
        Ok(_) | Err(LifetimeError::NotADirectory) => {
            return Err(CheckoutUnavailable::AdminReplaced);
        }
        Err(LifetimeError::Missing) => return Err(CheckoutUnavailable::AdminMissing),
        Err(LifetimeError::Unreadable(_)) => {
            return Err(CheckoutUnavailable::IdentityUnverifiable);
        }
    }
    let Ok(dirs) = git_dirs(&identity.root) else {
        return Err(CheckoutUnavailable::PointerRetargeted);
    };
    if dirs.toplevel != identity.root || dirs.admin != identity.admin_dir {
        return Err(CheckoutUnavailable::PointerRetargeted);
    }
    if dirs.common != identity.common_dir {
        return Err(CheckoutUnavailable::RepositoryMismatch);
    }
    Ok(())
}

enum SavedBinding {
    Legacy,
    Present(CheckoutBinding),
    Unsupported(CheckoutUnavailable),
}

impl SavedBinding {
    fn parse(payload: &Value) -> Self {
        let Some(record) = payload.get(CheckoutBinding::FIELD) else {
            return Self::Legacy;
        };
        let Some(version) = record.get("version").and_then(Value::as_u64) else {
            return Self::Unsupported(CheckoutUnavailable::InvalidBinding);
        };
        if version != CheckoutBinding::VERSION {
            return Self::Unsupported(CheckoutUnavailable::UnsupportedBindingVersion);
        }
        let mut record = record.clone();
        record
            .as_object_mut()
            .expect("a versioned binding is an object")
            .remove("version");
        serde_json::from_value(record).map_or(
            Self::Unsupported(CheckoutUnavailable::InvalidBinding),
            Self::Present,
        )
    }
}

#[derive(Debug)]
pub(crate) enum CheckoutAccess {
    Checkout(CheckoutIdentity),
    Unrooted,
    Legacy(Option<PathBuf>),
    Unavailable(CheckoutUnavailable, Option<CheckoutIdentity>),
}

impl From<CheckoutBinding> for CheckoutAccess {
    fn from(binding: CheckoutBinding) -> Self {
        match binding {
            CheckoutBinding::Bound(identity) => Self::Checkout(identity),
            CheckoutBinding::Unrooted {} => Self::Unrooted,
        }
    }
}

impl CheckoutAccess {
    pub(crate) fn root(&self) -> Result<&Path, String> {
        match self {
            Self::Checkout(identity) => Ok(&identity.root),
            Self::Legacy(Some(root)) => Ok(root),
            Self::Legacy(None) => Err(CheckoutRefusal::SpaceWorktreeRequired.to_string()),
            Self::Unrooted => Err(
                "this Working Context Snapshot was published without a checkout, so it carries \
                 no file or Git context"
                    .to_owned(),
            ),
            Self::Unavailable(reason, _) => Err(format!(
                "the snapshot's checkout is unavailable ({}); publish a new snapshot from the \
                 checkout's current location. Namedrop never reads another worktree in its place",
                reason.as_str()
            )),
        }
    }

    pub(crate) fn public_summary(&self) -> Value {
        match self {
            Self::Checkout(identity) => identity.summary("bound"),
            Self::Unrooted => json!({"state": "unrooted"}),
            Self::Legacy(root) => json!({
                "state": "legacy",
                "root": root.as_ref().map(|root| redact_disclosure_text(&root.to_string_lossy())),
            }),
            Self::Unavailable(reason, identity) => {
                let mut summary = identity.as_ref().map_or_else(
                    || json!({"state": "unavailable"}),
                    |identity| identity.summary("unavailable"),
                );
                summary["reason"] = json!(reason.as_str());
                summary
            }
        }
    }
}

pub(crate) fn resolve_saved_checkout(
    payload: &Value,
    space: &DiscoveredWorktree,
) -> CheckoutAccess {
    match SavedBinding::parse(payload) {
        SavedBinding::Legacy => CheckoutAccess::Legacy(space_root(space)),
        SavedBinding::Unsupported(reason) => CheckoutAccess::Unavailable(reason, None),
        SavedBinding::Present(CheckoutBinding::Unrooted {}) => CheckoutAccess::Unrooted,
        SavedBinding::Present(CheckoutBinding::Bound(identity)) => match revalidate(&identity) {
            Ok(()) => CheckoutAccess::Checkout(identity),
            Err(reason) => CheckoutAccess::Unavailable(reason, Some(identity)),
        },
    }
}

pub(crate) fn public_snapshot(mut payload: Value, access: &CheckoutAccess) -> Value {
    if matches!(access, CheckoutAccess::Legacy(_)) {
        return payload;
    }
    if let Some(fields) = payload.as_object_mut() {
        fields.remove(CheckoutBinding::FIELD);
        fields.insert("checkout".to_owned(), access.public_summary());
    }
    payload
}

fn space_root(space: &DiscoveredWorktree) -> Option<PathBuf> {
    space
        .worktree_attached
        .then(|| PathBuf::from(space.worktree_id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvidenceKind, EvidencePointer, WorktreeId};
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        base: PathBuf,
        main: PathBuf,
        worker: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "namedrop-checkout-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&base).unwrap();
            let base = base.canonicalize().unwrap();
            let main = base.join("main");
            fs::create_dir(&main).unwrap();
            git(&main, &["init", "-q", "-b", "main"]);
            fs::write(main.join("a.txt"), "one\n").unwrap();
            git(&main, &["add", "-A"]);
            git(&main, &["commit", "-q", "-m", "one"]);
            let worker = base.join("worker");
            git(
                &main,
                &["worktree", "add", "-q", "-b", "worker", path(&worker)],
            );
            Self { base, main, worker }
        }

        fn select(&self, candidate: &Path) -> Result<CheckoutIdentity, CheckoutRefusal> {
            match bind_checkout(
                CheckoutSelector::Explicit(candidate.to_owned()),
                &snapshot(Vec::new()),
                false,
                &space(Some(&self.main)),
            )? {
                CheckoutBinding::Bound(identity) => Ok(identity),
                CheckoutBinding::Unrooted {} => panic!("an explicit selector bound no checkout"),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn path(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    fn git(directory: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Checkout Test",
                "-c",
                "user.email=checkout@test",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn copy_tree(from: &Path, to: &Path) {
        let status = Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", from.display()))
            .arg(to)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn space(root: Option<&Path>) -> DiscoveredWorktree {
        DiscoveredWorktree {
            worktree_id: WorktreeId::new(
                root.map_or("herdr-space:workspace-1".to_owned(), |root| {
                    root.display().to_string()
                }),
            ),
            workspace_id: "workspace-1".to_owned(),
            worktree_attached: root.is_some(),
            label: None,
            branch: None,
            panes: Vec::new(),
        }
    }

    fn snapshot(evidence: Vec<EvidencePointer>) -> WorkingContextSnapshot {
        WorkingContextSnapshot {
            objective: "Review".to_owned(),
            applicable_user_instructions: Vec::new(),
            decisions: Vec::new(),
            completed_work: Vec::new(),
            current_work: Vec::new(),
            blockers: Vec::new(),
            evidence,
        }
    }

    fn pointer(kind: EvidenceKind, locator: &str) -> EvidencePointer {
        EvidencePointer {
            kind,
            locator: locator.to_owned(),
        }
    }

    #[test]
    fn an_alias_binds_the_canonical_root_and_retargeting_it_redirects_nothing() {
        let fixture = Fixture::new();
        let alias = fixture.base.join("alias");
        symlink(&fixture.worker, &alias).unwrap();
        let identity = fixture.select(&alias).unwrap();
        assert_eq!(identity, fixture.select(&fixture.worker).unwrap());
        assert_eq!(identity.root, fixture.worker);
        let stored =
            json!({ CheckoutBinding::FIELD: CheckoutBinding::Bound(identity.clone()).to_json() });
        fs::remove_file(&alias).unwrap();
        symlink(&fixture.main, &alias).unwrap();
        assert_eq!(revalidate(&identity), Ok(()));
        let access = resolve_saved_checkout(&stored, &space(Some(&alias)));
        assert_eq!(access.root(), Ok(fixture.worker.as_path()));
    }

    #[test]
    fn edits_commits_branch_switches_and_detached_head_keep_identity() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        assert_ne!(identity.admin_dir, identity.common_dir);
        fs::write(fixture.worker.join("a.txt"), "two\n").unwrap();
        fs::write(fixture.worker.join("new.txt"), "new\n").unwrap();
        git(&fixture.worker, &["add", "-A"]);
        git(&fixture.worker, &["commit", "-q", "-m", "two"]);
        git(&fixture.worker, &["checkout", "-q", "--detach"]);
        assert_eq!(revalidate(&identity), Ok(()));
        assert_eq!(fixture.select(&fixture.worker).unwrap(), identity);
        git(&fixture.worker, &["checkout", "-q", "-b", "renamed"]);
        assert_eq!(revalidate(&identity), Ok(()));
        let main = fixture.select(&fixture.main).unwrap();
        assert_eq!(main.admin_dir, main.common_dir);
        assert_eq!(main.common_dir, identity.common_dir);
    }

    #[test]
    fn selectors_that_name_no_registered_worktree_root_are_refused() {
        let fixture = Fixture::new();
        assert_eq!(
            CheckoutSelector::from_input(Some(CheckoutInput {
                path: "worker".to_owned()
            })),
            Err(CheckoutRefusal::RelativePath)
        );
        fs::create_dir(fixture.worker.join("subdir")).unwrap();
        assert_eq!(
            fixture.select(&fixture.worker.join("subdir")),
            Err(CheckoutRefusal::NotAWorktreeRoot {
                toplevel: fixture.worker.clone()
            })
        );
        let plain = fixture.base.join("plain");
        fs::create_dir(&plain).unwrap();
        assert_eq!(
            fixture.select(&plain),
            Err(CheckoutRefusal::NotAGitWorktree)
        );
        let other = fixture.base.join("other");
        fs::create_dir(&other).unwrap();
        git(&other, &["init", "-q"]);
        assert_eq!(
            fixture.select(&other),
            Err(CheckoutRefusal::DifferentRepository)
        );
        let bare = fixture.base.join("bare.git");
        git(&fixture.base, &["init", "-q", "--bare", path(&bare)]);
        assert_eq!(fixture.select(&bare), Err(CheckoutRefusal::BareRepository));
        let copy = fixture.base.join("pointer-copy");
        fs::create_dir(&copy).unwrap();
        fs::copy(fixture.worker.join(".git"), copy.join(".git")).unwrap();
        assert_eq!(
            fixture.select(&copy),
            Err(CheckoutRefusal::UnregisteredWorktree)
        );
        assert_eq!(
            fixture.select(&fixture.base.join("missing")),
            Err(CheckoutRefusal::Missing)
        );
        assert_eq!(
            fixture.select(&fixture.worker.join("a.txt")),
            Err(CheckoutRefusal::NotADirectory)
        );
    }

    #[test]
    fn only_context_that_reaches_files_or_git_needs_an_attached_space() {
        let fixture = Fixture::new();
        assert_eq!(
            bind_checkout(
                CheckoutSelector::Explicit(fixture.worker.clone()),
                &snapshot(Vec::new()),
                false,
                &space(None),
            ),
            Err(CheckoutRefusal::SpaceWorktreeRequired)
        );
        assert_eq!(
            bind_checkout(
                CheckoutSelector::AttachedSpace,
                &snapshot(vec![pointer(EvidenceKind::File, "a.txt")]),
                false,
                &space(None),
            ),
            Err(CheckoutRefusal::SpaceWorktreeRequired)
        );
        let plain = fixture.base.join("plain");
        fs::create_dir(&plain).unwrap();
        assert_eq!(
            bind_checkout(
                CheckoutSelector::AttachedSpace,
                &snapshot(vec![pointer(EvidenceKind::PaneOutput, "current")]),
                false,
                &space(Some(&plain)),
            ),
            Ok(CheckoutBinding::Unrooted {})
        );
        let CheckoutBinding::Bound(identity) = bind_checkout(
            CheckoutSelector::AttachedSpace,
            &snapshot(Vec::new()),
            true,
            &space(Some(&fixture.main)),
        )
        .unwrap() else {
            panic!("Git capture bound no checkout");
        };
        assert_eq!(identity.root, fixture.main);
        assert_eq!(identity.selection, Selection::SpaceDefault);
    }

    #[test]
    fn a_moved_or_removed_root_is_missing() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        let moved = fixture.base.join("worker-moved");
        git(
            &fixture.main,
            &["worktree", "move", path(&fixture.worker), path(&moved)],
        );
        assert_eq!(revalidate(&identity), Err(CheckoutUnavailable::RootMissing));
        let identity = fixture.select(&moved).unwrap();
        fs::rename(&moved, fixture.base.join("aside")).unwrap();
        assert_eq!(revalidate(&identity), Err(CheckoutUnavailable::RootMissing));
    }

    #[test]
    fn a_root_replaced_under_a_surviving_admin_dir_is_detected() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        let aside = fixture.base.join("worker-aside");
        fs::rename(&fixture.worker, &aside).unwrap();
        fs::create_dir(&fixture.worker).unwrap();
        copy_tree(&aside, &fixture.worker);
        assert_eq!(
            revalidate(&identity),
            Err(CheckoutUnavailable::RootReplaced)
        );
        let replacement = fixture.select(&fixture.worker).unwrap();
        assert_eq!(replacement.admin_lifetime, identity.admin_lifetime);
        assert_ne!(replacement.root_lifetime, identity.root_lifetime);
    }

    #[test]
    fn a_root_replaced_by_a_symlink_to_the_original_is_detected() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        let aside = fixture.base.join("worker-aside");
        fs::rename(&fixture.worker, &aside).unwrap();
        symlink(&aside, &fixture.worker).unwrap();
        assert_eq!(
            revalidate(&identity),
            Err(CheckoutUnavailable::RootReplaced)
        );
    }

    #[test]
    fn removing_and_re_adding_a_worktree_at_the_same_path_is_detected() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        git(
            &fixture.main,
            &["worktree", "remove", "--force", path(&fixture.worker)],
        );
        git(
            &fixture.main,
            &["worktree", "add", "-q", path(&fixture.worker), "worker"],
        );
        let readded = fixture.select(&fixture.worker).unwrap();
        assert_eq!(
            (&readded.root, &readded.admin_dir),
            (&identity.root, &identity.admin_dir)
        );
        assert_eq!(
            revalidate(&identity),
            Err(CheckoutUnavailable::RootReplaced)
        );
    }

    #[test]
    fn a_main_root_replaced_with_its_git_dir_moved_intact_is_detected() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.main).unwrap();
        let aside = fixture.base.join("main-aside");
        fs::rename(&fixture.main, &aside).unwrap();
        fs::create_dir(&fixture.main).unwrap();
        copy_tree(&aside, &fixture.main);
        fs::remove_dir_all(fixture.main.join(".git")).unwrap();
        fs::rename(aside.join(".git"), fixture.main.join(".git")).unwrap();
        assert_eq!(
            revalidate(&identity),
            Err(CheckoutUnavailable::RootReplaced)
        );
        let replacement = fixture.select(&fixture.main).unwrap();
        assert_eq!(replacement.admin_lifetime, identity.admin_lifetime);
    }

    #[test]
    fn a_missing_or_recreated_admin_dir_is_detected() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        let aside = fixture.base.join("admin-aside");
        fs::rename(&identity.admin_dir, &aside).unwrap();
        assert_eq!(
            revalidate(&identity),
            Err(CheckoutUnavailable::AdminMissing)
        );
        fs::create_dir(&identity.admin_dir).unwrap();
        copy_tree(&aside, &identity.admin_dir);
        assert_eq!(
            revalidate(&identity),
            Err(CheckoutUnavailable::AdminReplaced)
        );
    }

    #[test]
    fn a_root_pointer_retargeted_to_another_admin_dir_is_detected() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        let other = fixture.base.join("other-worker");
        git(
            &fixture.main,
            &["worktree", "add", "-q", "-b", "other", path(&other)],
        );
        let other_admin = fixture.select(&other).unwrap().admin_dir;
        fs::write(
            fixture.worker.join(".git"),
            format!("gitdir: {}\n", other_admin.display()),
        )
        .unwrap();
        assert_eq!(
            revalidate(&identity),
            Err(CheckoutUnavailable::PointerRetargeted)
        );
    }

    #[test]
    fn lifetimes_compare_inode_only_when_a_birth_time_is_missing() {
        let birth = |secs| Some(Timestamp { secs, nanos: 7 });
        let saved = DirectoryLifetime {
            inode: 9,
            birth: birth(100),
        };
        let reused = DirectoryLifetime {
            inode: 9,
            birth: birth(200),
        };
        let without_birth = DirectoryLifetime {
            inode: 9,
            birth: None,
        };
        let other_inode = DirectoryLifetime {
            inode: 10,
            birth: None,
        };
        assert!(saved.matches(&saved));
        assert!(!saved.matches(&reused));
        assert!(saved.matches(&without_birth));
        assert!(without_birth.matches(&saved));
        assert!(!without_birth.matches(&other_inode));
    }

    #[test]
    fn a_present_binding_never_resolves_as_legacy() {
        let fixture = Fixture::new();
        let attached = space(Some(&fixture.main));
        let identity = fixture.select(&fixture.worker).unwrap();
        let bound =
            json!({ CheckoutBinding::FIELD: CheckoutBinding::Bound(identity.clone()).to_json() });
        assert_eq!(bound[CheckoutBinding::FIELD]["version"], 1);
        assert!(matches!(
            resolve_saved_checkout(&bound, &attached),
            CheckoutAccess::Checkout(saved) if saved == identity
        ));
        let unrooted = json!({ CheckoutBinding::FIELD: CheckoutBinding::Unrooted {}.to_json() });
        assert_eq!(
            unrooted[CheckoutBinding::FIELD],
            json!({"kind": "unrooted", "version": 1})
        );
        assert!(matches!(
            resolve_saved_checkout(&unrooted, &attached),
            CheckoutAccess::Unrooted
        ));
        assert!(matches!(
            resolve_saved_checkout(&json!({"objective": "legacy"}), &attached),
            CheckoutAccess::Legacy(Some(root)) if root == fixture.main
        ));
        let mut newer = bound.clone();
        newer[CheckoutBinding::FIELD]["version"] = json!(2);
        let mut extra_field = bound.clone();
        extra_field[CheckoutBinding::FIELD]["device"] = json!(1);
        for (record, reason) in [
            (newer, CheckoutUnavailable::UnsupportedBindingVersion),
            (extra_field, CheckoutUnavailable::InvalidBinding),
            (
                json!({ CheckoutBinding::FIELD: {"kind": "unrooted"} }),
                CheckoutUnavailable::InvalidBinding,
            ),
            (
                json!({ CheckoutBinding::FIELD: {"version": 1, "kind": "unrooted", "root": "/x"} }),
                CheckoutUnavailable::InvalidBinding,
            ),
            (
                json!({ CheckoutBinding::FIELD: {"version": 1, "kind": "bound", "root": 3} }),
                CheckoutUnavailable::InvalidBinding,
            ),
            (
                json!({ CheckoutBinding::FIELD: null }),
                CheckoutUnavailable::InvalidBinding,
            ),
        ] {
            assert!(
                matches!(
                    resolve_saved_checkout(&record, &attached),
                    CheckoutAccess::Unavailable(found, None) if found == reason
                ),
                "{record}"
            );
        }
    }

    #[test]
    fn the_public_summary_shows_the_checkout_but_never_its_identity_proof() {
        let fixture = Fixture::new();
        let identity = fixture.select(&fixture.worker).unwrap();
        let summary = CheckoutAccess::Checkout(identity.clone()).public_summary();
        assert_eq!(
            summary,
            json!({
                "state": "bound",
                "selection": "explicit",
                "root": path(&fixture.worker),
                "label": "worker",
                "layout": "linked",
            })
        );
        let unavailable =
            CheckoutAccess::Unavailable(CheckoutUnavailable::RootMissing, Some(identity.clone()))
                .public_summary();
        assert_eq!(unavailable["state"], "unavailable");
        assert_eq!(unavailable["reason"], "root_missing");
        assert_eq!(unavailable["root"], path(&fixture.worker));
        for shown in [summary, unavailable] {
            let shown = shown.to_string();
            assert!(!shown.contains(path(&identity.admin_dir)), "{shown}");
            assert!(!shown.contains("inode"), "{shown}");
        }
    }
}
