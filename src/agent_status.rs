//! A bounded, read-only snapshot of one Herdr Space for the agent status panel.
//!
//! Every field here is an observed fact. Herdr reports pane identities and
//! statuses, the Namedrop state store reports outstanding requests, and pane
//! text travels as quoted, redacted evidence. Nothing in this module is written
//! by a model, and nothing here mutates Herdr or Namedrop state.
//!
//! Three properties the caller depends on:
//!
//! * **Bound origin.** The snapshot is refused unless the invoking pane's
//!   physical terminal and Agent Session are the ones the panel was opened for,
//!   checked before the first read *and* again before returning.
//! * **Verified panes.** A Herdr pane id is a short reusable handle, so a pane
//!   read under one identity can belong to another pane by the time it is
//!   shown. Every pane is re-checked after its excerpt is read, and a pane
//!   whose space, terminal or session moved is dropped rather than described.
//! * **Dependency truth.** `waits` comes from Delivery state, never from pane
//!   text, and an unreadable store is reported as unavailable evidence rather
//!   than as an empty list of outstanding requests. Delivery wording keeps the
//!   host's own distinctions: submitted is not receipt, receipt is not an
//!   answer, and an ambiguous submission is uncertain rather than undelivered.

use crate::{
    AgentRuntime, Clock, DeliveryState, DiscoveredPane, DiscoveredWorktree, Host, IdGenerator,
    LifecycleRuntime, PaneAddressRecord, StateStore,
};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Total wall-clock budget one collection may spend, shared by every Herdr read
/// and by the state store read. The panel paints before this runs, so the
/// budget bounds how late facts arrive, not how long the first frame waits.
const COLLECTION_BUDGET_MS: u64 = 2_000;

/// Budget held back from excerpt reads so the closing identity check can still
/// run. A snapshot that cannot re-check identities is not shown at all, so this
/// reserve is what keeps a slow space degrading into fewer excerpts rather than
/// into no panel.
const VERIFY_RESERVE_MS: u64 = 250;

/// Share of the collection budget the state store read may spend. A long page
/// of reports must degrade into fewer waits, not into a panel with no budget
/// left for pane reads or for the closing identity check.
const STATE_BUDGET_MS: u64 = 800;

/// Panes that receive a terminal excerpt. Later panes stay metadata-only rows.
const MAX_EXCERPT_PANES: usize = 8;

/// Metadata rows one snapshot may carry. Rows are cheap next to excerpts, but a
/// space with very many panes must still produce a bounded snapshot; panes
/// dropped here are counted in `omitted_panes` like any other pane the snapshot
/// cannot describe.
const MAX_PANE_ROWS: usize = 32;

/// Recent unwrapped lines requested per excerpt. The plan's ceiling is 120; a
/// glance only needs the tail, and every excerpt is byte-capped below.
const EXCERPT_LINES: usize = 40;

/// Bytes kept per excerpt, counted from the end.
const MAX_EXCERPT_BYTES: usize = 2_048;

/// Outstanding requests read in one page. A fuller page, or a page cut short by
/// the budget, is reported as incomplete rather than silently truncated.
const HANDOFF_PAGE_LIMIT: u32 = 100;

const MAX_LIST_OUTPUT_BYTES: usize = 512 * 1_024;
const MAX_EXCERPT_OUTPUT_BYTES: usize = 64 * 1_024;
const MAX_LABEL_CHARS: usize = 48;
const MAX_REQUEST_CHARS: usize = 120;

const CANCELLED: &str = "The agent status snapshot was cancelled.";
const REQUESTS_UNKNOWN: &str =
    "Outstanding requests could not be read, so this snapshot cannot say whether any are waiting:";

