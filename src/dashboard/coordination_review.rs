//! "Ask to review" and "Review all" on Channels' handoff cards. The press is
//! the operator's confirmation for the listed coordinations only: it records
//! one review grant per coordination for the reviewer's current session, then
//! sends the reviewer one operator message that says what it may do.
use super::snapshot::SpaceKey;
use crate::store::coordination_review::{CoordinationWarning, OperatorSession, WarningCause};
use crate::{PaneAddressId, StateNamespace, StateStore, StoreErrorKind};
use serde::Deserialize;
use std::path::Path;

const MAX_ITEMS: usize = 20;
const REPORT_CHARS: usize = 1500;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewPost {
    pub request_id: String,
    pub reviewer: String,
    pub coordination_ids: Vec<String>,
}

pub(super) struct ReviewDraft {
    pub namespace: StateNamespace,
    pub source: String,
    pub granted: Vec<(String, u64)>,
}

pub(super) enum ReviewError {
    NotFound,
    Refused(String),
    Unavailable,
}

/// Grants first, in one transaction, so the message the agent reads never
/// names a command no grant stands behind.
pub(super) fn commit(
    state: &Path,
    key: &SpaceKey,
    post: &ReviewPost,
    operator: &OperatorSession,
    now_ms: u64,
) -> Result<ReviewDraft, ReviewError> {
    if post.request_id.trim().is_empty() {
        return Err(ReviewError::Refused("request_id is required.".to_owned()));
    }
    if post.coordination_ids.is_empty() || post.coordination_ids.len() > MAX_ITEMS {
        return Err(ReviewError::Refused(format!(
            "Choose 1 to {MAX_ITEMS} handoffs to review."
        )));
    }
    let namespace = StateStore::open_read_only(state)
        .and_then(|store| store.map(|mut store| store.dashboard_spaces()).transpose())
        .map_err(|_| ReviewError::Unavailable)?
        .and_then(|spaces| {
            spaces
                .into_iter()
                .find(|space| SpaceKey::of(&space.namespace) == *key)
                .map(|space| space.namespace)
        })
        .ok_or(ReviewError::NotFound)?;
    let reviewer = PaneAddressId::new(post.reviewer.as_str());
    let mut store = StateStore::open(state).map_err(|_| ReviewError::Unavailable)?;
    let (alias, items, expires_at_ms) = store
        .transaction(|tx| {
            if tx.coordination_review_requested(&namespace, &post.request_id)? {
                return Err(crate::StoreError::invalid_input(
                    "This review request was already submitted. Press Ask to review again for a new review.",
                ));
            }
            let items: Vec<CoordinationWarning> = tx
                .coordination_warnings(&namespace)?
                .into_iter()
                .filter(|warning| {
                    post.coordination_ids
                        .iter()
                        .any(|id| id == warning.coordination_id.as_str())
                })
                .collect();
            if items.is_empty() {
                return Err(crate::StoreError::invalid_input(
                    "These handoffs no longer need a decision.",
                ));
            }
            let mut granted = (String::new(), 0);
            for item in &items {
                granted = tx.grant_coordination_review(
                    &namespace,
                    &item.coordination_id,
                    &reviewer,
                    operator,
                    &post.request_id,
                    now_ms,
                )?;
            }
            Ok((granted.0, items, granted.1))
        })
        .map_err(|error| match error.kind() {
            StoreErrorKind::InvalidInput => ReviewError::Refused(error.to_string()),
            _ => ReviewError::Unavailable,
        })?;
    Ok(ReviewDraft {
        source: render(&alias, &reviewer, &items),
        granted: items
            .iter()
            .map(|item| (item.coordination_id.as_str().to_owned(), expires_at_ms))
            .collect(),
        namespace,
    })
}

fn cause_words(item: &CoordinationWarning) -> String {
    let lead = format!("@{}", item.lead_alias);
    let reason = item
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map_or_else(String::new, |reason| format!(" Recorded reason: {reason}"));
    match item.cause {
        WarningCause::Blocked => format!("{lead} resolved it as blocked.{reason}"),
        WarningCause::Cancelled => format!("{lead} cancelled it.{reason}"),
        WarningCause::Stalled => format!(
            "It is still open. {lead}'s follow-up ended without a decision, or is held or paused. Only {lead} can continue or resolve it, and it can't be acknowledged while it's open."
        ),
        WarningCause::LeadRetired => format!(
            "It is still open, but {lead}'s pane closed, so {lead} can never resolve it. Nothing can continue it from here."
        ),
        WarningCause::Report => format!(
            "{lead} completed it, but its report is missing or the report's delivery did not finish."
        ),
    }
}

