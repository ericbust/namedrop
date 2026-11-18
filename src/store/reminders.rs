use super::*;
use crate::reminder::{Reminder, ReminderOwner, ReminderSpec, ReminderState};
use crate::{AgentSessionId, DeliveryId};
use serde_json::{Value, json};

pub(super) const SCHEMA: &str = r#"
CREATE TABLE reminders (
 session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL, reminder_id TEXT NOT NULL,
 owner_session_id TEXT NOT NULL, owner_json TEXT NOT NULL, spec_json TEXT NOT NULL,
 request_id TEXT NOT NULL, request_fingerprint TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('scheduled','awaiting_delivery','completed','cancelled','failed')),
 state_json TEXT NOT NULL, due_at_ms INTEGER,
 PRIMARY KEY(session_socket_id,worktree_id,reminder_id),
 UNIQUE(session_socket_id,worktree_id,owner_session_id,request_id),
 CHECK((state='scheduled')=(due_at_ms IS NOT NULL)),
 FOREIGN KEY(session_socket_id,worktree_id) REFERENCES worktrees(session_socket_id,worktree_id)
);
CREATE INDEX reminders_pending ON reminders(due_at_ms) WHERE state IN ('scheduled','awaiting_delivery');
CREATE INDEX reminders_owner ON reminders(session_socket_id,worktree_id,owner_session_id,state);
CREATE TABLE reminder_fires (
 session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL, reminder_id TEXT NOT NULL,
 scheduled_at_ms INTEGER NOT NULL, ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 100),
 message_id TEXT NOT NULL, delivery_id TEXT NOT NULL,
 PRIMARY KEY(session_socket_id,worktree_id,reminder_id,scheduled_at_ms),
 UNIQUE(session_socket_id,worktree_id,message_id), UNIQUE(session_socket_id,worktree_id,delivery_id),
 FOREIGN KEY(session_socket_id,worktree_id,reminder_id) REFERENCES reminders(session_socket_id,worktree_id,reminder_id),
 FOREIGN KEY(session_socket_id,worktree_id,message_id) REFERENCES collaboration_messages(session_socket_id,worktree_id,message_id),
 FOREIGN KEY(session_socket_id,worktree_id,delivery_id) REFERENCES mailbox_deliveries(session_socket_id,worktree_id,delivery_id)
);
"#;

fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|e| StoreError::invalid_input(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: String) -> Result<T, StoreError> {
    serde_json::from_str(&value)
        .map_err(|e| StoreError::invalid_input(format!("invalid persisted reminder: {e}")))
}