/// Collect the origin-bound snapshot the agent status panel renders and the
/// model worker summarizes.
///
/// `expected_terminal` and `expected_session` are the identities captured when
/// the panel opened: the physical terminal of the invoking pane, and its Agent
/// Session (`None`, or an empty string, when the pane held none). Both are
/// checked against live Herdr state before anything is read and again before
/// this returns, so a collection can never be attributed to a replacement
/// session that appeared while it ran.
///
/// `cancelled` is observed before and between every read; a cancelled
/// collection returns an error instead of a partial snapshot.
///
/// The returned value is the Snapshot JSON documented in the agent status plan:
/// `origin`, `observed_at_ms`, `panes`, `waits`, `handoffs_available`,
/// `warnings` and `omitted_panes`. `waits` rows also carry the recipient
/// `alias` so the panel can name a recipient whose pane is gone.
pub fn collect_agent_status<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    expected_terminal: &str,
    expected_session: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Value, String>
where
    E: Host,
    C: Clock,
    I: IdGenerator,
{
    let observed_at_ms = runtime.clock.now_unix_ms();
    let reads = Deadline {
        clock: runtime.clock,
        deadline_ms: observed_at_ms.saturating_add(COLLECTION_BUDGET_MS),
        cancelled,
    };
    let origin_pane_id = runtime
        .current_pane_id
        .ok_or("The agent status panel requires its invoking pane")?;
    let discovered = discover(runtime, &reads)?;
    let origin = bound_origin(
        &discovered,
        origin_pane_id,
        expected_terminal,
        expected_session,
    )?;

    let mut warnings = Vec::new();
    let observing_pane = std::env::var("HERDR_PANE_ID").ok();
    let visible = visible_panes(
        &discovered,
        origin,
        observing_pane.as_deref(),
        &mut warnings,
    );

    let (addresses, handoffs) = read_state(runtime, &discovered, origin, &reads, &mut warnings);
    let handoffs_available = handoffs.is_some();
    let waits = handoffs.map_or_else(Vec::new, |reports| {
        collect_waits(&reports, &addresses, &visible, &mut warnings)
    });

    let ordered = wait_first_order(&visible, origin, &waits);
    let rows = pane_rows(runtime.host, &ordered, &addresses, &reads, &mut warnings);

    // Identities are re-read after the excerpts, so every pane in the snapshot
    // is one whose space, terminal and session held for the whole collection.
    let verified = discover(runtime, &reads).map_err(|error| {
        format!("This space could not be re-checked before showing the panel: {error}")
    })?;
    if verified.workspace_id != discovered.workspace_id {
        return Err(
            "This pane moved to another space while the panel was collecting. Open the \
                    panel again from your pane."
                .to_owned(),
        );
    }
    bound_origin(
        &verified,
        origin_pane_id,
        expected_terminal,
        expected_session,
    )?;
    let panes = verified_rows(rows, &verified, &mut warnings);

    if cancelled.load(Ordering::Relaxed) {
        return Err(CANCELLED.to_owned());
    }
    let described = panes.iter().filter(|row| row["excerpt"] != "").count();
    Ok(json!({
        "origin": {
            "workspace_id": discovered.workspace_id,
            "pane_id": origin.pane_id,
            "terminal_id": origin.terminal_id,
            "session_id": origin.agent_session_id.as_ref().map(|session| session.as_str()),
            "host": host(origin),
            "cwd": origin.cwd,
            "label": label(origin, &addresses),
            "persona": persona(origin, &addresses),
            "status": origin.agent_status,
        },
        "observed_at_ms": observed_at_ms,
        "panes": panes,
        "waits": waits,
        "handoffs_available": handoffs_available,
        "warnings": warnings,
        "omitted_panes": visible.len().saturating_sub(described),
    }))
}

fn discover<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    reads: &Deadline<'_, C>,
) -> Result<DiscoveredWorktree, String>
where
    E: Host,
    C: Clock,
    I: IdGenerator,
{
    match runtime.workspace_id {
        Some(workspace_id) => runtime
            .host
            .space(crate::SpaceLocator::Workspace(workspace_id), Some(reads))
            .map_err(String::from),
        None => runtime
            .host
            .space(crate::SpaceLocator::Checkout(runtime.cwd), Some(reads))
            .map_err(String::from),
    }
}