fn excerpt(report: &str) -> String {
    let mut text: String = report.trim().chars().take(REPORT_CHARS).collect();
    if report.trim().chars().count() > REPORT_CHARS {
        text.push_str(" [cut]");
    }
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The reviewer's message.
pub(super) fn render(
    reviewer_alias: &str,
    reviewer: &PaneAddressId,
    items: &[CoordinationWarning],
) -> String {
    let count = if items.len() == 1 {
        "1 tracked handoff that needs".to_owned()
    } else {
        format!("{} tracked handoffs that need", items.len())
    };
    let mut text = format!(
        "@{reviewer_alias} The operator asks you to review {count} a decision. For each one, continue it, resolve it, or, if it is finished, acknowledge it. Then reply here with what you did for each.\n\n\
         Acknowledging only clears the warning. The outcome and report stay as they are, and no work resumes or finishes. Acknowledge a handoff when nothing more should happen on it now. You may acknowledge only the coordinations listed here, only with `handoff acknowledge-granted` as your control binds it, and never with `handoff acknowledge`. The operator's grant covers each one once, for your current session, for 15 minutes.\n"
    );
    for (n, item) in items.iter().enumerate() {
        text.push_str(&format!(
            "\n**{}. {}**\n\nCoordination `{}`. {}\n",
            n + 1,
            item.deliverable.trim(),
            item.coordination_id.as_str(),
            cause_words(item)
        ));
        if &item.lead_address_id != reviewer
            && let Some(report) = item.report.as_deref().filter(|r| !r.trim().is_empty())
        {
            text.push_str(&format!("\nSaved report:\n\n{}\n", excerpt(report)));
        }
        text.push_str(&format!(
            "\nTo acknowledge it, run `handoff acknowledge-granted` with `{{\"coordination_id\":\"{}\"}}`.\n",
            item.coordination_id.as_str()
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoordinationId;

    fn warning(id: &str, cause: WarningCause, lead: &str) -> CoordinationWarning {
        CoordinationWarning {
            coordination_id: CoordinationId::new(id),
            cause,
            deliverable: format!("Ship {id}"),
            reason: Some("Waiting on an API key.".into()),
            report: Some("Line one.\nLine two.".into()),
            summary: String::new(),
            lead_address_id: PaneAddressId::new(lead),
            lead_alias: lead.into(),
            lead_retired: cause == WarningCause::LeadRetired,
        }
    }

    #[test]
    fn the_lead_is_asked_to_decide_and_told_its_one_granted_command() {
        let text = render(
            "lead",
            &PaneAddressId::new("lead"),
            &[warning("c-1", WarningCause::Blocked, "lead")],
        );
        assert!(text.starts_with(
            "@lead The operator asks you to review 1 tracked handoff that needs a decision."
        ));
        assert!(text.contains("**1. Ship c-1**"));
        assert!(
            text.contains("@lead resolved it as blocked. Recorded reason: Waiting on an API key.")
        );
        assert!(
            text.contains(r#"run `handoff acknowledge-granted` with `{"coordination_id":"c-1"}`"#)
        );
        assert!(text.contains("never with `handoff acknowledge`"));
        assert!(
            !text.contains("Saved report"),
            "the lead reads its own report"
        );
    }

    #[test]
    fn review_all_lists_every_item_and_gives_another_agent_the_saved_reports() {
        let text = render(
            "claude",
            &PaneAddressId::new("claude"),
            &[
                warning("c-1", WarningCause::LeadRetired, "gone"),
                warning("c-2", WarningCause::Stalled, "codex"),
            ],
        );
        assert!(text.starts_with(
            "@claude The operator asks you to review 2 tracked handoffs that need a decision."
        ));
        assert!(text.contains("@gone's pane closed"));
        assert!(text.contains("Only @codex can continue or resolve it"));
        assert_eq!(
            text.matches("Saved report:\n\n> Line one.\n> Line two.")
                .count(),
            2
        );
        assert_eq!(text.matches("handoff acknowledge-granted").count(), 3);
    }
}
