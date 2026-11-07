use super::*;

pub const FINISHED_GRACE_MS: i64 = 10 * 60 * 1000;
pub const PARENT_GRACE_MS: i64 = 60 * 60 * 1000;
pub const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
pub(super) const CHECKPOINT_CAP: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParentPresence {
    Present,
    Absent,
    Unknown,
}

#[derive(Deserialize, Serialize)]
struct ParentMetadata {
    parent: ParentIdentity,
    missing_since_ms: Option<i64>,
    last_activity_ms: i64,
}

#[derive(Deserialize, Serialize)]
pub(super) struct LifecycleEntry {
    pub(super) at_ms: i64,
    pub(super) event: Event,
}

#[derive(Deserialize, Serialize)]
pub(super) struct LifecycleCheckpoint {
    pub(super) parent: ParentIdentity,
    pub(super) proof_floor_ms: Option<i64>,
    pub(super) entries: Vec<LifecycleEntry>,
}

impl LifecycleCheckpoint {
    pub(super) fn facts(self) -> Vec<Fact> {
        self.entries
            .into_iter()
            .map(|entry| Fact {
                parent: self.parent.clone(),
                at_ms: entry.at_ms,
                event: entry.event,
            })
            .collect()
    }
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum LifecycleGroup {
    Resume,
    UntypedBarrier,
    TypedBarrierTime,
    UntypedStop,
    TypedStopTime,
    End,
    UnresolvedBarrier(String),
    UnresolvedStop(String),
}

struct CapturedFile {
    path: PathBuf,
    metadata: std::fs::Metadata,
}

impl CapturedFile {
    fn unchanged(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(&self.path).is_ok_and(|current| {
            current.is_file()
                && current.dev() == self.metadata.dev()
                && current.ino() == self.metadata.ino()
                && current.mtime() == self.metadata.mtime()
                && current.mtime_nsec() == self.metadata.mtime_nsec()
                && current.ctime() == self.metadata.ctime()
                && current.ctime_nsec() == self.metadata.ctime_nsec()
                && current.len() == self.metadata.len()
        })
    }