/// Spends one collection budget across every read, so a slow pane cannot
/// consume the whole panel's latency budget. Reads made through the existing
/// discovery helpers pass through here too, and the state store is handed the
/// same remaining budget.
struct Deadline<'a, C> {
    clock: &'a C,
    deadline_ms: u64,
    cancelled: &'a AtomicBool,
}

impl<C: Clock> Deadline<'_, C> {
    fn remaining(&self) -> Result<Duration, String> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(CANCELLED.to_owned());
        }
        let remaining = self.deadline_ms.saturating_sub(self.clock.now_unix_ms());
        if remaining == 0 {
            return Err(format!(
                "Reading this space took longer than its {COLLECTION_BUDGET_MS} ms budget."
            ));
        }
        Ok(Duration::from_millis(remaining))
    }

    /// What an excerpt read may be given: the remaining budget less the reserve
    /// the closing identity check needs. Checking only whether a read may
    /// *start* is not enough, because a stalled read spends whatever timeout it
    /// was handed, so the reserve has to come out of the timeout itself.
    fn remaining_before_verification(&self) -> Result<Duration, String> {
        self.remaining()?
            .checked_sub(Duration::from_millis(VERIFY_RESERVE_MS))
            .filter(|left| !left.is_zero())
            .ok_or_else(|| {
                format!(
                    "Only the {VERIFY_RESERVE_MS} ms identity-check reserve is left of this \
                     snapshot's budget."
                )
            })
    }
}

impl<C: Clock> crate::Budget for Deadline<'_, C> {
    /// Discovery, including the closing identity check, may use everything that
    /// is left: it is the one read the snapshot cannot do without.
    fn remaining(&self) -> Result<Duration, String> {
        self.remaining()
    }
    fn cancelled(&self) -> &AtomicBool {
        self.cancelled
    }
    fn max_output_bytes(&self) -> usize {
        MAX_LIST_OUTPUT_BYTES
    }
}

struct ExcerptBudget<'a> {
    timeout: Duration,
    cancelled: &'a AtomicBool,
}

impl crate::Budget for ExcerptBudget<'_> {
    fn remaining(&self) -> Result<Duration, String> {
        Ok(self.timeout)
    }
    fn cancelled(&self) -> &AtomicBool {
        self.cancelled
    }
    fn max_output_bytes(&self) -> usize {
        MAX_EXCERPT_OUTPUT_BYTES
    }
}

/// Resolve the invoking pane and refuse anything but the exact identity the
/// panel was opened for. A pane id is reused, so the physical terminal and the
/// Agent Session are what pin the origin.
fn bound_origin<'a>(
    discovered: &'a DiscoveredWorktree,
    origin_pane_id: &str,
    expected_terminal: &str,
    expected_session: Option<&str>,
) -> Result<&'a DiscoveredPane, String> {
    let origin = discovered
        .panes
        .iter()
        .find(|pane| pane.pane_id == origin_pane_id)
        .ok_or("The invoking pane has closed. Open the panel again from your pane.")?;
    if origin.terminal_id != expected_terminal {
        return Err(
            "The invoking pane changed terminals. Close this panel and open it again.".to_owned(),
        );
    }
    let expected_session = expected_session.filter(|session| !session.is_empty());
    let observed_session = origin
        .agent_session_id
        .as_ref()
        .map(|session| session.as_str());
    if observed_session != expected_session {
        return Err(
            "The invoking pane is running a different agent session. Close this panel and open it \
             again."
                .to_owned(),
        );
    }
    Ok(origin)
}

