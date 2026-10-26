//! Tracked handoffs across workspaces. Coordination rows live only in the
//! lead's workspace and nothing is mirrored, so the worker's workspace finds
//! them through the bridge, and the lead's workspace reads the worker behind
//! its envoy where that worker really lives.

use super::*;
use crate::{CoordinationLink, SpaceId};

/// A tracked message's links, wherever they are written. Only the lead's
/// workspace holds links, so of a message, the original it copies, and its
/// copies, at most one carries them.
#[derive(Clone, Debug)]
pub(crate) struct TrackedMessage {
    pub home: StateNamespace,
    pub message_id: MessageId,
    pub links: Vec<CoordinationLink>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Principal {
    pub ns: StateNamespace,
    pub address_id: PaneAddressId,
}

pub(crate) struct Seat {
    pub principal: Principal,
    pub progress: DeliveryState,
}

pub(crate) fn inconsistent_bridge(what: &str) -> StoreError {
    StoreError::invalid_input(format!(
        "the cross-workspace record is inconsistent: {what}; nothing was changed"
    ))
}

impl StateTransaction<'_> {
    pub(crate) fn tracked_message(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Option<TrackedMessage>, StoreError> {
        let tracked = |home: StateNamespace, message_id: MessageId| {
            let links = self.coordination_links(&home, &message_id)?;
            Ok::<_, StoreError>((!links.is_empty()).then_some(TrackedMessage {
                home,
                message_id,
                links,
            }))
        };
        if let Some(found) = tracked(ns.clone(), message.clone())? {
            return Ok(Some(found));
        }
        if let Some((space, origin)) = self.copy_origin(ns, message)? {
            let home = StateNamespace::new(ns.session_socket_id.clone(), space);
            if !self.message_exists(&home, &origin)? {
                return Err(inconsistent_bridge(
                    "a forwarded copy has lost its original",
                ));
            }
            return tracked(home, origin);
        }
        for (space, copy) in self.copies_of(ns, message)? {
            let holder = StateNamespace::new(ns.session_socket_id.clone(), space);
            if let Some(found) = tracked(holder, copy)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    fn message_exists(&self, ns: &StateNamespace, message: &MessageId) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3)",
            params![ns.session_socket_id, ns.space_id.as_str(), message.as_str()],
            |row| row.get(0),
        )?)
    }

    fn copies_of(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Vec<(SpaceId, MessageId)>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT worktree_id, message_id FROM bridged_messages
             WHERE session_socket_id = ?1 AND origin_space_id = ?2 AND origin_message_id = ?3
             ORDER BY worktree_id",
        )?;
        statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), message.as_str()],
                |row| {
                    Ok((
                        SpaceId::new(row.get::<_, String>(0)?),
                        MessageId::new(row.get::<_, String>(1)?),
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn copy_in(
        &self,
        holder: &StateNamespace,
        from: &SpaceId,
        message: &MessageId,
    ) -> Result<MessageId, StoreError> {
        self.copy_of(holder, from, message)?
            .ok_or_else(|| inconsistent_bridge("a forwarded message has no copy"))
    }

    pub(crate) fn principal(
        &self,
        ns: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<Principal, StoreError> {
        Ok(match self.envoy_origin(ns, address)? {
            Some(origin) => Principal {
                ns: StateNamespace::new(ns.session_socket_id.clone(), origin.space),
                address_id: origin.address_id,
            },
            None => Principal {
                ns: ns.clone(),
                address_id: address.clone(),
            },
        })
    }

    pub(crate) fn address_retired(
        &self,
        ns: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<bool, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT retired FROM pane_addresses
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3",
                params![ns.session_socket_id, ns.space_id.as_str(), address.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(true))
    }

    /// `address` of `from` as `to` knows it: the real address when it is an
    /// envoy for one of `to`'s, otherwise `to`'s envoy for it. A lookup that
    /// never creates an envoy, so only someone `to` already knows can match.
    pub(crate) fn counterpart_address(
        &self,
        from: &StateNamespace,
        address: &PaneAddressId,
        to: &StateNamespace,
    ) -> Result<Option<PaneAddressId>, StoreError> {
        if from == to {
            return Ok(Some(address.clone()));
        }
        if let Some(origin) = self.envoy_origin(from, address)? {
            return Ok((origin.space == to.space_id).then_some(origin.address_id));
        }
        self.envoy_standing_for(to, &from.space_id, address)
    }

    pub(crate) fn pinned_session(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
        recipient: &crate::MessageRecipientDraft,
    ) -> Result<Option<crate::AgentSessionId>, StoreError> {
        let Some(origin) = self.envoy_origin(ns, &recipient.address_id)? else {
            return Ok(recipient.agent_session_snapshot.clone());
        };
        let there = StateNamespace::new(ns.session_socket_id.clone(), origin.space);
        let copy = self.copy_in(&there, &ns.space_id, message)?;
        self.recipient_session_snapshot(&there, &copy, &origin.address_id)?
            .ok_or_else(|| inconsistent_bridge("a forwarded copy lost its recipient"))
    }

    pub(crate) fn recipient_session_snapshot(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
        address: &PaneAddressId,
    ) -> Result<Option<Option<crate::AgentSessionId>>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT session_snapshot FROM message_recipients
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND message_id = ?3 AND address_id = ?4",
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    message.as_str(),
                    address.as_str()
                ],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .map(|session| session.map(crate::AgentSessionId::new)))
    }

    pub(crate) fn assignment_seat(
        &self,
        ns: &StateNamespace,
        address: &PaneAddressId,
        message: &MessageId,
    ) -> Result<Seat, StoreError> {
        let progress = self.transaction.query_row(
            "SELECT s.progress FROM mailbox_deliveries d
             JOIN delivery_seats s USING (session_socket_id, worktree_id, delivery_id)
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
               AND d.message_id = ?3 AND d.address_id = ?4",
            params![
                ns.session_socket_id,
                ns.space_id.as_str(),
                message.as_str(),
                address.as_str()
            ],
            |row| parse_delivery_state(row.get(0)?),
        )?;
        Ok(Seat {
            principal: self.principal(ns, address)?,
            progress,
        })
    }

    pub(crate) fn queued_remote_seat_spaces(
        &self,
        ns: &StateNamespace,
    ) -> Result<Vec<SpaceId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT DISTINCT e.origin_space_id
             FROM mailbox_deliveries d
             JOIN delivery_seats s USING (session_socket_id, worktree_id, delivery_id)
             JOIN space_envoys e
               ON e.session_socket_id = d.session_socket_id
              AND e.worktree_id = d.worktree_id AND e.address_id = d.address_id
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2 AND s.progress = 'queued'
             ORDER BY e.origin_space_id",
        )?;
        statement
            .query_map(params![ns.session_socket_id, ns.space_id.as_str()], |row| {
                row.get::<_, String>(0).map(SpaceId::new)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn remote_seat_spaces(
        &self,
        home: &StateNamespace,
    ) -> Result<Vec<SpaceId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT DISTINCT e.origin_space_id
             FROM coordination_assignments a
             JOIN coordinations c USING (session_socket_id, worktree_id, coordination_id)
             JOIN space_envoys e
               ON e.session_socket_id = a.session_socket_id
              AND e.worktree_id = a.worktree_id AND e.address_id = a.recipient_address_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND c.outcome = 'open'
             ORDER BY e.origin_space_id",
        )?;
        statement
            .query_map(
                params![home.session_socket_id, home.space_id.as_str()],
                |row| row.get::<_, String>(0).map(SpaceId::new),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Workspaces homing an open coordination with an assignee seated in
    /// `seat`. A wake there reconciles these too, so a worker going idle
    /// reaches its lead's nudge.
    pub(crate) fn coordination_homes_seated_in(
        &self,
        socket: &str,
        seat: &SpaceId,
    ) -> Result<Vec<SpaceId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT DISTINCT a.worktree_id
             FROM coordination_assignments a
             JOIN coordinations c USING (session_socket_id, worktree_id, coordination_id)
             JOIN space_envoys e
               ON e.session_socket_id = a.session_socket_id
              AND e.worktree_id = a.worktree_id AND e.address_id = a.recipient_address_id
             WHERE a.session_socket_id = ?1 AND e.origin_space_id = ?2 AND c.outcome = 'open'
             ORDER BY a.worktree_id",
        )?;
        statement
            .query_map(params![socket, seat.as_str()], |row| {
                row.get::<_, String>(0).map(SpaceId::new)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}