    fn expire(&self, directory: &Path, now_ms: i64, grace_ms: i64) {
        use std::os::unix::fs::MetadataExt;
        if !self.unchanged() {
            return;
        }
        let receipt = if fact_path(&self.path) {
            read_json::<Fact>(&self.path, FACT_CAP).map(|fact| fact.at_ms)
        } else if self.path == directory.join("identity") {
            read_json::<ParentMetadata>(&self.path, FACT_CAP).map(|parent| parent.last_activity_ms)
        } else if self.path == directory.join("lifecycle") {
            read_json::<LifecycleCheckpoint>(&self.path, CHECKPOINT_CAP)
                .and_then(|checkpoint| checkpoint.entries.iter().map(|entry| entry.at_ms).max())
        } else if self.path.parent() == Some(directory.join("tombstones").as_path()) {
            read_json::<i64>(&self.path, 32)
        } else {
            None
        };
        let at_ms = receipt.unwrap_or_else(|| {
            self.metadata
                .mtime()
                .saturating_mul(1_000)
                .saturating_add(self.metadata.mtime_nsec() / 1_000_000)
        });
        if now_ms.saturating_sub(at_ms) >= grace_ms && self.unchanged() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn checkpoint(
    parent: &ParentIdentity,
    facts: &[Fact],
    now_ms: i64,
    previous_floor: Option<i64>,
) -> LifecycleCheckpoint {
    use std::collections::{BTreeMap, BTreeSet};
    let mut prompts: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, fact) in facts.iter().enumerate() {
        if let Event::Prompt { generation } = &fact.event {
            let previous = prompts.entry(generation).or_insert(index);
            if fact.at_ms < facts[*previous].at_ms {
                *previous = index;
            }
        }
    }
    let prompt_time = |generation: &str| prompts.get(generation).map(|index| facts[*index].at_ms);
    let mut selected = BTreeSet::new();
    let mut groups: BTreeMap<LifecycleGroup, usize> = BTreeMap::new();
    let mut strongest_stop: Option<((i64, i64), usize)> = None;
    let mut strongest_barrier: Option<(i64, usize)> = None;
    for (index, fact) in facts.iter().enumerate() {
        if matches!(fact.event, Event::ParentEnd | Event::ParentEndFor { .. }) {
            let previous = groups.entry(LifecycleGroup::End).or_insert(index);
            if fact.at_ms > facts[*previous].at_ms {
                *previous = index;
            }
        }
        let (group, frontier, barrier) = match &fact.event {
            Event::Prompt { .. } | Event::Start { .. } | Event::Stop { .. } => continue,
            Event::Resume => (LifecycleGroup::Resume, None, false),
            Event::ParentStop { generation, .. } => (
                if generation.is_some() {
                    LifecycleGroup::TypedStopTime
                } else {
                    LifecycleGroup::UntypedStop
                },
                generation.as_deref(),
                false,
            ),
            Event::ParentEnd | Event::Reset => (LifecycleGroup::UntypedBarrier, None, true),
            Event::ParentEndFor { generation } | Event::ResetFor { generation } => (
                LifecycleGroup::TypedBarrierTime,
                Some(generation.as_str()),
                true,
            ),
        };
        let previous = groups.entry(group).or_insert(index);
        if fact.at_ms > facts[*previous].at_ms {
            *previous = index;
        }
        if let Some(frontier) = frontier {
            if let Some(time) = prompt_time(frontier) {
                if barrier {
                    if strongest_barrier.is_none_or(|(old, _)| time > old) {
                        strongest_barrier = Some((time, index));
                    }
                } else {
                    let order = (time, fact.at_ms);
                    if strongest_stop.is_none_or(|(old, _)| order > old) {
                        strongest_stop = Some((order, index));
                    }
                }
            } else if now_ms.saturating_sub(fact.at_ms) < RETENTION_MS {
                let group = if barrier {
                    LifecycleGroup::UnresolvedBarrier(frontier.to_owned())
                } else {
                    LifecycleGroup::UnresolvedStop(frontier.to_owned())
                };
                let previous = groups.entry(group).or_insert(index);
                if fact.at_ms > facts[*previous].at_ms {
                    *previous = index;
                }
            }
        }
    }
    selected.extend(groups.values().copied());
    if let Some(((frontier, _), _)) = strongest_stop {
        let mut tied = BTreeMap::<&str, usize>::new();
        for (index, fact) in facts.iter().enumerate() {
            if let Event::ParentStop {
                generation: Some(turn),
                ..
            } = &fact.event
                && prompt_time(turn) == Some(frontier)
            {
                let previous = tied.entry(turn).or_insert(index);
                if fact.at_ms > facts[*previous].at_ms {
                    *previous = index;
                }
            }
        }
        selected.extend(tied.values().copied());
    }
    selected.extend(strongest_barrier.map(|(_, index)| index));
    let cutoff = now_ms.saturating_sub(MAX_RUN_AGE_MS);
    let latest_prompt = prompts
        .values()
        .copied()
        .max_by_key(|index| facts[*index].at_ms);
    let required: Vec<_> = selected
        .iter()
        .filter_map(|index| match &facts[*index].event {
            Event::ParentStop {
                generation: Some(generation),
                ..
            }
            | Event::ParentEndFor { generation }
            | Event::ResetFor { generation } => prompts.get(generation.as_str()).copied(),
            _ => None,
        })
        .collect();
    selected.extend(required);
    for fact in facts {
        if let Event::Start { generation, .. } = &fact.event
            && now_ms.saturating_sub(fact.at_ms) < MAX_RUN_AGE_MS
        {
            let proof = match generation {
                Some(turn) => prompts.get(turn.as_str()).copied(),
                None => prompts
                    .values()
                    .copied()
                    .filter(|index| facts[*index].at_ms <= fact.at_ms)
                    .max_by_key(|index| facts[*index].at_ms),
            };
            selected.extend(proof);
        }
    }
    selected.extend(latest_prompt);
    for index in prompts.values() {
        if facts[*index].at_ms >= cutoff {
            selected.insert(*index);
        }
    }
    let dropped = prompts.values().any(|index| !selected.contains(index));
    LifecycleCheckpoint {
        parent: parent.clone(),
        proof_floor_ms: if dropped {
            Some(previous_floor.map_or(cutoff, |old| old.max(cutoff)))
        } else {
            previous_floor
        },
        entries: selected
            .into_iter()
            .map(|index| LifecycleEntry {
                at_ms: facts[index].at_ms,
                event: facts[index].event.clone(),
            })
            .collect(),
    }
}

/// All absence decisions come from a complete binding snapshot. Unknown status
/// preserves any existing grace but cannot advance it to destructive cleanup.
pub fn maintain(
    state: &Path,
    now_ms: i64,
    presence: impl Fn(&ParentIdentity) -> ParentPresence,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let root = state.join("subagents");
    if !real_directory(&root) {
        return Ok(());
    }
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(".maintenance.lock"))?;
    // Only collectors coordinate. Hooks never acquire or wait for this lock.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(());
    }
    let parents = root.join("v1");
    if !real_directory(&parents) {
        return Ok(());
    }
    for entry in std::fs::read_dir(parents)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        maintain_parent(state, &entry.path(), now_ms, &presence, || {})?;
    }
    Ok(())
}