/// The panes this snapshot may describe: this Space only, never the pane the
/// panel itself occupies. The origin always survives both filters because it is
/// the pane the panel was opened from.
fn visible_panes<'a>(
    discovered: &'a DiscoveredWorktree,
    origin: &DiscoveredPane,
    observing_pane: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<&'a DiscoveredPane> {
    let mut foreign = 0usize;
    let visible = discovered
        .panes
        .iter()
        .filter(|pane| {
            if pane.workspace_id != discovered.workspace_id {
                foreign += 1;
                return false;
            }
            pane.pane_id == origin.pane_id || observing_pane != Some(pane.pane_id.as_str())
        })
        .collect();
    if foreign > 0 {
        warnings.push(format!(
            "This snapshot left out {} outside this space.",
            panes(foreign)
        ));
    }
    visible
}

/// Read Namedrop's own state under the remaining collection budget: the names
/// bound to panes, and the origin's outstanding requests. Either half can be
/// unavailable on its own, and an unavailable half is reported rather than
/// rendered as emptiness.
fn read_state<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    discovered: &DiscoveredWorktree,
    origin: &DiscoveredPane,
    reads: &Deadline<'_, C>,
    warnings: &mut Vec<String>,
) -> (Vec<PaneAddressRecord>, Option<Vec<Value>>)
where
    E: Host,
    C: Clock,
    I: IdGenerator,
{
    let namespace = discovered.namespace(runtime.session_socket_id);
    let clock = runtime.clock;
    let now = clock.now_unix_ms();
    // The state read gets a share of the budget rather than all of it, and it
    // stops between report rows once that share is gone.
    let state_deadline_ms = now.saturating_add(STATE_BUDGET_MS).min(reads.deadline_ms);
    let budget = match reads.remaining() {
        Ok(budget) => budget.min(Duration::from_millis(STATE_BUDGET_MS)),
        Err(error) => {
            warnings.push(format!("{REQUESTS_UNKNOWN} {error}"));
            return (Vec::new(), None);
        }
    };
    // The store read shares the collection budget: integrity checking, lock
    // waits and a long page of reports are all interruptible here, instead of
    // running unbounded after the Herdr reads were carefully bounded.
    let read = StateStore::with_read_only_timeout(
        runtime.state_directory,
        budget,
        reads.cancelled,
        |store| {
            let mut notes = Vec::new();
            let Some(store) = store else {
                notes.push(
                    "Namedrop has no saved state here, so outstanding requests are unknown."
                        .to_owned(),
                );
                return Ok((Vec::new(), None, notes));
            };
            let addresses = store
                .transaction(|transaction| transaction.list_pane_addresses(&namespace))
                .unwrap_or_else(|error| {
                    notes.push(format!("Namedrop names could not be read: {error}"));
                    Vec::new()
                });
            let reports = store.transaction(|transaction| {
                let author = crate::context::registered_current_address(
                    transaction,
                    &namespace,
                    &origin.pane_id,
                    discovered,
                )?;
                let mut ids = transaction.handoff_message_ids(
                    &namespace,
                    &author,
                    false,
                    HANDOFF_PAGE_LIMIT + 1,
                    0,
                )?;
                let mut incomplete = ids.len() > HANDOFF_PAGE_LIMIT as usize;
                ids.truncate(HANDOFF_PAGE_LIMIT as usize);
                let mut reports = Vec::with_capacity(ids.len());
                let live = crate::spaces::LiveSpaces::here(discovered.clone());
                for id in &ids {
                    // Stop between rows rather than finish a long page after
                    // the panel needed it.
                    if reads.cancelled.load(Ordering::Relaxed)
                        || clock.now_unix_ms() >= state_deadline_ms
                    {
                        incomplete = true;
                        break;
                    }
                    let mut report =
                        transaction.handoff_report(&namespace, &author, id, now, false)?;
                    transaction.add_handoff_outlooks(&namespace, &live, &mut report)?;
                    reports.push(report);
                }
                Ok((reports, incomplete))
            });
            match reports {
                Ok((reports, incomplete)) => {
                    if incomplete {
                        notes.push(
                            "This snapshot did not read every outstanding request; the list is \
                             incomplete."
                                .to_owned(),
                        );
                    }
                    Ok((addresses, Some(reports), notes))
                }
                Err(error) => {
                    notes.push(format!("{REQUESTS_UNKNOWN} {error}"));
                    Ok((addresses, None, notes))
                }
            }
        },
    );
    match read {
        Ok((addresses, reports, notes)) => {
            warnings.extend(notes);
            (addresses, reports)
        }
        Err(error) => {
            warnings.push(format!("{REQUESTS_UNKNOWN} {error}"));
            (Vec::new(), None)
        }
    }
}