impl StateTransaction<'_> {
    fn reminder(&self, ns: &StateNamespace, id: &str) -> Result<Reminder, StoreError> {
        let (owner,spec,state,tag,due,session): (String,String,String,String,Option<i64>,String) = self.transaction.query_row("SELECT owner_json,spec_json,state_json,state,due_at_ms,owner_session_id FROM reminders WHERE session_socket_id=?1 AND worktree_id=?2 AND reminder_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        let owner: ReminderOwner = decode(owner)?;
        let state: ReminderState = decode(state)?;
        let expected_due = match &state {
            ReminderState::Scheduled { due_at_ms, ordinal } => {
                if !(1..=100).contains(ordinal) {
                    return Err(StoreError::invalid_input(
                        "invalid persisted reminder ordinal",
                    ));
                }
                Some(to_i64(*due_at_ms)?)
            }
            ReminderState::AwaitingDelivery { ordinal, .. } => {
                if !(1..=100).contains(ordinal) {
                    return Err(StoreError::invalid_input(
                        "invalid persisted reminder ordinal",
                    ));
                }
                None
            }
            _ => None,
        };
        if state.tag() != tag || expected_due != due || owner.session.as_str() != session {
            return Err(StoreError::invalid_input(
                "inconsistent persisted reminder state",
            ));
        }
        Ok(Reminder {
            reminder_id: id.to_owned(),
            owner,
            spec: decode(spec)?,
            state,
        })
    }

    pub(crate) fn replay_reminder(
        &self,
        ns: &StateNamespace,
        session: &AgentSessionId,
        request: &str,
        fingerprint: &str,
    ) -> Result<Option<Reminder>, StoreError> {
        let found: Option<(String,String)> = self.transaction.query_row("SELECT reminder_id,request_fingerprint FROM reminders WHERE session_socket_id=?1 AND worktree_id=?2 AND owner_session_id=?3 AND request_id=?4",params![ns.session_socket_id,ns.space_id.as_str(),session.as_str(),request],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        match found {
            None => Ok(None),
            Some((id, stored)) if stored == fingerprint => self.reminder(ns, &id).map(Some),
            Some(_) => Err(StoreError::invalid_input(
                "request_id was already used for different reminder content",
            )),
        }
    }

    pub(crate) fn set_reminder(
        &mut self,
        ns: &StateNamespace,
        id: String,
        owner: ReminderOwner,
        mut spec: ReminderSpec,
        request: &str,
        fingerprint: &str,
    ) -> Result<Reminder, StoreError> {
        let count:u32=self.transaction.query_row("SELECT count(*) FROM reminders WHERE session_socket_id=?1 AND worktree_id=?2 AND owner_session_id=?3 AND state IN ('scheduled','awaiting_delivery')",params![ns.session_socket_id,ns.space_id.as_str(),owner.session.as_str()],|r|r.get(0))?;
        if count >= 20 {
            return Err(StoreError::invalid_input(
                "at most 20 pending reminders per Agent Session",
            ));
        }
        if let Some(message) = &spec.in_reply_to {
            let received:bool=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id) WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND d.message_id=?3 AND a.address_id=?4 AND a.agent_session_id=?5)",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str(),owner.address.as_str(),owner.session.as_str()],|r|r.get(0))?;
            if !received {
                return Err(StoreError::invalid_input(
                    "in_reply_to must be a message delivered to this exact Agent Session",
                ));
            }
            spec.in_reply_to = self
                .reminder_reply_target(ns, message)?
                .unwrap_or_else(|| Some(message.clone()));
            if let Some(target) = &spec.in_reply_to {
                let route = self.message_route(ns, target)?;
                if route.from_namedrop
                    || route.reply_mode == crate::DeliveryReplyMode::ReportToHuman
                {
                    return Err(StoreError::invalid_input(
                        "in_reply_to system-only message does not support reminder replies",
                    ));
                }
            }
        }
        let state = ReminderState::Scheduled {
            due_at_ms: spec.first_due_at_ms,
            ordinal: 1,
        };
        self.transaction.execute("INSERT INTO reminders(session_socket_id,worktree_id,reminder_id,owner_session_id,owner_json,spec_json,request_id,request_fingerprint,state,state_json,due_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'scheduled',?9,?10)",params![ns.session_socket_id,ns.space_id.as_str(),id,owner.session.as_str(),encode(&owner)?,encode(&spec)?,request,fingerprint,encode(&state)?,to_i64(spec.first_due_at_ms)?])?;
        Ok(Reminder {
            reminder_id: id,
            owner,
            spec,
            state,
        })
    }

    pub(crate) fn list_reminders(
        &self,
        ns: &StateNamespace,
        owner: &ReminderOwner,
    ) -> Result<Vec<Reminder>, StoreError> {
        let mut statement=self.transaction.prepare("SELECT reminder_id FROM reminders WHERE session_socket_id=?1 AND worktree_id=?2 AND owner_session_id=?3 AND state IN ('scheduled','awaiting_delivery') ORDER BY due_at_ms,reminder_id")?;
        let ids = statement
            .query_map(
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    owner.session.as_str()
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| self.reminder(ns, id))
            .collect::<Result<Vec<_>, _>>()
            .map(|rows| rows.into_iter().filter(|r| r.owner == *owner).collect())
    }

    fn reminder_state(
        &self,
        ns: &StateNamespace,
        id: &str,
        state: &ReminderState,
    ) -> Result<(), StoreError> {
        let due = match state {
            ReminderState::Scheduled { due_at_ms, .. } => Some(to_i64(*due_at_ms)?),
            _ => None,
        };
        self.transaction.execute("UPDATE reminders SET state=?4,state_json=?5,due_at_ms=?6 WHERE session_socket_id=?1 AND worktree_id=?2 AND reminder_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),id,state.tag(),encode(state)?,due])?;
        Ok(())
    }

    pub(crate) fn conversation_reminders(
        &self,
        ns: &StateNamespace,
        thread: &ThreadId,
    ) -> Result<Vec<Reminder>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT r.reminder_id FROM reminders r
             JOIN collaboration_messages m ON m.session_socket_id=r.session_socket_id
               AND m.worktree_id=r.worktree_id AND m.message_id=json_extract(r.spec_json,'$.in_reply_to')
             WHERE r.session_socket_id=?1 AND r.worktree_id=?2 AND m.thread_id=?3
               AND r.state IN ('scheduled','awaiting_delivery')
             ORDER BY r.due_at_ms,r.reminder_id",
        )?;
        let ids = statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), thread.as_str()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter().map(|id| self.reminder(ns, id)).collect()
    }

    pub(crate) fn cancel_conversation_reminder(
        &mut self,
        ns: &StateNamespace,
        thread: &ThreadId,
        id: &str,
        now: u64,
    ) -> Result<Option<Value>, StoreError> {
        let scoped: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM reminders r
             JOIN collaboration_messages m ON m.session_socket_id=r.session_socket_id
               AND m.worktree_id=r.worktree_id AND m.message_id=json_extract(r.spec_json,'$.in_reply_to')
             WHERE r.session_socket_id=?1 AND r.worktree_id=?2 AND m.thread_id=?3 AND r.reminder_id=?4)",
            params![ns.session_socket_id, ns.space_id.as_str(), thread.as_str(), id],
            |row| row.get(0),
        )?;
        if !scoped {
            return Ok(None);
        }
        let reminder = self.reminder(ns, id)?;
        self.cancel_reminder(ns, &reminder.owner, id, now).map(Some)
    }

    pub(crate) fn cancel_reminder(
        &mut self,
        ns: &StateNamespace,
        owner: &ReminderOwner,
        id: &str,
        now: u64,
    ) -> Result<Value, StoreError> {
        let reminder = self.reminder(ns, id)?;
        if &reminder.owner != owner {
            return Err(StoreError::invalid_input(
                "reminder belongs to another Agent Session",
            ));
        }
        let mut transport_may_have_started = false;
        match &reminder.state {
            ReminderState::AwaitingDelivery { delivery_id, .. } => {
                let state:String=self.transaction.query_row("SELECT state FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),delivery_id.as_str()],|r|r.get(0))?;
                if state == "queued" {
                    self.cancel_unsent_delivery(
                        ns,
                        delivery_id,
                        &MutationAttribution::new(ActorKind::Plugin, "namedrop-reminder", now),
                    )?;
                } else {
                    transport_may_have_started = true;
                }
            }
            ReminderState::Scheduled { .. } => {}
            _ => {
                return Ok(
                    json!({"reminder_id":id,"state":reminder.state.tag(),"transport_may_have_started":false}),
                );
            }
        }
        self.reminder_state(ns, id, &ReminderState::Cancelled)?;
        Ok(
            json!({"reminder_id":id,"state":"cancelled","transport_may_have_started":transport_may_have_started}),
        )
    }

    pub(crate) fn due_reminders(
        &self,
        ns: &StateNamespace,
        now: u64,
    ) -> Result<Vec<Reminder>, StoreError> {
        let mut statement=self.transaction.prepare("SELECT reminder_id FROM reminders WHERE session_socket_id=?1 AND worktree_id=?2 AND state='scheduled' AND due_at_ms<=?3 ORDER BY due_at_ms")?;
        let ids = statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), to_i64(now)?],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter().map(|id| self.reminder(ns, id)).collect()
    }

    fn reminder_owner_current(
        &self,
        ns: &StateNamespace,
        owner: &ReminderOwner,
    ) -> Result<bool, StoreError> {
        let address = self.optional_routing_address_by_id(ns, &owner.address)?;
        Ok(address.is_some_and(|a| {
            a.agent_session_id.as_ref() == Some(&owner.session)
                && a.pane_binding_id.as_ref() == Some(&owner.binding)
        }))
    }

    pub(crate) fn enqueue_reminder(
        &mut self,
        ns: &StateNamespace,
        id: &str,
        live: &crate::DiscoveredWorktree,
        now: u64,
    ) -> Result<bool, StoreError> {
        let reminder = self.reminder(ns, id)?;
        let ReminderState::Scheduled { due_at_ms, ordinal } = reminder.state else {
            return Ok(false);
        };
        if due_at_ms > now {
            return Ok(false);
        }
        let owner = &reminder.owner;
        if !self.reminder_owner_current(ns, owner)?
            || live
                .panes
                .iter()
                .find(|p| p.pane_id == owner.pane)
                .and_then(|p| p.agent_session_id())
                != Some(&owner.session)
        {
            self.reminder_state(
                ns,
                id,
                &ReminderState::Failed {
                    reason: "owner pane, binding, or exact Agent Session is gone".into(),
                },
            )?;
            return Ok(false);
        }
        let address = self.routing_address_by_id(ns, &owner.address)?;
        let by = MutationAttribution::new(ActorKind::Plugin, "namedrop-reminder", now);
        let key = format!("{id}:{due_at_ms}");
        let thread = match &reminder.spec.in_reply_to {
            Some(message) => self.message_route(ns, message)?.thread_id,
            None => {
                let t = ThreadId::new(format!("reminder-thread:{key}"));
                self.create_thread(ns, &t, "Reminder", &by)?;
                t
            }
        };
        let message = MessageId::new(format!("reminder-message:{key}"));
        let delivery = DeliveryId::new(format!("reminder-delivery:{key}"));
        let body = format!("Reminder for @{}: {}", address.alias, reminder.spec.body);
        self.append_message_mode(
            ns,
            &MessageDraft {
                thread_id: thread,
                message_id: message.clone(),
                in_reply_to: reminder.spec.in_reply_to.clone(),
                author_address_id: owner.address.clone(),
                author_kind: ActorKind::AgentSession,
                author_alias_snapshot: "namedrop".into(),
                author_agent_session_snapshot: Some(owner.session.clone()),
                request_id: format!("reminder-fire:{key}"),
                authored_source: body.clone(),
                body,
                grammar_version: "reminder/v1".into(),
                recipients: vec![crate::MessageRecipientDraft {
                    address_id: owner.address.clone(),
                    alias_snapshot: address.alias,
                    agent_session_snapshot: Some(owner.session.clone()),
                    delivery_id: delivery.clone(),
                }],
                context_references: vec![],
            },
            &by,
            None,
            MessageAppendMode::Reminder,
        )?;
        self.transaction.execute("INSERT INTO reminder_fires(session_socket_id,worktree_id,reminder_id,scheduled_at_ms,ordinal,message_id,delivery_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![ns.session_socket_id,ns.space_id.as_str(),id,to_i64(due_at_ms)?,ordinal,message.as_str(),delivery.as_str()])?;
        self.reminder_state(
            ns,
            id,
            &ReminderState::AwaitingDelivery {
                delivery_id: delivery,
                ordinal,
            },
        )?;
        Ok(true)
    }

    pub(crate) fn reminder_delivery_allowed(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
        address: &PaneAddressId,
        session: &AgentSessionId,
        binding: &crate::PaneBindingId,
    ) -> Result<bool, StoreError> {
        let id:Option<String>=self.transaction.query_row("SELECT reminder_id FROM reminder_fires WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str()],|r|r.get(0)).optional()?;
        let Some(id) = id else { return Ok(true) };
        let reminder = self.reminder(ns, &id)?;
        if reminder.owner.address == *address
            && reminder.owner.session == *session
            && reminder.owner.binding == *binding
            && self.reminder_owner_current(ns, &reminder.owner)?
        {
            return Ok(true);
        }
        self.reminder_state(
            ns,
            &id,
            &ReminderState::Failed {
                reason: "queued reminder owner was replaced".into(),
            },
        )?;
        self.transaction.execute("UPDATE mailbox_deliveries SET state='failed' WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND state='queued'",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str()])?;
        Ok(false)
    }

    pub(crate) fn reminder_delivery_succeeded(
        &self,
        ns: &StateNamespace,
        delivery: &str,
        at: u64,
    ) -> Result<(), StoreError> {
        let id:Option<String>=self.transaction.query_row("SELECT reminder_id FROM reminder_fires WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),delivery],|r|r.get(0)).optional()?;
        let Some(id) = id else { return Ok(()) };
        let reminder = self.reminder(ns, &id)?;
        let ReminderState::AwaitingDelivery {
            delivery_id,
            ordinal,
        } = reminder.state
        else {
            return Ok(());
        };
        if delivery_id.as_str() != delivery {
            return Ok(());
        }
        let next =
            reminder
                .spec
                .repeat
                .next(ordinal, at)
                .map_or(ReminderState::Completed, |due_at_ms| {
                    ReminderState::Scheduled {
                        due_at_ms,
                        ordinal: ordinal + 1,
                    }
                });
        self.reminder_state(ns, &id, &next)
    }

    pub(crate) fn reminder_reply_target(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Option<Option<MessageId>>, StoreError> {
        let id:Option<String>=self.transaction.query_row("SELECT reminder_id FROM reminder_fires WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str()],|r|r.get(0)).optional()?;
        id.map(|id| self.reminder(ns, &id).map(|r| r.spec.in_reply_to))
            .transpose()
    }

    pub(crate) fn recover_reminder_cancel(
        &self,
        ns: &StateNamespace,
        session: &AgentSessionId,
        id: &str,
    ) -> Result<Option<Value>, StoreError> {
        let r = self.reminder(ns, id)?;
        if r.owner.session != *session || r.state != ReminderState::Cancelled {
            return Ok(None);
        }
        let transport_may_have_started:bool=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM reminder_fires f JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id) WHERE f.session_socket_id=?1 AND f.worktree_id=?2 AND f.reminder_id=?3 AND d.state IN ('dispatching','submitted','acknowledged','settled','held'))",params![ns.session_socket_id,ns.space_id.as_str(),id],|r|r.get(0))?;
        Ok(Some(
            json!({"reminder_id":id,"state":"cancelled","transport_may_have_started":transport_may_have_started}),
        ))
    }

    pub(crate) fn fail_reminder_workspace(&self, ns: &StateNamespace) -> Result<(), StoreError> {
        let mut statement = self.transaction.prepare("SELECT reminder_id FROM reminders WHERE session_socket_id=?1 AND worktree_id=?2 AND state IN ('scheduled','awaiting_delivery')")?;
        let ids = statement
            .query_map(params![ns.session_socket_id, ns.space_id.as_str()], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            let reminder = self.reminder(ns, &id)?;
            if let ReminderState::AwaitingDelivery { delivery_id, .. } = reminder.state {
                self.transaction.execute("UPDATE mailbox_deliveries SET state='failed' WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3 AND state='queued'",params![ns.session_socket_id,ns.space_id.as_str(),delivery_id.as_str()])?;
            }
            self.reminder_state(
                ns,
                &id,
                &ReminderState::Failed {
                    reason: "owner workspace is gone".into(),
                },
            )?;
        }
        Ok(())
    }

    pub(crate) fn maintain_reminder_owners(
        &self,
        ns: &StateNamespace,
        live: &crate::DiscoveredWorktree,
    ) -> Result<(), StoreError> {
        let mut statement=self.transaction.prepare("SELECT reminder_id FROM reminders WHERE session_socket_id=?1 AND worktree_id=?2 AND state='awaiting_delivery'")?;
        let ids = statement
            .query_map(params![ns.session_socket_id, ns.space_id.as_str()], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            let reminder = self.reminder(ns, &id)?;
            let ReminderState::AwaitingDelivery { delivery_id, .. } = reminder.state else {
                continue;
            };
            let state:String=self.transaction.query_row("SELECT state FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),delivery_id.as_str()],|r|r.get(0))?;
            if state == "queued"
                && (!self.reminder_owner_current(ns, &reminder.owner)?
                    || live
                        .panes
                        .iter()
                        .find(|p| p.pane_id == reminder.owner.pane)
                        .and_then(|p| p.agent_session_id())
                        != Some(&reminder.owner.session))
            {
                self.reminder_state(
                    ns,
                    &id,
                    &ReminderState::Failed {
                        reason:
                            "queued reminder owner pane, binding, or exact Agent Session is gone"
                                .into(),
                    },
                )?;
                self.transaction.execute("UPDATE mailbox_deliveries SET state='failed' WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3 AND state='queued'",params![ns.session_socket_id,ns.space_id.as_str(),delivery_id.as_str()])?;
            } else if matches!(state.as_str(), "failed" | "cancelled") {
                self.reminder_state(
                    ns,
                    &id,
                    &ReminderState::Failed {
                        reason: format!("reminder delivery {state}"),
                    },
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn reminder_spaces(&self, socket: &str) -> Result<Vec<crate::SpaceId>, StoreError> {
        let mut s=self.transaction.prepare("SELECT DISTINCT worktree_id FROM reminders WHERE session_socket_id=?1 AND state='awaiting_delivery'")?;
        Ok(
            s.query_map([socket], |r| r.get::<_, String>(0).map(crate::SpaceId::new))?
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

#[cfg(test)]
mod tests;
