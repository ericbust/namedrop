use super::*;
use crate::final_reply::AnswerClaim;

pub(super) const ITEM_ID_PREFIX: &str = "attention-agent-disposition-";

/// How long an address may have no pane before its session counts as ended.
/// Nothing binds a pane again on its own once reconcile has unbound it: the
/// person or the agent names the pane again, after Herdr restarts and the
/// agent is resumed. That takes minutes, not seconds, so this is long enough
/// for it and still short next to waiting for ever.
pub(crate) const UNBOUND_GRACE_MS: u64 = 10 * 60 * 1_000;

fn item_id(attempt: &crate::DeliveryAttemptId) -> crate::AttentionItemId {
    crate::AttentionItemId::new(format!("{ITEM_ID_PREFIX}{}", attempt.as_str()))
}

struct MissingDispositionFacts {
    disposed: bool,
    /// Another workspace judges this attempt: a copy of a request whose
    /// coordination lives elsewhere, or the reflection of an untracked attempt
    /// that its copy's own workspace judges.
    judged_elsewhere: bool,
    recipient_session: crate::AgentSessionId,
    recipient_retired: bool,
    recipient_session_now: RecipientSession,
    tracked: Option<TrackedRequest>,
    /// What the recipient session's final-reply claim says about its answer.
    claim: AnswerClaim,
}

pub(super) enum RecipientSession {
    /// The address is not routable, has had no pane for less than
    /// `UNBOUND_GRACE_MS`, or its pane's session is not seen yet.
    Unknown,
    /// The address's pane is bound and has no session now, as when its agent
    /// exited, or it has had no pane for `UNBOUND_GRACE_MS`. A pane that
    /// closed retires its address instead.
    Ended,
    Observed(crate::AgentSessionId),
}

impl RecipientSession {
    pub(super) fn replaced(&self, pinned: &crate::AgentSessionId) -> bool {
        matches!(self, Self::Observed(current) if current != pinned)
    }

    /// Why the `pinned` session can never answer again, if it can't.
    pub(super) fn gone(
        &self,
        pinned: &crate::AgentSessionId,
        retired: bool,
    ) -> Option<SessionGone> {
        if retired {
            Some(SessionGone::Retired)
        } else if self.replaced(pinned) {
            Some(SessionGone::Replaced)
        } else if matches!(self, Self::Ended) {
            Some(SessionGone::Ended)
        } else {
            None
        }
    }
}

/// Why a session can never answer again, as Channels names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionGone {
    Retired,
    Replaced,
    Ended,
}

struct TrackedRequest {
    coordination: CoordinationState,
    is_current_request: bool,
}

enum CoordinationState {
    Open,
    Resolved(String),
}