/// Turn the origin's request reports into per-recipient waits. Every wait is a
/// request Delivery with no recorded result, or one that needs attention; a
/// recorded result is finished work and is not a wait.
///
/// These are the origin's outbound requests only. A reply that exists but has
/// not reached the origin is a separate return Delivery and arrives in the
/// origin's own inbox, so it never appears here as a wait on the recipient.
fn collect_waits(
    reports: &[Value],
    addresses: &[PaneAddressRecord],
    visible: &[&DiscoveredPane],
    warnings: &mut Vec<String>,
) -> Vec<Value> {
    let mut waits = Vec::new();
    for report in reports {
        let request = one_line(
            report["request"].as_str().unwrap_or_default(),
            MAX_REQUEST_CHARS,
        );
        for recipient in report["recipients"].as_array().into_iter().flatten() {
            let needs_attention = recipient["needs_attention"] == json!(true);
            let result_status = recipient["result_status"].as_str();
            if result_status.is_some() && !needs_attention {
                continue;
            }
            let Some(state) = recipient["state"]
                .as_str()
                .and_then(|state| serde_json::from_value::<DeliveryState>(json!(state)).ok())
            else {
                warnings.push("A delivery reported a state this snapshot cannot read.".to_owned());
                continue;
            };
            let alias = recipient["target"]["alias"].as_str().unwrap_or_default();
            let pane_id = recipient["target"]["address_id"]
                .as_str()
                .and_then(|address_id| {
                    addresses
                        .iter()
                        .find(|address| address.id.as_str() == address_id)
                })
                .and_then(|address| address.pane_id.as_deref())
                .filter(|pane_id| visible.iter().any(|pane| pane.pane_id == *pane_id));
            waits.push(json!({
                "pane_id": pane_id,
                "request_id": recipient["delivery_id"],
                "alias": alias,
                "description": format!("@{alias} · {request}"),
                "state": recipient["state"],
                "reason": wait_reason(state, result_status, recipient),
            }));
        }
    }
    waits.sort_by_key(wait_rank);
    waits
}

/// Attention first, then requests whose receipt the recipient recorded, then
/// requests submitted without a receipt, then requests never submitted.
fn wait_rank(wait: &Value) -> u8 {
    match serde_json::from_value::<DeliveryState>(wait["state"].clone()) {
        Ok(DeliveryState::Held | DeliveryState::Failed) => 0,
        Ok(DeliveryState::Settled) => 1,
        Ok(DeliveryState::Acknowledged) => 2,
        Ok(state) if state.awaits_submission() => 4,
        Ok(_) => 3,
        Err(_) => 5,
    }
}

/// The factual cause of one wait, taken from Delivery state and the store's own
/// outlook constraints.
///
/// The wording keeps the distinctions the states actually carry. Submitted
/// means the request was handed to the recipient's host, which is not proof the
/// recipient received or read it. Acknowledged means the recipient's session
/// recorded receipt, which is not proof it acted on it. Held can follow a
/// submission whose outcome was never established, so it is described as
/// uncertain rather than as undelivered.
fn wait_reason(state: DeliveryState, result_status: Option<&str>, recipient: &Value) -> String {
    match state {
        DeliveryState::Held => {
            "held; whether it reached the recipient was never established, so it needs a decision \
             here"
                .to_owned()
        }
        DeliveryState::Failed => "delivery failed and needs attention".to_owned(),
        DeliveryState::Cancelled => "cancelled".to_owned(),
        DeliveryState::Settled if result_status.is_none() => {
            "receipt recorded and settled without a result".to_owned()
        }
        DeliveryState::Acknowledged => "receipt recorded; no result yet".to_owned(),
        DeliveryState::Submitted => {
            "submitted to the recipient's session; no receipt recorded yet".to_owned()
        }
        state if state.awaits_submission() => {
            let constraint = recipient["outlook"]["constraints"]
                .as_array()
                .and_then(|constraints| constraints.first())
                .and_then(Value::as_str);
            match constraint {
                Some(constraint) => format!("not submitted yet: {}", constraint_reason(constraint)),
                None => "queued; not submitted to the recipient yet".to_owned(),
            }
        }
        _ => "outstanding; no result recorded".to_owned(),
    }
}

