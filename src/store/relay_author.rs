use super::*;
use crate::PaneRoleId;
use sha2::{Digest, Sha256};

pub(super) const ADDRESS_PREFIX: &str = "relay:";
pub(super) const ROLE_PREFIX: &str = "role-relay:";
const DESCRIPTION: &str = "Paneless relay author.";

/// Where a remote agent is, as one machine knows it: on the recipient's
/// machine by the `host_id` the sender's server reported, and on the
/// sender's machine by the `rex hosts` label it sends to, because its first
/// message to the agent is stored before any call has read that host's
/// `host_id`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RelayOrigin<'a> {
    Label(&'a str),
}

impl RelayOrigin<'_> {
    fn key(self) -> String {
        match self {
            Self::Label(label) => format!("label:{label}"),
        }
    }
}

/// One remote agent's paneless author in a space: `relay:` and 16 hex of
/// the digest of its origin and alias. It has no alias claim, binding,
/// observation, or registration, and its role is named for `display`, as
/// `bob@devbox`, which every message it writes shows as its sender.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelayAuthor {
    pub address_id: PaneAddressId,
    pub role_id: PaneRoleId,
    pub display: String,
}

impl RelayAuthor {
    pub(crate) fn new(origin: RelayOrigin<'_>, alias: &str, display: String) -> Self {
        let digest = crate::lower_hex(&Sha256::digest(format!("{}\0{alias}", origin.key())));
        Self {
            address_id: PaneAddressId::new(format!("{ADDRESS_PREFIX}{}", &digest[..16])),
            role_id: PaneRoleId::new(format!("{ROLE_PREFIX}{}", &digest[..16])),
            display,
        }
    }

    fn snapshot(&self) -> RoutingAddressSnapshot {
        RoutingAddressSnapshot {
            address_id: self.address_id.clone(),
            alias: self.display.clone(),
            pane_binding_id: None,
            agent_session_id: None,
        }
    }
}

impl StateTransaction<'_> {
    pub(crate) fn ensure_relay_author(
        &mut self,
        namespace: &StateNamespace,
        author: &RelayAuthor,
        attribution: &MutationAttribution,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        self.transaction.execute(
            "INSERT OR IGNORE INTO pane_roles (
                session_socket_id, worktree_id, role_id, name, description
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                author.role_id.as_str(),
                author.display,
                DESCRIPTION
            ],
        )?;
        self.transaction.execute(
            "INSERT OR IGNORE INTO pane_addresses (
                session_socket_id, worktree_id, address_id, role_id,
                retired, collaboration_disabled, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                author.address_id.as_str(),
                author.role_id.as_str(),
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        if !self.is_relay_author(namespace, &author.address_id)? {
            return Err(StoreError::internal(
                "a remote agent's reserved address conflicts with existing state",
            ));
        }
        Ok(author.snapshot())
    }

    pub(crate) fn is_relay_author(
        &self,
        namespace: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<bool, StoreError> {
        let Some(digest) = address.as_str().strip_prefix(ADDRESS_PREFIX) else {
            return Ok(false);
        };
        self.transaction
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM pane_addresses a
                    JOIN pane_roles r USING (session_socket_id, worktree_id, role_id)
                    WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                      AND a.address_id = ?3 AND a.role_id = ?4 AND a.retired = 0
                      AND r.description = ?5
                      AND a.collaboration_disabled = 0 AND a.pane_type = 'agent'
                      AND a.remote_commands_terminal_id IS NULL AND a.persona IS NULL
                      AND NOT EXISTS (
                        SELECT 1 FROM pane_addresses WHERE session_socket_id = ?1
                          AND worktree_id = ?2 AND role_id = ?4 AND address_id <> ?3
                        UNION ALL
                        SELECT 1 FROM alias_claims WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                        UNION ALL
                        SELECT 1 FROM pane_bindings WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                        UNION ALL
                        SELECT 1 FROM agent_session_observations WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                        UNION ALL
                        SELECT 1 FROM integration_registrations WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                      )
                 )",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address.as_str(),
                    format!("{ROLE_PREFIX}{digest}"),
                    DESCRIPTION
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }
}
