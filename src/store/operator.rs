use super::*;

const ADDRESS: &str = "operator:local";
const ROLE: &str = "role-operator";
const DESCRIPTION: &str = "Paneless local operator.";

pub(super) fn refuse_reserved_creation(draft: &PaneAddressDraft) -> Result<(), StoreError> {
    if draft.id.as_str() == ADDRESS || draft.role_id.as_str() == ROLE {
        return Err(StoreError::invalid_input(
            "the operator identity is reserved",
        ));
    }
    Ok(())
}

pub(super) fn refuse_pane_mutation(address: &PaneAddressId) -> Result<(), StoreError> {
    if address.as_str() == ADDRESS {
        return Err(StoreError::invalid_input(
            "the operator has no pane lifecycle",
        ));
    }
    Ok(())
}

fn snapshot() -> RoutingAddressSnapshot {
    RoutingAddressSnapshot {
        address_id: PaneAddressId::new(ADDRESS),
        alias: "operator".to_owned(),
        pane_binding_id: None,
        agent_session_id: None,
    }
}

impl StateTransaction<'_> {
    pub(crate) fn ensure_operator_author(
        &mut self,
        namespace: &StateNamespace,
        attribution: &MutationAttribution,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        let ns = params![
            namespace.session_socket_id,
            namespace.space_id.as_str(),
            ROLE
        ];
        let role: Option<(String, String)> = self
            .transaction
            .query_row(
                "SELECT name, description FROM pane_roles
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND role_id = ?3",
                ns,
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if role
            .as_ref()
            .is_some_and(|(name, description)| name != "operator" || description != DESCRIPTION)
        {
            return Err(StoreError::invalid_input(
                "the reserved operator role conflicts with existing state",
            ));
        }
        let shared_role: bool = self.transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM pane_addresses WHERE session_socket_id = ?1
             AND worktree_id = ?2 AND role_id = ?3 AND address_id <> ?4)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                ROLE,
                ADDRESS
            ],
            |row| row.get(0),
        )?;
        if shared_role {
            return Err(StoreError::invalid_input(
                "the reserved operator role belongs to another address",
            ));
        }
        self.transaction.execute(
            "INSERT OR IGNORE INTO pane_roles (
                session_socket_id, worktree_id, role_id, name, description
             ) VALUES (?1, ?2, ?3, 'operator', ?4)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                ROLE,
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
                ADDRESS,
                ROLE,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        if !self.is_operator_address(namespace, &PaneAddressId::new(ADDRESS))? {
            return Err(StoreError::invalid_input(
                "the reserved operator address conflicts with existing state",
            ));
        }
        let contaminated: bool = self.transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM pane_addresses WHERE session_socket_id = ?1 AND worktree_id = ?2
                  AND address_id = ?3 AND (collaboration_disabled <> 0 OR pane_type <> 'agent'
                    OR remote_commands_terminal_id IS NOT NULL OR persona IS NOT NULL)
                UNION ALL
                SELECT 1 FROM alias_claims WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                UNION ALL
                SELECT 1 FROM pane_bindings WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                UNION ALL
                SELECT 1 FROM agent_session_observations WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                UNION ALL
                SELECT 1 FROM integration_registrations WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
             )",
            params![namespace.session_socket_id, namespace.space_id.as_str(), ADDRESS],
            |row| row.get(0),
        )?;
        if contaminated {
            return Err(StoreError::invalid_input(
                "the operator must have no pane identity or configuration",
            ));
        }
        Ok(snapshot())
    }

    pub(crate) fn is_operator_address(
        &self,
        namespace: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<bool, StoreError> {
        if address.as_str() != ADDRESS {
            return Ok(false);
        }
        self.transaction
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM pane_addresses
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
               AND role_id = ?4 AND retired = 0)",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    ADDRESS,
                    ROLE
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub(super) fn operator_projection_addresses(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<String>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT address_id FROM pane_addresses
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3 AND role_id = ?4 AND retired = 0
             UNION
             SELECT e.address_id FROM space_envoys e JOIN pane_addresses a
               ON a.session_socket_id = e.session_socket_id AND a.worktree_id = e.origin_space_id
              AND a.address_id = e.origin_address_id
             WHERE e.session_socket_id = ?1 AND e.worktree_id = ?2
               AND a.address_id = ?3 AND a.role_id = ?4 AND a.retired = 0"
        )?;
        let addresses = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    ADDRESS,
                    ROLE
                ],
                |row| row.get(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(addresses)
    }

    pub(crate) fn operator_snapshot(
        &self,
        namespace: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<Option<RoutingAddressSnapshot>, StoreError> {
        Ok(self.is_operator_address(namespace, address)?.then(snapshot))
    }
}