/// The store's outlook vocabulary, in the panel's words. An unrecognized
/// constraint is shown verbatim rather than dropped.
fn constraint_reason(constraint: &str) -> String {
    match constraint {
        "earlier_delivery_pending" => "an earlier delivery to that recipient is still ahead of it",
        "recipient_busy_or_unknown" => "the recipient is not idle",
        "recipient_disconnected" => "the recipient's pane is gone",
        "recipient_unbound" | "recipient_removed" => "the recipient has no pane",
        "recipient_disabled" => "the recipient turned collaboration off",
        "session_changed" => "the recipient started a different agent session",
        "registration_required" => "the recipient has no active Namedrop registration",
        "integration_repair_required" => "the recipient's Namedrop integration needs repair",
        "terminal_contains_agent" => "that terminal now holds an agent",
        "space_paused" => "this space is paused",
        "space_inactive" => "Namedrop is not active in this space",
        "reconciliation_required" => "this space needs reconciliation",
        "thread_paused" => "the thread is paused",
        "thread_stopped" => "the thread is stopped",
        other => return other.to_owned(),
    }
    .to_owned()
}

/// Origin first, then the panes the origin is outstanding on in wait order,
/// then everyone else. Excerpt budget is spent down this list, so the panes the
/// origin depends on are the ones that get described.
fn wait_first_order<'a>(
    visible: &[&'a DiscoveredPane],
    origin: &'a DiscoveredPane,
    waits: &[Value],
) -> Vec<&'a DiscoveredPane> {
    let mut ordered: Vec<&DiscoveredPane> = vec![origin];
    let take = |pane_id: &str, ordered: &mut Vec<&'a DiscoveredPane>| {
        if ordered.iter().any(|pane| pane.pane_id == pane_id) {
            return;
        }
        if let Some(pane) = visible.iter().find(|pane| pane.pane_id == pane_id) {
            ordered.push(pane);
        }
    };
    for wait in waits {
        if let Some(pane_id) = wait["pane_id"].as_str() {
            take(pane_id, &mut ordered);
        }
    }
    for pane in visible {
        take(&pane.pane_id, &mut ordered);
    }
    ordered
}