fn maintain_parent(
    state: &Path,
    directory: &Path,
    now_ms: i64,
    presence: &impl Fn(&ParentIdentity) -> ParentPresence,
    after_capture: impl FnOnce(),
) -> std::io::Result<()> {
    let mut captured: Vec<_> = std::fs::read_dir(directory)?.collect::<Result<_, _>>()?;
    let inactive = directory.join("inactive");
    if std::fs::symlink_metadata(&inactive).is_ok() {
        if !real_directory(&inactive) {
            return Ok(());
        }
        captured.extend(std::fs::read_dir(&inactive)?.collect::<Result<Vec<_>, _>>()?);
    }
    let tombstones = directory.join("tombstones");
    if std::fs::symlink_metadata(&tombstones).is_ok() && !real_directory(&tombstones) {
        return Ok(());
    }
    let markers: Vec<_> = if real_directory(&tombstones) {
        std::fs::read_dir(&tombstones)?.collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    let snapshot: Vec<_> = captured
        .iter()
        .chain(&markers)
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            metadata
                .is_file()
                .then_some(CapturedFile { path, metadata })
        })
        .collect();
    after_capture();
    for file in &snapshot {
        if file
            .path
            .extension()
            .is_some_and(|extension| extension == "tmp")
        {
            file.expire(directory, now_ms, RETENTION_MS);
        }
    }
    let expire_orphans = || {
        for file in &snapshot {
            file.expire(directory, now_ms, RETENTION_MS);
        }
        let _ = std::fs::remove_dir(&tombstones);
        let _ = std::fs::remove_dir(&inactive);
        let _ = std::fs::remove_dir(directory);
    };
    let mut source = Vec::new();
    for entry in &captured {
        if fact_path(&entry.path()) {
            let Some(fact) = read_json::<Fact>(&entry.path(), FACT_CAP) else {
                expire_orphans();
                return Ok(());
            };
            source.push((entry.path(), fact));
        }
    }
    let metadata_path = directory.join("identity");
    let mut metadata = if std::fs::symlink_metadata(&metadata_path).is_ok() {
        let Some(metadata) = read_json::<ParentMetadata>(&metadata_path, FACT_CAP) else {
            expire_orphans();
            return Ok(());
        };
        metadata
    } else {
        let Some((_, fact)) = source.first() else {
            expire_orphans();
            return Ok(());
        };
        ParentMetadata {
            parent: fact.parent.clone(),
            missing_since_ms: None,
            last_activity_ms: fact.at_ms,
        }
    };
    // A misplaced identity must never authorize deleting another parent's facts.
    if metadata.parent.directory(state) != directory
        || source
            .iter()
            .any(|(_, fact)| fact.parent != metadata.parent)
    {
        expire_orphans();
        return Ok(());
    }
    let old_checkpoint = if std::fs::symlink_metadata(directory.join("lifecycle")).is_ok() {
        let Some(checkpoint) =
            read_json::<LifecycleCheckpoint>(&directory.join("lifecycle"), CHECKPOINT_CAP)
        else {
            expire_orphans();
            return Ok(());
        };
        if checkpoint.parent != metadata.parent {
            expire_orphans();
            return Ok(());
        }
        Some(checkpoint)
    } else {
        None
    };
    let previous_floor = old_checkpoint
        .as_ref()
        .and_then(|checkpoint| checkpoint.proof_floor_ms);
    let mut all: Vec<_> = source.iter().map(|(_, fact)| fact.clone()).collect();
    all.extend(
        old_checkpoint
            .into_iter()
            .flat_map(LifecycleCheckpoint::facts),
    );
    let status = presence(&metadata.parent);
    let latest_prompt = all
        .iter()
        .filter_map(|fact| match fact.event {
            Event::Prompt { .. } => Some(fact.at_ms),
            _ => None,
        })
        .max();
    let ended = all
        .iter()
        .filter_map(|fact| {
            let frontier = match &fact.event {
                Event::ParentEnd => fact.at_ms,
                Event::ParentEndFor { generation } => all
                    .iter()
                    .filter_map(|prompt| match &prompt.event {
                        Event::Prompt { generation: turn } if turn == generation => {
                            Some(prompt.at_ms)
                        }
                        _ => None,
                    })
                    .min()
                    .unwrap_or(fact.at_ms),
                _ => return None,
            };
            latest_prompt
                .is_none_or(|prompt| prompt <= frontier)
                .then_some(fact.at_ms)
        })
        .max();
    match status {
        ParentPresence::Present if ended.is_none() => metadata.missing_since_ms = None,
        ParentPresence::Absent => {
            metadata.missing_since_ms.get_or_insert(now_ms);
        }
        _ => {}
    }
    if let Some(ended) = ended {
        metadata.missing_since_ms = Some(
            metadata
                .missing_since_ms
                .map_or(ended, |old| old.min(ended)),
        );
    }
    if let Some(activity) = source.iter().map(|(_, fact)| fact.at_ms).max() {
        metadata.last_activity_ms = metadata.last_activity_ms.max(activity);
    }
    if let Some(missing) = metadata.missing_since_ms
        && metadata.last_activity_ms > missing
    {
        metadata.missing_since_ms = Some(metadata.last_activity_ms.min(now_ms));
    }
    publish_bytes(directory, "identity", &serde_json::to_vec(&metadata)?, true)?;
    let stale = now_ms.saturating_sub(metadata.last_activity_ms) >= RETENTION_MS;
    let retired = (status != ParentPresence::Unknown || ended.is_some())
        && metadata
            .missing_since_ms
            .is_some_and(|at| now_ms.saturating_sub(at) >= PARENT_GRACE_MS);
    if stale || retired {
        for file in &snapshot {
            if file
                .path
                .extension()
                .is_some_and(|extension| extension == "tmp")
            {
                file.expire(directory, now_ms, FINISHED_GRACE_MS);
            }
        }
        for (path, _) in &source {
            let _ = std::fs::remove_file(path);
        }
        for marker in &markers {
            if marker.file_type()?.is_file() {
                let _ = std::fs::remove_file(marker.path());
            }
        }
        let _ = std::fs::remove_dir(&tombstones);
        let _ = std::fs::remove_dir(&inactive);
        for name in ["lifecycle", "identity"] {
            let _ = std::fs::remove_file(directory.join(name));
        }
        let _ = std::fs::remove_dir(directory);
        return Ok(());
    }
    let compacted = checkpoint(&metadata.parent, &all, now_ms, previous_floor);
    publish_bytes(
        directory,
        "lifecycle",
        &serde_json::to_vec(&compacted)?,
        true,
    )?;
    use std::collections::HashMap;
    let outcomes = evaluate_runs(
        state,
        &metadata.parent,
        now_ms,
        None,
        directory,
        FactSnapshot {
            facts: all.clone(),
            compacted: previous_floor.is_some(),
        },
    );
    let completions: HashMap<_, _> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            RunOutcome::Finished { key, at_ms } => Some((key.clone(), *at_ms)),
            _ => None,
        })
        .collect();
    let suppressed: HashMap<_, _> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            RunOutcome::Inactive { key, at_ms } => Some((key.clone(), *at_ms)),
            _ => None,
        })
        .collect();
    for (path, fact) in &source {
        if let Event::Start { id, generation, .. } = &fact.event
            && is_tombstoned(directory, id, generation.as_deref())
        {
            let _ = std::fs::remove_file(path);
            continue;
        }
        if let Event::Start { id, generation, .. } = &fact.event {
            let key = child_key(id, generation.as_deref());
            if !completions.contains_key(&key)
                && suppressed
                    .get(&key)
                    .is_some_and(|at| now_ms.saturating_sub(*at) >= FINISHED_GRACE_MS)
                && path.parent() == Some(directory)
            {
                create_directory(&inactive)?;
                publish_bytes(
                    &inactive,
                    &format!("{key}.start.json"),
                    &serde_json::to_vec(fact)?,
                    false,
                )?;
                let _ = std::fs::remove_file(path);
                continue;
            }
        }
        let completion = match &fact.event {
            Event::Start { id, generation, .. } => completions
                .get(&child_key(id, generation.as_deref()))
                .copied(),
            Event::Stop { .. } => Some(fact.at_ms),
            _ => {
                let _ = std::fs::remove_file(path);
                continue;
            }
        };
        if let Some(completion) = completion
            && now_ms.saturating_sub(completion) >= FINISHED_GRACE_MS
        {
            let (id, generation) = match &fact.event {
                Event::Start { id, generation, .. } | Event::Stop { id, generation } => {
                    (id, generation)
                }
                _ => unreachable!(),
            };
            if now_ms.saturating_sub(completion) >= RETENTION_MS {
                let _ = std::fs::remove_file(path);
                continue;
            }
            create_directory(&tombstones)?;
            let key = child_key(id, generation.as_deref());
            publish_bytes(&tombstones, &key, &serde_json::to_vec(&completion)?, false)?;
            let _ = std::fs::remove_file(path);
        }
    }
    for marker in markers {
        if read_json::<i64>(&marker.path(), 32)
            .is_some_and(|at| now_ms.saturating_sub(at) >= RETENTION_MS)
        {
            let _ = std::fs::remove_file(marker.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct State(PathBuf);
    impl State {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "subagent-maintenance-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn parent(&self) -> ParentIdentity {
            ParentIdentity::new("test-socket", "test-space", "test-pane", "test-session").unwrap()
        }
        fn record(&self, event: Event, at: i64) {
            record(&self.0, &self.parent(), event, at).unwrap();
        }
        fn clean(&self, now: i64, status: ParentPresence) {
            maintain(&self.0, now, |_| status).unwrap();
        }
        fn runs(&self, now: i64) -> Vec<String> {
            running(&self.0, &self.parent(), now, None)
                .into_iter()
                .map(|run| run.id)
                .collect()
        }
    }
    impl Drop for State {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn start(id: &str, generation: Option<&str>) -> Event {
        Event::Start {
            id: id.to_owned(),
            generation: generation.map(str::to_owned),
            kind: None,
            description: None,
        }
    }
    fn stop(id: &str, generation: Option<&str>) -> Event {
        Event::Stop {
            id: id.to_owned(),
            generation: generation.map(str::to_owned),
        }
    }

    fn modified_at(path: &Path, at_ms: i64) {
        fs::File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(at_ms as u64),
            ))
            .unwrap();
    }

    #[test]
    fn orphan_temporary_files_expire_at_retention_boundary_idempotently() {
        let state = State::new();
        let directory = state.parent().directory(&state.0);
        create_directory(&directory).unwrap();
        let temporary = directory.join(".interrupted.tmp");
        fs::write(&temporary, "partial").unwrap();
        modified_at(&temporary, 1_000);
        state.clean(1_000 + RETENTION_MS - 1, ParentPresence::Unknown);
        assert!(temporary.exists());
        state.clean(1_000 + RETENTION_MS, ParentPresence::Unknown);
        assert!(!directory.exists());
        state.clean(1_000 + RETENTION_MS, ParentPresence::Unknown);
        assert!(!directory.exists());
    }

    #[test]
    fn malformed_parent_files_expire_at_retention_boundary() {
        for corrupt in ["parent-broken.json", "identity", "lifecycle"] {
            let state = State::new();
            state.record(Event::Resume, 1_000);
            state.clean(1_000, ParentPresence::Present);
            let directory = state.parent().directory(&state.0);
            fs::write(directory.join(corrupt), "partial").unwrap();
            for entry in fs::read_dir(&directory).unwrap() {
                modified_at(&entry.unwrap().path(), 1_000);
            }
            state.clean(1_000 + RETENTION_MS - 1, ParentPresence::Unknown);
            assert!(directory.join(corrupt).exists(), "{corrupt}");
            state.clean(1_000 + RETENTION_MS, ParentPresence::Unknown);
            assert!(!directory.exists(), "{corrupt}");
            state.clean(1_000 + RETENTION_MS, ParentPresence::Unknown);
        }
    }

    #[test]
    fn malformed_identity_does_not_expire_recent_receipts_with_old_mtimes() {
        let state = State::new();
        let now = 1_000 + RETENTION_MS;
        state.record(start("recent", None), now);
        let directory = state.parent().directory(&state.0);
        fs::write(directory.join("identity"), "partial").unwrap();
        for entry in fs::read_dir(&directory).unwrap() {
            modified_at(&entry.unwrap().path(), 1_000);
        }
        state.clean(now, ParentPresence::Unknown);
        assert_eq!(state.runs(now), ["recent"]);
        state.clean(now, ParentPresence::Unknown);
        assert_eq!(state.runs(now), ["recent"]);
    }

    #[test]
    fn live_parent_activity_does_not_pin_stale_temporary_files() {
        let state = State::new();
        let now = 1_000 + RETENTION_MS;
        state.record(start("recent", None), now);
        let temporary = state.parent().directory(&state.0).join(".interrupted.tmp");
        fs::write(&temporary, "partial").unwrap();
        modified_at(&temporary, 1_000);
        state.clean(now - 1, ParentPresence::Present);
        assert!(temporary.exists());
        state.clean(now, ParentPresence::Present);
        assert!(!temporary.exists());
        assert_eq!(state.runs(now), ["recent"]);
    }

    #[test]
    fn retired_parent_reaps_old_temporary_files_and_preserves_fresh_writes() {
        let state = State::new();
        state.record(Event::Resume, 1_000);
        state.clean(1_000, ParentPresence::Absent);
        let directory = state.parent().directory(&state.0);
        let now = 1_000 + PARENT_GRACE_MS;
        let old = directory.join(".interrupted.tmp");
        let young = directory.join(".writing.tmp");
        fs::write(&old, "partial").unwrap();
        fs::write(&young, "writing").unwrap();
        modified_at(&old, now - FINISHED_GRACE_MS);
        modified_at(&young, now - FINISHED_GRACE_MS + 1);
        state.clean(now, ParentPresence::Absent);
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(&young).unwrap(), "writing");
        state.clean(now, ParentPresence::Absent);
        assert!(young.exists());
        state.clean(
            now - FINISHED_GRACE_MS + 1 + RETENTION_MS,
            ParentPresence::Unknown,
        );
        assert!(!directory.exists());

        let state = State::new();
        state.record(Event::Resume, 1_000);
        state.clean(1_000, ParentPresence::Absent);
        let directory = state.parent().directory(&state.0);
        let old = directory.join(".interrupted.tmp");
        fs::write(&old, "partial").unwrap();
        modified_at(&old, now - FINISHED_GRACE_MS);
        state.clean(now, ParentPresence::Absent);
        assert!(!directory.exists());
    }

    #[test]
    fn orphan_cleanup_preserves_files_published_replaced_or_changed_after_capture() {
        let state = State::new();
        let directory = state.parent().directory(&state.0);
        create_directory(&directory).unwrap();
        let replaced = directory.join(".replaced.tmp");
        let changed = directory.join(".changed.tmp");
        for path in [&replaced, &changed, &directory.join("identity")] {
            fs::write(path, "stale").unwrap();
            modified_at(path, 1_000);
        }
        let original = fs::File::open(&replaced).unwrap();
        let now = 1_000 + RETENTION_MS;
        maintain_parent(
            &state.0,
            &directory,
            now,
            &|_| ParentPresence::Unknown,
            || {
                publish_bytes(&directory, ".replaced.tmp", b"fresh", true).unwrap();
                modified_at(&replaced, 1_000);
                fs::write(&changed, "fresh contents").unwrap();
                state.record(start("concurrent", None), now);
            },
        )
        .unwrap();
        drop(original);
        assert_eq!(fs::read_to_string(&replaced).unwrap(), "fresh");
        assert_eq!(fs::read_to_string(&changed).unwrap(), "fresh contents");
        assert_eq!(state.runs(now), ["concurrent"]);
        assert!(!directory.join("identity").exists());
        state.clean(now, ParentPresence::Present);
        assert_eq!(state.runs(now), ["concurrent"]);
    }

    #[test]
    fn orphan_cleanup_only_visits_regular_files_and_known_subdirectories() {
        use std::os::unix::fs::symlink;
        let state = State::new();
        let directory = state.parent().directory(&state.0);
        create_directory(&directory).unwrap();
        let outside = state.0.join("outside");
        fs::write(&outside, "keep").unwrap();
        modified_at(&outside, 1_000);
        symlink(&outside, directory.join(".linked.tmp")).unwrap();
        let nested = directory.join("unowned");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("keep"), "keep").unwrap();
        modified_at(&nested.join("keep"), 1_000);
        for subdirectory in ["inactive", "tombstones"] {
            let subdirectory = directory.join(subdirectory);
            fs::create_dir(&subdirectory).unwrap();
            fs::write(subdirectory.join("broken.start.json"), "partial").unwrap();
            modified_at(&subdirectory.join("broken.start.json"), 1_000);
        }
        state.clean(1_000 + RETENTION_MS, ParentPresence::Unknown);
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");
        assert!(
            fs::symlink_metadata(directory.join(".linked.tmp"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(nested.join("keep")).unwrap(), "keep");
        assert!(!directory.join("inactive").exists());
        assert!(!directory.join("tombstones").exists());
    }

    #[test]
    fn finished_boundary_tombstones_and_replay_are_idempotent() {
        let state = State::new();
        state.record(start("done", Some("turn")), 1);
        state.record(stop("done", None), 2);
        state.clean(2 + FINISHED_GRACE_MS - 1, ParentPresence::Present);
        assert_eq!(facts(&state.parent().directory(&state.0)).len(), 2);
        state.clean(2 + FINISHED_GRACE_MS, ParentPresence::Present);
        assert!(facts(&state.parent().directory(&state.0)).is_empty());
        let marker = state
            .parent()
            .directory(&state.0)
            .join("tombstones")
            .join(child_key("done", None));
        assert_eq!(read_json::<i64>(&marker, 32), Some(2));
        let before = fs::read(&marker).unwrap();
        state.record(start("done", Some("another-turn")), 3 + FINISHED_GRACE_MS);
        state.clean(3 + FINISHED_GRACE_MS, ParentPresence::Present);
        assert!(state.runs(3 + FINISHED_GRACE_MS).is_empty());
        assert_eq!(fs::read(marker).unwrap(), before);
    }

    #[test]
    fn generation_specific_idle_stop_does_not_suppress_a_new_generation() {
        let state = State::new();
        state.record(start("same", Some("old")), 1);
        state.record(stop("same", Some("old")), 2);
        state.clean(2 + FINISHED_GRACE_MS, ParentPresence::Present);
        state.record(start("same", Some("new")), 3 + FINISHED_GRACE_MS);
        assert_eq!(state.runs(3 + FINISHED_GRACE_MS), ["same"]);
    }

    #[test]
    fn ended_and_gone_parents_have_exact_grace_and_live_observation_resets_it() {
        let ended = State::new();
        ended.record(Event::ParentEnd, 100);
        ended.clean(100 + PARENT_GRACE_MS - 1, ParentPresence::Present);
        assert!(ended.parent().directory(&ended.0).exists());
        ended.clean(100 + PARENT_GRACE_MS, ParentPresence::Present);
        assert!(!ended.parent().directory(&ended.0).exists());
        let gone = State::new();
        gone.record(start("old", None), 100);
        gone.clean(100, ParentPresence::Absent);
        gone.clean(100 + PARENT_GRACE_MS - 1, ParentPresence::Present);
        gone.clean(100 + PARENT_GRACE_MS, ParentPresence::Absent);
        gone.clean(100 + 2 * PARENT_GRACE_MS - 1, ParentPresence::Absent);
        assert!(gone.parent().directory(&gone.0).exists());
        gone.clean(100 + 2 * PARENT_GRACE_MS, ParentPresence::Absent);
        assert!(!gone.parent().directory(&gone.0).exists());
    }

    #[test]
    fn unknown_status_does_not_retire_a_parent_after_grace() {
        let state = State::new();
        state.record(start("live", None), 100);
        state.clean(100, ParentPresence::Absent);
        state.clean(100 + PARENT_GRACE_MS, ParentPresence::Unknown);
        assert_eq!(state.runs(100 + PARENT_GRACE_MS), ["live"]);
    }

    #[test]
    fn retention_uses_fact_age_and_preserves_recent_children_in_an_old_parent() {
        let state = State::new();
        state.record(start("old", None), 1);
        state.clean(RETENTION_MS, ParentPresence::Present);
        assert!(state.parent().directory(&state.0).exists());
        state.record(start("current", None), RETENTION_MS);
        state.clean(RETENTION_MS + 1, ParentPresence::Present);
        assert_eq!(state.runs(RETENTION_MS + 1), ["current"]);
        let stale = State::new();
        stale.record(start("old", None), 1);
        stale.clean(RETENTION_MS, ParentPresence::Unknown);
        assert!(stale.parent().directory(&stale.0).exists());
        stale.clean(RETENTION_MS + 1, ParentPresence::Unknown);
        assert!(!stale.parent().directory(&stale.0).exists());
    }

    #[test]
    fn over_cap_completed_children_and_parent_turns_do_not_hide_new_work() {
        let state = State::new();
        for index in 0..=FACTS_PER_PARENT {
            let id = format!("done-{index}");
            state.record(start(&id, None), 1);
            state.record(stop(&id, None), 2);
            state.record(
                Event::Prompt {
                    generation: format!("turn-{index}"),
                },
                10 + index as i64,
            );
            state.record(
                Event::ParentStop {
                    background: Vec::new(),
                    generation: Some(format!("turn-{index}")),
                },
                11 + index as i64,
            );
        }
        let now = FINISHED_GRACE_MS + 10;
        state.record(
            Event::Prompt {
                generation: "current".to_owned(),
            },
            now - 1,
        );
        state.record(start("live", Some("current")), now);
        state.clean(now, ParentPresence::Present);
        assert_eq!(state.runs(now), ["live"]);
        assert_eq!(
            fs::read_dir(state.parent().directory(&state.0))
                .unwrap()
                .count(),
            4
        );
        state.clean(now, ParentPresence::Present);
        assert_eq!(state.runs(now), ["live"]);
        state.record(start("done-0", Some("current")), now + 1);
        assert_eq!(state.runs(now + 1), ["live"]);
    }

    #[test]
    fn lifecycle_summary_keeps_background_and_prompt_order_with_late_barriers() {
        let state = State::new();
        for (event, at) in [
            (
                Event::Prompt {
                    generation: "one".to_owned(),
                },
                10,
            ),
            (
                Event::Prompt {
                    generation: "two".to_owned(),
                },
                20,
            ),
            (start("background", Some("one")), 21),
            (start("foreground", Some("one")), 22),
            (
                Event::ParentStop {
                    background: vec!["background".to_owned()],
                    generation: Some("two".to_owned()),
                },
                30,
            ),
            (
                Event::ParentStop {
                    background: Vec::new(),
                    generation: Some("one".to_owned()),
                },
                40,
            ),
            (
                Event::ParentEndFor {
                    generation: "one".to_owned(),
                },
                50,
            ),
            (start("current", Some("two")), 51),
        ] {
            state.record(event, at);
        }
        let before = state.runs(100);
        state.clean(100, ParentPresence::Present);
        assert_eq!(state.runs(100), before);
        state.record(start("old-replay", Some("one")), 101);
        assert!(!state.runs(101).contains(&"old-replay".to_owned()));
    }

    #[test]
    fn old_prompt_proofs_are_bounded_and_late_starts_fail_closed() {
        let state = State::new();
        for index in 0..100 {
            state.record(
                Event::Prompt {
                    generation: format!("turn-{index}"),
                },
                1 + index,
            );
        }
        let now = MAX_RUN_AGE_MS + 200;
        state.record(
            Event::Prompt {
                generation: "current".to_owned(),
            },
            now - 1,
        );
        state.record(start("live", Some("current")), now);
        state.clean(now, ParentPresence::Present);
        let checkpoint: LifecycleCheckpoint = read_json(
            &state.parent().directory(&state.0).join("lifecycle"),
            CHECKPOINT_CAP,
        )
        .unwrap();
        assert_eq!(checkpoint.entries.len(), 1);
        assert!(checkpoint.proof_floor_ms.is_some());
        state.record(start("late", Some("turn-0")), now + 1);
        assert_eq!(state.runs(now + 1), ["live"]);
        state.clean(now + 1, ParentPresence::Present);
        assert_eq!(state.runs(now + 1), ["live"]);
    }

    #[test]
    fn marker_expiry_boundary_and_corrupt_identity_fail_closed() {
        let state = State::new();
        state.record(stop("old", None), 1);
        state.clean(FINISHED_GRACE_MS + 1, ParentPresence::Present);
        let dir = state.parent().directory(&state.0);
        state.record(start("new", None), RETENTION_MS);
        state.clean(RETENTION_MS, ParentPresence::Present);
        assert!(is_tombstoned(&dir, "old", None));
        state.clean(RETENTION_MS + 1, ParentPresence::Present);
        assert!(!is_tombstoned(&dir, "old", None));
        fs::write(dir.join("identity"), "corrupt").unwrap();
        state.clean(RETENTION_MS + PARENT_GRACE_MS, ParentPresence::Absent);
        assert!(
            dir.join(format!("{}.start.json", child_key("new", None)))
                .exists()
        );
    }

    #[test]
    fn symlinked_parent_or_marker_directory_is_never_followed() {
        use std::os::unix::fs::symlink;
        let state = State::new();
        state.record(start("live", None), 1);
        let dir = state.parent().directory(&state.0);
        let outside = state.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "keep").unwrap();
        symlink(&outside, dir.join("tombstones")).unwrap();
        state.clean(RETENTION_MS + 1, ParentPresence::Absent);
        assert_eq!(fs::read_to_string(outside.join("keep")).unwrap(), "keep");
        fs::remove_file(dir.join("tombstones")).unwrap();
        let saved = state.0.join("saved");
        fs::rename(&dir, &saved).unwrap();
        symlink(&saved, &dir).unwrap();
        assert!(state.runs(1).is_empty());
        assert!(record(&state.0, &state.parent(), start("new", None), 2).is_err());
        state.clean(RETENTION_MS + 1, ParentPresence::Absent);
        assert!(
            saved
                .join(format!("{}.start.json", child_key("live", None)))
                .exists()
        );
    }

    #[test]
    fn cleanup_cannot_lose_a_writer_at_any_publication_checkpoint() {
        for stage in [
            PublicationStage::BeforeOpen,
            PublicationStage::BeforeLink,
            PublicationStage::AfterLink,
        ] {
            let state = State::new();
            state.record(start("expired", None), 1);
            state.clean(2, ParentPresence::Absent);
            let now = 2 + PARENT_GRACE_MS;
            let root = state.0.clone();
            let parent = state.parent();
            let (paused, pause) = std::sync::mpsc::channel();
            let (resume, resumed) = std::sync::mpsc::channel();
            let writer = std::thread::spawn(move || {
                let mut waiting = true;
                publish(&root, &parent, start("new", None), now, |current| {
                    if waiting && current == stage {
                        waiting = false;
                        paused.send(()).unwrap();
                        resumed.recv().unwrap();
                    }
                })
                .unwrap();
            });
            pause.recv().unwrap();
            state.clean(now, ParentPresence::Absent);
            resume.send(()).unwrap();
            writer.join().unwrap();
            let before = state.runs(now);
            assert!(
                before.contains(&"new".to_owned()),
                "publication checkpoint {stage:?}: {before:?}"
            );
            state.clean(now, ParentPresence::Present);
            assert_eq!(state.runs(now), before);
        }
    }
    #[test]
    fn reader_survives_captured_fact_unlinks_and_keeps_background_snapshot() {
        let state = State::new();
        state.record(
            Event::Prompt {
                generation: "turn".to_owned(),
            },
            1,
        );
        state.record(start("background", Some("turn")), 2);
        state.record(
            Event::ParentStop {
                background: vec!["background".to_owned()],
                generation: Some("turn".to_owned()),
            },
            3,
        );
        let directory = state.parent().directory(&state.0);
        let mut cleaned = false;
        let snapshot = read_facts(&directory, |_| {
            if !cleaned {
                cleaned = true;
                state.clean(100, ParentPresence::Present);
            }
        });
        let runs = running_snapshot(
            &state.0,
            &state.parent(),
            100,
            Some(99),
            &directory,
            snapshot,
        );
        assert_eq!(
            runs.into_iter().map(|run| run.id).collect::<Vec<_>>(),
            ["background"]
        );
        assert_eq!(state.runs(100), ["background"]);
    }

    #[test]
    fn collector_lock_never_blocks_hook_publication_or_loses_a_stop() {
        use std::os::fd::AsRawFd;
        let state = State::new();
        state.record(start("done", None), 1);
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(state.0.join("subagents/.maintenance.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        state.record(stop("done", None), 2);
        state.record(start("live", None), 3);
        state.clean(FINISHED_GRACE_MS + 2, ParentPresence::Present);
        assert_eq!(state.runs(FINISHED_GRACE_MS + 2), ["live"]);
        drop(lock);
        state.clean(FINISHED_GRACE_MS + 2, ParentPresence::Present);
        assert_eq!(state.runs(FINISHED_GRACE_MS + 2), ["live"]);
        state.record(start("done", None), FINISHED_GRACE_MS + 3);
        assert_eq!(state.runs(FINISHED_GRACE_MS + 3), ["live"]);
    }

    #[test]
    fn lifecycle_checkpoint_matches_the_reducer_across_mixed_histories() {
        let mut random = 17_u64;
        for _ in 0..32 {
            let state = State::new();
            let mut turn = 0;
            for at in 1..=80 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let chosen = (random >> 32) as usize;
                let generation = format!("turn-{}", chosen % (turn + 2));
                let id = format!("child-{}", chosen % 7);
                let event = match chosen % 9 {
                    0 | 1 => {
                        turn += 1;
                        Event::Prompt {
                            generation: format!("turn-{turn}"),
                        }
                    }
                    2 => start(&id, Some(&generation)),
                    3 => start(&id, None),
                    4 => Event::ParentStop {
                        generation: Some(generation),
                        background: vec![id],
                    },
                    5 => Event::ParentStop {
                        generation: None,
                        background: vec![id],
                    },
                    6 => Event::ParentEndFor { generation },
                    7 => Event::Reset,
                    _ => Event::Resume,
                };
                state.record(event, at);
            }
            let before = state.runs(100);
            state.clean(100, ParentPresence::Present);
            assert_eq!(state.runs(100), before);
            state.clean(100, ParentPresence::Present);
            assert_eq!(state.runs(100), before);
        }
    }
    #[test]
    fn inactive_history_does_not_spend_live_budget_and_background_can_readmit_it() {
        let state = State::new();
        for index in 0..=FACTS_PER_PARENT {
            state.record(start(&format!("old-{index}"), None), 1);
        }
        state.record(
            Event::ParentStop {
                background: Vec::new(),
                generation: None,
            },
            2,
        );
        let now = FINISHED_GRACE_MS + 2;
        state.record(
            Event::Prompt {
                generation: "current".into(),
            },
            now - 1,
        );
        state.record(start("live", Some("current")), now);
        assert_eq!(state.runs(now), ["live"]);
        state.clean(now, ParentPresence::Present);
        let inactive = state.parent().directory(&state.0).join("inactive");
        assert_eq!(
            fs::read_dir(&inactive).unwrap().count(),
            FACTS_PER_PARENT + 1
        );
        assert_eq!(state.runs(now), ["live"]);
        state.record(
            Event::ParentStop {
                background: vec!["old-0".into(), "live".into()],
                generation: Some("current".into()),
            },
            now + 1,
        );
        assert_eq!(state.runs(now + 1), ["old-0", "live"]);
        state.clean(now + 1, ParentPresence::Present);
        assert_eq!(state.runs(now + 1), ["old-0", "live"]);
        state.record(start("old-0", None), now + 2);
        assert_eq!(state.runs(now + 2), ["old-0", "live"]);
        assert!(
            !state
                .parent()
                .directory(&state.0)
                .join(format!("{}.start.json", child_key("old-0", None)))
                .exists()
        );
    }

    #[test]
    fn unproven_suppression_never_tombstones_a_delayed_prompt() {
        let state = State::new();
        state.record(
            Event::Prompt {
                generation: "old".into(),
            },
            1,
        );
        state.record(
            Event::ParentEndFor {
                generation: "old".into(),
            },
            2,
        );
        state.record(start("pending", Some("new")), 20);
        let now = FINISHED_GRACE_MS + 20;
        state.clean(now, ParentPresence::Unknown);
        assert!(state.runs(now).is_empty());
        assert!(!is_tombstoned(
            &state.parent().directory(&state.0),
            "pending",
            Some("new")
        ));
        state.record(
            Event::Prompt {
                generation: "new".into(),
            },
            10,
        );
        assert_eq!(state.runs(now), ["pending"]);
    }

    #[test]
    fn typed_end_grace_uses_end_receipt_on_repeated_checkpoint_only_cleanup() {
        let state = State::new();
        state.record(
            Event::Prompt {
                generation: "turn".into(),
            },
            0,
        );
        state.record(
            Event::ParentEndFor {
                generation: "turn".into(),
            },
            MAX_RUN_AGE_MS,
        );
        for _ in 0..2 {
            state.clean(MAX_RUN_AGE_MS, ParentPresence::Present);
        }
        assert!(state.parent().directory(&state.0).exists());
        state.clean(
            MAX_RUN_AGE_MS + PARENT_GRACE_MS - 1,
            ParentPresence::Present,
        );
        assert!(state.parent().directory(&state.0).exists());
        state.clean(MAX_RUN_AGE_MS + PARENT_GRACE_MS, ParentPresence::Present);
        assert!(!state.parent().directory(&state.0).exists());
    }
    #[test]
    fn archived_original_wins_a_delayed_duplicate_start_publication() {
        let state = State::new();
        state.record(start("old", None), 1);
        state.record(
            Event::ParentStop {
                background: Vec::new(),
                generation: None,
            },
            2,
        );
        let now = FINISHED_GRACE_MS + 2;
        let mut cleaned = false;
        publish(
            &state.0,
            &state.parent(),
            start("old", None),
            now,
            |stage| {
                if !cleaned && stage == PublicationStage::BeforeLink {
                    cleaned = true;
                    state.clean(now, ParentPresence::Present);
                }
            },
        )
        .unwrap();
        assert!(state.runs(now).is_empty());
        state.record(
            Event::ParentStop {
                background: vec!["old".into()],
                generation: None,
            },
            now + 1,
        );
        let runs = running(&state.0, &state.parent(), now + 1, None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].started_at_ms, 1);
    }
}
