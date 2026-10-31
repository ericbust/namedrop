use super::*;
use serde_json::{Value, json};

pub(super) const SCHEMA: &str = r#"
CREATE TABLE mention_groups (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, alias),
    FOREIGN KEY (session_socket_id, worktree_id) REFERENCES worktrees(session_socket_id, worktree_id)
);
CREATE TABLE mention_group_members (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    member_order INTEGER NOT NULL,
    address_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, alias, member_order),
    UNIQUE (session_socket_id, worktree_id, alias, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, alias) REFERENCES mention_groups(session_socket_id, worktree_id, alias) ON DELETE CASCADE,
    FOREIGN KEY (session_socket_id, worktree_id, address_id) REFERENCES pane_addresses(session_socket_id, worktree_id, address_id)
);
CREATE TRIGGER group_alias_conflict BEFORE INSERT ON mention_groups
WHEN EXISTS (SELECT 1 FROM alias_claims WHERE session_socket_id=NEW.session_socket_id AND worktree_id=NEW.worktree_id AND alias=NEW.alias)
BEGIN SELECT RAISE(ABORT, 'group alias is already reserved by an agent'); END;
CREATE TRIGGER agent_alias_group_conflict BEFORE INSERT ON alias_claims
WHEN EXISTS (SELECT 1 FROM mention_groups WHERE session_socket_id=NEW.session_socket_id AND worktree_id=NEW.worktree_id AND alias=NEW.alias)
BEGIN SELECT RAISE(ABORT, 'agent alias is already reserved by a group'); END;
"#;