/// One metadata row per pane, with a bounded terminal excerpt for as many as
/// the excerpt cap and the remaining budget allow. Each row carries the exact
/// identity its excerpt was read under, which is what [`verified_rows`] checks
/// afterwards.
fn pane_rows<E, C>(
    pane_host: &E,
    ordered: &[&DiscoveredPane],
    addresses: &[PaneAddressRecord],
    reads: &Deadline<'_, C>,
    warnings: &mut Vec<String>,
) -> Vec<Value>
where
    E: Host,
    C: Clock,
{
    let listed = ordered.len().min(MAX_PANE_ROWS);
    let mut rows = Vec::with_capacity(listed);
    let mut read_error: Option<String> = None;
    let mut stopped_early = false;
    for (index, pane) in ordered.iter().take(listed).enumerate() {
        let mut excerpt = String::new();
        if index < MAX_EXCERPT_PANES {
            match reads.remaining_before_verification() {
                Ok(timeout) => match pane_host.read(
                    &pane.pane_id,
                    EXCERPT_LINES,
                    Some(&ExcerptBudget {
                        timeout,
                        cancelled: reads.cancelled,
                    }),
                ) {
                    Ok(output) => excerpt = excerpt_text(&output),
                    Err(error) => {
                        read_error.get_or_insert(error.into());
                    }
                },
                Err(_) => stopped_early = true,
            }
        }
        rows.push(json!({
            "pane_id": pane.pane_id,
            "terminal_id": pane.terminal_id,
            "session_id": pane.agent_session_id.as_ref().map(|session| session.as_str()),
            "label": label(pane, addresses),
            "persona": persona(pane, addresses),
            "host": host(pane),
            "status": pane.agent_status,
            "excerpt": excerpt,
        }));
    }
    if let Some(error) = read_error {
        warnings.push(format!("Some pane output could not be read: {error}"));
    }
    if stopped_early {
        warnings.push(format!(
            "Collection stopped at its {COLLECTION_BUDGET_MS} ms budget, so some panes have no \
             recent output."
        ));
    }
    if listed > MAX_EXCERPT_PANES {
        warnings.push(format!(
            "Only the first {MAX_EXCERPT_PANES} panes were read for recent output."
        ));
    }
    if ordered.len() > listed {
        warnings.push(format!(
            "This snapshot left out {} beyond the {MAX_PANE_ROWS} it lists.",
            panes(ordered.len() - listed)
        ));
    }
    rows
}

/// Keep only the rows whose pane still holds the exact space, terminal and
/// Agent Session it was read under. A Herdr pane id is reused, so a row that
/// cannot be matched again describes output that may belong to a different
/// pane; it is dropped rather than shown.
fn verified_rows(
    rows: Vec<Value>,
    verified: &DiscoveredWorktree,
    warnings: &mut Vec<String>,
) -> Vec<Value> {
    let mut changed = 0usize;
    let kept: Vec<Value> = rows
        .into_iter()
        .filter(|row| {
            let held = verified.panes.iter().any(|pane| {
                pane.workspace_id == verified.workspace_id
                    && Some(pane.pane_id.as_str()) == row["pane_id"].as_str()
                    && Some(pane.terminal_id.as_str()) == row["terminal_id"].as_str()
                    && pane.agent_session_id.as_ref().map(|s| s.as_str())
                        == row["session_id"].as_str()
            });
            if !held {
                changed += 1;
            }
            held
        })
        .collect();
    if changed > 0 {
        warnings.push(format!(
            "This snapshot dropped {} whose identity changed while it was being collected.",
            panes(changed)
        ));
    }
    kept
}

/// "1 pane" rather than "1 panes": these warnings are shown to a person.
fn panes(count: usize) -> String {
    format!("{count} pane{}", if count == 1 { "" } else { "s" })
}

/// Codex and Claude Code are the hosts Namedrop drives. Anything else, a shell
/// included, has no host and gets facts without a description.
fn host(pane: &DiscoveredPane) -> Option<&'static str> {
    match crate::integration::pane_agent_runtime(pane) {
        Some(AgentRuntime::Codex) => Some("codex"),
        Some(AgentRuntime::ClaudeCode) => Some("claude"),
        None => None,
    }
}

/// The Namedrop name for a pane when it has one, because that is the identity
/// the agents address each other by. Otherwise the pane's own title.
fn label(pane: &DiscoveredPane, addresses: &[PaneAddressRecord]) -> String {
    let alias = addresses
        .iter()
        .find(|address| address.pane_id.as_deref() == Some(pane.pane_id.as_str()))
        .map(|address| format!("@{}", address.alias));
    let label = alias
        .or_else(|| pane.pane_label.clone())
        .or_else(|| pane.occupant_label.clone())
        .unwrap_or_else(|| pane.pane_id.clone());
    one_line(&label, MAX_LABEL_CHARS)
}