impl From<String> for CoordinationState {
    fn from(outcome: String) -> Self {
        if outcome == "open" {
            Self::Open
        } else {
            Self::Resolved(outcome)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Unanswered {
    Untracked,
    RecipientRetired,
    RecipientSessionReplaced,
    CoordinationResolved { outcome: String },
    HeldAnswerLost(SessionGone),
}

fn unanswered(facts: &MissingDispositionFacts) -> Option<Unanswered> {
    if facts.disposed {
        return None;
    }
    let Some(tracked) = &facts.tracked else {
        if facts.judged_elsewhere {
            return None;
        }
        let gone = facts
            .recipient_session_now
            .gone(&facts.recipient_session, facts.recipient_retired);
        return match facts.claim {
            AnswerClaim::Neither => Some(Unanswered::Untracked),
            // Its Stop has not decided yet, and judges it again when it does.
            AnswerClaim::Deciding { .. } => gone.map(|_| Unanswered::Untracked),
            AnswerClaim::Held => gone.map(Unanswered::HeldAnswerLost),
        };
    };
    if !tracked.is_current_request {
        return None;
    }
    if facts.recipient_retired {
        return Some(Unanswered::RecipientRetired);
    }
    if facts
        .recipient_session_now
        .replaced(&facts.recipient_session)
    {
        return Some(Unanswered::RecipientSessionReplaced);
    }
    if let CoordinationState::Resolved(outcome) = &tracked.coordination {
        return Some(Unanswered::CoordinationResolved {
            outcome: outcome.clone(),
        });
    }
    None
}

impl Unanswered {
    fn summary(&self) -> String {
        match self {
            Self::Untracked => "The agent received this message and ended its turn without a reply or a no-reply. Its exact session can still answer it from delivery.inbox. Once that agent is retired or its session was replaced, delivery acknowledge clears this warning.".to_owned(),
            Self::RecipientRetired => "The agent that received this tracked request was retired before it answered. Nothing can answer it now. Review it, then delivery acknowledge clears this warning.".to_owned(),
            Self::RecipientSessionReplaced => "The agent session that received this tracked request is gone and a new session is registered on its name. The new session cannot answer for it. Review it, then delivery acknowledge clears this warning.".to_owned(),
            Self::CoordinationResolved { outcome } => format!("This tracked request was still unanswered when its coordination was resolved {outcome}. The worker's exact session can still answer it from delivery.inbox. Once that agent is retired or its session was replaced, delivery acknowledge clears this warning."),
            Self::HeldAnswerLost(SessionGone::Retired) => "The agent's pane was closed, or the agent was removed, before it finished working on this message, so its answer can't arrive. Ask again if you still need one. After you review it, delivery acknowledge clears this warning.".to_owned(),
            Self::HeldAnswerLost(SessionGone::Replaced) => "A new session took the agent's name before the old one finished working on this message, so its answer can't arrive. Ask again if you still need one. After you review it, delivery acknowledge clears this warning.".to_owned(),
            Self::HeldAnswerLost(SessionGone::Ended) => "The agent's session ended before it finished working on this message, because the agent exited in its pane or its pane has been gone for 10 minutes. If that session comes back, the agent can still answer. Otherwise ask again if you still need one. After you review it, delivery acknowledge clears this warning.".to_owned(),
        }
    }

    /// Whether a judgement of an untracked attempt wrote `summary`, as
    /// opposed to a Stop whose send failed.
    fn judged(summary: &str) -> bool {
        [
            Self::Untracked,
            Self::HeldAnswerLost(SessionGone::Retired),
            Self::HeldAnswerLost(SessionGone::Replaced),
            Self::HeldAnswerLost(SessionGone::Ended),
        ]
        .iter()
        .any(|reason| reason.summary() == summary)
    }
}

/// How a settle got lost, which is what the warning has to say happened.
#[derive(Clone, Copy)]
pub(crate) enum LostSettle {
    FinalMessage,
    TurnEnd,
    QueuedReply,
    QueuedNoReply,
}

impl LostSettle {
    fn what(self) -> &'static str {
        match self {
            Self::FinalMessage => "The agent's final message could not be sent as its answer",
            Self::TurnEnd => "Namedrop could not close this delivery when the agent's turn ended",
            Self::QueuedReply => "The agent's queued reply was refused",
            Self::QueuedNoReply => "The agent's queued no-reply was refused",
        }
    }
}

impl StateTransaction<'_> {
    /// Opens ND-DELIVERY-003 for a settled attempt that is owed to a human
    /// now. A warning already raised, or closed by a disposition or a human
    /// acknowledgement, is never raised again or reworded.
    pub(super) fn flag_missing_disposition(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        if self.missing_disposition_judged(ns, attempt)? {
            return Ok(());
        }
        let facts = self.missing_disposition_facts(ns, attempt, by.occurred_at_ms)?;
        let Some(reason) = unanswered(&facts) else {
            return Ok(());
        };
        self.open_attention_item(
            ns,
            &AttentionItemDraft {
                id: item_id(attempt),
                source_kind: crate::AttentionSourceKind::AgentDisposition,
                source_id: attempt.as_str().to_owned(),
                summary: reason.summary(),
            },
            by,
        )?;
        Ok(())
    }

    /// Opens ND-DELIVERY-003 for a Stop that could not settle its delivery, or
    /// rewords the warning a settled observation already raised, because this
    /// reason is the one that helps. It covers tracked attempts too, which
    /// `unanswered` stays silent about while their coordination is open.
    pub(crate) fn flag_failed_settle(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        lost: LostSettle,
        reason: &str,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        // Nobody could answer one that isn't, and acknowledge refuses it. This
        // also leaves a warning the person acknowledged closed.
        if !self.attempt_outstanding(ns, attempt)? {
            return Ok(());
        }
        let reason = reason.trim().trim_end_matches('.');
        self.open_attention_item(
            ns,
            &AttentionItemDraft {
                id: item_id(attempt),
                source_kind: crate::AttentionSourceKind::AgentDisposition,
                source_id: attempt.as_str().to_owned(),
                summary: format!("{}. The reason given was: {reason}. Its exact session can still answer it from delivery.inbox. Once that agent is retired or its session was replaced, delivery acknowledge clears this warning.", lost.what()),
            },
            by,
        )?;
        Ok(())
    }

    pub(super) fn reconcile_missing_dispositions(
        &mut self,
        ns: &StateNamespace,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        for attempt in self.unflagged_settled_requests(ns)? {
            self.flag_missing_disposition(ns, &attempt, by)?;
        }
        Ok(())
    }

    /// Brings ND-DELIVERY-003 in line with a final-reply claim its Stop or a
    /// later prompt just decided. A held answer withdraws any warning raised
    /// before, and is judged afresh, so a session already gone still warns.
    /// Any other ending is judged as at settle.
    pub(crate) fn judge_decided_claim(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        if !self.attempt_settled(ns, attempt)? {
            return Ok(());
        }
        self.judge_afresh(ns, attempt, by)
    }

    /// Judges a settled attempt whose claim is still open, and says until
    /// when its Stop may still decide it, if it may. A turn
    /// interrupted with Esc or ended by an API error has no Stop, so once the
    /// wait is over its attempt is judged like any other.
    pub(crate) fn judge_awaiting_stop(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        by: &MutationAttribution,
    ) -> Result<Option<u64>, StoreError> {
        if !self.attempt_settled(ns, attempt)? || self.missing_disposition_judged(ns, attempt)? {
            return Ok(None);
        }
        self.flag_missing_disposition(ns, attempt, by)?;
        let facts = self.missing_disposition_facts(ns, attempt, by.occurred_at_ms)?;
        Ok(match facts.claim {
            AnswerClaim::Deciding { until_ms }
                if facts.tracked.is_none() && !facts.disposed && !facts.judged_elsewhere =>
            {
                Some(until_ms)
            }
            _ => None,
        })
    }

    /// Judges the held answers of every address that has lost its pane with
    /// no close event to retire it, and says when to look again at those
    /// still inside `UNBOUND_GRACE_MS`. It looks only at settled attempts
    /// with no disposition, no warning and no coordination, on addresses
    /// that had a pane and have none now. No attempt is made for an address
    /// without a pane, so that set is fixed when the pane goes, and each held
    /// answer in it leaves at its first drain past the grace.
    pub(crate) fn judge_unbound_attempts(
        &mut self,
        ns: &StateNamespace,
        by: &MutationAttribution,
    ) -> Result<Vec<(crate::DeliveryAttemptId, u64)>, StoreError> {
        let unbound = {
            let mut statement = self.transaction.prepare(
                "SELECT a.attempt_id, MAX(b.unbound_at_ms)
                 FROM delivery_attempts a
                 JOIN delivery_attempt_state s USING (
                     session_socket_id, worktree_id, attempt_id, address_id
                 )
                 JOIN pane_addresses p USING (session_socket_id, worktree_id, address_id)
                 JOIN pane_bindings b USING (session_socket_id, worktree_id, address_id)
                 JOIN mailbox_deliveries d USING (
                     session_socket_id, worktree_id, delivery_id, address_id
                 )
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND s.state = 'settled' AND p.retired = 0
                   AND NOT EXISTS (
                       SELECT 1 FROM delivery_dispositions x
                       WHERE x.session_socket_id = a.session_socket_id
                         AND x.worktree_id = a.worktree_id
                         AND x.attempt_id = a.attempt_id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM attention_items i
                       WHERE i.session_socket_id = a.session_socket_id
                         AND i.worktree_id = a.worktree_id
                         AND i.attention_item_id = ?3 || a.attempt_id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM coordination_links l
                       JOIN coordination_assignments g USING (
                           session_socket_id, worktree_id, assignment_id
                       )
                       WHERE l.session_socket_id = a.session_socket_id
                         AND l.worktree_id = a.worktree_id
                         AND l.message_id = d.message_id AND l.kind = 'request'
                         AND g.recipient_address_id = a.address_id
                         AND g.recipient_session_id = a.agent_session_id
                   )
                 GROUP BY a.attempt_id
                 HAVING MAX(b.active) = 0
                 ORDER BY MIN(a.created_at_ms), a.attempt_id",
            )?;
            statement
                .query_map(
                    params![ns.session_socket_id, ns.space_id.as_str(), ITEM_ID_PREFIX],
                    |row| {
                        Ok((
                            crate::DeliveryAttemptId::new(row.get::<_, String>(0)?),
                            row.get::<_, Option<i64>>(1)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut pending = Vec::new();
        for (attempt, unbound_at) in unbound {
            let until = unbound_at
                .and_then(|at| u64::try_from(at).ok())
                .unwrap_or(0)
                .saturating_add(UNBOUND_GRACE_MS);
            if by.occurred_at_ms < until {
                pending.push((attempt, until));
            } else if self
                .missing_disposition_facts(ns, &attempt, by.occurred_at_ms)?
                .claim
                != AnswerClaim::Neither
            {
                self.flag_missing_disposition(ns, &attempt, by)?;
            }
        }
        Ok(pending)
    }

    fn attempt_settled(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<bool, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT state = 'settled' FROM delivery_attempt_state
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                params![ns.session_socket_id, ns.space_id.as_str(), attempt.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// When the attempt settled, which is when its pane was seen idle.
    fn settled_at(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<u64, StoreError> {
        let at: Option<i64> = self.transaction.query_row(
            "SELECT MAX(occurred_at_ms) FROM transition_facts
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND entity_kind = ?3 AND entity_id = ?4 AND transition_kind = ?5",
            params![
                ns.session_socket_id,
                ns.space_id.as_str(),
                TransitionEntityKind::DeliveryAttempt.as_str(),
                attempt.as_str(),
                TransitionKind::Settled.as_str()
            ],
            |row| row.get(0),
        )?;
        Ok(at.map_or(0, |at| u64::try_from(at).unwrap_or(0)))
    }

    /// Judges a settled attempt again. A held answer's open warning that a
    /// judgement raised is withdrawn first, unless it already says what is
    /// true now, since why its session can't answer, or whether it can, may
    /// have changed. A warning that a send failed, one the person
    /// acknowledged, or one on an answer not held, stays as it is, and so
    /// does any while the address has no pane to say whether its session is
    /// still there.
    fn judge_afresh(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let facts = self.missing_disposition_facts(ns, attempt, by.occurred_at_ms)?;
        if facts.claim == AnswerClaim::Held
            && (facts.recipient_retired
                || !matches!(facts.recipient_session_now, RecipientSession::Unknown))
            && let Some(open) = self.open_warning(ns, attempt)?
            && Unanswered::judged(&open)
            && unanswered(&facts).map(|reason| reason.summary()) != Some(open)
        {
            self.withdraw_open_warning(ns, attempt, by)?;
        }
        self.flag_missing_disposition(ns, attempt, by)
    }

    /// Judges afresh every open ND-DELIVERY-003 warning in every space, so
    /// one an earlier release raised on a held answer is withdrawn while its
    /// session can still answer, and says why when it can't.
    pub(crate) fn judge_open_disposition_warnings(
        &mut self,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let open = {
            let mut statement = self.transaction.prepare(
                "SELECT i.session_socket_id, i.worktree_id, i.source_id
                 FROM attention_items i
                 WHERE i.source_kind = ?1 AND i.resolved_at_ms IS NULL
                   AND EXISTS (SELECT 1 FROM delivery_attempt_state s
                       WHERE s.session_socket_id = i.session_socket_id
                         AND s.worktree_id = i.worktree_id
                         AND s.attempt_id = i.source_id AND s.state = 'settled')",
            )?;
            statement
                .query_map(
                    params![crate::AttentionSourceKind::AgentDisposition.as_str()],
                    |row| {
                        Ok((
                            StateNamespace::for_space(
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                            ),
                            crate::DeliveryAttemptId::new(row.get::<_, String>(2)?),
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (ns, attempt) in open {
            self.judge_afresh(&ns, &attempt, by)?;
        }
        Ok(())
    }

    /// Why a held answer to `attempt` can never arrive, as its open warning
    /// says.
    pub(super) fn held_answer_lost(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<Option<SessionGone>, StoreError> {
        let Some(open) = self.open_warning(ns, attempt)? else {
            return Ok(None);
        };
        Ok([
            SessionGone::Retired,
            SessionGone::Replaced,
            SessionGone::Ended,
        ]
        .into_iter()
        .find(|gone| Unanswered::HeldAnswerLost(*gone).summary() == open))
    }

    /// The summary of the attempt's open warning, if it has one.
    fn open_warning(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT summary FROM attention_items
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    crate::AttentionSourceKind::AgentDisposition.as_str(),
                    attempt.as_str()
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Deletes the open warning rather than resolving it: a resolved one
    /// takes the attempt out of its session's inbox, where the held answer
    /// comes back through, and deleting leaves it unjudged.
    fn withdraw_open_warning(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let deleted = self.transaction.execute(
            "DELETE FROM attention_items
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
            params![
                ns.session_socket_id,
                ns.space_id.as_str(),
                crate::AttentionSourceKind::AgentDisposition.as_str(),
                attempt.as_str()
            ],
        )?;
        if deleted == 0 {
            return Ok(());
        }
        self.append_transition(
            ns,
            TransitionEntityKind::AttentionItem,
            item_id(attempt).as_str(),
            TransitionKind::Resolved,
            by,
        )
    }

    /// Judges again the settled attempts of `address` the person has not
    /// acknowledged, once its session changed, its pane closed or it was
    /// retired: an answer held for one of them may no longer arrive, or may
    /// again.
    pub(super) fn judge_address_attempts(
        &mut self,
        ns: &StateNamespace,
        address: &PaneAddressId,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let attempts = {
            let mut statement = self.transaction.prepare(
                "SELECT a.attempt_id
                 FROM delivery_attempts a
                 JOIN delivery_attempt_state s USING (
                     session_socket_id, worktree_id, attempt_id, address_id
                 )
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.address_id = ?3 AND s.state = 'settled'
                   AND NOT EXISTS (
                       SELECT 1 FROM delivery_dispositions p
                       WHERE p.session_socket_id = a.session_socket_id
                         AND p.worktree_id = a.worktree_id
                         AND p.attempt_id = a.attempt_id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM attention_items i
                       WHERE i.session_socket_id = a.session_socket_id
                         AND i.worktree_id = a.worktree_id
                         AND i.attention_item_id = ?4 || a.attempt_id
                         AND i.resolved_at_ms IS NOT NULL
                   )
                 ORDER BY a.created_at_ms, a.rowid",
            )?;
            statement
                .query_map(
                    params![
                        ns.session_socket_id,
                        ns.space_id.as_str(),
                        address.as_str(),
                        ITEM_ID_PREFIX
                    ],
                    |row| Ok(crate::DeliveryAttemptId::new(row.get::<_, String>(0)?)),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for attempt in attempts {
            self.judge_afresh(ns, &attempt, by)?;
        }
        Ok(())
    }

    /// The session the address's pane was last seen with since it was bound,
    /// the empty one included.
    pub(super) fn latest_session_observation(
        &self,
        ns: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT o.agent_session_id
                 FROM agent_session_observations o
                 JOIN pane_bindings b
                   ON b.session_socket_id = o.session_socket_id
                  AND b.worktree_id = o.worktree_id
                  AND b.address_id = o.address_id
                  AND b.active = 1
                 WHERE o.session_socket_id = ?1 AND o.worktree_id = ?2
                   AND o.address_id = ?3 AND o.observed_at_ms >= b.bound_at_ms
                 ORDER BY o.observed_at_ms DESC, o.rowid DESC
                 LIMIT 1",
                params![ns.session_socket_id, ns.space_id.as_str(), address.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn missing_disposition_judged(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM attention_items
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attention_item_id = ?3)",
            params![
                ns.session_socket_id,
                ns.space_id.as_str(),
                item_id(attempt).as_str()
            ],
            |row| row.get(0),
        )?)
    }

    pub(super) fn recipient_session_now(
        &self,
        ns: &StateNamespace,
        address: &PaneAddressId,
        now_ms: u64,
    ) -> Result<RecipientSession, StoreError> {
        let Some(routing) = self.optional_routing_address_by_id(ns, address)? else {
            return Ok(RecipientSession::Unknown);
        };
        let ended = match (routing.agent_session_id, routing.pane_binding_id) {
            (Some(session), _) => return Ok(RecipientSession::Observed(session)),
            (None, Some(_)) => self.latest_session_observation(ns, address)?.as_deref() == Some(""),
            (None, None) => self
                .unbound_since(ns, address)?
                .is_some_and(|since| now_ms >= since.saturating_add(UNBOUND_GRACE_MS)),
        };
        Ok(if ended {
            RecipientSession::Ended
        } else {
            RecipientSession::Unknown
        })
    }

    /// When the address's last pane was unbound, if it ever had one.
    fn unbound_since(
        &self,
        ns: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<Option<u64>, StoreError> {
        let at: Option<i64> = self.transaction.query_row(
            "SELECT MAX(unbound_at_ms) FROM pane_bindings
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3",
            params![ns.session_socket_id, ns.space_id.as_str(), address.as_str()],
            |row| row.get(0),
        )?;
        Ok(at.and_then(|at| u64::try_from(at).ok()))
    }

    fn missing_disposition_facts(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        now_ms: u64,
    ) -> Result<MissingDispositionFacts, StoreError> {
        let (address, session, retired, tracked, message) = self.transaction.query_row(
            "SELECT a.address_id, a.agent_session_id, p.retired, c.outcome,
                    t.current_message_id = d.message_id, t.coordination_id, d.message_id
             FROM delivery_attempts a
             JOIN mailbox_deliveries d USING (
                 session_socket_id, worktree_id, delivery_id, address_id
             )
             JOIN pane_addresses p USING (session_socket_id, worktree_id, address_id)
             LEFT JOIN (
                 SELECT l.session_socket_id, l.worktree_id, l.message_id,
                        g.recipient_address_id, g.recipient_session_id,
                        g.coordination_id, g.current_message_id
                 FROM coordination_links l
                 JOIN coordination_assignments g USING (
                     session_socket_id, worktree_id, assignment_id
                 )
                 WHERE l.kind = 'request'
             ) t ON t.session_socket_id = a.session_socket_id
                AND t.worktree_id = a.worktree_id
                AND t.message_id = d.message_id
                AND t.recipient_address_id = a.address_id
                AND t.recipient_session_id = a.agent_session_id
             LEFT JOIN coordinations c
               ON c.session_socket_id = t.session_socket_id
              AND c.worktree_id = t.worktree_id
              AND c.coordination_id = t.coordination_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![ns.session_socket_id, ns.space_id.as_str(), attempt.as_str()],
            |row| {
                let tracked = row
                    .get::<_, Option<String>>(5)?
                    .map(|_coordination_id| {
                        Ok::<_, rusqlite::Error>(TrackedRequest {
                            coordination: row.get::<_, String>(3)?.into(),
                            is_current_request: row.get(4)?,
                        })
                    })
                    .transpose()?;
                Ok((
                    PaneAddressId::new(row.get::<_, String>(0)?),
                    crate::AgentSessionId::new(row.get::<_, String>(1)?),
                    row.get::<_, bool>(2)?,
                    tracked,
                    MessageId::new(row.get::<_, String>(6)?),
                ))
            },
        )?;
        // An envoy's attempt is the reflection of its worker's, whose
        // retirement and current session are read where that worker lives.
        let principal = self.principal(ns, &address)?;
        let judged_elsewhere = tracked.is_none()
            && (principal.ns != *ns
                || self
                    .tracked_message(ns, &message)?
                    .is_some_and(|found| found.home != *ns));
        let claim = crate::final_reply::answer_claim(
            self.state_directory,
            ns,
            &session,
            attempt,
            || self.settled_at(ns, attempt),
            now_ms,
        )?;
        Ok(MissingDispositionFacts {
            claim,
            disposed: self.delivery_disposition_exists(ns, attempt)?,
            judged_elsewhere,
            recipient_session: session,
            recipient_retired: retired
                || self.address_retired(&principal.ns, &principal.address_id)?,
            recipient_session_now: self.recipient_session_now(
                &principal.ns,
                &principal.address_id,
                now_ms,
            )?,
            tracked,
        })
    }

    /// Untracked attempts are judged once, at settle.
    fn unflagged_settled_requests(
        &self,
        ns: &StateNamespace,
    ) -> Result<Vec<crate::DeliveryAttemptId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.attempt_id
             FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             JOIN mailbox_deliveries d USING (
                 session_socket_id, worktree_id, delivery_id, address_id
             )
             JOIN coordination_links l
               ON l.session_socket_id = a.session_socket_id
              AND l.worktree_id = a.worktree_id
              AND l.message_id = d.message_id
              AND l.kind = 'request'
             JOIN coordination_assignments g
               ON g.session_socket_id = l.session_socket_id
              AND g.worktree_id = l.worktree_id
              AND g.assignment_id = l.assignment_id
              AND g.recipient_address_id = a.address_id
              AND g.recipient_session_id = a.agent_session_id
              AND g.current_message_id = d.message_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               AND s.state = 'settled'
               AND NOT EXISTS (
                   SELECT 1 FROM delivery_dispositions p
                   WHERE p.session_socket_id = a.session_socket_id
                     AND p.worktree_id = a.worktree_id
                     AND p.attempt_id = a.attempt_id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM attention_items i
                   WHERE i.session_socket_id = a.session_socket_id
                     AND i.worktree_id = a.worktree_id
                     AND i.attention_item_id = ?3 || a.attempt_id
               )
             ORDER BY a.created_at_ms, a.rowid",
        )?;
        statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), ITEM_ID_PREFIX],
                |row| Ok(crate::DeliveryAttemptId::new(row.get::<_, String>(0)?)),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> crate::AgentSessionId {
        crate::AgentSessionId::new(id)
    }

    fn awaited() -> TrackedRequest {
        TrackedRequest {
            coordination: CoordinationState::Open,
            is_current_request: true,
        }
    }

    fn facts(tracked: Option<TrackedRequest>) -> MissingDispositionFacts {
        MissingDispositionFacts {
            disposed: false,
            judged_elsewhere: false,
            recipient_session: session("session-worker"),
            recipient_retired: false,
            recipient_session_now: RecipientSession::Observed(session("session-worker")),
            tracked,
            claim: AnswerClaim::Neither,
        }
    }

    #[test]
    fn a_held_answer_is_owed_only_once_its_session_is_gone() {
        let mut held = facts(None);
        held.claim = AnswerClaim::Held;
        assert_eq!(unanswered(&held), None, "the answer may still arrive");
        held.recipient_session_now = RecipientSession::Unknown;
        assert_eq!(
            unanswered(&held),
            None,
            "an unroutable address is not a gone session"
        );
        held.recipient_session_now = RecipientSession::Ended;
        assert_eq!(
            unanswered(&held),
            Some(Unanswered::HeldAnswerLost(SessionGone::Ended))
        );
        held.recipient_session_now = RecipientSession::Observed(session("session-worker-2"));
        assert_eq!(
            unanswered(&held),
            Some(Unanswered::HeldAnswerLost(SessionGone::Replaced))
        );
        held.recipient_retired = true;
        assert_eq!(
            unanswered(&held),
            Some(Unanswered::HeldAnswerLost(SessionGone::Retired))
        );
        held.judged_elsewhere = true;
        assert_eq!(unanswered(&held), None);
    }

    #[test]
    fn an_answer_its_stop_has_not_decided_is_owed_only_once_its_session_is_gone() {
        let mut deciding = facts(None);
        deciding.claim = AnswerClaim::Deciding { until_ms: 1 };
        assert_eq!(unanswered(&deciding), None, "its Stop may still answer");
        deciding.recipient_session_now = RecipientSession::Unknown;
        assert_eq!(unanswered(&deciding), None);
        for now in [
            RecipientSession::Ended,
            RecipientSession::Observed(session("session-worker-2")),
        ] {
            deciding.recipient_session_now = now;
            assert_eq!(unanswered(&deciding), Some(Unanswered::Untracked));
        }
        deciding.recipient_session_now = RecipientSession::Observed(session("session-worker"));
        deciding.recipient_retired = true;
        assert_eq!(unanswered(&deciding), Some(Unanswered::Untracked));
    }

    #[test]
    fn an_ended_session_changes_nothing_for_tracked_work() {
        let mut ended = facts(Some(awaited()));
        ended.recipient_session_now = RecipientSession::Ended;
        ended.claim = AnswerClaim::Held;
        assert_eq!(unanswered(&ended), None);
    }

    #[test]
    fn nothing_is_owed_while_an_open_assignment_awaits_the_request() {
        assert_eq!(unanswered(&facts(Some(awaited()))), None);
    }

    #[test]
    fn a_disposed_attempt_is_never_owed() {
        let mut disposed = facts(None);
        disposed.disposed = true;
        assert_eq!(unanswered(&disposed), None);
    }

    #[test]
    fn an_untracked_attempt_is_owed_at_settle() {
        assert_eq!(unanswered(&facts(None)), Some(Unanswered::Untracked));
    }

    #[test]
    fn an_attempt_another_workspace_judges_is_never_owed_here() {
        let mut elsewhere = facts(None);
        elsewhere.judged_elsewhere = true;
        assert_eq!(unanswered(&elsewhere), None);
        let mut reflected = facts(Some(TrackedRequest {
            coordination: CoordinationState::Resolved("blocked".to_owned()),
            is_current_request: true,
        }));
        reflected.judged_elsewhere = true;
        assert_eq!(
            unanswered(&reflected),
            Some(Unanswered::CoordinationResolved {
                outcome: "blocked".to_owned()
            }),
            "the coordination's own workspace judges a tracked attempt"
        );
    }

    #[test]
    fn an_unroutable_address_is_not_a_replaced_session() {
        let mut unroutable = facts(Some(awaited()));
        unroutable.recipient_session_now = RecipientSession::Unknown;
        assert_eq!(unanswered(&unroutable), None);
    }

    #[test]
    fn the_first_reason_that_applies_names_the_action() {
        let mut facts = facts(Some(TrackedRequest {
            coordination: CoordinationState::Resolved("cancelled".to_owned()),
            is_current_request: false,
        }));
        facts.recipient_retired = true;
        facts.recipient_session_now = RecipientSession::Observed(session("session-worker-2"));
        assert_eq!(
            unanswered(&facts),
            None,
            "a superseded request is never owed, whatever else is true"
        );
        facts.tracked.as_mut().unwrap().is_current_request = true;
        assert_eq!(unanswered(&facts), Some(Unanswered::RecipientRetired));
        facts.recipient_retired = false;
        assert_eq!(
            unanswered(&facts),
            Some(Unanswered::RecipientSessionReplaced)
        );
        facts.recipient_session_now = RecipientSession::Observed(session("session-worker"));
        assert_eq!(
            unanswered(&facts),
            Some(Unanswered::CoordinationResolved {
                outcome: "cancelled".to_owned()
            })
        );
        facts.tracked.as_mut().unwrap().coordination = CoordinationState::Open;
        assert_eq!(unanswered(&facts), None);
    }
}
