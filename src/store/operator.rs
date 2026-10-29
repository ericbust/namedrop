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
                "SELECT EXISTS (
                    SELECT 1 FROM pane_addresses a
                    JOIN pane_roles r USING (session_socket_id, worktree_id, role_id)
                    WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                      AND a.address_id = ?3 AND a.role_id = ?4 AND a.retired = 0
                      AND r.name = 'operator' AND r.description = ?5
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
                    ADDRESS,
                    ROLE,
                    DESCRIPTION
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub(super) fn operator_projection_addresses(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<String>, StoreError> {
        let operator = PaneAddressId::new(ADDRESS);
        let mut addresses = Vec::new();
        if self.is_operator_address(namespace, &operator)? {
            addresses.push(ADDRESS.to_owned());
        }
        let mut statement = self.transaction.prepare(
            "SELECT address_id, origin_space_id FROM space_envoys
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND origin_address_id = ?3",
        )?;
        let envoys = statement.query_map(
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                ADDRESS
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        for envoy in envoys {
            let (address, origin_space) = envoy?;
            let origin = StateNamespace::for_space(&namespace.session_socket_id, origin_space);
            if self.is_operator_address(&origin, &operator)? {
                addresses.push(address);
            }
        }
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