/// The persona saved for a pane's Namedrop name: the user's own description of
/// what the agent is for, which is a different fact from what it is doing now.
/// It is read from persisted state only, so nothing a pane printed and nothing
/// a model wrote can become one. `None` is Generic.
///
/// A persona migrated from a legacy role description can be longer than the
/// current bound or carry control characters. It is made terminal-safe and
/// bounded for display here; what is saved is left exactly as it is until the
/// user edits it.
///
/// A name retyped from agent to terminal keeps its persona stored but inert,
/// because only explicit user direction changes one. The panel describes what
/// is in effect, so an inert persona is left out rather than cleared.
fn persona(pane: &DiscoveredPane, addresses: &[PaneAddressRecord]) -> Option<String> {
    addresses
        .iter()
        .find(|address| address.pane_id.as_deref() == Some(pane.pane_id.as_str()))
        .filter(|address| address.pane_type == crate::PaneType::Agent)
        .and_then(|address| address.persona.as_deref())
        .map(|persona| one_line(persona, crate::MAX_PERSONA_CHARS))
        .filter(|persona| !persona.is_empty())
}

/// Pane text is evidence, never instruction: it is redacted, stripped of
/// control sequences and cut to its most recent bytes before anything else
/// sees it.
fn excerpt_text(raw: &str) -> String {
    let mut safe = String::new();
    for character in crate::context::redact_disclosure_text(raw).chars() {
        if character == '\n' || !character.is_control() {
            safe.push(character);
        } else {
            safe.extend(character.escape_default());
        }
    }
    let safe = safe.trim_end();
    let mut start = safe.len().saturating_sub(MAX_EXCERPT_BYTES);
    while start < safe.len() && !safe.is_char_boundary(start) {
        start += 1;
    }
    safe[start..].trim_start().to_owned()
}

/// A single terminal-safe line, cut to `max_chars` characters.
fn one_line(value: &str, max_chars: usize) -> String {
    let line = crate::lifecycle::terminal_safe_text(
        value
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default(),
    );
    if line.chars().count() <= max_chars {
        return line;
    }
    line.chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        + "…"
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::Budget;
    use std::cell::Cell;

    struct TestClock(Cell<u64>);
    impl Clock for TestClock {
        fn now_unix_ms(&self) -> u64 {
            self.0.get()
        }
    }

    #[test]
    fn budget_messages() {
        assert_eq!(COLLECTION_BUDGET_MS, 2_000);
        assert_eq!(VERIFY_RESERVE_MS, 250);
        let clock = TestClock(Cell::new(0));
        let cancelled = AtomicBool::new(false);
        let deadline = Deadline {
            clock: &clock,
            deadline_ms: COLLECTION_BUDGET_MS,
            cancelled: &cancelled,
        };
        assert_eq!(
            Budget::remaining(&deadline).unwrap(),
            Duration::from_millis(2_000)
        );
        clock.0.set(1_750);
        assert_eq!(
            deadline.remaining_before_verification().unwrap_err(),
            "Only the 250 ms identity-check reserve is left of this snapshot's budget."
        );
        clock.0.set(2_000);
        assert_eq!(
            Budget::remaining(&deadline).unwrap_err(),
            "Reading this space took longer than its 2000 ms budget."
        );
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            Budget::remaining(&deadline).unwrap_err(),
            "The agent status snapshot was cancelled."
        );
        let host = crate::TuiHost::new(crate::ProcessHerdr::default());
        let excerpt = ExcerptBudget {
            timeout: Duration::from_secs(1),
            cancelled: &cancelled,
        };
        assert_eq!(
            host.read("pane", 40, Some(&excerpt)).unwrap_err().message,
            "Herdr read cancelled"
        );
        let identity_source = include_str!("agent_status_ui.rs");
        assert!(
            identity_source
                .contains("const IDENTITY_BUDGET: Duration = Duration::from_millis(1_500);")
        );
        assert!(
            identity_source.contains(
                "return Err(\"confirming the invoking pane ran out of time\".to_owned());"
            )
        );
    }
}