impl StateTransaction<'_> {
    fn mention_group_members(
        &self,
        namespace: &StateNamespace,
        alias: &str,
    ) -> Result<Option<Vec<PaneAddressId>>, StoreError> {
        let exists: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM mention_groups WHERE session_socket_id=?1 AND worktree_id=?2 AND alias=?3)",
            params![namespace.session_socket_id,namespace.space_id.as_str(),alias], |r|r.get(0))?;
        if !exists {
            return Ok(None);
        }
        let mut query = self.transaction.prepare("SELECT address_id FROM mention_group_members WHERE session_socket_id=?1 AND worktree_id=?2 AND alias=?3 ORDER BY member_order")?;
        Ok(Some(
            query
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        alias
                    ],
                    |r| r.get::<_, String>(0).map(PaneAddressId::new),
                )?
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    /// Resolves who a message goes to. The author never receives its own
    /// message: naming yourself among the leading Mentions labels the message,
    /// as in "@lead @reviewer top 3", rather than addressing it.
    pub(crate) fn resolve_message_recipients(
        &self,
        namespace: &StateNamespace,
        aliases: &[String],
        author: &PaneAddressId,
    ) -> Result<Vec<RoutingAddressSnapshot>, StoreError> {
        addressed_recipients(self.resolve_recipient_aliases(namespace, aliases)?, author)
    }

    pub(crate) fn resolve_recipient_aliases(
        &self,
        namespace: &StateNamespace,
        aliases: &[String],
    ) -> Result<Vec<RoutingAddressSnapshot>, StoreError> {
        let mut recipients = Vec::new();
        for alias in aliases {
            let resolved = if let Some(members) = self.mention_group_members(namespace, alias)? {
                members
                    .iter()
                    .map(|id| {
                        self.routing_address_by_id(namespace, id).map_err(|_| {
                            StoreError::invalid_input(format!(
                                "@{alias} contains an unavailable member {}; update the group",
                                id.as_str()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                vec![self.resolve_routing_alias(namespace, alias)?]
            };
            for member in resolved {
                if !recipients.iter().any(|existing: &RoutingAddressSnapshot| {
                    existing.address_id == member.address_id
                }) {
                    recipients.push(member);
                }
            }
        }
        Ok(recipients)
    }

    pub(crate) fn set_mention_group(
        &mut self,
        namespace: &StateNamespace,
        alias: &str,
        members: &[String],
    ) -> Result<Value, StoreError> {
        let alias = alias.strip_prefix('@').unwrap_or(alias);
        crate::validate_alias(alias).map_err(StoreError::invalid_input)?;
        if members.is_empty() || members.len() > 64 {
            return Err(StoreError::invalid_input(
                "groups require 1 to 64 agent members",
            ));
        }
        let state = self.worktree_state(namespace)?;
        if !state.is_some_and(|s| s.activated && !s.deactivated) {
            return Err(StoreError::invalid_input(
                "Namedrop is not activated for this space",
            ));
        }
        let closed_alias: bool = self.transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM alias_history h
                JOIN pane_addresses a USING (session_socket_id, worktree_id, address_id)
                WHERE h.session_socket_id = ?1 AND h.worktree_id = ?2
                  AND h.alias = ?3 AND a.retired = 1
                  AND h.history_order = (
                      SELECT MAX(latest.history_order) FROM alias_history latest
                      WHERE latest.session_socket_id = h.session_socket_id
                        AND latest.worktree_id = h.worktree_id
                        AND latest.address_id = h.address_id
                        AND latest.transition_kind = 'claimed'))",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                alias
            ],
            |row| row.get(0),
        )?;
        if closed_alias {
            return Err(StoreError::invalid_input(format!(
                "@{alias} belongs to a closed pane; choose a different group name"
            )));
        }
        let resolved = members
            .iter()
            .map(|alias| {
                self.resolve_routing_alias(namespace, alias.strip_prefix('@').unwrap_or(alias))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.transaction.execute("INSERT INTO mention_groups(session_socket_id,worktree_id,alias) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![namespace.session_socket_id,namespace.space_id.as_str(),alias])?;
        self.transaction.execute("DELETE FROM mention_group_members WHERE session_socket_id=?1 AND worktree_id=?2 AND alias=?3",params![namespace.session_socket_id,namespace.space_id.as_str(),alias])?;
        for (order, member) in resolved.iter().enumerate() {
            self.transaction.execute("INSERT OR IGNORE INTO mention_group_members(session_socket_id,worktree_id,alias,member_order,address_id) VALUES(?1,?2,?3,?4,?5)",params![namespace.session_socket_id,namespace.space_id.as_str(),alias,order as i64,member.address_id.as_str()])?;
        }
        self.mention_group_json(namespace, alias)
    }

    pub(crate) fn list_mention_groups(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<Value>, StoreError> {
        let mut query = self.transaction.prepare("SELECT alias FROM mention_groups WHERE session_socket_id=?1 AND worktree_id=?2 ORDER BY alias")?;
        let aliases = query
            .query_map(namespace_params(namespace), |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        aliases
            .iter()
            .map(|alias| self.mention_group_json(namespace, alias))
            .collect()
    }

    fn mention_group_json(
        &self,
        namespace: &StateNamespace,
        alias: &str,
    ) -> Result<Value, StoreError> {
        let members = self
            .mention_group_members(namespace, alias)?
            .ok_or_else(|| StoreError::invalid_input(format!("unknown group: @{alias}")))?;
        let members = members
            .iter()
            .map(|id| {
                let current = self.optional_routing_address_by_id(namespace, id)?;
                Ok(json!({"address_id":id,"alias":current.map(|a|a.alias)}))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(json!({"alias":alias,"members":members}))
    }

    pub(crate) fn remove_mention_group(
        &mut self,
        namespace: &StateNamespace,
        alias: &str,
    ) -> Result<Value, StoreError> {
        let alias = alias.strip_prefix('@').unwrap_or(alias);
        let removed = self.transaction.execute(
            "DELETE FROM mention_groups WHERE session_socket_id=?1 AND worktree_id=?2 AND alias=?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                alias
            ],
        )?;
        Ok(json!({"alias":alias,"removed":removed!=0}))
    }
}

pub(crate) fn addressed_recipients(
    recipients: Vec<RoutingAddressSnapshot>,
    author: &PaneAddressId,
) -> Result<Vec<RoutingAddressSnapshot>, StoreError> {
    let mut unique: Vec<RoutingAddressSnapshot> = Vec::new();
    for recipient in recipients {
        if &recipient.address_id != author
            && !unique
                .iter()
                .any(|existing| existing.address_id == recipient.address_id)
        {
            unique.push(recipient);
        }
    }
    if unique.is_empty() {
        return Err(StoreError::invalid_input(
            "this message is addressed only to its author; mention at least one other agent",
        ));
    }
    Ok(unique)
}
