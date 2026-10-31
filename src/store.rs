use crate::context::redact_disclosure_text;
use crate::{
    ActorKind, AgentRuntime, AttentionItemDraft, ContextGrantDraft, ContextGrantId,
    ContextGrantRecord, ContextGrantState, ContextReferenceRecord, ContextSnapshotId,
    DeliveryAddress, DeliveryControl, DeliveryEnvelope, DeliveryState, DispatchCandidate,
    EvidenceKind, EvidencePointer, InflightDeliveryAttempt, IntegrationChallenge,
    IntegrationRegistration, IntegrationToolInvocation, LeaseDraft, LeasedDelivery,
    MailboxDeliveryRecord, MessageDraft, MessageId, MessageReceipt, MessageRoute,
    MutationAttribution, PaneAddressDraft, PaneAddressId, PaneAddressRecord, RoutedMessageReceipt,
    RoutingAddressSnapshot, StateNamespace, ThreadContextSnapshotRecord, ThreadId,
    TransitionEntityKind, TransitionFact, TransitionKind,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params, types::Type,
};
use serde::Serialize;
use std::{
    error::Error,
    fmt, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const DATABASE_FILE_NAME: &str = "namedrop.sqlite3";
pub(crate) const SCHEMA_VERSION: u32 = 6;

/// Personas live on the Pane Address, which survives renaming and rebinding,
/// rather than on the role record the native naming path overwrites. Upgrading
/// keeps a meaningful description a human wrote; an auto-registered native name
/// has no description of its own, so it starts Generic.
const PERSONA_SCHEMA: &str = r#"
ALTER TABLE pane_addresses ADD COLUMN persona TEXT;
UPDATE pane_addresses SET persona = (
    SELECT r.description FROM pane_roles r
    WHERE r.session_socket_id = pane_addresses.session_socket_id
      AND r.worktree_id = pane_addresses.worktree_id
      AND r.role_id = pane_addresses.role_id
      AND r.description NOT LIKE 'Native collaborator addressed as @%'
      AND trim(r.description) <> ''
);
"#;

mod bridge;
pub(crate) mod channel_routing;
pub(crate) use bridge::{EnvoyOrigin, ForwardedCopy};
pub(crate) use groups::addressed_recipients;
mod coordination;
mod coordination_bridge;
mod coordination_ownership;
pub mod dashboard;
mod missing_disposition;
mod operator;
pub(crate) use coordination_bridge::TrackedMessage;
pub(crate) use coordination_ownership::CoordinationRole;
mod groups;
mod handoff;
mod terminal;

pub struct StateStore {
    connection: Connection,
    state_directory: PathBuf,
    read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedTabLabel {
    pub tab_id: String,
    pub address_id: PaneAddressId,
    pub managed_label: String,
    pub previous_label: String,
    pub pending_label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorktreePurgeSummary {
    pub space_id: String,
    pub deactivated: bool,
    pub nonterminal_deliveries: u64,
    pub affected_records: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorktreePurgeResult {
    pub summary: WorktreePurgeSummary,
    pub maintenance_complete: bool,
    pub maintenance_warnings: Vec<String>,
}

pub(crate) struct HeldDelivery {
    pub attempt_id: Option<crate::DeliveryAttemptId>,
    pub delivery_id: crate::DeliveryId,
    pub address_id: PaneAddressId,
    pub alias: String,
    pub message_id: MessageId,
    pub thread_id: ThreadId,
}

impl StateStore {
    pub fn open(state_directory: impl AsRef<Path>) -> Result<Self, StoreError> {
        let state_directory = state_directory.as_ref();
        fs::create_dir_all(state_directory)?;
        fs::set_permissions(state_directory, fs::Permissions::from_mode(0o700))?;

        let database_path = state_directory.join(DATABASE_FILE_NAME);
        let mut connection = Connection::open(&database_path)?;
        fs::set_permissions(&database_path, fs::Permissions::from_mode(0o600))?;
        connection.busy_timeout(Duration::from_secs(2))?;
        verify_integrity(&connection, &database_path)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        initialize_schema(&mut connection, state_directory)?;

        Ok(Self {
            connection,
            state_directory: state_directory.to_path_buf(),
            read_only: false,
        })
    }

    /// Open existing state for diagnosis without creating files, changing
    /// permissions, migrating schemas, or permitting a write transaction.
    pub fn open_read_only(state_directory: impl AsRef<Path>) -> Result<Option<Self>, StoreError> {
        let state_directory = state_directory.as_ref();
        let database_path = state_directory.join(DATABASE_FILE_NAME);
        if !database_path.is_file() {
            return Ok(None);
        }
        let connection =
            Connection::open_with_flags(&database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        verify_integrity(&connection, &database_path)?;
        check_read_only_schema(&connection)?;
        Ok(Some(Self {
            connection,
            state_directory: state_directory.to_path_buf(),
            read_only: true,
        }))
    }

    /// Keep status reads, including integrity checks and long queries, within
    /// the popup's remaining collection budget. This never migrates state.
    pub(crate) fn with_read_only_timeout<T>(
        state_directory: &Path,
        timeout: Duration,
        cancelled: &std::sync::atomic::AtomicBool,
        read: impl FnOnce(Option<&mut Self>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        use std::{
            sync::atomic::{AtomicBool, Ordering},
            thread,
            time::Instant,
        };
        let deadline = Instant::now() + timeout;
        if cancelled.load(Ordering::Relaxed) || timeout.is_zero() {
            return Err(StoreError::invalid_input(
                "Status read cancelled or timed out",
            ));
        }
        let database_path = state_directory.join(DATABASE_FILE_NAME);
        if !database_path.is_file() {
            return read(None);
        }
        let connection =
            Connection::open_with_flags(&database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        // A lock need not consume the entire popup deadline: report unavailable
        // facts promptly, and let an explicit refresh retry later.
        connection.busy_timeout(timeout.min(Duration::from_millis(25)))?;
        let interrupt = connection.get_interrupt_handle();
        let finished = AtomicBool::new(false);
        struct Finish<'a>(&'a AtomicBool);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        thread::scope(|scope| {
            scope.spawn(|| {
                while !finished.load(Ordering::Relaxed) {
                    if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
                        interrupt.interrupt();
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            });
            let _finish = Finish(&finished);
            verify_integrity(&connection, &database_path)?;
            check_read_only_schema(&connection)?;
            if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return Err(StoreError::invalid_input(
                    "Status read cancelled or timed out",
                ));
            }
            let mut store = Self {
                connection,
                state_directory: state_directory.to_path_buf(),
                read_only: true,
            };
            let result = read(Some(&mut store));
            if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return Err(StoreError::invalid_input(
                    "Status read cancelled or timed out",
                ));
            }
            result
        })
    }

    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut StateTransaction<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let behavior = if self.read_only {
            TransactionBehavior::Deferred
        } else {
            TransactionBehavior::Immediate
        };
        let transaction = self.connection.transaction_with_behavior(behavior)?;
        let mut state_transaction = StateTransaction {
            transaction,
            state_directory: &self.state_directory,
        };
        let value = operation(&mut state_transaction)?;
        state_transaction.transaction.commit()?;
        Ok(value)
    }

    pub fn purge_summary(
        &self,
        namespace: &StateNamespace,
    ) -> Result<WorktreePurgeSummary, StoreError> {
        purge_summary(&self.connection, namespace)
    }

    pub fn purge_worktree(
        &mut self,
        namespace: &StateNamespace,
        typed_space_id: &str,
        attribution: &MutationAttribution,
    ) -> Result<WorktreePurgeResult, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let summary = purge_summary(&transaction, namespace)?;
        validate_purge(&summary, namespace, typed_space_id)?;
        {
            let mut state = StateTransaction {
                transaction,
                state_directory: &self.state_directory,
            };
            state.cancel_purged_copies(namespace, attribution)?;
            delete_namespace_rows(&state.transaction, "main", namespace)?;
            state.transaction.commit()?;
        }

        let mut maintenance_warnings = Vec::new();
        let database_path = self.state_directory.join(DATABASE_FILE_NAME);
        if let Err(error) = compact_database(&self.connection)
            .and_then(|()| verify_integrity(&self.connection, &database_path))
        {
            maintenance_warnings.push(format!("{}: {error}", database_path.display()));
        }
        Ok(WorktreePurgeResult {
            summary,
            maintenance_complete: maintenance_warnings.is_empty(),
            maintenance_warnings,
        })
    }
}

pub struct StateTransaction<'connection> {
    transaction: Transaction<'connection>,
    state_directory: &'connection Path,
}

impl StateTransaction<'_> {
    pub(crate) fn managed_tab_labels(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<ManagedTabLabel>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT tab_id, address_id, managed_label, previous_label, pending_label
             FROM managed_tab_labels
             WHERE session_socket_id = ?1 AND worktree_id = ?2
             ORDER BY tab_id",
        )?;
        statement
            .query_map(
                params![namespace.session_socket_id, namespace.space_id.as_str()],
                |row| {
                    Ok(ManagedTabLabel {
                        tab_id: row.get(0)?,
                        address_id: PaneAddressId::new(row.get::<_, String>(1)?),
                        managed_label: row.get(2)?,
                        previous_label: row.get(3)?,
                        pending_label: row.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn put_managed_tab_label(
        &mut self,
        namespace: &StateNamespace,
        label: &ManagedTabLabel,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO managed_tab_labels (
                session_socket_id, worktree_id, tab_id, address_id,
                managed_label, previous_label, pending_label
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_socket_id, worktree_id, tab_id) DO UPDATE SET
                address_id = excluded.address_id,
                managed_label = excluded.managed_label,
                previous_label = excluded.previous_label,
                pending_label = excluded.pending_label",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                label.tab_id,
                label.address_id.as_str(),
                label.managed_label,
                label.previous_label,
                label.pending_label,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn remove_managed_tab_label(
        &mut self,
        namespace: &StateNamespace,
        tab_id: &str,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "DELETE FROM managed_tab_labels
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND tab_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                tab_id
            ],
        )?;
        Ok(())
    }

    pub fn ensure_namespace(
        &mut self,
        namespace: &StateNamespace,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.activate_worktree(namespace, attribution)
    }

    pub fn activate_worktree(
        &mut self,
        namespace: &StateNamespace,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let previous = self.worktree_state(namespace)?;
        self.transaction.execute(
            "INSERT INTO worktrees (
                session_socket_id, worktree_id, activated, paused, deactivated, created_at_ms
             ) VALUES (?1, ?2, 1, 0, 0, ?3)
             ON CONFLICT(session_socket_id, worktree_id) DO UPDATE SET
                activated = 1,
                deactivated = 0",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.transaction.execute(
            "INSERT OR IGNORE INTO worktree_settings (
                session_socket_id, worktree_id, settled_delay_ms
             ) VALUES (?1, ?2, 500)",
            namespace_params(namespace),
        )?;
        self.transaction.execute(
            "INSERT OR IGNORE INTO reconciliation_epochs (
                session_socket_id, worktree_id, epoch, completed
             ) VALUES (?1, ?2, 0, 0)",
            namespace_params(namespace),
        )?;
        if previous
            .as_ref()
            .is_some_and(|state| !state.activated || state.deactivated)
        {
            self.transaction.execute(
                "UPDATE reconciliation_epochs SET completed = 0
                 WHERE session_socket_id = ?1 AND worktree_id = ?2",
                namespace_params(namespace),
            )?;
        }
        if previous.is_none_or(|state| !state.activated) {
            self.append_transition(
                namespace,
                TransitionEntityKind::Worktree,
                namespace.space_id.as_str(),
                TransitionKind::Activated,
                attribution,
            )?;
        }
        Ok(())
    }

    pub fn deactivate_worktree(
        &mut self,
        namespace: &StateNamespace,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let changed = self.transaction.execute(
            "UPDATE worktrees
             SET activated = 0, paused = 0, deactivated = 1
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND (activated != 0 OR deactivated != 1)",
            namespace_params(namespace),
        )?;
        if changed == 1 {
            self.append_transition(
                namespace,
                TransitionEntityKind::Worktree,
                namespace.space_id.as_str(),
                TransitionKind::Deactivated,
                attribution,
            )?;
            return Ok(());
        }
        match self.worktree_state(namespace)? {
            Some(state) if state.deactivated => Ok(()),
            Some(_) => Err(StoreError::invalid_input("Worktree cannot be deactivated")),
            None => Err(StoreError::invalid_input(
                "Namedrop is not activated for this space",
            )),
        }
    }

    pub(crate) fn any_activated_worktrees(&self) -> Result<bool, StoreError> {
        self.transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM worktrees WHERE activated=1 AND deactivated=0)",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn worktree_state(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Option<crate::WorktreeState>, StoreError> {
        self.transaction
            .query_row(
                "SELECT activated, paused, deactivated
                 FROM worktrees
                 WHERE session_socket_id = ?1 AND worktree_id = ?2",
                namespace_params(namespace),
                |row| {
                    Ok(crate::WorktreeState {
                        activated: row.get(0)?,
                        paused: row.get(1)?,
                        deactivated: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn set_worktree_paused(
        &mut self,
        namespace: &StateNamespace,
        paused: bool,
        attribution: &MutationAttribution,
    ) -> Result<crate::WorktreeState, StoreError> {
        let changed = self.transaction.execute(
            "UPDATE worktrees SET paused = ?3
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND activated = 1 AND deactivated = 0 AND paused != ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                paused
            ],
        )?;
        let state = self
            .worktree_state(namespace)?
            .ok_or_else(|| StoreError::invalid_input("Namedrop is not activated"))?;
        if !state.activated || state.deactivated {
            return Err(StoreError::invalid_input(
                "Namedrop is not activated for this space",
            ));
        }
        if changed == 1 {
            self.append_transition(
                namespace,
                TransitionEntityKind::Worktree,
                namespace.space_id.as_str(),
                if paused {
                    TransitionKind::Paused
                } else {
                    TransitionKind::Resumed
                },
                attribution,
            )?;
        }
        Ok(state)
    }

    fn refuse_claimed_alias(
        &self,
        namespace: &StateNamespace,
        alias: &str,
    ) -> Result<(), StoreError> {
        let claim = self
            .transaction
            .query_row(
                "SELECT a.retired, c.claim_state, c.replacement_alias
                 FROM alias_claims c
                 JOIN pane_addresses a
                   ON a.session_socket_id = c.session_socket_id
                  AND a.worktree_id = c.worktree_id
                  AND a.address_id = c.address_id
                 WHERE c.session_socket_id = ?1 AND c.worktree_id = ?2
                   AND c.alias = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    alias
                ],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((retired, claim_state, replacement)) = claim else {
            return Ok(());
        };
        let history = if retired {
            "was retired".to_owned()
        } else if claim_state == "tombstone" {
            match replacement {
                Some(replacement) => format!("was renamed to @{replacement}"),
                None => "was renamed away".to_owned(),
            }
        } else {
            return Err(StoreError::invalid_input(format!(
                "the name @{alias} is already in use in this workspace; choose a new name"
            )));
        };
        Err(StoreError::invalid_input(format!(
            "the name @{alias} {history} in this workspace and cannot be reused; choose a new name"
        )))
    }

    pub fn create_pane_address(
        &mut self,
        namespace: &StateNamespace,
        draft: &PaneAddressDraft,
        attribution: &MutationAttribution,
    ) -> Result<PaneAddressRecord, StoreError> {
        operator::refuse_reserved_creation(draft)?;
        crate::validate_alias(&draft.alias).map_err(StoreError::invalid_input)?;
        self.refuse_claimed_alias(namespace, &draft.alias)?;
        self.transaction.execute(
            "INSERT INTO pane_roles (
                session_socket_id, worktree_id, role_id, name, description
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.role_id.as_str(),
                draft.role_name,
                draft.role_description
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO pane_addresses (
                session_socket_id, worktree_id, address_id, role_id,
                retired, collaboration_disabled, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.id.as_str(),
                draft.role_id.as_str(),
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO alias_claims (
                session_socket_id, worktree_id, alias, address_id, claim_state
             ) VALUES (?1, ?2, ?3, ?4, 'active')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.alias,
                draft.id.as_str()
            ],
        )?;
        let alias_history_order = self.next_order(namespace, "alias_history", "")?;
        self.transaction.execute(
            "INSERT INTO alias_history (
                session_socket_id, worktree_id, history_order, alias, address_id,
                transition_kind, replacement_alias, occurred_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'claimed', NULL, ?6)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                to_i64(alias_history_order)?,
                draft.alias,
                draft.id.as_str(),
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::PaneAddress,
            draft.id.as_str(),
            TransitionKind::Created,
            attribution,
        )?;

        Ok(PaneAddressRecord {
            pane_type: crate::PaneType::Agent,
            id: draft.id.clone(),
            role_id: draft.role_id.clone(),
            role_name: draft.role_name.clone(),
            role_description: draft.role_description.clone(),
            persona: None,
            alias: draft.alias.clone(),
            lifecycle: crate::PaneAddressLifecycle::Unbound,
            pane_id: None,
            pane_label: None,
            agent_session_id: None,
            occupant_label: None,
            agent_status: None,
            retired: false,
            collaboration_disabled: false,
            remote_commands: false,
        })
    }

    pub fn list_pane_addresses(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<PaneAddressRecord>, StoreError> {
        self.list_pane_address_records(namespace, false)
    }

    fn list_pane_address_records(
        &self,
        namespace: &StateNamespace,
        include_retired: bool,
    ) -> Result<Vec<PaneAddressRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.address_id, a.role_id, r.name, r.description, c.alias,
                    a.retired, a.collaboration_disabled, b.pane_id,
                    (
                        SELECT o.agent_session_id
                        FROM agent_session_observations o
                        WHERE o.session_socket_id = a.session_socket_id
                          AND o.worktree_id = a.worktree_id
                          AND o.address_id = a.address_id
                          AND o.observed_at_ms >= b.bound_at_ms
                        ORDER BY o.observed_at_ms DESC, o.rowid DESC
                        LIMIT 1
                    ),
                    (
                        SELECT o.status
                        FROM agent_session_observations o
                        WHERE o.session_socket_id = a.session_socket_id
                          AND o.worktree_id = a.worktree_id
                          AND o.address_id = a.address_id
                          AND o.observed_at_ms >= b.bound_at_ms
                        ORDER BY o.observed_at_ms DESC, o.rowid DESC
                        LIMIT 1
                    ), a.pane_type, a.persona, a.remote_commands_terminal_id IS NOT NULL
             FROM pane_addresses a
             JOIN pane_roles r USING (session_socket_id, worktree_id, role_id)
             JOIN alias_claims c USING (session_socket_id, worktree_id, address_id)
             LEFT JOIN pane_bindings b
               ON b.session_socket_id = a.session_socket_id
              AND b.worktree_id = a.worktree_id
              AND b.address_id = a.address_id
              AND b.active = 1
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               AND c.claim_state = 'active'
               AND (?3 = 1 OR a.retired = 0)
             ORDER BY a.address_id",
        )?;
        let records = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    include_retired
                ],
                |row| {
                    let retired = row.get(5)?;
                    let pane_id: Option<String> = row.get(7)?;
                    let agent_session_id = row
                        .get::<_, Option<String>>(8)?
                        .filter(|value| !value.is_empty())
                        .map(crate::AgentSessionId::new);
                    let lifecycle = if retired {
                        crate::PaneAddressLifecycle::Retired
                    } else if pane_id.is_none() {
                        crate::PaneAddressLifecycle::Unbound
                    } else if agent_session_id.is_none() {
                        crate::PaneAddressLifecycle::Empty
                    } else {
                        crate::PaneAddressLifecycle::Occupied
                    };
                    Ok(PaneAddressRecord {
                        pane_type: if row.get::<_, String>(10)? == "terminal" {
                            crate::PaneType::Terminal
                        } else {
                            crate::PaneType::Agent
                        },
                        id: PaneAddressId::new(row.get::<_, String>(0)?),
                        role_id: crate::PaneRoleId::new(row.get::<_, String>(1)?),
                        role_name: row.get(2)?,
                        role_description: row.get(3)?,
                        persona: row.get(11)?,
                        alias: row.get(4)?,
                        lifecycle,
                        pane_id,
                        pane_label: None,
                        agent_session_id,
                        occupant_label: None,
                        agent_status: row.get(9)?,
                        retired,
                        collaboration_disabled: row.get(6)?,
                        remote_commands: row.get(12)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    pub fn set_address_collaboration_disabled(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        disabled: bool,
        attribution: &MutationAttribution,
    ) -> Result<PaneAddressRecord, StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        let changed = self.transaction.execute(
            "UPDATE pane_addresses SET collaboration_disabled = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3 AND retired = 0
               AND collaboration_disabled != ?4",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str(),
                disabled
            ],
        )?;
        let record = self
            .list_pane_addresses(namespace)?
            .into_iter()
            .find(|record| &record.id == address_id)
            .ok_or_else(|| StoreError::invalid_input("Pane Address is not active"))?;
        if changed == 1 {
            self.append_transition(
                namespace,
                TransitionEntityKind::PaneAddress,
                address_id.as_str(),
                if disabled {
                    TransitionKind::Disabled
                } else {
                    TransitionKind::Enabled
                },
                attribution,
            )?;
        }
        Ok(record)
    }

    pub fn active_pane_bindings(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<crate::PaneBindingRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT binding_id, address_id, pane_id, tab_id
             FROM pane_bindings
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND active = 1
             ORDER BY address_id",
        )?;
        statement
            .query_map(namespace_params(namespace), |row| {
                Ok(crate::PaneBindingRecord {
                    id: crate::PaneBindingId::new(row.get::<_, String>(0)?),
                    address_id: PaneAddressId::new(row.get::<_, String>(1)?),
                    pane_id: row.get(2)?,
                    tab_id: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn routing_author_for_pane(
        &self,
        namespace: &StateNamespace,
        pane_id: &str,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        self.transaction
            .query_row(
                "SELECT a.address_id, c.alias, b.binding_id,
                        (
                            SELECT o.agent_session_id
                            FROM agent_session_observations o
                            WHERE o.session_socket_id = a.session_socket_id
                              AND o.worktree_id = a.worktree_id
                              AND o.address_id = a.address_id
                              AND o.observed_at_ms >= b.bound_at_ms
                            ORDER BY o.observed_at_ms DESC, o.rowid DESC
                            LIMIT 1
                        )
                 FROM pane_bindings b
                 JOIN pane_addresses a
                   ON a.session_socket_id = b.session_socket_id
                  AND a.worktree_id = b.worktree_id
                  AND a.address_id = b.address_id
                 JOIN alias_claims c
                   ON c.session_socket_id = a.session_socket_id
                  AND c.worktree_id = a.worktree_id
                  AND c.address_id = a.address_id
                  AND c.claim_state = 'active'
                 WHERE b.session_socket_id = ?1 AND b.worktree_id = ?2
                   AND b.pane_id = ?3 AND b.active = 1 AND a.retired = 0",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    pane_id
                ],
                |row| {
                    Ok(RoutingAddressSnapshot {
                        address_id: PaneAddressId::new(row.get::<_, String>(0)?),
                        alias: row.get(1)?,
                        pane_binding_id: Some(crate::PaneBindingId::new(row.get::<_, String>(2)?)),
                        agent_session_id: row
                            .get::<_, Option<String>>(3)?
                            .filter(|value| !value.is_empty())
                            .map(crate::AgentSessionId::new),
                    })
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("the current pane does not have an active Pane Address")
            })
    }

    /// The address that holds `alias` here, whether the claim is active or a
    /// tombstone left by a rename.
    pub(crate) fn alias_holder(
        &self,
        namespace: &StateNamespace,
        alias: &str,
    ) -> Result<Option<PaneAddressId>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT address_id FROM alias_claims
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND alias = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    alias
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(PaneAddressId::new))
    }

    pub(crate) fn resolve_routing_alias(
        &self,
        namespace: &StateNamespace,
        alias: &str,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        let resolved = self
            .transaction
            .query_row(
                "SELECT c.address_id, c.claim_state, c.replacement_alias, a.retired,
                        b.binding_id,
                        (
                            SELECT o.agent_session_id
                            FROM agent_session_observations o
                            WHERE o.session_socket_id = a.session_socket_id
                              AND o.worktree_id = a.worktree_id
                              AND o.address_id = a.address_id
                              AND o.observed_at_ms >= b.bound_at_ms
                            ORDER BY o.observed_at_ms DESC, o.rowid DESC
                            LIMIT 1
                        )
                 FROM alias_claims c
                 JOIN pane_addresses a
                   ON a.session_socket_id = c.session_socket_id
                  AND a.worktree_id = c.worktree_id
                  AND a.address_id = c.address_id
                 LEFT JOIN pane_bindings b
                   ON b.session_socket_id = a.session_socket_id
                  AND b.worktree_id = a.worktree_id
                  AND b.address_id = a.address_id
                  AND b.active = 1
                 WHERE c.session_socket_id = ?1 AND c.worktree_id = ?2
                   AND c.alias = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    alias
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((address_id, claim_state, replacement, retired, binding_id, session_id)) =
            resolved
        else {
            return Err(StoreError::invalid_input(format!(
                "unknown Mention: @{alias}"
            )));
        };
        if claim_state == "tombstone" {
            let replacement = replacement
                .map(|value| format!("; use @{value}"))
                .unwrap_or_default();
            return Err(StoreError::invalid_input(format!(
                "retired Mention: @{alias}{replacement}"
            )));
        }
        if retired {
            return Err(StoreError::invalid_input(format!(
                "retired Mention: @{alias}"
            )));
        }
        Ok(RoutingAddressSnapshot {
            address_id: PaneAddressId::new(address_id),
            alias: alias.to_owned(),
            pane_binding_id: binding_id.map(crate::PaneBindingId::new),
            agent_session_id: session_id
                .filter(|value| !value.is_empty())
                .map(crate::AgentSessionId::new),
        })
    }

    pub(crate) fn routing_address_by_id(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        self.optional_routing_address_by_id(namespace, address_id)?
            .ok_or_else(|| StoreError::invalid_input("reply participant is no longer routable"))
    }

    pub(crate) fn optional_routing_address_by_id(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
    ) -> Result<Option<RoutingAddressSnapshot>, StoreError> {
        if let Some(operator) = self.operator_snapshot(namespace, address_id)? {
            return Ok(Some(operator));
        }
        let found = self
            .transaction
            .query_row(
                "SELECT c.alias, e.origin_alias
                 FROM pane_addresses a
                 LEFT JOIN alias_claims c
                   ON c.session_socket_id = a.session_socket_id
                  AND c.worktree_id = a.worktree_id
                  AND c.address_id = a.address_id
                  AND c.claim_state = 'active'
                 LEFT JOIN space_envoys e
                   ON e.session_socket_id = a.session_socket_id
                  AND e.worktree_id = a.worktree_id
                  AND e.address_id = a.address_id
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.address_id = ?3 AND a.retired = 0",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str()
                ],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        match found {
            Some((Some(alias), _)) => self.resolve_routing_alias(namespace, &alias).map(Some),
            // An envoy holds no name of its own, so it never resolves by alias;
            // it answers to the name of the address it stands for.
            Some((None, Some(origin_alias))) => Ok(Some(RoutingAddressSnapshot {
                address_id: address_id.clone(),
                alias: origin_alias,
                pane_binding_id: None,
                agent_session_id: None,
            })),
            Some((None, None)) | None => Ok(None),
        }
    }

    pub fn move_pane_binding(
        &mut self,
        namespace: &StateNamespace,
        previous_pane_id: &str,
        replacement_pane_id: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let address_id = self
            .transaction
            .query_row(
                "SELECT address_id FROM pane_bindings
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND pane_id = ?3 AND active = 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    previous_pane_id
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::invalid_input("pane has no active Pane Address"))?;
        self.transaction.execute(
            "UPDATE pane_bindings SET pane_id = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND pane_id = ?3 AND active = 1",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                previous_pane_id,
                replacement_pane_id
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::PaneAddress,
            &address_id,
            TransitionKind::Bound,
            attribution,
        )
    }

    pub fn observe_pane_binding_location(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        pane_id: &str,
        tab_id: &str,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        if pane_id.is_empty() || tab_id.is_empty() {
            return Err(StoreError::invalid_input(
                "pane and tab identities are required for a binding location",
            ));
        }
        let changed = self.transaction.execute(
            "UPDATE pane_bindings SET tab_id = ?5
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3 AND pane_id = ?4 AND active = 1",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str(),
                pane_id,
                tab_id,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input(
                "pane binding location does not identify one active Pane Address",
            ));
        }
        Ok(())
    }

    pub fn bind_pane(
        &mut self,
        namespace: &StateNamespace,
        binding_id: &crate::PaneBindingId,
        address_id: &PaneAddressId,
        pane_id: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        self.transaction.execute(
            "INSERT INTO pane_bindings (
                session_socket_id, worktree_id, binding_id, address_id, pane_id,
                active, bound_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                binding_id.as_str(),
                address_id.as_str(),
                pane_id,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::PaneAddress,
            address_id.as_str(),
            TransitionKind::Bound,
            attribution,
        )?;
        self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::PaneBinding,
            address_id.as_str(),
            attribution,
        )?;
        Ok(())
    }

    pub fn rebind_pane(
        &mut self,
        namespace: &StateNamespace,
        binding_id: &crate::PaneBindingId,
        address_id: &PaneAddressId,
        pane_id: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        let pane_owner = self
            .transaction
            .query_row(
                "SELECT address_id FROM pane_bindings
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND pane_id = ?3 AND active = 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    pane_id
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if pane_owner
            .as_deref()
            .is_some_and(|owner| owner != address_id.as_str())
        {
            return Err(StoreError::invalid_input(
                "pane already has an active Pane Address",
            ));
        }
        if pane_owner.as_deref() == Some(address_id.as_str()) {
            return Ok(());
        }
        self.unbind_pane(namespace, address_id, attribution)?;
        self.bind_pane(namespace, binding_id, address_id, pane_id, attribution)
    }

    pub fn unbind_pane(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        let changed = self.transaction.execute(
            "UPDATE pane_bindings
             SET active = 0, unbound_at_ms = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3 AND active = 1",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str(),
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        if changed == 1 {
            self.expire_context_grants(namespace, address_id, None)?;
            self.expire_integration_registrations(namespace, address_id, None, attribution)?;
            self.clear_remote_commands(namespace, address_id, attribution)?;
            self.append_transition(
                namespace,
                TransitionEntityKind::PaneAddress,
                address_id.as_str(),
                TransitionKind::Unbound,
                attribution,
            )?;
        }
        Ok(())
    }

    pub fn observe_agent_session(
        &mut self,
        namespace: &StateNamespace,
        observation_id: &str,
        address_id: &PaneAddressId,
        agent_session_id: &crate::AgentSessionId,
        status: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        let bound = self
            .transaction
            .query_row(
                "SELECT 1 FROM pane_bindings
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3 AND active = 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str()
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !bound {
            return Err(StoreError::invalid_input(
                "an Agent Session can be observed only for a bound Pane Address",
            ));
        }
        self.expire_integration_registrations(
            namespace,
            address_id,
            Some(agent_session_id),
            attribution,
        )?;
        self.expire_context_grants(namespace, address_id, Some(agent_session_id))?;
        self.transaction.execute(
            "INSERT INTO agent_session_observations (
                session_socket_id, worktree_id, observation_id, address_id,
                agent_session_id, status, observed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                observation_id,
                address_id.as_str(),
                agent_session_id.as_str(),
                status,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::PaneAddress,
            address_id.as_str(),
            if agent_session_id.as_str().is_empty() {
                TransitionKind::Empty
            } else {
                TransitionKind::Occupied
            },
            attribution,
        )
    }

    fn expire_context_grants(
        &mut self,
        namespace: &StateNamespace,
        source_address_id: &PaneAddressId,
        current_session: Option<&crate::AgentSessionId>,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "UPDATE context_grants
             SET state = 'expired'
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND source_address_id = ?3 AND state IN ('pending', 'active')
               AND (
                    ?4 IS NULL
                    OR agent_session_id != ?4
                    OR pane_binding_id IS NULL
                    OR pane_binding_id != (
                        SELECT binding_id FROM pane_bindings
                        WHERE session_socket_id = ?1 AND worktree_id = ?2
                          AND address_id = ?3 AND active = 1
                    )
               )",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                source_address_id.as_str(),
                current_session.map(crate::AgentSessionId::as_str),
            ],
        )?;
        Ok(())
    }

    pub fn issue_integration_challenge(
        &mut self,
        namespace: &StateNamespace,
        registration_id: &str,
        address_id: &PaneAddressId,
        runtime: AgentRuntime,
        nonce: &str,
        attribution: &MutationAttribution,
    ) -> Result<IntegrationChallenge, StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        if registration_id.starts_with(crate::INTEGRATION_PACKAGE_ATTENTION_SOURCE_PREFIX) {
            return Err(StoreError::invalid_input(
                "registration id uses the reserved integration-package Attention namespace",
            ));
        }
        if nonce.is_empty() {
            return Err(StoreError::invalid_input(
                "registration challenge nonce is required",
            ));
        }
        let agent_session_id = self.current_agent_session(namespace, address_id)?;
        if self
            .active_integration_registration(namespace, address_id, &agent_session_id)?
            .is_some()
        {
            return Err(StoreError::invalid_input(
                "the current Agent Session is already registered",
            ));
        }
        if let Some((registration_id, nonce)) = self
            .transaction
            .query_row(
                "SELECT r.registration_id, r.capability_profile
                 FROM integration_registrations r
                 WHERE r.session_socket_id = ?1 AND r.worktree_id = ?2
                   AND r.address_id = ?3 AND r.agent_session_id = ?4
                   AND r.runtime = ?5 AND r.protocol_version = ?6 AND r.active = 0
                   AND (
                       SELECT t.transition_kind
                       FROM transition_facts t
                       WHERE t.session_socket_id = r.session_socket_id
                         AND t.worktree_id = r.worktree_id
                         AND t.entity_kind = 'integration_registration'
                         AND t.entity_id = r.registration_id
                       ORDER BY t.worktree_order DESC LIMIT 1
                   ) IN ('challenged', 'dispatching', 'submitted', 'invoked', 'failed')
                 ORDER BY r.rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str(),
                    agent_session_id.as_str(),
                    runtime.as_str(),
                    crate::INTEGRATION_PROTOCOL_VERSION,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            return Ok(IntegrationChallenge {
                registration_id,
                address_id: address_id.clone(),
                agent_session_id,
                runtime,
                protocol_version: crate::INTEGRATION_PROTOCOL_VERSION,
                nonce,
                capabilities: crate::complete_capability_profile(),
            });
        }
        self.transaction.execute(
            "INSERT INTO integration_registrations (
                session_socket_id, worktree_id, registration_id, address_id,
                agent_session_id, runtime, protocol_version, capability_profile, active
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                registration_id,
                address_id.as_str(),
                agent_session_id.as_str(),
                runtime.as_str(),
                crate::INTEGRATION_PROTOCOL_VERSION,
                nonce,
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::IntegrationRegistration,
            registration_id,
            TransitionKind::Challenged,
            attribution,
        )?;
        Ok(IntegrationChallenge {
            registration_id: registration_id.to_owned(),
            address_id: address_id.clone(),
            agent_session_id,
            runtime,
            protocol_version: crate::INTEGRATION_PROTOCOL_VERSION,
            nonce: nonce.to_owned(),
            capabilities: crate::complete_capability_profile(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn complete_integration_registration(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        agent_session_id: &crate::AgentSessionId,
        runtime: AgentRuntime,
        protocol_version: &str,
        nonce: &str,
        capabilities: &[String],
        attribution: &MutationAttribution,
    ) -> Result<IntegrationRegistration, StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        let current_session = self.current_agent_session(namespace, address_id)?;
        if current_session != *agent_session_id {
            return Err(StoreError::invalid_input(
                "registration must come from the challenged Agent Session",
            ));
        }
        let registration_id = self
            .transaction
            .query_row(
                "SELECT registration_id FROM integration_registrations
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3 AND agent_session_id = ?4
                   AND runtime = ?5 AND protocol_version = ?6
                   AND capability_profile = ?7 AND active = 0
                   AND (
                       SELECT transition_kind
                       FROM transition_facts
                       WHERE session_socket_id = integration_registrations.session_socket_id
                         AND worktree_id = integration_registrations.worktree_id
                         AND entity_kind = 'integration_registration'
                         AND entity_id = integration_registrations.registration_id
                       ORDER BY worktree_order DESC LIMIT 1
                   ) IN ('challenged', 'dispatching', 'submitted', 'invoked', 'failed')
                 ORDER BY rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str(),
                    agent_session_id.as_str(),
                    runtime.as_str(),
                    protocol_version,
                    nonce,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input(
                    "registration nonce is invalid, already used, or belongs to another session",
                )
            })?;
        let capability_profile = serde_json::to_string(capabilities)
            .map_err(|error| StoreError::invalid_input(error.to_string()))?;
        let changed = self.transaction.execute(
            "UPDATE integration_registrations
             SET capability_profile = ?4, active = 1
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND registration_id = ?3 AND active = 0",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                registration_id,
                capability_profile,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input(
                "registration challenge was already consumed",
            ));
        }
        self.append_transition(
            namespace,
            TransitionEntityKind::IntegrationRegistration,
            &registration_id,
            TransitionKind::Registered,
            attribution,
        )?;
        if !registration_id.starts_with(crate::INTEGRATION_PACKAGE_ATTENTION_SOURCE_PREFIX) {
            self.resolve_attention_source(
                namespace,
                crate::AttentionSourceKind::IntegrationRegistration,
                &registration_id,
                attribution,
            )?;
        }
        self.resolve_superseded_integration_registration_attention(
            namespace,
            address_id,
            &registration_id,
            attribution,
        )?;
        Ok(IntegrationRegistration {
            registration_id,
            address_id: address_id.clone(),
            agent_session_id: agent_session_id.clone(),
            runtime,
            protocol_version: protocol_version.to_owned(),
            capabilities: capabilities.to_vec(),
            active: true,
        })
    }

    pub fn active_integration_registration(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        agent_session_id: &crate::AgentSessionId,
    ) -> Result<Option<IntegrationRegistration>, StoreError> {
        self.transaction
            .query_row(
                "SELECT registration_id, runtime, protocol_version, capability_profile
                 FROM integration_registrations
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3 AND agent_session_id = ?4 AND active = 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str(),
                    agent_session_id.as_str(),
                ],
                |row| {
                    let runtime = row.get::<_, String>(1)?;
                    let capabilities = row.get::<_, String>(3)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        runtime,
                        row.get::<_, String>(2)?,
                        capabilities,
                    ))
                },
            )
            .optional()?
            .map(
                |(registration_id, runtime, protocol_version, capabilities)| {
                    Ok(IntegrationRegistration {
                        registration_id,
                        address_id: address_id.clone(),
                        agent_session_id: agent_session_id.clone(),
                        runtime: AgentRuntime::parse(&runtime)
                            .map_err(StoreError::invalid_input)?,
                        protocol_version,
                        capabilities: serde_json::from_str(&capabilities)
                            .map_err(|error| StoreError::invalid_input(error.to_string()))?,
                        active: true,
                    })
                },
            )
            .transpose()
    }

    pub(crate) fn integration_challenge_transition(
        &self,
        namespace: &StateNamespace,
        registration_id: &str,
    ) -> Result<TransitionKind, StoreError> {
        self.transaction
            .query_row(
                "SELECT transition_kind
                 FROM transition_facts
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND entity_kind = 'integration_registration' AND entity_id = ?3
                 ORDER BY worktree_order DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    registration_id,
                ],
                |row| parse_transition_kind(row.get::<_, String>(0)?),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("Integration Registration challenge does not exist")
            })
    }

    pub(crate) fn claim_integration_challenge_prompt(
        &mut self,
        namespace: &StateNamespace,
        registration_id: &str,
        attribution: &MutationAttribution,
    ) -> Result<bool, StoreError> {
        if self.integration_challenge_transition(namespace, registration_id)?
            != TransitionKind::Challenged
        {
            return Ok(false);
        }
        self.append_transition(
            namespace,
            TransitionEntityKind::IntegrationRegistration,
            registration_id,
            TransitionKind::Dispatching,
            attribution,
        )?;
        Ok(true)
    }

    pub(crate) fn mark_integration_challenge_invoked(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        agent_session_id: &crate::AgentSessionId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let registration_id = self
            .transaction
            .query_row(
                "SELECT r.registration_id
                 FROM integration_registrations r
                 WHERE r.session_socket_id = ?1 AND r.worktree_id = ?2
                   AND r.address_id = ?3 AND r.agent_session_id = ?4 AND r.active = 0
                   AND (
                       SELECT t.transition_kind
                       FROM transition_facts t
                       WHERE t.session_socket_id = r.session_socket_id
                         AND t.worktree_id = r.worktree_id
                         AND t.entity_kind = 'integration_registration'
                         AND t.entity_id = r.registration_id
                       ORDER BY t.worktree_order DESC LIMIT 1
                   ) IN ('dispatching', 'submitted')
                 ORDER BY r.rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str(),
                    agent_session_id.as_str(),
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(registration_id) = registration_id {
            self.append_transition(
                namespace,
                TransitionEntityKind::IntegrationRegistration,
                &registration_id,
                TransitionKind::Invoked,
                attribution,
            )?;
        }
        Ok(())
    }

    pub(crate) fn finish_integration_challenge_prompt(
        &mut self,
        namespace: &StateNamespace,
        registration_id: &str,
        outcome: TransitionKind,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        if !matches!(
            outcome,
            TransitionKind::Challenged | TransitionKind::Submitted
        ) {
            return Err(StoreError::invalid_input(
                "invalid Integration Registration challenge prompt outcome",
            ));
        }
        let current = self.integration_challenge_transition(namespace, registration_id)?;
        if matches!(
            current,
            TransitionKind::Registered
                | TransitionKind::Invoked
                | TransitionKind::Failed
                | TransitionKind::Expired
        ) {
            return Ok(());
        }
        if current != TransitionKind::Dispatching {
            return Err(StoreError::invalid_input(
                "Integration Registration challenge prompt is not dispatching",
            ));
        }
        self.append_transition(
            namespace,
            TransitionEntityKind::IntegrationRegistration,
            registration_id,
            outcome,
            attribution,
        )
    }

    fn current_agent_session(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
    ) -> Result<crate::AgentSessionId, StoreError> {
        self.current_agent_session_optional(namespace, address_id)?
            .ok_or_else(|| {
                StoreError::invalid_input("a current Agent Session is required on the Pane Address")
            })
    }

    fn current_agent_session_optional(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
    ) -> Result<Option<crate::AgentSessionId>, StoreError> {
        let agent_session_id = self
            .transaction
            .query_row(
                "SELECT o.agent_session_id
                 FROM pane_bindings b
                 JOIN agent_session_observations o
                   ON o.session_socket_id = b.session_socket_id
                  AND o.worktree_id = b.worktree_id
                  AND o.address_id = b.address_id
                  AND o.observed_at_ms >= b.bound_at_ms
                 WHERE b.session_socket_id = ?1 AND b.worktree_id = ?2
                   AND b.address_id = ?3 AND b.active = 1
                 ORDER BY o.observed_at_ms DESC, o.rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str(),
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .filter(|session_id| !session_id.is_empty())
            .map(crate::AgentSessionId::new);
        Ok(agent_session_id)
    }

    fn expire_integration_registrations(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        current_session: Option<&crate::AgentSessionId>,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let comparison = current_session.map_or("", crate::AgentSessionId::as_str);
        let accepted_profiles = crate::integration::accepted_capability_profiles()
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::invalid_input(error.to_string()))?;
        let accepted_placeholders = (0..accepted_profiles.len())
            .map(|index| format!("?{}", 5 + index))
            .collect::<Vec<_>>()
            .join(", ");
        let parameters = [
            namespace.session_socket_id.clone(),
            namespace.space_id.as_str().to_owned(),
            address_id.as_str().to_owned(),
            comparison.to_owned(),
        ]
        .into_iter()
        .chain(accepted_profiles)
        .collect::<Vec<_>>();
        let mut statement = self.transaction.prepare(&format!(
            "SELECT r.registration_id FROM integration_registrations r
             WHERE r.session_socket_id = ?1 AND r.worktree_id = ?2
               AND r.address_id = ?3
               AND (
                   (
                       r.agent_session_id != ?4
                       AND (
                           ?4 = ''
                           OR NOT EXISTS (
                               SELECT 1 FROM transition_facts registered
                               WHERE registered.session_socket_id = r.session_socket_id
                                 AND registered.worktree_id = r.worktree_id
                                 AND registered.entity_kind = 'integration_registration'
                                 AND registered.entity_id = r.registration_id
                                 AND registered.transition_kind = 'registered'
                           )
                       )
                   )
                   OR (r.active = 1 AND r.capability_profile NOT IN ({accepted_placeholders}))
               )
               AND NOT EXISTS (
                   SELECT 1 FROM transition_facts t
                   WHERE t.session_socket_id = r.session_socket_id
                     AND t.worktree_id = r.worktree_id
                     AND t.entity_kind = 'integration_registration'
                     AND t.entity_id = r.registration_id
                     AND t.transition_kind = 'expired'
               )"
        ))?;
        let expired = statement
            .query_map(rusqlite::params_from_iter(&parameters), |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        self.transaction.execute(
            &format!(
                "UPDATE integration_registrations SET active = 0
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3 AND active = 1
               AND (
                   capability_profile NOT IN ({accepted_placeholders})
                   OR (
                       agent_session_id != ?4
                       AND (
                           ?4 = ''
                           OR NOT EXISTS (
                               SELECT 1 FROM transition_facts registered
                               WHERE registered.session_socket_id =
                                         integration_registrations.session_socket_id
                                 AND registered.worktree_id =
                                         integration_registrations.worktree_id
                                 AND registered.entity_kind = 'integration_registration'
                                 AND registered.entity_id =
                                         integration_registrations.registration_id
                                 AND registered.transition_kind = 'registered'
                           )
                       )
                   )
               )"
            ),
            rusqlite::params_from_iter(&parameters),
        )?;
        for registration_id in expired {
            self.append_transition(
                namespace,
                TransitionEntityKind::IntegrationRegistration,
                &registration_id,
                TransitionKind::Expired,
                attribution,
            )?;
        }
        Ok(())
    }

    pub fn complete_reconciliation(
        &mut self,
        namespace: &StateNamespace,
    ) -> Result<u64, StoreError> {
        let changed = self.transaction.execute(
            "UPDATE reconciliation_epochs
             SET epoch = epoch + 1, completed = 1
             WHERE session_socket_id = ?1 AND worktree_id = ?2",
            namespace_params(namespace),
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input(
                "Namedrop is not activated for this space",
            ));
        }
        self.transaction
            .query_row(
                "SELECT epoch FROM reconciliation_epochs
                 WHERE session_socket_id = ?1 AND worktree_id = ?2",
                namespace_params(namespace),
                |row| from_i64(row.get(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn record_integration_registration_failure(
        &mut self,
        namespace: &StateNamespace,
        nonce: &str,
        summary: &str,
        attribution: &MutationAttribution,
    ) -> Result<crate::AttentionItemId, StoreError> {
        let registration_id = self
            .transaction
            .query_row(
                "SELECT registration_id
                 FROM integration_registrations
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND capability_profile = ?3 AND active = 0
                 ORDER BY rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    nonce
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input(
                    "failed registration does not match an active Integration Registration challenge",
                )
            })?;
        if !matches!(
            self.integration_challenge_transition(namespace, &registration_id)?,
            TransitionKind::Failed | TransitionKind::Registered | TransitionKind::Expired
        ) {
            self.append_transition(
                namespace,
                TransitionEntityKind::IntegrationRegistration,
                &registration_id,
                TransitionKind::Failed,
                attribution,
            )?;
        }
        self.open_attention_item(
            namespace,
            &AttentionItemDraft {
                id: crate::AttentionItemId::new(format!(
                    "attention-integration-registration-{registration_id}"
                )),
                source_kind: crate::AttentionSourceKind::IntegrationRegistration,
                source_id: registration_id,
                summary: summary.to_owned(),
            },
            attribution,
        )
    }

    pub(crate) fn reconciliation_completed(
        &self,
        namespace: &StateNamespace,
    ) -> Result<bool, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT completed FROM reconciliation_epochs
                 WHERE session_socket_id = ?1 AND worktree_id = ?2",
                namespace_params(namespace),
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn replace_alias(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        replacement_alias: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        crate::validate_alias(replacement_alias).map_err(StoreError::invalid_input)?;
        self.refuse_claimed_alias(namespace, replacement_alias)?;
        let previous_alias = self.transaction.query_row(
            "SELECT alias FROM alias_claims
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3 AND claim_state = 'active'",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str()
            ],
            |row| row.get::<_, String>(0),
        )?;
        self.transaction.execute(
            "UPDATE alias_claims
             SET claim_state = 'tombstone', replacement_alias = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3 AND claim_state = 'active'",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str(),
                replacement_alias
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO alias_claims (
                session_socket_id, worktree_id, alias, address_id, claim_state
             ) VALUES (?1, ?2, ?3, ?4, 'active')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                replacement_alias,
                address_id.as_str()
            ],
        )?;
        for (alias, transition_kind, replacement) in [
            (
                previous_alias.as_str(),
                "tombstoned",
                Some(replacement_alias),
            ),
            (replacement_alias, "claimed", None),
        ] {
            let history_order = self.next_order(namespace, "alias_history", "")?;
            self.transaction.execute(
                "INSERT INTO alias_history (
                    session_socket_id, worktree_id, history_order, alias, address_id,
                    transition_kind, replacement_alias, occurred_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    to_i64(history_order)?,
                    alias,
                    address_id.as_str(),
                    transition_kind,
                    replacement,
                    to_i64(attribution.occurred_at_ms)?
                ],
            )?;
        }
        self.append_transition(
            namespace,
            TransitionEntityKind::PaneAddress,
            address_id.as_str(),
            TransitionKind::Renamed,
            attribution,
        )?;
        Ok(())
    }

    /// Marks a Pane Address as owned by the native one-command collaboration
    /// path. The marker lives on the existing role record so old TUI-created
    /// addresses can opt in without a schema migration or identity replacement.
    pub(crate) fn mark_native_pane_address(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        alias: &str,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        crate::validate_alias(alias).map_err(StoreError::invalid_input)?;
        let changed = self.transaction.execute(
            "UPDATE pane_roles
             SET description = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND role_id = (
                   SELECT role_id FROM pane_addresses
                   WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
               )",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str(),
                format!("Native collaborator addressed as @{alias}."),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input("Pane Address does not exist"));
        }
        Ok(())
    }

    /// Saves or clears one Pane Address's persona. `None` is Generic, and is
    /// also what clearing leaves behind, so an explicit Generic choice is never
    /// re-derived from the role description a later upgrade reads.
    pub fn set_address_persona(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        persona: Option<&str>,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        if let Some(description) = persona {
            crate::validate_persona(description).map_err(StoreError::invalid_input)?;
            if self.pane_type(namespace, address_id)? == crate::PaneType::Terminal {
                return Err(StoreError::invalid_input(
                    "a named terminal cannot hold a persona",
                ));
            }
        }
        let changed = self.transaction.execute(
            "UPDATE pane_addresses SET persona = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str(),
                persona
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input("Pane Address does not exist"));
        }
        Ok(())
    }

    pub fn release_alias(
        &mut self,
        namespace: &StateNamespace,
        alias: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let address_id = self
            .transaction
            .query_row(
                "SELECT c.address_id
                 FROM alias_claims c
                 JOIN pane_addresses a
                   ON a.session_socket_id = c.session_socket_id
                  AND a.worktree_id = c.worktree_id
                  AND a.address_id = c.address_id
                 WHERE c.session_socket_id = ?1 AND c.worktree_id = ?2
                   AND c.alias = ?3
                   AND (c.claim_state = 'tombstone' OR a.retired = 1)",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    alias
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::invalid_input("alias is not releasable"))?;
        self.transaction.execute(
            "DELETE FROM alias_claims
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND alias = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                alias
            ],
        )?;
        let history_order = self.next_order(namespace, "alias_history", "")?;
        self.transaction.execute(
            "INSERT INTO alias_history (
                session_socket_id, worktree_id, history_order, alias, address_id,
                transition_kind, replacement_alias, occurred_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'released', NULL, ?6)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                to_i64(history_order)?,
                alias,
                address_id,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::PaneAddress,
            &address_id,
            TransitionKind::Released,
            attribution,
        )
    }

    pub fn retire_pane_address(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        let outstanding: i64 = self.transaction.query_row(
            "SELECT COUNT(*) FROM mailbox_deliveries
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3
               AND state NOT IN ('settled', 'failed', 'cancelled')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str()
            ],
            |row| row.get(0),
        )?;
        if outstanding != 0 {
            return Err(StoreError::invalid_input(
                "Pane Address retirement requires every queued delivery to be terminal or cancelled",
            ));
        }
        self.unbind_pane(namespace, address_id, attribution)?;
        let changed = self.transaction.execute(
            "UPDATE pane_addresses
             SET retired = 1, collaboration_disabled = 1
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND address_id = ?3 AND retired = 0",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input(
                "Pane Address does not exist or is already retired",
            ));
        }
        self.append_transition(
            namespace,
            TransitionEntityKind::PaneAddress,
            address_id.as_str(),
            TransitionKind::Retired,
            attribution,
        )
    }

    /// Remove one Pane Address as a single human action: retirement performs
    /// the unbind, then every alias held by that address is released for reuse.
    /// The surrounding transaction keeps the visible removal all-or-nothing.
    pub fn remove_pane_address(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        attribution: &MutationAttribution,
    ) -> Result<Vec<String>, StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        self.retire_pane_address(namespace, address_id, attribution)?;
        let aliases = {
            let mut statement = self.transaction.prepare(
                "SELECT alias FROM alias_claims
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3
                 ORDER BY alias",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        address_id.as_str()
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for alias in &aliases {
            self.release_alias(namespace, alias, attribution)?;
        }
        Ok(aliases)
    }

    /// A host close is the user's cancellation authority for the Pane Address:
    /// cancel every nonterminal delivery before retiring the address and
    /// releasing its aliases in this transaction.
    pub fn cancel_and_remove_pane_address(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        attribution: &MutationAttribution,
    ) -> Result<Vec<String>, StoreError> {
        operator::refuse_pane_mutation(address_id)?;
        let attempt_ids = {
            let mut statement = self.transaction.prepare(
                "SELECT a.attempt_id
                 FROM delivery_attempts a
                 JOIN delivery_attempt_state s USING (
                    session_socket_id, worktree_id, attempt_id, address_id
                 )
                 JOIN mailbox_deliveries d USING (
                    session_socket_id, worktree_id, delivery_id, address_id
                 )
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.address_id = ?3
                   AND d.state NOT IN ('settled', 'failed', 'cancelled')
                   AND s.state = d.state
                 ORDER BY a.created_at_ms, a.rowid",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        address_id.as_str(),
                    ],
                    |row| row.get::<_, String>(0).map(crate::DeliveryAttemptId::new),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for attempt_id in attempt_ids {
            self.transition_delivery_attempt(
                namespace,
                &attempt_id,
                DeliveryState::Cancelled,
                attribution,
            )?;
        }

        let unattempted_delivery_ids = {
            let mut statement = self.transaction.prepare(
                "SELECT delivery_id FROM mailbox_deliveries
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3 AND state IN ('queued', 'held')
                 ORDER BY mailbox_order",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        address_id.as_str(),
                    ],
                    |row| row.get::<_, String>(0).map(crate::DeliveryId::new),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for delivery_id in unattempted_delivery_ids {
            self.cancel_unsent_delivery(namespace, &delivery_id, attribution)?;
        }

        self.remove_pane_address(namespace, address_id, attribution)
    }

    pub fn create_thread(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        title: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO collaboration_threads (
                session_socket_id, worktree_id, thread_id, title, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str(),
                title,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO thread_controls (
                session_socket_id, worktree_id, thread_id, paused, stopped,
                reply_allowance
             ) VALUES (?1, ?2, ?3, 0, 0, 10)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::CollaborationThread,
            thread_id.as_str(),
            TransitionKind::Created,
            attribution,
        )?;
        Ok(())
    }

    pub(crate) fn replay_message(
        &self,
        namespace: &StateNamespace,
        author_address_id: &PaneAddressId,
        request_id: &str,
        authored_source: &str,
        in_reply_to: Option<&MessageId>,
        author_kind: ActorKind,
    ) -> Result<Option<RoutedMessageReceipt>, StoreError> {
        let existing = self
            .transaction
            .query_row(
                "SELECT m.message_id, m.authored_source, m.in_reply_to, m.author_kind
                 FROM idempotency_records i
                 JOIN collaboration_messages m
                   ON m.session_socket_id = i.session_socket_id
                  AND m.worktree_id = i.worktree_id
                  AND m.message_id = i.message_id
                 WHERE i.session_socket_id = ?1 AND i.worktree_id = ?2
                   AND i.author_address_id = ?3 AND i.request_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    author_address_id.as_str(),
                    request_id
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((message_id, stored_source, stored_reply, stored_author_kind)) = existing else {
            return Ok(None);
        };
        if stored_source != authored_source
            || stored_reply.as_deref() != in_reply_to.map(MessageId::as_str)
            || stored_author_kind != author_kind.as_str()
        {
            return Err(StoreError::invalid_input(
                "request id was already used for different message content",
            ));
        }
        let message_id = MessageId::new(message_id);
        let recipients = self.message_routing_snapshots(
            namespace,
            &message_id,
            "message_recipients",
            "recipient_order",
            false,
        )?;
        let references = self.message_routing_snapshots(
            namespace,
            &message_id,
            "context_references",
            "reference_order",
            true,
        )?;
        Ok(Some(RoutedMessageReceipt {
            receipt: self.message_receipt(namespace, &message_id, true)?,
            recipients,
            context_references: references,
        }))
    }

    pub(crate) fn channel_thread(
        &self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
               AND in_reply_to IS NULL AND grammar_version = 'channel/v1')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn preferred_router_host(&self) -> Result<Option<crate::AgentRuntime>, StoreError> {
        let runtime = self
            .transaction
            .query_row(
                "SELECT runtime FROM integration_registrations
             WHERE active = 1 AND runtime IN ('claude-code', 'codex')
             ORDER BY CASE runtime WHEN 'claude-code' THEN 0 ELSE 1 END LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(runtime.map(|runtime| {
            if runtime == "claude-code" {
                crate::AgentRuntime::ClaudeCode
            } else {
                crate::AgentRuntime::Codex
            }
        }))
    }

    pub(crate) fn thread_agent_participants(
        &self,
        namespace: &StateNamespace,
        thread: &ThreadId,
    ) -> Result<Vec<RoutingAddressSnapshot>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT p.address_id FROM thread_participants p
             JOIN pane_addresses a USING (session_socket_id, worktree_id, address_id)
             LEFT JOIN space_envoys e USING (session_socket_id, worktree_id, address_id)
             WHERE p.session_socket_id = ?1 AND p.worktree_id = ?2 AND p.thread_id = ?3
               AND a.retired = 0 AND a.pane_type = 'agent'
               AND (e.address_id IS NULL OR EXISTS (
                   SELECT 1 FROM pane_addresses origin
                   WHERE origin.session_socket_id = p.session_socket_id
                     AND origin.worktree_id = e.origin_space_id
                     AND origin.address_id = e.origin_address_id
                     AND origin.retired = 0 AND origin.pane_type = 'agent'))
             ORDER BY p.participant_order",
        )?;
        let ids = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread.as_str()
                ],
                |row| row.get::<_, String>(0).map(PaneAddressId::new),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut participants = Vec::new();
        for id in ids {
            if self.is_operator_address(namespace, &id)? {
                continue;
            }
            if let Some(origin) = self.envoy_origin(namespace, &id)? {
                let home = StateNamespace::new(&namespace.session_socket_id, origin.space);
                if self.is_operator_address(&home, &origin.address_id)? {
                    continue;
                }
            }
            if let Some(participant) = self.optional_routing_address_by_id(namespace, &id)? {
                participants.push(participant);
            }
        }
        Ok(participants)
    }

    pub(crate) fn channel_mentioned(
        &self,
        namespace: &StateNamespace,
        source: &str,
    ) -> Result<Vec<String>, StoreError> {
        let mentions = crate::message::context_mentions(source);
        Ok(self
            .channel_members(namespace)?
            .into_iter()
            .filter(|member| {
                mentions
                    .iter()
                    .any(|mention| mention.workspace.is_none() && mention.alias == member.alias)
            })
            .map(|member| member.alias)
            .collect())
    }

    pub(crate) fn channel_members(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<RoutingAddressSnapshot>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.address_id, c.alias, b.binding_id,
                    (SELECT o.agent_session_id FROM agent_session_observations o
                     WHERE o.session_socket_id = a.session_socket_id AND o.worktree_id = a.worktree_id
                       AND o.address_id = a.address_id AND o.observed_at_ms >= b.bound_at_ms
                     ORDER BY o.observed_at_ms DESC, o.rowid DESC LIMIT 1)
             FROM pane_addresses a
             JOIN alias_claims c USING (session_socket_id, worktree_id, address_id)
             LEFT JOIN pane_bindings b ON b.session_socket_id = a.session_socket_id
               AND b.worktree_id = a.worktree_id AND b.address_id = a.address_id AND b.active = 1
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               AND a.retired = 0 AND a.collaboration_disabled = 0 AND a.pane_type = 'agent'
               AND a.address_id <> 'operator:local' AND c.claim_state = 'active' AND c.alias <> 'operator'
               AND NOT EXISTS (SELECT 1 FROM space_envoys e WHERE e.session_socket_id = a.session_socket_id
                   AND e.worktree_id = a.worktree_id AND e.address_id = a.address_id)
             ORDER BY a.address_id")?;
        Ok(statement
            .query_map(
                params![namespace.session_socket_id, namespace.space_id.as_str()],
                |row| {
                    Ok(RoutingAddressSnapshot {
                        address_id: PaneAddressId::new(row.get::<_, String>(0)?),
                        alias: row.get(1)?,
                        pane_binding_id: row
                            .get::<_, Option<String>>(2)?
                            .map(crate::PaneBindingId::new),
                        agent_session_id: row
                            .get::<_, Option<String>>(3)?
                            .filter(|id| !id.is_empty())
                            .map(crate::AgentSessionId::new),
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn channel_recap(
        &self,
        namespace: &StateNamespace,
        exclude: &MessageId,
    ) -> Result<String, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT m.author_alias_snapshot, m.body FROM collaboration_messages m
             WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2 AND m.message_id <> ?3
               AND EXISTS(SELECT 1 FROM collaboration_messages root
                   WHERE root.session_socket_id = m.session_socket_id AND root.worktree_id = m.worktree_id
                     AND root.thread_id = m.thread_id AND root.in_reply_to IS NULL AND root.grammar_version = 'channel/v1')
             ORDER BY m.created_at_ms DESC, m.rowid DESC LIMIT 6")?;
        let mut lines = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    exclude.as_str()
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        lines.reverse();
        let single_line = |source: &str, cap| {
            source
                .chars()
                .map(|ch| {
                    if ch.is_whitespace() || ch.is_control() {
                        ' '
                    } else {
                        ch
                    }
                })
                .take(cap)
                .collect::<String>()
        };
        Ok(lines
            .into_iter()
            .map(|(author, body)| {
                format!("{}: {}", single_line(&author, 32), single_line(&body, 160))
            })
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(1200)
            .collect())
    }

    pub(crate) fn message_route(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
    ) -> Result<MessageRoute, StoreError> {
        let (thread_id, author, grammar_version, source, author_kind, body, in_reply_to) = self
            .transaction
            .query_row(
                "SELECT thread_id, author_address_id, author_alias_snapshot,
                        author_session_snapshot, grammar_version, authored_source,
                        author_kind, body, in_reply_to
                 FROM collaboration_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |row| {
                    Ok((
                        ThreadId::new(row.get::<_, String>(0)?),
                        RoutingAddressSnapshot {
                            address_id: PaneAddressId::new(row.get::<_, String>(1)?),
                            alias: row.get(2)?,
                            pane_binding_id: None,
                            agent_session_id: row
                                .get::<_, Option<String>>(3)?
                                .map(crate::AgentSessionId::new),
                        },
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::invalid_input("reply target message does not exist"))?;
        let recipients = self.message_routing_snapshots(
            namespace,
            message_id,
            "message_recipients",
            "recipient_order",
            false,
        )?;
        let channel_thread = self.channel_thread(namespace, &thread_id)?;
        let returned_to = match in_reply_to {
            Some(parent_id)
                if channel_thread && author_kind == ActorKind::AgentSession.as_str() =>
            {
                self.message_route(namespace, &MessageId::new(parent_id))?
                    .asker_for(&author.address_id)
                    .map(|asker| asker.address_id.clone())
            }
            _ => None,
        };
        let body_mentions = crate::message::context_mentions(&body);
        let mentions = crate::message::context_mentions(&source);
        let operator_selection = author_kind == ActorKind::Human.as_str()
            && self.is_operator_address(namespace, &author.address_id)?
            && (channel_thread || self.channel_routing_state(namespace, message_id)?.is_some());
        let asked_addresses = recipients
            .iter()
            .filter(|recipient| {
                let named = |list: &[crate::message::BodyMention]| {
                    list.iter().any(|mention| {
                        mention.workspace.is_none() && mention.alias == recipient.alias
                    })
                };
                operator_selection
                    || named(&body_mentions)
                    || (returned_to.as_ref() != Some(&recipient.address_id) && named(&mentions))
            })
            .map(|recipient| recipient.address_id.clone())
            .collect();
        let mut reply_mode = crate::DeliveryReplyMode::for_message_grammar(&grammar_version);
        if reply_mode == crate::DeliveryReplyMode::AgentThread && channel_thread {
            reply_mode = crate::DeliveryReplyMode::Channel;
        }
        Ok(MessageRoute::new(
            thread_id,
            author,
            recipients,
            reply_mode,
            matches!(
                grammar_version.as_str(),
                "coordination-nudge/v1" | "coordination-report/v1" | "result-set/v1"
            ),
            asked_addresses,
        ))
    }

    pub fn append_message(
        &mut self,
        namespace: &StateNamespace,
        draft: &MessageDraft,
        attribution: &MutationAttribution,
    ) -> Result<MessageReceipt, StoreError> {
        self.append_message_from(namespace, draft, attribution, None)
    }

    fn append_message_from(
        &mut self,
        namespace: &StateNamespace,
        draft: &MessageDraft,
        attribution: &MutationAttribution,
        bridged_from: Option<&bridge::BridgeOrigin<'_>>,
    ) -> Result<MessageReceipt, StoreError> {
        self.append_message_mode(namespace, draft, attribution, bridged_from, false)
    }

    fn append_message_mode(
        &mut self,
        namespace: &StateNamespace,
        draft: &MessageDraft,
        attribution: &MutationAttribution,
        bridged_from: Option<&bridge::BridgeOrigin<'_>>,
        pending_channel: bool,
    ) -> Result<MessageReceipt, StoreError> {
        let fingerprint = serde_json::to_string(&serde_json::json!({
            "in_reply_to": draft.in_reply_to.as_ref().map(MessageId::as_str),
            "author_address_id": draft.author_address_id.as_str(),
            "author_kind": draft.author_kind.as_str(),
            "authored_source": draft.authored_source,
            "body": draft.body,
            "grammar_version": draft.grammar_version,
            "recipients": draft.recipients.iter().map(|recipient| serde_json::json!({
                "address_id": recipient.address_id.as_str(),
            })).collect::<Vec<_>>(),
            "context_references": draft.context_references.iter().map(|reference| {
                serde_json::json!({
                    "address_id": reference.address_id.as_str(),
                })
            }).collect::<Vec<_>>(),
        }))
        .map_err(|error| StoreError::invalid_input(error.to_string()))?;
        let existing = self
            .transaction
            .query_row(
                "SELECT request_fingerprint, message_id
                 FROM idempotency_records
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND author_address_id = ?3 AND request_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    draft.author_address_id.as_str(),
                    draft.request_id
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((stored_fingerprint, message_id)) = existing {
            if stored_fingerprint != fingerprint {
                return Err(StoreError::invalid_input(
                    "request id was already used for different message content".to_owned(),
                ));
            }
            return self.message_receipt(namespace, &MessageId::new(message_id), true);
        }
        if draft.recipients.is_empty()
            && !((pending_channel
                || (draft.grammar_version == "channel/v1" && draft.in_reply_to.is_none()))
                && draft.author_kind == ActorKind::Human
                && self.is_operator_address(namespace, &draft.author_address_id)?)
        {
            return Err(StoreError::invalid_input(
                "a collaboration message needs at least one recipient".to_owned(),
            ));
        }
        let author_disabled = self.transaction.query_row(
            "SELECT collaboration_disabled
             FROM pane_addresses
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
               AND retired = 0",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.author_address_id.as_str()
            ],
            |row| row.get::<_, bool>(0),
        )?;
        if author_disabled && draft.grammar_version != "terminal-result/v1" {
            return Err(StoreError::invalid_input(
                "Collaboration Disabled blocks sends from this Pane Address",
            ));
        }
        // A copy's reply was already counted against the author's own thread.
        let is_agent_reply = bridged_from.is_none()
            && draft.author_kind == ActorKind::AgentSession
            && draft.in_reply_to.is_some()
            && !matches!(
                draft.grammar_version.as_str(),
                "result-set/v1" | "terminal-result/v1"
            );
        let controls = self.thread_control_state(namespace, &draft.thread_id)?;
        if draft.in_reply_to.is_some()
            && controls.stopped
            && !matches!(
                draft.grammar_version.as_str(),
                "result-set/v1" | "terminal-result/v1"
            )
        {
            return Err(StoreError::invalid_input(
                "stopped Collaboration Threads reject new replies",
            ));
        }
        if is_agent_reply && controls.reply_allowance == 0 {
            return Err(StoreError::invalid_input(
                "the Automatic Reply Limit requires human continuation",
            ));
        }

        let thread_order =
            self.next_order(namespace, "thread_message", draft.thread_id.as_str())?;
        self.transaction.execute(
            "INSERT INTO collaboration_messages (
                session_socket_id, worktree_id, message_id, thread_id, thread_order,
                in_reply_to, author_address_id, author_alias_snapshot,
                author_kind, author_session_snapshot, request_id, authored_source, body,
                grammar_version, created_at_ms
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
             )",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.message_id.as_str(),
                draft.thread_id.as_str(),
                to_i64(thread_order)?,
                draft.in_reply_to.as_ref().map(MessageId::as_str),
                draft.author_address_id.as_str(),
                draft.author_alias_snapshot,
                draft.author_kind.as_str(),
                draft
                    .author_agent_session_snapshot
                    .as_ref()
                    .map(crate::AgentSessionId::as_str),
                draft.request_id,
                draft.authored_source,
                draft.body,
                draft.grammar_version,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO idempotency_records (
                session_socket_id, worktree_id, author_address_id, request_id,
                request_fingerprint, message_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.author_address_id.as_str(),
                draft.request_id,
                fingerprint,
                draft.message_id.as_str()
            ],
        )?;
        if let Some(origin) = bridged_from {
            self.record_bridged_message(namespace, &draft.message_id, origin)?;
        }
        for (reference_order, reference) in draft.context_references.iter().enumerate() {
            self.transaction.execute(
                "INSERT INTO context_references (
                    session_socket_id, worktree_id, message_id, reference_order,
                    address_id, alias_snapshot, binding_snapshot, session_snapshot
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    draft.message_id.as_str(),
                    i64::try_from(reference_order + 1).map_err(|_| {
                        StoreError::invalid_input("context reference order overflow")
                    })?,
                    reference.address_id.as_str(),
                    reference.alias_snapshot,
                    reference
                        .pane_binding_snapshot
                        .as_ref()
                        .map(crate::PaneBindingId::as_str),
                    reference
                        .agent_session_snapshot
                        .as_ref()
                        .map(crate::AgentSessionId::as_str)
                ],
            )?;
        }
        self.insert_message_recipients(namespace, draft, thread_order, attribution)?;
        self.append_transition(
            namespace,
            TransitionEntityKind::CollaborationMessage,
            draft.message_id.as_str(),
            TransitionKind::Created,
            attribution,
        )?;
        if is_agent_reply {
            for (peer, peer_thread) in self.paired_threads(namespace, &draft.thread_id)? {
                self.charge_agent_reply(&peer, &peer_thread, attribution)?;
            }
            self.charge_agent_reply(namespace, &draft.thread_id, attribution)?;
        }
        self.message_receipt(namespace, &draft.message_id, false)
    }

    fn insert_message_recipients(
        &mut self,
        namespace: &StateNamespace,
        draft: &MessageDraft,
        thread_order: u64,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.insert_recipient_deliveries(
            namespace,
            &draft.message_id,
            &draft.grammar_version,
            &draft.recipients,
            attribution,
        )?;
        self.forward_to_envoys(namespace, draft, attribution)?;
        let mut participants = vec![draft.author_address_id.clone()];
        participants.extend(
            draft
                .recipients
                .iter()
                .map(|recipient| recipient.address_id.clone()),
        );
        for participant in participants {
            self.ensure_thread_participant(
                namespace,
                &draft.thread_id,
                &participant,
                thread_order > 1,
                attribution,
            )?;
        }
        Ok(())
    }

    fn insert_recipient_deliveries(
        &mut self,
        namespace: &StateNamespace,
        message: &MessageId,
        grammar: &str,
        recipients: &[crate::MessageRecipientDraft],
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let mut recipient_order: i64 = self.transaction.query_row(
            "SELECT COALESCE(MAX(recipient_order), 0) FROM message_recipients
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                message.as_str()
            ],
            |row| row.get(0),
        )?;
        for recipient in recipients {
            recipient_order = recipient_order
                .checked_add(1)
                .ok_or_else(|| StoreError::invalid_input("recipient order overflow"))?;
            self.transaction.execute(
                "INSERT INTO message_recipients (
                    session_socket_id, worktree_id, message_id, recipient_order,
                    address_id, alias_snapshot, session_snapshot
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message.as_str(),
                    recipient_order,
                    recipient.address_id.as_str(),
                    recipient.alias_snapshot,
                    recipient
                        .agent_session_snapshot
                        .as_ref()
                        .map(crate::AgentSessionId::as_str)
                ],
            )?;
            let mailbox_order =
                self.next_order(namespace, "mailbox", recipient.address_id.as_str())?;
            self.transaction.execute(
                "INSERT INTO mailbox_deliveries (
                    session_socket_id, worktree_id, delivery_id, address_id,
                    mailbox_order, message_id, state, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7)",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    recipient.delivery_id.as_str(),
                    recipient.address_id.as_str(),
                    to_i64(mailbox_order)?,
                    message.as_str(),
                    to_i64(attribution.occurred_at_ms)?
                ],
            )?;
            self.append_transition(
                namespace,
                TransitionEntityKind::MailboxDelivery,
                recipient.delivery_id.as_str(),
                TransitionKind::Queued,
                attribution,
            )?;
            if self.is_operator_address(namespace, &recipient.address_id)?
                || (matches!(
                    grammar,
                    "terminal-result/v1" | "result-set/v1" | "mention-return/v1"
                ) && self.pane_type(namespace, &recipient.address_id)?
                    == crate::PaneType::Terminal)
            {
                self.transaction.execute("UPDATE mailbox_deliveries SET state='settled' WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3",
                    params![namespace.session_socket_id,namespace.space_id.as_str(),recipient.delivery_id.as_str()])?;
                self.append_transition(
                    namespace,
                    TransitionEntityKind::MailboxDelivery,
                    recipient.delivery_id.as_str(),
                    TransitionKind::Settled,
                    attribution,
                )?;
            }
        }
        Ok(())
    }

    fn charge_agent_reply(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let changed = self.transaction.execute(
            "UPDATE thread_controls
             SET reply_allowance = reply_allowance - 1,
                 paused = CASE WHEN reply_allowance = 1 THEN 1 ELSE paused END
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
               AND reply_allowance > 0",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input(
                "the Automatic Reply Limit requires human continuation",
            ));
        }
        if self
            .thread_control_state(namespace, thread_id)?
            .reply_limit_paused
        {
            self.append_transition(
                namespace,
                TransitionEntityKind::CollaborationThread,
                thread_id.as_str(),
                TransitionKind::Paused,
                attribution,
            )?;
            self.open_attention_item(
                namespace,
                &AttentionItemDraft {
                    id: crate::AttentionItemId::new(format!(
                        "attention-automatic-reply-limit-{}",
                        thread_id.as_str()
                    )),
                    source_kind: crate::AttentionSourceKind::AutomaticReplyLimit,
                    source_id: thread_id.as_str().to_owned(),
                    summary: "Automatic Reply Limit reached; continue or stop this thread."
                        .to_owned(),
                },
                attribution,
            )?;
        }
        Ok(())
    }

    pub fn thread_control_state(
        &self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
    ) -> Result<crate::ThreadControlState, StoreError> {
        self.transaction
            .query_row(
                "SELECT paused, stopped, reply_allowance
                 FROM thread_controls
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread_id.as_str()
                ],
                |row| {
                    let paused = row.get(0)?;
                    let reply_allowance = from_i64(row.get(2)?)?;
                    Ok(crate::ThreadControlState {
                        paused,
                        stopped: row.get(1)?,
                        reply_allowance,
                        reply_limit_paused: paused && reply_allowance == 0,
                    })
                },
            )
            .map_err(StoreError::from)
    }

    pub fn continue_thread(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadControlState, StoreError> {
        for (peer, peer_thread) in self.paired_threads(namespace, thread_id)? {
            self.continue_thread_here(&peer, &peer_thread, attribution)?;
        }
        self.continue_thread_here(namespace, thread_id, attribution)
    }

    fn continue_thread_here(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadControlState, StoreError> {
        let changed = self.transaction.execute(
            "UPDATE thread_controls
             SET paused = 0, reply_allowance = 10
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
               AND paused = 1 AND stopped = 0 AND reply_allowance = 0",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input(
                "only a thread paused by the Automatic Reply Limit can continue",
            ));
        }
        self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::AutomaticReplyLimit,
            thread_id.as_str(),
            attribution,
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::CollaborationThread,
            thread_id.as_str(),
            TransitionKind::Continued,
            attribution,
        )?;
        self.thread_control_state(namespace, thread_id)
    }

    pub fn pause_thread(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadControlState, StoreError> {
        for (peer, peer_thread) in self.paired_threads(namespace, thread_id)? {
            self.pause_thread_here(&peer, &peer_thread, attribution)?;
        }
        self.pause_thread_here(namespace, thread_id, attribution)
    }

    fn pause_thread_here(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadControlState, StoreError> {
        let current = self.thread_control_state(namespace, thread_id)?;
        if current.stopped {
            return Err(StoreError::invalid_input(
                "a stopped Collaboration Thread cannot be paused",
            ));
        }
        let changed = self.transaction.execute(
            "UPDATE thread_controls SET paused = 1
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
               AND paused = 0",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
        )?;
        if changed == 1 {
            self.append_transition(
                namespace,
                TransitionEntityKind::CollaborationThread,
                thread_id.as_str(),
                TransitionKind::Paused,
                attribution,
            )?;
        }
        self.thread_control_state(namespace, thread_id)
    }

    pub fn resume_thread(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadControlState, StoreError> {
        for (peer, peer_thread) in self.paired_threads(namespace, thread_id)? {
            self.resume_thread_here(&peer, &peer_thread, attribution)?;
        }
        self.resume_thread_here(namespace, thread_id, attribution)
    }

    fn resume_thread_here(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadControlState, StoreError> {
        let current = self.thread_control_state(namespace, thread_id)?;
        if current.stopped {
            return Err(StoreError::invalid_input(
                "a stopped Collaboration Thread cannot resume",
            ));
        }
        if current.reply_limit_paused {
            return Err(StoreError::invalid_input(
                "use thread continue after the Automatic Reply Limit",
            ));
        }
        let changed = self.transaction.execute(
            "UPDATE thread_controls SET paused = 0
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
               AND paused = 1",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
        )?;
        if changed == 1 {
            self.append_transition(
                namespace,
                TransitionEntityKind::CollaborationThread,
                thread_id.as_str(),
                TransitionKind::Resumed,
                attribution,
            )?;
        }
        self.thread_control_state(namespace, thread_id)
    }

    /// Stops the thread and every thread paired with it in another workspace.
    /// The result reports this workspace's side.
    pub fn stop_thread(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadStopResult, StoreError> {
        for (peer, peer_thread) in self.paired_threads(namespace, thread_id)? {
            self.stop_thread_here(&peer, &peer_thread, attribution)?;
        }
        self.stop_thread_here(namespace, thread_id, attribution)
    }

    fn stop_thread_here(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<crate::ThreadStopResult, StoreError> {
        self.thread_control_state(namespace, thread_id)?;
        let active_responses = {
            let mut statement = self.transaction.prepare(
                "SELECT a.attempt_id, b.pane_id, a.agent_session_id, b.active, a.binding_id
                 FROM delivery_attempts a
                 JOIN delivery_attempt_state s USING (
                    session_socket_id, worktree_id, attempt_id, address_id
                 )
                 JOIN mailbox_deliveries d USING (
                    session_socket_id, worktree_id, delivery_id, address_id
                 )
                 JOIN collaboration_messages m USING (
                    session_socket_id, worktree_id, message_id
                 )
                 JOIN pane_bindings b USING (
                    session_socket_id, worktree_id, binding_id, address_id
                 )
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND m.thread_id = ?3 AND s.state IN ('submitted', 'acknowledged')
                   AND s.active = 1",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        thread_id.as_str()
                    ],
                    |row| {
                        Ok(crate::ActiveThreadResponse {
                            attempt_id: crate::DeliveryAttemptId::new(row.get::<_, String>(0)?),
                            pane_id: row.get(1)?,
                            agent_session_id: crate::AgentSessionId::new(row.get::<_, String>(2)?),
                            binding_active: row.get(3)?,
                            binding_id: crate::PaneBindingId::new(row.get::<_, String>(4)?),
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let cancelled = {
            let mut statement = self.transaction.prepare(
                "SELECT d.delivery_id, d.message_id
                 FROM mailbox_deliveries d
                 JOIN collaboration_messages m USING (
                    session_socket_id, worktree_id, message_id
                 )
                 WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
                   AND m.thread_id = ?3 AND d.state = 'queued'
                 ORDER BY d.address_id, d.mailbox_order",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        thread_id.as_str()
                    ],
                    |row| {
                        Ok((
                            crate::DeliveryId::new(row.get::<_, String>(0)?),
                            MessageId::new(row.get::<_, String>(1)?),
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        self.transaction.execute(
            "UPDATE thread_controls SET stopped = 1
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
        )?;
        for (delivery_id, _) in &cancelled {
            self.cancel_unsent_delivery(namespace, delivery_id, attribution)?;
        }
        self.complete_ready_result_sets(namespace, attribution)?;
        self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::AutomaticReplyLimit,
            thread_id.as_str(),
            attribution,
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::CollaborationThread,
            thread_id.as_str(),
            TransitionKind::Stopped,
            attribution,
        )?;
        Ok(crate::ThreadStopResult {
            state: self.thread_control_state(namespace, thread_id)?,
            cancelled_delivery_ids: cancelled
                .into_iter()
                .map(|(delivery_id, _)| delivery_id)
                .collect(),
            active_responses,
        })
    }

    pub fn cancel_sender_delivery(
        &mut self,
        namespace: &StateNamespace,
        sender_address_id: &PaneAddressId,
        delivery_id: &crate::DeliveryId,
        attribution: &MutationAttribution,
    ) -> Result<DeliveryState, StoreError> {
        let own_queued: bool = self.transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM mailbox_deliveries d
                JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
                WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
                  AND d.delivery_id = ?3 AND d.state = 'queued' AND m.author_address_id = ?4
             )",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery_id.as_str(),
                sender_address_id.as_str()
            ],
            |row| row.get(0),
        )?;
        if !own_queued || !self.cancel_unsent_delivery(namespace, delivery_id, attribution)? {
            return Err(StoreError::invalid_input(
                "a sender may cancel only its own queued per-recipient delivery",
            ));
        }
        Ok(DeliveryState::Cancelled)
    }

    /// The one way a delivery that never reached its recipient is cancelled.
    /// A forwarded copy's cancel is carried to the other workspace in the same
    /// transaction, so neither side shows work the other already dropped.
    fn cancel_unsent_delivery(
        &mut self,
        namespace: &StateNamespace,
        delivery_id: &crate::DeliveryId,
        attribution: &MutationAttribution,
    ) -> Result<bool, StoreError> {
        let changed = self.transaction.execute(
            "UPDATE mailbox_deliveries SET state = 'cancelled'
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND delivery_id = ?3
               AND state IN ('queued', 'held')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery_id.as_str()
            ],
        )? == 1;
        if changed {
            self.append_transition(
                namespace,
                TransitionEntityKind::MailboxDelivery,
                delivery_id.as_str(),
                TransitionKind::Cancelled,
                attribution,
            )?;
            self.complete_result_set_for_delivery(namespace, delivery_id, attribution)?;
        }
        Ok(changed)
    }

    pub fn cancel_held_delivery(
        &mut self,
        namespace: &StateNamespace,
        delivery_id: &crate::DeliveryId,
        attribution: &MutationAttribution,
    ) -> Result<DeliveryState, StoreError> {
        let attempt_id = self
            .transaction
            .query_row(
                "SELECT a.attempt_id
                 FROM delivery_attempts a
                 JOIN delivery_attempt_state s USING (
                     session_socket_id, worktree_id, attempt_id, address_id
                 )
                 JOIN mailbox_deliveries d USING (
                     session_socket_id, worktree_id, delivery_id, address_id
                 )
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.delivery_id = ?3 AND s.state = 'held' AND d.state = 'held'
                 ORDER BY a.rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    delivery_id.as_str()
                ],
                |row| row.get::<_, String>(0).map(crate::DeliveryAttemptId::new),
            )
            .optional()?;
        if let Some(attempt_id) = attempt_id {
            self.transition_delivery_attempt(
                namespace,
                &attempt_id,
                DeliveryState::Cancelled,
                attribution,
            )?;
        } else {
            self.require_unsent_held_delivery(namespace, delivery_id)?;
            if !self.cancel_unsent_delivery(namespace, delivery_id, attribution)? {
                return Err(StoreError::invalid_input(
                    "only a held Delivery Attempt or unsent held delivery may be cancelled",
                ));
            }
            self.resolve_failed_delivery_attention(
                namespace,
                delivery_id.as_str(),
                None,
                attribution,
            )?;
        }
        Ok(DeliveryState::Cancelled)
    }

    pub(crate) fn reply_limit_blocked_mailbox_addresses(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<PaneAddressId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT d.address_id
             FROM mailbox_deliveries d
             JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
             JOIN thread_controls c USING (session_socket_id, worktree_id, thread_id)
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
               AND d.state = 'queued' AND c.paused = 1 AND c.stopped = 0
               AND c.reply_allowance = 0
               AND NOT EXISTS (
                   SELECT 1 FROM mailbox_deliveries earlier
                   WHERE earlier.session_socket_id = d.session_socket_id
                     AND earlier.worktree_id = d.worktree_id
                     AND earlier.address_id = d.address_id
                     AND earlier.mailbox_order < d.mailbox_order
                     AND earlier.state NOT IN ('settled', 'failed', 'cancelled')
               )",
        )?;
        statement
            .query_map(namespace_params(namespace), |row| {
                row.get::<_, String>(0).map(PaneAddressId::new)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn held_deliveries(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<HeldDelivery>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.attempt_id, d.delivery_id, d.address_id, r.alias_snapshot,
                    d.message_id, m.thread_id
             FROM mailbox_deliveries d
             JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
             JOIN message_recipients r USING (session_socket_id, worktree_id, message_id, address_id)
             LEFT JOIN delivery_attempts a ON a.rowid = (
                 SELECT MAX(latest.rowid) FROM delivery_attempts latest
                 WHERE latest.session_socket_id = d.session_socket_id
                   AND latest.worktree_id = d.worktree_id AND latest.delivery_id = d.delivery_id
             ) AND EXISTS (
                 SELECT 1 FROM delivery_attempt_state s
                 WHERE s.session_socket_id = a.session_socket_id AND s.worktree_id = a.worktree_id
                   AND s.attempt_id = a.attempt_id AND s.state = 'held'
             )
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2 AND d.state = 'held'
             ORDER BY d.mailbox_order",
        )?;
        statement
            .query_map(namespace_params(namespace), |row| {
                Ok(HeldDelivery {
                    attempt_id: row
                        .get::<_, Option<String>>(0)?
                        .map(crate::DeliveryAttemptId::new),
                    delivery_id: crate::DeliveryId::new(row.get::<_, String>(1)?),
                    address_id: PaneAddressId::new(row.get::<_, String>(2)?),
                    alias: row.get(3)?,
                    message_id: MessageId::new(row.get::<_, String>(4)?),
                    thread_id: ThreadId::new(row.get::<_, String>(5)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn cancel_unsent_held_delivery(
        &mut self,
        namespace: &StateNamespace,
        delivery_id: &crate::DeliveryId,
        attribution: &MutationAttribution,
    ) -> Result<DeliveryState, StoreError> {
        self.require_unsent_held_delivery(namespace, delivery_id)?;
        self.cancel_held_delivery(namespace, delivery_id, attribution)
    }

    fn require_unsent_held_delivery(
        &self,
        namespace: &StateNamespace,
        delivery_id: &crate::DeliveryId,
    ) -> Result<(), StoreError> {
        // A Failed state alone cannot prove the host was never invoked. The
        // immutable Failed -> Queued fact pair is written only by the explicit
        // pre-invocation requeue path; require it for every historical attempt.
        let unsent = self.transaction.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM mailbox_deliveries d
                 WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
                   AND d.delivery_id = ?3 AND d.state = 'held'
                   AND NOT EXISTS (
                       SELECT 1 FROM delivery_attempts a
                       LEFT JOIN delivery_attempt_state s USING (session_socket_id, worktree_id, attempt_id, address_id)
                       WHERE a.session_socket_id = d.session_socket_id
                         AND a.worktree_id = d.worktree_id AND a.delivery_id = d.delivery_id
                         AND (s.state IS NOT 'failed' OR NOT EXISTS (
                             SELECT 1 FROM transition_facts failed
                             JOIN transition_facts queued ON queued.session_socket_id = failed.session_socket_id
                               AND queued.worktree_id = failed.worktree_id
                               AND queued.worktree_order = failed.worktree_order + 1
                             WHERE failed.session_socket_id = a.session_socket_id
                               AND failed.worktree_id = a.worktree_id
                               AND failed.entity_kind = 'delivery_attempt' AND failed.entity_id = a.attempt_id
                               AND failed.transition_kind = 'failed'
                               AND queued.entity_kind = 'mailbox_delivery' AND queued.entity_id = d.delivery_id
                               AND queued.transition_kind = 'queued'
                         ))
                   )
             )",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery_id.as_str()
            ],
            |row| row.get::<_, bool>(0),
        )?;
        if !unsent {
            return Err(StoreError::invalid_input(
                "delivery_id cancellation requires a held delivery proven never invoked; use the exact held attempt_id for uncertain work",
            ));
        }
        Ok(())
    }

    pub(crate) fn cancel_held_attempt(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        attribution: &MutationAttribution,
    ) -> Result<DeliveryState, StoreError> {
        let delivery_id = self
            .transaction
            .query_row(
                "SELECT a.delivery_id FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             JOIN mailbox_deliveries d USING (
                 session_socket_id, worktree_id, delivery_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3
               AND s.state = 'held' AND d.state = 'held'
               AND NOT EXISTS (
                   SELECT 1 FROM delivery_attempts newer
                   WHERE newer.session_socket_id = a.session_socket_id
                     AND newer.worktree_id = a.worktree_id AND newer.delivery_id = a.delivery_id
                     AND newer.rowid > a.rowid
               )",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |row| row.get::<_, String>(0).map(crate::DeliveryId::new),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("only the current held Delivery Attempt may be cancelled")
            })?;
        self.cancel_held_delivery(namespace, &delivery_id, attribution)
    }

    fn ensure_thread_participant(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        address_id: &PaneAddressId,
        publish_snapshot: bool,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let exists = self.transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM thread_participants
                WHERE session_socket_id = ?1 AND worktree_id = ?2
                  AND thread_id = ?3 AND address_id = ?4
             )",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str(),
                address_id.as_str(),
            ],
            |row| row.get::<_, bool>(0),
        )?;
        if exists {
            return Ok(());
        }
        let participant_order =
            self.next_order(namespace, "thread_participant", thread_id.as_str())?;
        self.transaction.execute(
            "INSERT INTO thread_participants (
                session_socket_id, worktree_id, thread_id, participant_order, address_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str(),
                to_i64(participant_order)?,
                address_id.as_str(),
            ],
        )?;
        if !publish_snapshot {
            return Ok(());
        }

        let participant_aliases = {
            let mut statement = self.transaction.prepare(
                "SELECT c.alias
                 FROM thread_participants p
                 JOIN alias_claims c
                   ON c.session_socket_id = p.session_socket_id
                  AND c.worktree_id = p.worktree_id
                  AND c.address_id = p.address_id
                  AND c.claim_state = 'active'
                 WHERE p.session_socket_id = ?1 AND p.worktree_id = ?2
                   AND p.thread_id = ?3
                 ORDER BY p.participant_order
                 LIMIT 64",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        thread_id.as_str(),
                    ],
                    |row| {
                        row.get::<_, String>(0)
                            .map(|alias| bounded_redacted(&alias, 100))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let snapshot_id = ContextSnapshotId::new(format!(
            "thread-context:{}:{}",
            thread_id.as_str(),
            address_id.as_str()
        ));
        let title = self.transaction.query_row(
            "SELECT title FROM collaboration_threads
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
            |row| row.get::<_, String>(0),
        )?;
        let prior_messages = {
            let mut statement = self.transaction.prepare(
                "SELECT body FROM collaboration_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
                 ORDER BY thread_order DESC LIMIT 20",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        thread_id.as_str()
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let decisions = prior_messages
            .iter()
            .rev()
            .flat_map(|body| body.lines())
            .filter_map(|line| line.strip_prefix("Decision:"))
            .take(10)
            .map(|line| bounded_redacted(line.trim(), 300))
            .collect::<Vec<_>>();
        let current_state = prior_messages
            .first()
            .map_or_else(String::new, |body| bounded_redacted(body, 500));
        let evidence = {
            let mut statement = self.transaction.prepare(
                "SELECT DISTINCT r.address_id, r.alias_snapshot
                 FROM context_references r
                 JOIN collaboration_messages m
                   ON m.session_socket_id = r.session_socket_id
                  AND m.worktree_id = r.worktree_id
                  AND m.message_id = r.message_id
                 WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2
                   AND m.thread_id = ?3
                 ORDER BY r.address_id LIMIT 20",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        thread_id.as_str()
                    ],
                    |row| {
                        Ok(serde_json::json!({
                            "address_id": row.get::<_, String>(0)?,
                            "alias": row.get::<_, String>(1)?,
                        }))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let payload = serde_json::json!({
            "objective": bounded_redacted(&title, 500),
            "decisions": decisions,
            "current_state": current_state,
            "participants": participant_aliases,
            "evidence": evidence,
        });
        self.transaction.execute(
            "INSERT INTO context_snapshot_catalog (
                session_socket_id, worktree_id, snapshot_id, snapshot_kind
             ) VALUES (?1, ?2, ?3, 'thread')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                snapshot_id.as_str(),
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO thread_context_snapshots (
                session_socket_id, worktree_id, snapshot_id, thread_id,
                participant_address_id, payload_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                snapshot_id.as_str(),
                thread_id.as_str(),
                address_id.as_str(),
                serde_json::to_string(&payload)
                    .map_err(|error| StoreError::invalid_input(error.to_string()))?,
                to_i64(attribution.occurred_at_ms)?,
            ],
        )?;
        Ok(())
    }

    pub fn create_context_grant(
        &mut self,
        namespace: &StateNamespace,
        draft: &ContextGrantDraft,
        attribution: &MutationAttribution,
    ) -> Result<ContextGrantRecord, StoreError> {
        for address_id in [&draft.source_address_id, &draft.grantee_address_id] {
            let is_participant = self.transaction.query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM thread_participants
                    WHERE session_socket_id = ?1 AND worktree_id = ?2
                      AND thread_id = ?3 AND address_id = ?4
                 )",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    draft.thread_id.as_str(),
                    address_id.as_str(),
                ],
                |row| row.get::<_, bool>(0),
            )?;
            if !is_participant {
                return Err(StoreError::invalid_input(
                    "Context Grants may name only current Collaboration Thread participants",
                ));
            }
        }
        if draft.source_address_id == draft.grantee_address_id {
            return Err(StoreError::invalid_input(
                "a Context Grant source and grantee must differ",
            ));
        }
        let current = self.routing_address_by_id(namespace, &draft.source_address_id)?;
        if current.pane_binding_id.as_ref() != Some(&draft.pane_binding_id)
            || current.agent_session_id.as_ref() != Some(&draft.agent_session_id)
        {
            return Err(StoreError::invalid_input(
                "Context Grant identity no longer matches the current binding and Agent Session",
            ));
        }
        self.transaction.execute(
            "INSERT INTO context_grants (
                session_socket_id, worktree_id, grant_id, thread_id,
                source_address_id, grantee_address_id, agent_session_id, state,
                pane_binding_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.id.as_str(),
                draft.thread_id.as_str(),
                draft.source_address_id.as_str(),
                draft.grantee_address_id.as_str(),
                draft.agent_session_id.as_str(),
                draft.pane_binding_id.as_str(),
            ],
        )?;
        let _ = attribution;
        self.context_grant(namespace, &draft.id)
    }

    pub fn context_grant(
        &self,
        namespace: &StateNamespace,
        grant_id: &ContextGrantId,
    ) -> Result<ContextGrantRecord, StoreError> {
        self.transaction
            .query_row(
                "SELECT g.thread_id, g.source_address_id, g.grantee_address_id,
                        g.pane_binding_id, g.agent_session_id, g.state, s.snapshot_id
                 FROM context_grants g
                 LEFT JOIN working_context_snapshots s
                   ON s.session_socket_id = g.session_socket_id
                  AND s.worktree_id = g.worktree_id
                  AND s.grant_id = g.grant_id
                 WHERE g.session_socket_id = ?1 AND g.worktree_id = ?2
                   AND g.grant_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    grant_id.as_str(),
                ],
                |row| {
                    Ok(ContextGrantRecord {
                        grant_id: grant_id.clone(),
                        thread_id: ThreadId::new(row.get::<_, String>(0)?),
                        source_address_id: PaneAddressId::new(row.get::<_, String>(1)?),
                        grantee_address_id: PaneAddressId::new(row.get::<_, String>(2)?),
                        pane_binding_id: row
                            .get::<_, Option<String>>(3)?
                            .map(crate::PaneBindingId::new),
                        agent_session_id: crate::AgentSessionId::new(row.get::<_, String>(4)?),
                        state: parse_context_grant_state(row.get::<_, String>(5)?)?,
                        snapshot_id: row.get::<_, Option<String>>(6)?.map(ContextSnapshotId::new),
                    })
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::invalid_input("Context Grant does not exist"))
    }

    /// Every Context Grant's identity and state, for diagnostic evidence: id
    /// order keeps the projection deterministic, and no snapshot content is
    /// carried.
    pub fn context_grant_evidence(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT grant_id, thread_id, source_address_id, grantee_address_id, state
             FROM context_grants
             WHERE session_socket_id = ?1 AND worktree_id = ?2
             ORDER BY grant_id",
        )?;
        let grants = statement
            .query_map(
                params![namespace.session_socket_id, namespace.space_id.as_str()],
                |row| {
                    Ok(serde_json::json!({
                        "grant_id": row.get::<_, String>(0)?,
                        "thread_id": row.get::<_, String>(1)?,
                        "source_address_id": row.get::<_, String>(2)?,
                        "grantee_address_id": row.get::<_, String>(3)?,
                        "state": row.get::<_, String>(4)?,
                    }))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(grants)
    }

    pub fn refresh_context_grant(
        &mut self,
        namespace: &StateNamespace,
        grant_id: &ContextGrantId,
    ) -> Result<ContextGrantRecord, StoreError> {
        let grant = self.context_grant(namespace, grant_id)?;
        if !matches!(
            grant.state,
            ContextGrantState::Pending | ContextGrantState::Active
        ) {
            return Ok(grant);
        }
        let current = self.routing_address_by_id(namespace, &grant.source_address_id)?;
        if grant.pane_binding_id.is_some()
            && current.pane_binding_id == grant.pane_binding_id
            && current.agent_session_id.as_ref() == Some(&grant.agent_session_id)
        {
            return Ok(grant);
        }
        self.transaction.execute(
            "UPDATE context_grants SET state = 'expired'
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND grant_id = ?3 AND state IN ('pending', 'active')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                grant_id.as_str(),
            ],
        )?;
        self.context_grant(namespace, grant_id)
    }

    pub fn latest_context_grant(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        source_address_id: &PaneAddressId,
        grantee_address_id: &PaneAddressId,
    ) -> Result<Option<ContextGrantRecord>, StoreError> {
        let grant_id = self
            .transaction
            .query_row(
                "SELECT grant_id FROM context_grants
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND thread_id = ?3 AND source_address_id = ?4
                   AND grantee_address_id = ?5
                 ORDER BY rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread_id.as_str(),
                    source_address_id.as_str(),
                    grantee_address_id.as_str(),
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        grant_id
            .map(|grant_id| self.refresh_context_grant(namespace, &ContextGrantId::new(grant_id)))
            .transpose()
    }

    pub fn latest_context_grant_for_source(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        source_address_id: &PaneAddressId,
    ) -> Result<Option<ContextGrantRecord>, StoreError> {
        let grant_id = self
            .transaction
            .query_row(
                "SELECT grant_id FROM context_grants
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND thread_id = ?3 AND source_address_id = ?4
                 ORDER BY CASE state
                     WHEN 'active' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END,
                     rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread_id.as_str(),
                    source_address_id.as_str(),
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        grant_id
            .map(|grant_id| self.refresh_context_grant(namespace, &ContextGrantId::new(grant_id)))
            .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn activate_context_grant(
        &mut self,
        namespace: &StateNamespace,
        grant_id: &ContextGrantId,
        source_address_id: &PaneAddressId,
        agent_session_id: &crate::AgentSessionId,
        snapshot_id: &ContextSnapshotId,
        payload: &serde_json::Value,
        evidence: &[EvidencePointer],
        attribution: &MutationAttribution,
    ) -> Result<ContextGrantRecord, StoreError> {
        let grant = self.refresh_context_grant(namespace, grant_id)?;
        if grant.state != ContextGrantState::Pending {
            return Err(StoreError::invalid_input(
                "only a pending Context Grant can be activated",
            ));
        }
        if grant.source_address_id != *source_address_id
            || grant.agent_session_id != *agent_session_id
        {
            return Err(StoreError::invalid_input(
                "snapshot publication must come from the Context Grant's exact Agent Session",
            ));
        }
        self.transaction.execute(
            "INSERT INTO context_snapshot_catalog (
                session_socket_id, worktree_id, snapshot_id, snapshot_kind
             ) VALUES (?1, ?2, ?3, 'working')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                snapshot_id.as_str(),
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO working_context_snapshots (
                session_socket_id, worktree_id, snapshot_id, grant_id,
                agent_session_id, payload_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                snapshot_id.as_str(),
                grant_id.as_str(),
                agent_session_id.as_str(),
                serde_json::to_string(payload)
                    .map_err(|error| StoreError::invalid_input(error.to_string()))?,
                to_i64(attribution.occurred_at_ms)?,
            ],
        )?;
        for (index, pointer) in evidence.iter().enumerate() {
            self.transaction.execute(
                "INSERT INTO evidence_index (
                    session_socket_id, worktree_id, snapshot_id,
                    evidence_order, evidence_kind, locator
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    snapshot_id.as_str(),
                    i64::try_from(index + 1)
                        .map_err(|_| { StoreError::invalid_input("evidence order overflow") })?,
                    pointer.kind.as_str(),
                    pointer.locator,
                ],
            )?;
        }
        self.transaction.execute(
            "UPDATE context_grants SET state = 'active'
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND grant_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                grant_id.as_str(),
            ],
        )?;
        self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::ContextGrant,
            grant_id.as_str(),
            attribution,
        )?;
        self.context_grant(namespace, grant_id)
    }

    /// Records which Context Grant a send-with-context created for one exact
    /// recipient of one Collaboration Message, so that recipient's delivery
    /// envelope can carry its grant id as `attached_context`.
    pub fn record_message_context_attachment(
        &mut self,
        namespace: &StateNamespace,
        message_id: &crate::MessageId,
        address_id: &PaneAddressId,
        grant_id: &ContextGrantId,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO message_context_attachments (
                session_socket_id, worktree_id, message_id, address_id, grant_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                message_id.as_str(),
                address_id.as_str(),
                grant_id.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Records which pending Context Grant a reply's `request_context` created
    /// against one exact recipient of that reply — the disposed delivery's
    /// sender — so the caller's delivery envelope can carry the grant id as
    /// `context_request` with a pre-bound fulfillment reply.
    pub fn record_message_context_request(
        &mut self,
        namespace: &StateNamespace,
        message_id: &crate::MessageId,
        address_id: &PaneAddressId,
        grant_id: &ContextGrantId,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO message_context_requests (
                session_socket_id, worktree_id, message_id, address_id, grant_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                message_id.as_str(),
                address_id.as_str(),
                grant_id.as_str(),
            ],
        )?;
        Ok(())
    }

    /// The exact pending grant a message asked one recipient to fulfill. This
    /// is also the authorization seam for the otherwise terminal
    /// report_to_human leg of a one-shot human relay.
    pub(crate) fn answers_report_to_human_context_request(
        &self,
        namespace: &StateNamespace,
        message_id: Option<&crate::MessageId>,
        reply_mode: Option<crate::DeliveryReplyMode>,
        address_id: &PaneAddressId,
        fulfillment_grant_ids: &[String],
        decline_context_request: Option<&ContextGrantId>,
    ) -> Result<bool, StoreError> {
        let Some(message_id) =
            message_id.filter(|_| reply_mode == Some(crate::DeliveryReplyMode::ReportToHuman))
        else {
            return Ok(false);
        };
        let grant_id = self
            .transaction
            .query_row(
                "SELECT grant_id FROM message_context_requests
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND message_id = ?3 AND address_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str(),
                    address_id.as_str(),
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(grant_id) = grant_id else {
            return Ok(false);
        };
        Ok(fulfillment_grant_ids == [grant_id.as_str()]
            || (fulfillment_grant_ids.is_empty()
                && decline_context_request.is_some_and(|declined| declined.as_str() == grant_id)))
    }

    /// Settles a context request by refusal: Pending -> Revoked, recorded with
    /// the declining Agent Session's attribution so the requester can tell a
    /// decline from an expiry. Only the request's exact source may decline, and
    /// only while the request is still pending.
    pub fn decline_context_grant(
        &mut self,
        namespace: &StateNamespace,
        grant_id: &ContextGrantId,
        decliner_address_id: &PaneAddressId,
        decliner_session_id: &crate::AgentSessionId,
        attribution: &MutationAttribution,
    ) -> Result<ContextGrantRecord, StoreError> {
        let grant = self.refresh_context_grant(namespace, grant_id)?;
        if grant.source_address_id != *decliner_address_id
            || grant.agent_session_id != *decliner_session_id
        {
            return Err(StoreError::invalid_input(
                "a context request may be declined only by its exact source Agent Session",
            ));
        }
        if grant.state != ContextGrantState::Pending {
            return Err(StoreError::invalid_input(
                "only a pending context request can be declined",
            ));
        }
        self.transaction.execute(
            "UPDATE context_grants SET state = 'revoked'
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND grant_id = ?3 AND state = 'pending'",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                grant_id.as_str(),
            ],
        )?;
        self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::ContextGrant,
            grant_id.as_str(),
            attribution,
        )?;
        self.context_grant(namespace, grant_id)
    }

    pub fn record_context_grant_activation_failure(
        &mut self,
        namespace: &StateNamespace,
        grant_id: &ContextGrantId,
        summary: &str,
        attribution: &MutationAttribution,
    ) -> Result<crate::AttentionItemId, StoreError> {
        let grant = self.context_grant(namespace, grant_id)?;
        if grant.state != ContextGrantState::Pending {
            return Err(StoreError::invalid_input(
                "only a pending Context Grant can have an activation failure",
            ));
        }
        self.open_attention_item(
            namespace,
            &AttentionItemDraft {
                id: crate::AttentionItemId::new(format!(
                    "attention-context-grant-{}",
                    grant_id.as_str()
                )),
                source_kind: crate::AttentionSourceKind::ContextGrant,
                source_id: grant_id.as_str().to_owned(),
                summary: summary.to_owned(),
            },
            attribution,
        )
    }

    pub fn revoke_context_grant(
        &mut self,
        namespace: &StateNamespace,
        grant_id: &ContextGrantId,
        attribution: &MutationAttribution,
    ) -> Result<ContextGrantRecord, StoreError> {
        let changed = self.transaction.execute(
            "UPDATE context_grants SET state = 'revoked'
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND grant_id = ?3 AND state IN ('pending', 'active')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                grant_id.as_str(),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::invalid_input(
                "Context Grant does not exist or is already ended",
            ));
        }
        self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::ContextGrant,
            grant_id.as_str(),
            attribution,
        )?;
        self.context_grant(namespace, grant_id)
    }

    /// The states of every Context Grant whose stored snapshot names one
    /// git-state pin, refreshed first so lazily expired grants report terminal
    /// before a pin-ref sweep judges them. Deliberately unscoped by namespace:
    /// a pin ref in the repository may belong to any namespace this store holds.
    pub fn refresh_context_grants_for_git_pin(
        &mut self,
        pin_id: &str,
    ) -> Result<Vec<ContextGrantState>, StoreError> {
        let rows = {
            let mut statement = self.transaction.prepare(
                "SELECT g.session_socket_id, g.worktree_id, g.grant_id
                 FROM context_grants g
                 JOIN working_context_snapshots s
                   ON s.session_socket_id = g.session_socket_id
                  AND s.worktree_id = g.worktree_id
                  AND s.grant_id = g.grant_id
                 WHERE json_extract(s.payload_json, '$.git_state.pin_id') = ?1",
            )?;
            statement
                .query_map(params![pin_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        rows.into_iter()
            .map(|(session_socket_id, space_id, grant_id)| {
                let namespace = StateNamespace::for_space(session_socket_id, space_id);
                Ok(self
                    .refresh_context_grant(&namespace, &ContextGrantId::new(grant_id))?
                    .state)
            })
            .collect()
    }

    pub fn context_reference(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
        source_address_id: &PaneAddressId,
    ) -> Result<ContextReferenceRecord, StoreError> {
        self.transaction
            .query_row(
                "SELECT m.thread_id, r.alias_snapshot, r.binding_snapshot,
                        r.session_snapshot
                 FROM context_references r
                 JOIN collaboration_messages m
                   ON m.session_socket_id = r.session_socket_id
                  AND m.worktree_id = r.worktree_id
                  AND m.message_id = r.message_id
                 WHERE r.session_socket_id = ?1 AND r.worktree_id = ?2
                   AND r.message_id = ?3 AND r.address_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str(),
                    source_address_id.as_str(),
                ],
                |row| {
                    Ok(ContextReferenceRecord {
                        message_id: message_id.clone(),
                        thread_id: ThreadId::new(row.get::<_, String>(0)?),
                        source_address_id: source_address_id.clone(),
                        alias_snapshot: row.get(1)?,
                        pane_binding_snapshot: row
                            .get::<_, Option<String>>(2)?
                            .map(crate::PaneBindingId::new),
                        agent_session_snapshot: row
                            .get::<_, Option<String>>(3)?
                            .map(crate::AgentSessionId::new),
                    })
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("the message does not contain that Context Reference")
            })
    }

    pub fn evidence_index(
        &self,
        namespace: &StateNamespace,
        snapshot_id: &ContextSnapshotId,
    ) -> Result<Vec<EvidencePointer>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT evidence_kind, locator FROM evidence_index
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND snapshot_id = ?3
             ORDER BY evidence_order",
        )?;
        statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    snapshot_id.as_str(),
                ],
                |row| {
                    let kind = row.get::<_, String>(0)?;
                    Ok(EvidencePointer {
                        kind: EvidenceKind::parse(&kind).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(0, Type::Text, error.into())
                        })?,
                        locator: row.get(1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn working_context_snapshot(
        &self,
        namespace: &StateNamespace,
        snapshot_id: &ContextSnapshotId,
    ) -> Result<serde_json::Value, StoreError> {
        let payload = self
            .transaction
            .query_row(
                "SELECT payload_json FROM working_context_snapshots
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND snapshot_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    snapshot_id.as_str(),
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::invalid_input("Working Context Snapshot does not exist"))?;
        serde_json::from_str(&payload).map_err(|error| StoreError::invalid_input(error.to_string()))
    }

    pub fn thread_context_snapshot(
        &self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        participant_address_id: &PaneAddressId,
    ) -> Result<ThreadContextSnapshotRecord, StoreError> {
        self.transaction
            .query_row(
                "SELECT snapshot_id, payload_json FROM thread_context_snapshots
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND thread_id = ?3 AND participant_address_id = ?4
                 ORDER BY rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread_id.as_str(),
                    participant_address_id.as_str(),
                ],
                |row| {
                    let payload = row.get::<_, String>(1)?;
                    Ok(ThreadContextSnapshotRecord {
                        snapshot_id: ContextSnapshotId::new(row.get::<_, String>(0)?),
                        thread_id: thread_id.clone(),
                        participant_address_id: participant_address_id.clone(),
                        payload: serde_json::from_str(&payload).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(1, Type::Text, error.into())
                        })?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("no Thread Context Snapshot exists for this participant")
            })
    }

    pub fn is_thread_participant(
        &self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        address_id: &PaneAddressId,
    ) -> Result<bool, StoreError> {
        self.transaction
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM thread_participants
                    WHERE session_socket_id = ?1 AND worktree_id = ?2
                      AND thread_id = ?3 AND address_id = ?4
                 )",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread_id.as_str(),
                    address_id.as_str(),
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub fn record_context_read(
        &mut self,
        namespace: &StateNamespace,
        grant_id: &ContextGrantId,
        reader_address_id: Option<&PaneAddressId>,
        evidence_locator: &str,
        attribution: &MutationAttribution,
    ) -> Result<u64, StoreError> {
        let audit_order = self.next_order(namespace, "context_read", "worktree")?;
        self.transaction.execute(
            "INSERT INTO context_read_audit (
                session_socket_id, worktree_id, audit_order, grant_id,
                reader_address_id, actor_kind, actor_id, evidence_locator, occurred_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                to_i64(audit_order)?,
                grant_id.as_str(),
                reader_address_id.map(PaneAddressId::as_str),
                attribution.actor_kind.as_str(),
                attribution.actor_id,
                evidence_locator,
                to_i64(attribution.occurred_at_ms)?,
            ],
        )?;
        Ok(audit_order)
    }

    pub(crate) fn queued_mailbox_addresses(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<PaneAddressId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT d.address_id
             FROM mailbox_deliveries d
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
               AND d.state = 'queued'
               AND NOT EXISTS (
                   SELECT 1
                   FROM mailbox_deliveries earlier
                   WHERE earlier.session_socket_id = d.session_socket_id
                     AND earlier.worktree_id = d.worktree_id
                     AND earlier.address_id = d.address_id
                     AND earlier.mailbox_order < d.mailbox_order
                     AND earlier.state NOT IN ('settled', 'failed', 'cancelled')
               )
             ORDER BY d.address_id",
        )?;
        statement
            .query_map(namespace_params(namespace), |row| {
                row.get::<_, String>(0).map(PaneAddressId::new)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn hold_expired_queued_deliveries(
        &mut self,
        namespace: &StateNamespace,
        cutoff_ms: u64,
        attribution: &MutationAttribution,
    ) -> Result<Vec<crate::DeliveryId>, StoreError> {
        let cutoff_ms = to_i64(cutoff_ms)?;
        let delivery_ids = {
            let mut statement = self.transaction.prepare(
                "SELECT delivery_id
                 FROM mailbox_deliveries
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND state = 'queued' AND created_at_ms <= ?3
                 ORDER BY address_id, mailbox_order",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        cutoff_ms
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for delivery_id in &delivery_ids {
            self.transaction.execute(
                "UPDATE mailbox_deliveries SET state = 'held'
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND delivery_id = ?3 AND state = 'queued'",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    delivery_id
                ],
            )?;
            self.append_transition(
                namespace,
                TransitionEntityKind::MailboxDelivery,
                delivery_id,
                TransitionKind::Held,
                attribution,
            )?;
        }
        Ok(delivery_ids
            .into_iter()
            .map(crate::DeliveryId::new)
            .collect())
    }

    pub(crate) fn inflight_delivery_attempts(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<InflightDeliveryAttempt>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.attempt_id, a.address_id, a.binding_id, s.state, b.active,
                    b.pane_id, a.agent_session_id, a.created_at_ms, l.expires_at_ms
             FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             JOIN pane_bindings b USING (
                 session_socket_id, worktree_id, binding_id, address_id
             )
             LEFT JOIN leases l USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND s.active = 1
             ORDER BY a.attempt_id",
        )?;
        statement
            .query_map(namespace_params(namespace), |row| {
                Ok(InflightDeliveryAttempt {
                    attempt_id: crate::DeliveryAttemptId::new(row.get::<_, String>(0)?),
                    address_id: crate::PaneAddressId::new(row.get::<_, String>(1)?),
                    binding_id: crate::PaneBindingId::new(row.get::<_, String>(2)?),
                    state: parse_delivery_state(row.get::<_, String>(3)?)?,
                    binding_active: row.get(4)?,
                    pane_id: row.get(5)?,
                    agent_session_id: crate::AgentSessionId::new(row.get::<_, String>(6)?),
                    created_at_ms: from_i64(row.get(7)?)?,
                    lease_expires_at_ms: row.get::<_, Option<i64>>(8)?.map(from_i64).transpose()?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn dispatch_candidate(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
    ) -> Result<Option<DispatchCandidate>, StoreError> {
        let state = self.worktree_state(namespace)?;
        if !state.is_some_and(|state| state.activated && !state.paused && !state.deactivated) {
            return Ok(None);
        }
        if !self.reconciliation_completed(namespace)? {
            return Ok(None);
        }
        let Some(address) = self
            .list_pane_addresses(namespace)?
            .into_iter()
            .find(|address| &address.id == address_id)
        else {
            return Ok(None);
        };
        // Settled status is checked against two live host snapshots by the
        // dispatcher; a saved status observation can lag behind either one.
        if address.collaboration_disabled
            || address.pane_id.is_none()
            || (address.pane_type == crate::PaneType::Agent
                && !matches!(address.lifecycle, crate::PaneAddressLifecycle::Occupied))
        {
            return Ok(None);
        }
        let (agent_session_id, registration_id) = if address.pane_type == crate::PaneType::Terminal
        {
            // Empty denotes no Agent Session, as in occupant observations.
            // Terminal execution is instead pinned by terminal_executions.
            (crate::AgentSessionId::new(""), String::new())
        } else {
            let agent_session_id = address.agent_session_id.ok_or_else(|| {
                StoreError::invalid_input("occupied Pane Address has no Agent Session")
            })?;
            let Some(registration) =
                self.active_integration_registration(namespace, address_id, &agent_session_id)?
            else {
                return Ok(None);
            };
            // Package readiness gates delivery, not only challenge issuance. A
            // runtime whose package could not be installed is not a delivery
            // target even when a registration taken against an earlier, healthy
            // package is still active: its queued work waits for the refresh that
            // resolves the Attention Item the failure opened.
            if self.integration_package_failed(namespace, registration.runtime)? {
                return Ok(None);
            }
            let registration_id = registration.registration_id;
            (agent_session_id, registration_id)
        };
        let binding = self
            .transaction
            .query_row(
                "SELECT binding_id, pane_id
                 FROM pane_bindings
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3 AND active = 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str()
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((binding_id, pane_id)) = binding else {
            return Ok(None);
        };
        let head_delivery_id = self
            .transaction
            .query_row(
                "SELECT d.delivery_id
                 FROM mailbox_deliveries d
                 JOIN collaboration_messages m USING (
                     session_socket_id, worktree_id, message_id
                 )
                 JOIN thread_controls c USING (
                     session_socket_id, worktree_id, thread_id
                 )
                 WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
                   AND d.address_id = ?3 AND d.state = 'queued'
                   AND c.paused = 0 AND c.stopped = 0
                   AND NOT EXISTS (
                       SELECT 1
                       FROM mailbox_deliveries earlier
                       WHERE earlier.session_socket_id = d.session_socket_id
                         AND earlier.worktree_id = d.worktree_id
                         AND earlier.address_id = d.address_id
                         AND earlier.mailbox_order < d.mailbox_order
                         AND earlier.state NOT IN ('settled', 'failed', 'cancelled')
                   )
                 ORDER BY d.mailbox_order
                 LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(head_delivery_id) = head_delivery_id else {
            return Ok(None);
        };
        let head_message = self.transaction.query_row("SELECT message_id FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3",params![namespace.session_socket_id,namespace.space_id.as_str(),head_delivery_id],|r|r.get::<_,String>(0).map(MessageId::new))?;
        if !self.coordination_delivery_allowed(
            namespace,
            &head_message,
            address_id,
            &agent_session_id,
        )? {
            return Ok(None);
        }
        let retry_of_attempt_id = self
            .transaction
            .query_row(
                "SELECT a.attempt_id
                 FROM delivery_attempts a
                 JOIN delivery_attempt_state s USING (
                     session_socket_id, worktree_id, attempt_id, address_id
                 )
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.delivery_id = ?3 AND s.state = 'failed'
                 ORDER BY a.created_at_ms DESC, a.rowid DESC
                 LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    head_delivery_id
                ],
                |row| row.get::<_, String>(0).map(crate::DeliveryAttemptId::new),
            )
            .optional()?;
        let settled_delay_ms = self.transaction.query_row(
            "SELECT settled_delay_ms FROM worktree_settings
             WHERE session_socket_id = ?1 AND worktree_id = ?2",
            namespace_params(namespace),
            |row| from_i64(row.get(0)?),
        )?;
        Ok(Some(DispatchCandidate {
            alias: address.alias,
            delivery_id: crate::DeliveryId::new(head_delivery_id),
            pane_type: address.pane_type,
            address_id: address_id.clone(),
            binding_id: crate::PaneBindingId::new(binding_id),
            pane_id,
            agent_session_id,
            registration_id,
            message_id: head_message,
            retry_of_attempt_id,
            settled_delay_ms,
        }))
    }

    pub fn lease_mailbox_head(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        lease: &LeaseDraft,
        attribution: &MutationAttribution,
    ) -> Result<Option<LeasedDelivery>, StoreError> {
        let binding_is_active = self
            .transaction
            .query_row(
                "SELECT 1 FROM pane_bindings
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND binding_id = ?3 AND address_id = ?4 AND active = 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    lease.binding_id.as_str(),
                    address_id.as_str()
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !binding_is_active {
            return Err(StoreError::invalid_input(
                "delivery attempt requires the address's active pane binding",
            ));
        }
        let active_attempt = self
            .transaction
            .query_row(
                "SELECT 1 FROM delivery_attempt_state
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3 AND active = 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str()
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if active_attempt {
            return Ok(None);
        }
        let delivery = self
            .transaction
            .query_row(
                "SELECT delivery_id, mailbox_order, message_id, state
                 FROM mailbox_deliveries
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND address_id = ?3
                   AND state NOT IN ('settled', 'failed', 'cancelled')
                 ORDER BY mailbox_order
                 LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str()
                ],
                |row| {
                    Ok(MailboxDeliveryRecord {
                        delivery_id: crate::DeliveryId::new(row.get::<_, String>(0)?),
                        address_id: address_id.clone(),
                        mailbox_order: from_i64(row.get(1)?)?,
                        message_id: MessageId::new(row.get::<_, String>(2)?),
                        state: parse_delivery_state(row.get::<_, String>(3)?)?,
                    })
                },
            )
            .optional()?;
        let Some(mut delivery) = delivery else {
            return Ok(None);
        };
        if !self.coordination_delivery_allowed(
            namespace,
            &delivery.message_id,
            address_id,
            &lease.agent_session_id,
        )? {
            return Ok(None);
        }
        if delivery.state != DeliveryState::Queued {
            return Ok(None);
        }
        let controls_allow_delivery = self.transaction.query_row(
            "SELECT w.activated = 1 AND w.paused = 0 AND w.deactivated = 0
                    AND p.collaboration_disabled = 0
                    AND c.paused = 0 AND c.stopped = 0
             FROM mailbox_deliveries d
             JOIN collaboration_messages m USING (
                 session_socket_id, worktree_id, message_id
             )
             JOIN thread_controls c USING (
                 session_socket_id, worktree_id, thread_id
             )
             JOIN pane_addresses p
               ON p.session_socket_id = d.session_socket_id
              AND p.worktree_id = d.worktree_id
              AND p.address_id = d.address_id
             JOIN worktrees w USING (session_socket_id, worktree_id)
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
               AND d.delivery_id = ?3 AND d.address_id = ?4",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery.delivery_id.as_str(),
                address_id.as_str()
            ],
            |row| row.get::<_, bool>(0),
        )?;
        if !controls_allow_delivery {
            return Ok(None);
        }
        if let Some(retry_of_attempt_id) = &lease.retry_of_attempt_id {
            let retry_matches = self
                .transaction
                .query_row(
                    "SELECT 1 FROM delivery_attempts
                     WHERE session_socket_id = ?1 AND worktree_id = ?2
                       AND attempt_id = ?3 AND delivery_id = ?4 AND address_id = ?5",
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        retry_of_attempt_id.as_str(),
                        delivery.delivery_id.as_str(),
                        address_id.as_str()
                    ],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !retry_matches {
                return Err(StoreError::invalid_input(
                    "retry target must belong to the same delivery and Pane Address",
                ));
            }
        }

        self.transaction.execute(
            "INSERT INTO delivery_attempts (
                session_socket_id, worktree_id, attempt_id, delivery_id,
                address_id, binding_id, agent_session_id, retry_of_attempt_id,
                created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                lease.attempt_id.as_str(),
                delivery.delivery_id.as_str(),
                address_id.as_str(),
                lease.binding_id.as_str(),
                lease.agent_session_id.as_str(),
                lease
                    .retry_of_attempt_id
                    .as_ref()
                    .map(crate::DeliveryAttemptId::as_str),
                to_i64(lease.acquired_at_ms)?
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO delivery_attempt_state (
                session_socket_id, worktree_id, attempt_id, address_id, state, active
             ) VALUES (?1, ?2, ?3, ?4, 'dispatching', 1)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                lease.attempt_id.as_str(),
                address_id.as_str()
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO leases (
                session_socket_id, worktree_id, address_id, attempt_id,
                owner, acquired_at_ms, expires_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address_id.as_str(),
                lease.attempt_id.as_str(),
                lease.owner,
                to_i64(lease.acquired_at_ms)?,
                to_i64(lease.expires_at_ms)?
            ],
        )?;
        self.transaction.execute(
            "UPDATE mailbox_deliveries SET state = 'dispatching'
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND delivery_id = ?3 AND state = 'queued'",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery.delivery_id.as_str()
            ],
        )?;
        delivery.state = DeliveryState::Dispatching;
        self.append_transition(
            namespace,
            TransitionEntityKind::DeliveryAttempt,
            lease.attempt_id.as_str(),
            TransitionKind::Dispatching,
            attribution,
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::MailboxDelivery,
            delivery.delivery_id.as_str(),
            TransitionKind::Dispatching,
            attribution,
        )?;
        Ok(Some(LeasedDelivery {
            delivery,
            attempt_id: lease.attempt_id.clone(),
            binding_id: lease.binding_id.clone(),
            agent_session_id: lease.agent_session_id.clone(),
            owner: lease.owner.clone(),
            expires_at_ms: lease.expires_at_ms,
        }))
    }

    pub(crate) fn delivery_envelope(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        tool: IntegrationToolInvocation,
    ) -> Result<DeliveryEnvelope, StoreError> {
        let (thread_id, message_id, author_address_id, author_alias, body, grammar_version, recipient_address_id) = self
            .transaction
            .query_row(
            "SELECT m.thread_id, m.message_id, m.author_address_id, m.author_alias_snapshot, m.body,
                    m.grammar_version, a.address_id
             FROM delivery_attempts a
             JOIN mailbox_deliveries d USING (
                 session_socket_id, worktree_id, delivery_id, address_id
             )
             JOIN collaboration_messages m USING (
                 session_socket_id, worktree_id, message_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        let attached_context_grant = self
            .transaction
            .query_row(
                "SELECT grant_id FROM message_context_attachments
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND message_id = ?3 AND address_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id,
                    recipient_address_id,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let (attached_context, attached_context_git_state_paths) = attached_context_grant
            .map(
                |grant_id| -> Result<(crate::AttachedContext, Vec<String>), StoreError> {
                    let grant_id = ContextGrantId::new(grant_id);
                    let grant = self.context_grant(namespace, &grant_id)?;
                    let git_state_paths = match grant.snapshot_id {
                        Some(snapshot_id) => {
                            let payload = self.working_context_snapshot(namespace, &snapshot_id)?;
                            pinned_diff_evidence_paths(&payload)
                        }
                        None => Vec::new(),
                    };
                    Ok((crate::AttachedContext { grant_id }, git_state_paths))
                },
            )
            .transpose()?
            .map_or((None, Vec::new()), |(context, paths)| {
                (Some(context), paths)
            });
        let context_request = self
            .transaction
            .query_row(
                "SELECT grant_id FROM message_context_requests
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND message_id = ?3 AND address_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id,
                    recipient_address_id,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|grant_id| crate::ContextRequest {
                grant_id: ContextGrantId::new(grant_id),
            });
        let recipients =
            self.delivery_message_addresses(namespace, &message_id, "message_recipients")?;
        let references =
            self.delivery_message_addresses(namespace, &message_id, "context_references")?;
        let result_set = if grammar_version == "result-set/v1" {
            self.result_set_completion(namespace, &message_id)?
        } else {
            None
        };
        let coordination_message = result_set
            .as_ref()
            .map(|set| set.source_message_id.clone())
            .unwrap_or_else(|| MessageId::new(message_id.clone()));
        let tracked = self.tracked_message(namespace, &coordination_message)?;
        let assignments = tracked
            .as_ref()
            .map(|tracked| tracked.links.clone())
            .unwrap_or_default();
        let nudge = grammar_version == "coordination-nudge/v1";
        let wake_identity = if nudge {
            self.wake_identity(namespace, &MessageId::new(message_id.clone()))?
        } else {
            None
        };
        let stale_nudge = nudge
            && !self.coordination_wake_current(namespace, &MessageId::new(message_id.clone()))?;
        let tracked_join = result_set.is_some() && !assignments.is_empty();
        let reply_mode = if !tracked_join
            && crate::DeliveryReplyMode::for_message_grammar(&grammar_version)
                == crate::DeliveryReplyMode::AgentThread
            && self.channel_thread(namespace, &ThreadId::new(&thread_id))?
        {
            crate::DeliveryReplyMode::Channel
        } else if tracked_join {
            crate::DeliveryReplyMode::AgentThread
        } else {
            crate::DeliveryReplyMode::for_message_grammar(&grammar_version)
        };
        let mut permitted_operations = if stale_nudge {
            vec!["complete.no-reply".to_owned(), "handoff.results".to_owned()]
        } else if nudge || tracked_join {
            vec![
                "complete.no-reply".to_owned(),
                "handoff.results".to_owned(),
                "handoff.resolve".to_owned(),
                "send".to_owned(),
                "address.list".to_owned(),
                "handoff.preview".to_owned(),
            ]
        } else if reply_mode == crate::DeliveryReplyMode::ReportToHuman && context_request.is_none()
        {
            vec!["complete.no-reply".to_owned()]
        } else {
            crate::DELIVERY_PERMITTED_OPERATIONS
                .iter()
                .map(|token| (*token).to_owned())
                .collect::<Vec<_>>()
        };
        if matches!(
            reply_mode,
            crate::DeliveryReplyMode::AgentThread | crate::DeliveryReplyMode::Channel
        ) || attached_context.is_some()
        {
            permitted_operations.extend(
                crate::CONTEXT_DELIVERY_OPERATIONS
                    .iter()
                    .map(|token| (*token).to_owned()),
            );
        }
        let review_source_message_id = self.returned_review_source(namespace, attempt_id)?;
        if review_source_message_id.is_some() && !tracked_join && !nudge {
            permitted_operations.push("handoff.results".to_owned());
        }
        let tracked_worker = match &tracked {
            Some(tracked) => {
                let worker = self.counterpart_address(
                    namespace,
                    &PaneAddressId::new(recipient_address_id.clone()),
                    &tracked.home,
                )?;
                assignments.iter().any(|link| {
                    self.coordination_assignment(&tracked.home, &link.assignment_id)
                        .is_ok_and(|a| {
                            a["recipient_address_id"].as_str()
                                == worker.as_ref().map(PaneAddressId::as_str)
                        })
                })
            }
            None => false,
        };
        let sender_workspace =
            self.bridged_sender_label(namespace, &MessageId::new(message_id.clone()))?;
        let draft = crate::DeliveryControlDraft {
            attempt_id: attempt_id.clone(),
            thread_id: ThreadId::new(thread_id),
            message_id: MessageId::new(message_id),
            sender: DeliveryAddress {
                workspace: sender_workspace,
                ..DeliveryAddress::new(PaneAddressId::new(author_address_id), author_alias)
            },
            recipients,
            context_references: references,
            attached_context,
            context_request,
            result_set,
            permitted_operations,
            reply_mode,
        };
        // The control is finished first; the instruction is chosen from it.
        let mut control = DeliveryControl::new_with_git_state_paths(
            draft,
            tool,
            &attached_context_git_state_paths,
            review_source_message_id,
        )
        .map_err(StoreError::invalid_input)?;
        if reply_mode == crate::DeliveryReplyMode::Channel {
            let recap = self.channel_recap(namespace, &control.message_id)?;
            if !recap.is_empty() {
                control.untrusted_channel_recap = Some(recap);
            }
        }
        control.coordination_id = assignments.first().map(|a| a.coordination_id.clone());
        if nudge {
            control.coordination_id = wake_identity.map(|(id, _)| id);
        }
        if control.coordination_id.is_none() {
            control.coordination_id = self.transaction.query_row("SELECT coordination_id FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND report_message_id=?3",params![namespace.session_socket_id,namespace.space_id.as_str(),control.message_id.as_str()],|r|r.get::<_,String>(0).map(crate::CoordinationId::new)).optional()?;
        }
        control.assignments = assignments;
        control.coordination_nudge = nudge;
        control.coordination_nudge_stale = stale_nudge;
        if nudge || tracked_join {
            let id = control.coordination_id.clone().ok_or_else(|| {
                StoreError::invalid_input("continuation lost its coordination identity")
            })?;
            let mut statement=self.transaction.prepare("SELECT assignment_id,revision,recipient_address_id,recipient_session_id,work_outcome FROM coordination_assignments WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3 ORDER BY rowid")?;
            let rows = statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        id.as_str()
                    ],
                    |r| {
                        Ok((
                            crate::AssignmentId::new(r.get::<_, String>(0)?),
                            from_i64(r.get::<_, i64>(1)?)?,
                            crate::PaneAddressId::new(r.get::<_, String>(2)?),
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let all_completed = rows
                .iter()
                .all(|(_, _, _, _, outcome)| outcome == "completed");
            let actions = rows
                .into_iter()
                .map(|(assignment, revision, address, session, _)| {
                    // An assignee in another workspace is checked there and
                    // named by that workspace's id, which a rename never moves.
                    let principal = self.principal(namespace, &address)?;
                    let (ns, address) = (&principal.ns, &principal.address_id);
                    let disabled: bool = self.transaction.query_row("SELECT collaboration_disabled FROM pane_addresses WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),address.as_str()],|r|r.get(0))?;
                    let mention = self.optional_routing_address_by_id(ns, address)?
                        .filter(|snapshot| !disabled && snapshot.agent_session_id.as_ref().map(|s| s.as_str()) == Some(session.as_str()))
                        .map(|snapshot| if ns == namespace {
                            format!("@{}", snapshot.alias)
                        } else {
                            format!("{} @{}", crate::WorkspaceLabel::from_herdr(ns.space_id.as_str()), snapshot.alias)
                        });
                    Ok((assignment, revision, mention))
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            control.bind_coordination_actions(&id, &actions, all_completed);
        }
        if tracked_worker {
            for operation in &mut control.tool.operations {
                if operation.token == "reply" {
                    operation.input["work_outcome"] = serde_json::json!("needs_continuation");
                }
            }
        }
        Ok(DeliveryEnvelope::new(control, body))
    }

    fn result_set_completion(
        &self,
        namespace: &StateNamespace,
        result_message_id: &str,
    ) -> Result<Option<crate::ResultSetCompletion>, StoreError> {
        let source_message_id = self
            .transaction
            .query_row(
                "SELECT in_reply_to FROM collaboration_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND message_id = ?3 AND grammar_version = 'result-set/v1'",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    result_message_id
                ],
                |row| row.get::<_, Option<String>>(0),
            )?
            .map(MessageId::new);
        source_message_id
            .map(|message_id| self.result_set_outcomes(namespace, &message_id))
            .transpose()
    }

    fn result_set_outcomes(
        &self,
        namespace: &StateNamespace,
        source_message_id: &MessageId,
    ) -> Result<crate::ResultSetCompletion, StoreError> {
        let progress = self.handoff_recipients(namespace, source_message_id)?;
        let outcomes = progress
            .into_iter()
            .map(|recipient| {
                let status = recipient.result_status().ok_or_else(|| {
                    StoreError::invalid_input(
                        "result set is not ready because a recipient is still pending",
                    )
                })?;
                Ok(crate::ResultSetOutcome {
                    target: recipient.target,
                    status,
                    reply_message_id: recipient.reply_message_id,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(crate::ResultSetCompletion {
            source_message_id: source_message_id.clone(),
            expected: outcomes.len(),
            outcomes,
        })
    }

    /// Materializes one durable, idempotent completion delivery for every
    /// explicitly delegated fan-out authored in this workspace whose existing
    /// per-recipient rows are terminal. A forwarded copy is joined where it was
    /// sent, and scripts/update.py counts pending joins by the same rule.
    pub(crate) fn complete_ready_result_sets(
        &mut self,
        namespace: &StateNamespace,
        attribution: &MutationAttribution,
    ) -> Result<Vec<MessageId>, StoreError> {
        let source_ids = {
            let mut statement = self.transaction.prepare(
                "SELECT m.message_id
                 FROM collaboration_messages m
                 WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2
                   AND m.author_kind = 'agent_session'
                   AND m.grammar_version = 'delegated/v1'
                   AND (SELECT COUNT(*) FROM message_recipients r
                        WHERE r.session_socket_id = m.session_socket_id
                          AND r.worktree_id = m.worktree_id
                          AND r.message_id = m.message_id) > 1
                   AND NOT EXISTS (
                        SELECT 1 FROM collaboration_messages result
                        WHERE result.session_socket_id = m.session_socket_id
                          AND result.worktree_id = m.worktree_id
                          AND result.in_reply_to = m.message_id
                          AND result.grammar_version = 'result-set/v1'
                   )
                   AND NOT EXISTS (
                        SELECT 1 FROM space_envoys e
                        WHERE e.session_socket_id = m.session_socket_id
                          AND e.worktree_id = m.worktree_id
                          AND e.address_id = m.author_address_id
                   )
                 ORDER BY m.thread_order",
            )?;
            statement
                .query_map(
                    params![namespace.session_socket_id, namespace.space_id.as_str()],
                    |row| row.get::<_, String>(0).map(MessageId::new),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut completed = Vec::new();
        for source_message_id in source_ids {
            if let Some(message_id) =
                self.complete_result_set_if_ready(namespace, &source_message_id, attribution)?
            {
                completed.push(message_id);
            }
        }
        Ok(completed)
    }

    fn complete_result_set_if_ready(
        &mut self,
        namespace: &StateNamespace,
        source_message_id: &MessageId,
        attribution: &MutationAttribution,
    ) -> Result<Option<MessageId>, StoreError> {
        self.reflect_copy_outcomes(namespace, source_message_id, attribution)?;
        let completion = match self.result_set_outcomes(namespace, source_message_id) {
            Ok(completion) if completion.expected > 1 => completion,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => return Ok(None),
            Err(error) => return Err(error),
        };
        let source = self
            .transaction
            .query_row(
                "SELECT m.thread_id, m.author_address_id, m.author_alias_snapshot,
                        m.author_session_snapshot, a.retired, a.collaboration_disabled
                 FROM collaboration_messages m
                 JOIN pane_addresses a
                   ON a.session_socket_id = m.session_socket_id
                  AND a.worktree_id = m.worktree_id
                  AND a.address_id = m.author_address_id
                 WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2
                   AND m.message_id = ?3 AND m.author_kind = 'agent_session'
                   AND m.grammar_version = 'delegated/v1'",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    source_message_id.as_str()
                ],
                |row| {
                    Ok((
                        ThreadId::new(row.get::<_, String>(0)?),
                        PaneAddressId::new(row.get::<_, String>(1)?),
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?
                            .map(crate::AgentSessionId::new),
                        row.get::<_, bool>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((thread_id, address_id, alias, session_id, retired, disabled)) = source else {
            return Ok(None);
        };
        // The author's own space joins over its envoys, so a copy never builds
        // a second result set for the same request.
        if self.is_envoy(namespace, &address_id)? {
            return Ok(None);
        }
        if retired || disabled || session_id.is_none() {
            return Ok(None);
        }
        let mut body = format!(
            "All {} delegated results are ready for {}.",
            completion.expected,
            source_message_id.as_str()
        );
        for outcome in &completion.outcomes {
            let status = match outcome.status {
                crate::ResultSetStatus::Replied => "replied",
                crate::ResultSetStatus::NoReply => "no reply",
                crate::ResultSetStatus::Failed => "failed",
                crate::ResultSetStatus::Cancelled => "cancelled",
            };
            body.push_str(&format!("\n@{}: {status}", outcome.target.alias));
            if let Some(reply_message_id) = &outcome.reply_message_id {
                let reply_body = self.transaction.query_row(
                    "SELECT body FROM collaboration_messages
                     WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        reply_message_id.as_str()
                    ],
                    |row| row.get::<_, String>(0),
                )?;
                body.push_str(&format!("\n{reply_body}"));
            }
        }
        let result_message_id =
            MessageId::new(format!("result-set:{}", source_message_id.as_str()));
        let receipt = self.append_message(
            namespace,
            &MessageDraft {
                thread_id,
                message_id: result_message_id.clone(),
                in_reply_to: Some(source_message_id.clone()),
                author_address_id: address_id.clone(),
                // The v1 schema permits only human and agent-session message
                // authors. The reserved grammar identifies this router-owned
                // record, authored by the system rather than either participant.
                author_kind: ActorKind::AgentSession,
                author_alias_snapshot: "namedrop".to_owned(),
                author_agent_session_snapshot: session_id.clone(),
                request_id: format!("result-set:{}", source_message_id.as_str()),
                authored_source: body.clone(),
                body,
                grammar_version: "result-set/v1".to_owned(),
                recipients: vec![crate::MessageRecipientDraft {
                    address_id,
                    alias_snapshot: alias,
                    agent_session_snapshot: session_id,
                    delivery_id: crate::DeliveryId::new(format!(
                        "result-set-delivery:{}",
                        source_message_id.as_str()
                    )),
                }],
                context_references: Vec::new(),
            },
            &MutationAttribution::new(
                ActorKind::Plugin,
                "namedrop-result-set",
                attribution.occurred_at_ms,
            ),
        )?;
        Ok(Some(receipt.message_id))
    }

    /// The Delivery Attempts one exact Agent Session still owes at one Pane
    /// Address, in mailbox order. A settled attempt stays owed until a
    /// disposition or a human acknowledgement closes it.
    pub(crate) fn outstanding_delivery_attempts(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        agent_session_id: &crate::AgentSessionId,
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
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               AND a.address_id = ?3 AND a.agent_session_id = ?4
               AND NOT EXISTS (
                   SELECT 1 FROM delivery_dispositions p
                   WHERE p.session_socket_id = a.session_socket_id
                     AND p.worktree_id = a.worktree_id
                     AND p.attempt_id = a.attempt_id
               )
               AND s.state IN ('submitted', 'acknowledged', 'settled')
               AND NOT EXISTS (
                   SELECT 1 FROM attention_items i
                   WHERE i.session_socket_id = a.session_socket_id
                     AND i.worktree_id = a.worktree_id
                     AND i.source_kind = ?5
                     AND i.source_id = a.attempt_id
                     AND i.resolved_at_ms IS NOT NULL
               )
             ORDER BY d.mailbox_order",
        )?;
        statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str(),
                    agent_session_id.as_str(),
                    crate::AttentionSourceKind::AgentDisposition.as_str()
                ],
                |row| Ok(crate::DeliveryAttemptId::new(row.get::<_, String>(0)?)),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    fn delivery_message_addresses(
        &self,
        namespace: &StateNamespace,
        message_id: &str,
        source: &str,
    ) -> Result<Vec<DeliveryAddress>, StoreError> {
        let sql = match source {
            "message_recipients" => {
                "SELECT address_id, alias_snapshot
                 FROM message_recipients
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3
                 ORDER BY recipient_order"
            }
            "context_references" => {
                "SELECT address_id, alias_snapshot
                 FROM context_references
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3
                 ORDER BY reference_order"
            }
            _ => {
                return Err(StoreError::invalid_input(
                    "unknown delivery message address source",
                ));
            }
        };
        let mut statement = self.transaction.prepare(sql)?;
        statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id
                ],
                |row| {
                    Ok(DeliveryAddress::new(
                        PaneAddressId::new(row.get::<_, String>(0)?),
                        row.get::<_, String>(1)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn hold_delivery_attempt(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.transition_delivery_attempt(namespace, attempt_id, DeliveryState::Held, attribution)
    }

    pub fn return_delivery_attempt_to_queue(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let (delivery_id, state) = self.transaction.query_row(
            "SELECT a.delivery_id, s.state
             FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    parse_delivery_state(row.get::<_, String>(1)?)?,
                ))
            },
        )?;
        if !state.can_attempt_transition_to(DeliveryState::Failed) {
            return Err(StoreError::invalid_input(
                "only a dispatching pre-invocation attempt can return to queued",
            ));
        }
        self.transaction.execute(
            "UPDATE delivery_attempt_state SET state = 'failed', active = 0
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
        )?;
        crate::diagnostic_evidence::record(
            self.state_directory,
            serde_json::json!({
                "kind": "Failed", "phase": "sql_write_in_open_transaction",
                "namespace": namespace, "entity": "DeliveryAttempt", "id": attempt_id,
                "source": "return_delivery_attempt_to_queue", "previous": state, "target": "Failed", "attribution": attribution,
            }),
        );
        self.transaction.execute(
            "UPDATE mailbox_deliveries SET state = 'queued'
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND delivery_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery_id
            ],
        )?;
        self.transaction.execute(
            "DELETE FROM leases
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::DeliveryAttempt,
            attempt_id.as_str(),
            TransitionKind::Failed,
            attribution,
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::MailboxDelivery,
            &delivery_id,
            TransitionKind::Queued,
            attribution,
        )?;
        self.open_delivery_attempt_attention(
            namespace,
            attempt_id,
            "Delivery attempt failed before invocation and the mailbox item was requeued.",
            attribution,
        )?;
        Ok(())
    }

    pub fn delivery_attempt_state(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
    ) -> Result<DeliveryState, StoreError> {
        self.transaction
            .query_row(
                "SELECT state FROM delivery_attempt_state
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |row| parse_delivery_state(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn transition_delivery_attempt(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        next: DeliveryState,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let (delivery_id, source_message_id, current, mailbox_current) =
            self.transaction.query_row(
                "SELECT a.delivery_id, d.message_id, s.state, d.state
             FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             JOIN mailbox_deliveries d USING (
                 session_socket_id, worktree_id, delivery_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        MessageId::new(row.get::<_, String>(1)?),
                        parse_delivery_state(row.get::<_, String>(2)?)?,
                        parse_delivery_state(row.get::<_, String>(3)?)?,
                    ))
                },
            )?;
        if !current.can_attempt_transition_to(next) {
            return Err(StoreError::invalid_input(format!(
                "delivery attempt cannot transition from {} to {}",
                delivery_state_name(current),
                delivery_state_name(next)
            )));
        }
        if !mailbox_current.can_mailbox_transition_to(next) {
            return Err(StoreError::invalid_input(format!(
                "mailbox delivery cannot transition from {} to {}",
                delivery_state_name(mailbox_current),
                delivery_state_name(next)
            )));
        }
        if next == DeliveryState::Acknowledged {
            let acknowledgement_exists = self
                .transaction
                .query_row(
                    "SELECT 1 FROM delivery_acknowledgements
                     WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        attempt_id.as_str()
                    ],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !acknowledgement_exists {
                return Err(StoreError::invalid_input(
                    "acknowledged state requires an immutable Delivery Acknowledgement",
                ));
            }
        }
        let active = matches!(
            next,
            DeliveryState::Dispatching | DeliveryState::Submitted | DeliveryState::Acknowledged
        );
        self.transaction.execute(
            "UPDATE delivery_attempt_state SET state = ?4, active = ?5
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str(),
                delivery_state_name(next),
                active
            ],
        )?;
        if next == DeliveryState::Failed {
            crate::diagnostic_evidence::record(
                self.state_directory,
                serde_json::json!({
                    "kind": "Failed", "phase": "sql_write_in_open_transaction",
                    "namespace": namespace, "entity": "DeliveryAttempt", "id": attempt_id,
                    "source": "transition_delivery_attempt", "previous": current, "target": "Failed", "attribution": attribution,
                }),
            );
        }
        self.transaction.execute(
            "UPDATE mailbox_deliveries SET state = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND delivery_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery_id,
                delivery_state_name(next)
            ],
        )?;
        if next == DeliveryState::Failed {
            crate::diagnostic_evidence::record(
                self.state_directory,
                serde_json::json!({
                    "kind": "Failed", "phase": "sql_write_in_open_transaction",
                    "namespace": namespace, "entity": "MailboxDelivery", "id": delivery_id,
                    "source": "transition_delivery_attempt", "previous": mailbox_current, "target": "Failed", "attribution": attribution,
                }),
            );
        }
        if next != DeliveryState::Dispatching {
            self.transaction.execute(
                "DELETE FROM leases
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
            )?;
        }
        if matches!(
            next,
            DeliveryState::Acknowledged | DeliveryState::Cancelled | DeliveryState::Failed
        ) {
            self.resolve_failed_delivery_attention(
                namespace,
                &delivery_id,
                Some(attempt_id),
                attribution,
            )?;
        }
        let transition = delivery_transition_kind(next);
        self.append_transition(
            namespace,
            TransitionEntityKind::DeliveryAttempt,
            attempt_id.as_str(),
            transition,
            attribution,
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::MailboxDelivery,
            &delivery_id,
            transition,
            attribution,
        )?;
        if matches!(next, DeliveryState::Held | DeliveryState::Failed) {
            self.open_delivery_attempt_attention(
                namespace,
                attempt_id,
                &format!("Delivery attempt requires attention after entering {next:?}."),
                attribution,
            )?;
        } else if next == DeliveryState::Cancelled {
            self.resolve_attention_source(
                namespace,
                crate::AttentionSourceKind::DeliveryAttempt,
                attempt_id.as_str(),
                attribution,
            )?;
        } else if next == DeliveryState::Settled {
            self.flag_missing_disposition(namespace, attempt_id, attribution)?;
            self.reflect_copy_outcomes(namespace, &source_message_id, attribution)?;
        }
        if matches!(next, DeliveryState::Failed | DeliveryState::Cancelled) {
            self.complete_result_set_if_ready(namespace, &source_message_id, attribution)?;
        }
        Ok(())
    }

    fn resolve_failed_delivery_attention(
        &mut self,
        namespace: &StateNamespace,
        delivery_id: &str,
        except_attempt: Option<&crate::DeliveryAttemptId>,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let superseded = {
            let mut statement = self.transaction.prepare(
                "SELECT a.attempt_id FROM delivery_attempts a
                 JOIN delivery_attempt_state s USING (
                     session_socket_id, worktree_id, attempt_id, address_id
                 )
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.delivery_id = ?3 AND s.state = 'failed'
                   AND (?4 IS NULL OR a.attempt_id != ?4)",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        delivery_id,
                        except_attempt.map(crate::DeliveryAttemptId::as_str)
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for prior_attempt in superseded {
            self.resolve_attention_source(
                namespace,
                crate::AttentionSourceKind::DeliveryAttempt,
                &prior_attempt,
                attribution,
            )?;
        }
        Ok(())
    }

    pub(crate) fn delivery_disposition_exists(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
    ) -> Result<bool, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT 1 FROM delivery_dispositions
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub(crate) fn open_delivery_attempt_attention(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        summary: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.open_attention_item(
            namespace,
            &AttentionItemDraft {
                id: crate::AttentionItemId::new(format!(
                    "attention-delivery-attempt-{}",
                    attempt_id.as_str()
                )),
                source_kind: crate::AttentionSourceKind::DeliveryAttempt,
                source_id: attempt_id.as_str().to_owned(),
                summary: summary.to_owned(),
            },
            attribution,
        )?;
        Ok(())
    }

    pub fn acknowledge_delivery_attempt(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        acknowledgement_id: &str,
        agent_session_id: &crate::AgentSessionId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let (pinned_session, state) = self.transaction.query_row(
            "SELECT a.agent_session_id, s.state
             FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    parse_delivery_state(row.get::<_, String>(1)?)?,
                ))
            },
        )?;
        if pinned_session != agent_session_id.as_str() {
            return Err(StoreError::invalid_input(
                "Delivery Acknowledgement must come from the attempt's exact Agent Session",
            ));
        }
        if state == DeliveryState::Acknowledged {
            let existing_session = self
                .transaction
                .query_row(
                    "SELECT agent_session_id FROM delivery_acknowledgements
                     WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        attempt_id.as_str()
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if existing_session.as_deref() == Some(agent_session_id.as_str()) {
                return Ok(());
            }
        }
        if state != DeliveryState::Submitted {
            return Err(StoreError::invalid_input(
                "only a submitted Delivery Attempt can be acknowledged",
            ));
        }
        self.transaction.execute(
            "INSERT INTO delivery_acknowledgements (
                session_socket_id, worktree_id, acknowledgement_id,
                attempt_id, agent_session_id, acknowledged_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                acknowledgement_id,
                attempt_id.as_str(),
                agent_session_id.as_str(),
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.transition_delivery_attempt(
            namespace,
            attempt_id,
            DeliveryState::Acknowledged,
            attribution,
        )
    }

    /// Records receipt on behalf of the adapter when prompt submission is
    /// confirmed. Terminal operations call this too, so an older submitted
    /// attempt can be settled with one command.
    pub(crate) fn ensure_delivery_attempt_acknowledged(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        agent_session_id: &crate::AgentSessionId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        match self.delivery_attempt_state(namespace, attempt_id)? {
            DeliveryState::Submitted => self.acknowledge_delivery_attempt(
                namespace,
                attempt_id,
                &format!("router-receipt-{}", attempt_id.as_str()),
                agent_session_id,
                attribution,
            ),
            DeliveryState::Acknowledged | DeliveryState::Settled => Ok(()),
            DeliveryState::Held => Err(StoreError::invalid_input(
                "delivery receipt cannot be recorded from held; use `delivery cancel-held` \
                 after explicit human confirmation to cancel this held delivery",
            )),
            state => Err(StoreError::invalid_input(format!(
                "delivery receipt cannot be recorded from {}",
                delivery_state_name(state)
            ))),
        }
    }

    pub(crate) fn record_no_reply_disposition(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        disposition_id: &str,
        agent_session_id: &crate::AgentSessionId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.record_delivery_disposition(
            namespace,
            attempt_id,
            disposition_id,
            agent_session_id,
            "no_reply",
            TransitionKind::CompletedNoReply,
            attribution,
        )
    }

    pub(crate) fn settle_observed_attempt(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let (message_id, pinned_session) = self.transaction.query_row(
            "SELECT d.message_id, a.agent_session_id FROM delivery_attempts a
             JOIN mailbox_deliveries d USING (
                session_socket_id, worktree_id, delivery_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
            |row| {
                Ok((
                    MessageId::new(row.get::<_, String>(0)?),
                    crate::AgentSessionId::new(row.get::<_, String>(1)?),
                ))
            },
        )?;
        if self.wake_identity(namespace, &message_id)?.is_some() {
            self.record_no_reply_disposition(
                namespace,
                attempt_id,
                &format!("observed-no-reply-{}", attempt_id.as_str()),
                &pinned_session,
                attribution,
            )
        } else {
            self.transition_delivery_attempt(
                namespace,
                attempt_id,
                DeliveryState::Settled,
                attribution,
            )
        }
    }

    pub fn record_reply_disposition(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        reply_message_id: &MessageId,
        agent_session_id: &crate::AgentSessionId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let reply_matches_attempt = self
            .transaction
            .query_row(
                "SELECT 1
                 FROM delivery_attempts a
                 JOIN mailbox_deliveries d USING (
                     session_socket_id, worktree_id, delivery_id, address_id
                 )
                 JOIN collaboration_messages original USING (
                     session_socket_id, worktree_id, message_id
                 )
                 JOIN collaboration_messages reply
                   ON reply.session_socket_id = a.session_socket_id
                  AND reply.worktree_id = a.worktree_id
                  AND reply.message_id = ?4
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.attempt_id = ?3
                   AND reply.thread_id = original.thread_id
                   AND reply.author_address_id = a.address_id",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str(),
                    reply_message_id.as_str()
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !reply_matches_attempt {
            return Err(StoreError::invalid_input(
                "reply disposition must be authored by the attempt recipient in its Collaboration Thread",
            ));
        }
        self.record_delivery_disposition(
            namespace,
            attempt_id,
            reply_message_id.as_str(),
            agent_session_id,
            "reply",
            TransitionKind::CompletedReply,
            attribution,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_delivery_disposition(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        disposition_id: &str,
        agent_session_id: &crate::AgentSessionId,
        disposition_kind: &str,
        transition_kind: TransitionKind,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let (pinned_session, state) = self.transaction.query_row(
            "SELECT a.agent_session_id, s.state
             FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    parse_delivery_state(row.get::<_, String>(1)?)?,
                ))
            },
        )?;
        if pinned_session != agent_session_id.as_str() {
            return Err(StoreError::invalid_input(
                "Delivery disposition must come from the attempt's exact Agent Session",
            ));
        }
        let existing = self
            .transaction
            .query_row(
                "SELECT disposition_kind FROM delivery_dispositions
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing.as_deref() == Some(disposition_kind) {
            return Ok(());
        }
        if !matches!(state, DeliveryState::Acknowledged | DeliveryState::Settled) {
            return Err(StoreError::invalid_input(
                "delivery disposition requires an acknowledged or settled Delivery Attempt",
            ));
        }
        self.transaction.execute(
            "INSERT INTO delivery_dispositions (
                session_socket_id, worktree_id, disposition_id,
                attempt_id, disposition_kind, occurred_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                disposition_id,
                attempt_id.as_str(),
                disposition_kind,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::AgentDisposition,
            disposition_id,
            transition_kind,
            attribution,
        )?;
        self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::AgentDisposition,
            attempt_id.as_str(),
            attribution,
        )?;
        if state == DeliveryState::Acknowledged {
            self.transition_delivery_attempt(
                namespace,
                attempt_id,
                DeliveryState::Settled,
                attribution,
            )?;
        }
        let source_message_id = self.transaction.query_row(
            "SELECT d.message_id FROM delivery_attempts a
             JOIN mailbox_deliveries d USING (
                session_socket_id, worktree_id, delivery_id, address_id
             )
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str()
            ],
            |row| row.get::<_, String>(0).map(MessageId::new),
        )?;
        self.complete_result_set_if_ready(namespace, &source_message_id, attribution)?;
        Ok(())
    }

    pub fn acknowledge_disposition_attention(
        &mut self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        by: &MutationAttribution,
    ) -> Result<bool, StoreError> {
        if by.actor_kind != ActorKind::Human {
            return Err(StoreError::invalid_input(
                "only a human may acknowledge a missing disposition",
            ));
        }
        let (address, pinned_session, retired) = self
            .transaction
            .query_row(
                "SELECT a.address_id, a.agent_session_id, p.retired FROM delivery_attempts a
                 JOIN pane_addresses p USING (session_socket_id, worktree_id, address_id)
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |row| {
                    Ok((
                        PaneAddressId::new(row.get::<_, String>(0)?),
                        crate::AgentSessionId::new(row.get::<_, String>(1)?),
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("delivery attempt does not exist in this workspace")
            })?;
        let principal = self.principal(namespace, &address)?;
        let retired = retired || self.address_retired(&principal.ns, &principal.address_id)?;
        if !retired
            && !self
                .recipient_session_now(&principal.ns, &principal.address_id)?
                .replaced(&pinned_session)
        {
            return Err(StoreError::invalid_input(
                "the recipient address is not retired and its session is unchanged; only its exact Agent Session can record this attempt's disposition",
            ));
        }
        let acknowledged: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM attention_items a JOIN transition_facts f
               ON f.session_socket_id = a.session_socket_id AND f.worktree_id = a.worktree_id
                 AND f.entity_id = a.attention_item_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               AND a.source_kind = ?3 AND a.source_id = ?4
               AND f.entity_kind = 'attention_item' AND f.transition_kind = 'acknowledged'
               AND f.actor_kind = 'human')",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                crate::AttentionSourceKind::AgentDisposition.as_str(),
                attempt_id.as_str()
            ],
            |row| row.get(0),
        )?;
        if !acknowledged {
            let attention_id: String = self
                .transaction
                .query_row(
                    "SELECT attention_item_id FROM attention_items
                     WHERE session_socket_id = ?1 AND worktree_id = ?2
                       AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        crate::AttentionSourceKind::AgentDisposition.as_str(),
                        attempt_id.as_str()
                    ],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::invalid_input(
                        "delivery attempt has no disposition warning to acknowledge",
                    )
                })?;
            self.append_transition(
                namespace,
                TransitionEntityKind::AttentionItem,
                &attention_id,
                TransitionKind::Acknowledged,
                by,
            )?;
        }
        let resolved = self.resolve_attention_source(
            namespace,
            crate::AttentionSourceKind::AgentDisposition,
            attempt_id.as_str(),
            by,
        )?;
        Ok(!acknowledged || resolved)
    }

    fn complete_result_set_for_delivery(
        &mut self,
        namespace: &StateNamespace,
        delivery_id: &crate::DeliveryId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let source_message_id = self.transaction.query_row(
            "SELECT message_id FROM mailbox_deliveries
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND delivery_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery_id.as_str()
            ],
            |row| row.get::<_, String>(0).map(MessageId::new),
        )?;
        self.complete_result_set_if_ready(namespace, &source_message_id, attribution)?;
        Ok(())
    }

    pub fn open_attention_item(
        &mut self,
        namespace: &StateNamespace,
        draft: &AttentionItemDraft,
        attribution: &MutationAttribution,
    ) -> Result<crate::AttentionItemId, StoreError> {
        let existing = self
            .transaction
            .query_row(
                "SELECT attention_item_id FROM attention_items
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    draft.source_kind.as_str(),
                    draft.source_id
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            self.transaction.execute(
                "UPDATE attention_items SET summary = ?5
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    draft.source_kind.as_str(),
                    draft.source_id,
                    draft.summary,
                ],
            )?;
            return Ok(crate::AttentionItemId::new(existing));
        }
        let inserted = self.transaction.execute(
            "INSERT INTO attention_items (
                session_socket_id, worktree_id, attention_item_id, source_kind,
                source_id, summary, opened_at_ms, resolved_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
             ON CONFLICT (
                 session_socket_id, worktree_id, attention_item_id
             ) DO UPDATE SET
                 summary = excluded.summary,
                 opened_at_ms = excluded.opened_at_ms,
                 resolved_at_ms = NULL
             WHERE attention_items.source_kind = excluded.source_kind
               AND attention_items.source_id = excluded.source_id
               AND attention_items.resolved_at_ms IS NOT NULL",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.id.as_str(),
                draft.source_kind.as_str(),
                draft.source_id,
                draft.summary,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        let id = self.transaction.query_row(
            "SELECT attention_item_id FROM attention_items
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                draft.source_kind.as_str(),
                draft.source_id
            ],
            |row| row.get::<_, String>(0),
        )?;
        if inserted == 1 {
            self.append_transition(
                namespace,
                TransitionEntityKind::AttentionItem,
                &id,
                TransitionKind::Opened,
                attribution,
            )?;
        }
        Ok(crate::AttentionItemId::new(id))
    }

    pub fn unresolved_attention_items(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<crate::AttentionItemRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT attention_item_id, source_kind, source_id, summary,
                    opened_at_ms, resolved_at_ms
             FROM attention_items
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND resolved_at_ms IS NULL
             ORDER BY opened_at_ms, attention_item_id",
        )?;
        statement
            .query_map(namespace_params(namespace), |row| {
                Ok(crate::AttentionItemRecord {
                    id: crate::AttentionItemId::new(row.get::<_, String>(0)?),
                    source_kind: parse_attention_source_kind(row.get::<_, String>(1)?)?,
                    source_id: row.get(2)?,
                    summary: row.get(3)?,
                    opened_at_ms: from_i64(row.get(4)?)?,
                    resolved_at_ms: row.get::<_, Option<i64>>(5)?.map(from_i64).transpose()?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Whether this runtime's Agent Integration package is durably recorded as
    /// failing to install. The structured attention source the refresh writes is
    /// the readiness record; nothing here reads its human-readable summary.
    fn integration_package_failed(
        &self,
        namespace: &StateNamespace,
        runtime: crate::AgentRuntime,
    ) -> Result<bool, StoreError> {
        self.attention_source_is_unresolved(
            namespace,
            crate::AttentionSourceKind::IntegrationRegistration,
            &crate::integration_package_attention_source(runtime),
        )
    }

    fn attention_source_is_unresolved(
        &self,
        namespace: &StateNamespace,
        source_kind: crate::AttentionSourceKind,
        source_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT 1 FROM attention_items
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    source_kind.as_str(),
                    source_id
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub(crate) fn resolve_attention_source(
        &mut self,
        namespace: &StateNamespace,
        source_kind: crate::AttentionSourceKind,
        source_id: &str,
        attribution: &MutationAttribution,
    ) -> Result<bool, StoreError> {
        let attention_item_id = self
            .transaction
            .query_row(
                "SELECT attention_item_id
                 FROM attention_items
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    source_kind.as_str(),
                    source_id
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(attention_item_id) = attention_item_id else {
            return Ok(false);
        };
        self.transaction.execute(
            "UPDATE attention_items SET resolved_at_ms = ?5
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND source_kind = ?3 AND source_id = ?4 AND resolved_at_ms IS NULL",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                source_kind.as_str(),
                source_id,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::AttentionItem,
            &attention_item_id,
            TransitionKind::Resolved,
            attribution,
        )?;
        Ok(true)
    }

    fn resolve_superseded_integration_registration_attention(
        &mut self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
        current_registration_id: &str,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.source_id
             FROM attention_items a
             JOIN integration_registrations r
               ON r.session_socket_id = a.session_socket_id
              AND r.worktree_id = a.worktree_id
              AND r.registration_id = a.source_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               AND a.source_kind = 'integration_registration'
               AND a.resolved_at_ms IS NULL
               AND r.address_id = ?3
               AND r.registration_id != ?4
             ORDER BY a.opened_at_ms, a.attention_item_id",
        )?;
        let source_ids = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address_id.as_str(),
                    current_registration_id,
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for source_id in source_ids.into_iter().filter(|source_id| {
            !source_id.starts_with(crate::INTEGRATION_PACKAGE_ATTENTION_SOURCE_PREFIX)
        }) {
            self.resolve_attention_source(
                namespace,
                crate::AttentionSourceKind::IntegrationRegistration,
                &source_id,
                attribution,
            )?;
        }
        Ok(())
    }

    pub fn transition_facts(
        &self,
        namespace: &StateNamespace,
    ) -> Result<Vec<TransitionFact>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT worktree_order, entity_kind, entity_id, transition_kind,
                    actor_kind, actor_id, occurred_at_ms
             FROM transition_facts
             WHERE session_socket_id = ?1 AND worktree_id = ?2
             ORDER BY worktree_order",
        )?;
        let facts = statement
            .query_map(namespace_params(namespace), |row| {
                Ok(TransitionFact {
                    worktree_order: from_i64(row.get(0)?)?,
                    entity_kind: parse_transition_entity_kind(row.get::<_, String>(1)?)?,
                    entity_id: row.get(2)?,
                    transition_kind: parse_transition_kind(row.get::<_, String>(3)?)?,
                    actor_kind: parse_actor_kind(row.get::<_, String>(4)?)?,
                    actor_id: row.get(5)?,
                    occurred_at_ms: from_i64(row.get(6)?)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(facts)
    }

    pub(crate) fn delivery_health(
        &self,
        namespace: &StateNamespace,
    ) -> Result<(u64, Vec<String>), StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT d.state, r.alias_snapshot
             FROM mailbox_deliveries d
             JOIN message_recipients r
               ON r.session_socket_id = d.session_socket_id
              AND r.worktree_id = d.worktree_id
              AND r.message_id = d.message_id
              AND r.address_id = d.address_id
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
             ORDER BY d.mailbox_order",
        )?;
        let rows = statement
            .query_map(namespace_params(namespace), |row| {
                Ok((
                    parse_delivery_state(row.get::<_, String>(0)?)?,
                    row.get::<_, String>(1)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let waiting = rows
            .iter()
            .filter(|(state, _)| state.awaits_progress())
            .count() as u64;
        let mut held_aliases = rows
            .into_iter()
            .filter_map(|(state, alias)| (state == DeliveryState::Held).then_some(alias))
            .collect::<Vec<_>>();
        held_aliases.sort();
        held_aliases.dedup();
        Ok((waiting, held_aliases))
    }

    fn message_receipt(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
        idempotent_replay: bool,
    ) -> Result<MessageReceipt, StoreError> {
        let thread_order = self.transaction.query_row(
            "SELECT thread_order FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                message_id.as_str()
            ],
            |row| from_i64(row.get(0)?),
        )?;
        let mut statement = self.transaction.prepare(
            "SELECT delivery_id, address_id, mailbox_order, state
             FROM mailbox_deliveries
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3
             ORDER BY address_id, mailbox_order",
        )?;
        let deliveries = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |row| {
                    Ok(MailboxDeliveryRecord {
                        delivery_id: crate::DeliveryId::new(row.get::<_, String>(0)?),
                        address_id: PaneAddressId::new(row.get::<_, String>(1)?),
                        mailbox_order: from_i64(row.get(2)?)?,
                        message_id: message_id.clone(),
                        state: parse_delivery_state(row.get::<_, String>(3)?)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MessageReceipt {
            message_id: message_id.clone(),
            thread_id: self.transaction.query_row(
                "SELECT thread_id FROM collaboration_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |row| row.get::<_, String>(0).map(ThreadId::new),
            )?,
            thread_order,
            deliveries,
            idempotent_replay,
        })
    }

    fn message_routing_snapshots(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
        table: &str,
        order_column: &str,
        has_binding: bool,
    ) -> Result<Vec<RoutingAddressSnapshot>, StoreError> {
        let binding_column = if has_binding {
            "binding_snapshot"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT address_id, alias_snapshot, {binding_column}, session_snapshot
             FROM {table}
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3
             ORDER BY {order_column}"
        );
        let mut statement = self.transaction.prepare(&sql)?;
        statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |row| {
                    Ok(RoutingAddressSnapshot {
                        address_id: PaneAddressId::new(row.get::<_, String>(0)?),
                        alias: row.get(1)?,
                        pane_binding_id: row
                            .get::<_, Option<String>>(2)?
                            .map(crate::PaneBindingId::new),
                        agent_session_id: row
                            .get::<_, Option<String>>(3)?
                            .map(crate::AgentSessionId::new),
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    fn next_order(
        &mut self,
        namespace: &StateNamespace,
        stream_kind: &str,
        stream_id: &str,
    ) -> Result<u64, StoreError> {
        self.transaction.execute(
            "INSERT INTO order_counters (
                session_socket_id, worktree_id, stream_kind, stream_id, next_order
             ) VALUES (?1, ?2, ?3, ?4, 2)
             ON CONFLICT(session_socket_id, worktree_id, stream_kind, stream_id)
             DO UPDATE SET next_order = next_order + 1",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                stream_kind,
                stream_id
            ],
        )?;
        self.transaction
            .query_row(
                "SELECT next_order - 1 FROM order_counters
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND stream_kind = ?3 AND stream_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    stream_kind,
                    stream_id
                ],
                |row| from_i64(row.get(0)?),
            )
            .map_err(StoreError::from)
    }

    fn append_transition(
        &mut self,
        namespace: &StateNamespace,
        entity_kind: TransitionEntityKind,
        entity_id: &str,
        transition_kind: TransitionKind,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let worktree_order = self.next_order(namespace, "worktree_transition", "")?;
        self.transaction.execute(
            "INSERT INTO transition_facts (
                session_socket_id, worktree_id, worktree_order, entity_kind,
                entity_id, transition_kind, actor_kind, actor_id, occurred_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                to_i64(worktree_order)?,
                entity_kind.as_str(),
                entity_id,
                transition_kind.as_str(),
                attribution.actor_kind.as_str(),
                attribution.actor_id,
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreErrorKind {
    InvalidInput,
    Internal,
    Conflict,
    Busy,
    UnsupportedSchema,
    Storage,
}

#[derive(Debug)]
pub struct StoreError {
    kind: StoreErrorKind,
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl StoreError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            kind: StoreErrorKind::InvalidInput,
            message: message.into(),
            source: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: StoreErrorKind::Internal,
            message: message.into(),
            source: None,
        }
    }

    pub fn kind(&self) -> StoreErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn unsupported_schema(message: impl Into<String>) -> Self {
        Self {
            kind: StoreErrorKind::UnsupportedSchema,
            message: message.into(),
            source: None,
        }
    }

    fn integrity_failed(
        database_path: &Path,
        details: impl Into<String>,
        source: Option<Box<dyn Error + Send + Sync>>,
    ) -> Self {
        Self {
            kind: StoreErrorKind::Storage,
            message: format!(
                "database integrity check failed at {}: {}. Back up the file before replacing or \
                 removing it",
                database_path.display(),
                details.into()
            ),
            source,
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Operation boundaries carry strings. Preserve a machine-readable code
        // for transient contention so a durable worker can retry safely.
        if self.kind == StoreErrorKind::Busy {
            return write!(
                formatter,
                "{}",
                serde_json::json!({
                    "error": {"code": "state_busy", "message": self.message}
                })
            );
        }
        write!(formatter, "{}", self.message)
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self {
            kind: StoreErrorKind::Storage,
            message: format!("state storage I/O failed: {error}"),
            source: Some(Box::new(error)),
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        let kind = match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::ConstraintViolation) => StoreErrorKind::Conflict,
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                StoreErrorKind::Busy
            }
            _ => StoreErrorKind::Storage,
        };
        Self {
            kind,
            message: format!("state storage failed: {error}"),
            source: Some(Box::new(error)),
        }
    }
}

fn namespace_params(namespace: &StateNamespace) -> [&str; 2] {
    [
        namespace.session_socket_id.as_str(),
        namespace.space_id.as_str(),
    ]
}

fn purge_summary(
    connection: &Connection,
    namespace: &StateNamespace,
) -> Result<WorktreePurgeSummary, StoreError> {
    let deactivated = connection
        .query_row(
            "SELECT deactivated FROM worktrees
             WHERE session_socket_id = ?1 AND worktree_id = ?2",
            namespace_params(namespace),
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::invalid_input("Namedrop state does not exist for this space"))?;
    let nonterminal_deliveries = connection.query_row(
        "SELECT COUNT(*) FROM mailbox_deliveries
         WHERE session_socket_id = ?1 AND worktree_id = ?2
           AND state NOT IN ('settled', 'failed', 'cancelled')",
        namespace_params(namespace),
        |row| row.get::<_, i64>(0),
    )? as u64;
    Ok(WorktreePurgeSummary {
        space_id: namespace.space_id.as_str().to_owned(),
        deactivated,
        nonterminal_deliveries,
        affected_records: affected_record_count(connection, namespace)?,
    })
}

fn validate_purge(
    summary: &WorktreePurgeSummary,
    namespace: &StateNamespace,
    typed_space_id: &str,
) -> Result<(), StoreError> {
    if !summary.deactivated {
        return Err(StoreError::invalid_input(
            "Space Collaboration Data Purge requires prior space deactivation",
        ));
    }
    if summary.nonterminal_deliveries != 0 {
        return Err(StoreError::invalid_input(format!(
            "Worktree Collaboration Data Purge requires every delivery to be settled, failed, \
             or explicitly cancelled; {} deliveries are still nonterminal",
            summary.nonterminal_deliveries
        )));
    }
    if typed_space_id != namespace.space_id.as_str() {
        return Err(StoreError::invalid_input(format!(
            "typed Worktree name must exactly match {}",
            namespace.space_id.as_str()
        )));
    }
    Ok(())
}

fn affected_record_count(
    connection: &Connection,
    namespace: &StateNamespace,
) -> Result<u64, StoreError> {
    affected_record_count_for_storage_key(
        connection,
        &namespace.session_socket_id,
        namespace.space_id.as_str(),
    )
}

fn affected_record_count_for_storage_key(
    connection: &Connection,
    session_socket_id: &str,
    storage_key: &str,
) -> Result<u64, StoreError> {
    namespaced_tables(connection, "main")?
        .into_iter()
        .try_fold(0_u64, |total, table| {
            let count = connection.query_row(
                &format!(
                    "SELECT COUNT(*) FROM main.{} \
                     WHERE session_socket_id = ?1 AND worktree_id = ?2",
                    quoted_identifier(&table)
                ),
                params![session_socket_id, storage_key],
                |row| row.get::<_, i64>(0),
            )? as u64;
            Ok(total + count)
        })
}

fn namespaced_tables(connection: &Connection, schema: &str) -> Result<Vec<String>, StoreError> {
    let tables = {
        let mut statement = connection.prepare(&format!(
            "SELECT name FROM {}.sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            quoted_identifier(schema)
        ))?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    tables
        .into_iter()
        .filter_map(|table| {
            let result = table_has_namespace_columns(connection, schema, &table);
            match result {
                Ok(true) => Some(Ok(table)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

fn table_has_namespace_columns(
    connection: &Connection,
    schema: &str,
    table: &str,
) -> Result<bool, StoreError> {
    let mut statement = connection.prepare(&format!(
        "PRAGMA {}.table_info({})",
        quoted_identifier(schema),
        quoted_identifier(table)
    ))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns.iter().any(|column| column == "session_socket_id")
        && columns.iter().any(|column| column == "worktree_id"))
}

fn delete_namespace_rows(
    transaction: &Transaction<'_>,
    schema: &str,
    namespace: &StateNamespace,
) -> Result<(), StoreError> {
    transaction.pragma_update(None, "defer_foreign_keys", true)?;
    for table in namespaced_tables(transaction, schema)? {
        transaction.execute(
            &format!(
                "DELETE FROM {}.{} WHERE session_socket_id = ?1 AND worktree_id = ?2",
                quoted_identifier(schema),
                quoted_identifier(&table)
            ),
            namespace_params(namespace),
        )?;
    }
    Ok(())
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn compact_database(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    Ok(())
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::invalid_input("monotonic value exceeds SQLite range"))
}

fn from_i64(column_value: i64) -> rusqlite::Result<u64> {
    u64::try_from(column_value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, Type::Integer, Box::new(error))
    })
}

fn delivery_state_name(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Queued => "queued",
        DeliveryState::Dispatching => "dispatching",
        DeliveryState::Submitted => "submitted",
        DeliveryState::Acknowledged => "acknowledged",
        DeliveryState::Settled => "settled",
        DeliveryState::Held => "held",
        DeliveryState::Failed => "failed",
        DeliveryState::Cancelled => "cancelled",
    }
}

fn delivery_transition_kind(state: DeliveryState) -> TransitionKind {
    match state {
        DeliveryState::Queued => TransitionKind::Queued,
        DeliveryState::Dispatching => TransitionKind::Dispatching,
        DeliveryState::Submitted => TransitionKind::Submitted,
        DeliveryState::Acknowledged => TransitionKind::Acknowledged,
        DeliveryState::Settled => TransitionKind::Settled,
        DeliveryState::Held => TransitionKind::Held,
        DeliveryState::Failed => TransitionKind::Failed,
        DeliveryState::Cancelled => TransitionKind::Cancelled,
    }
}

fn parse_delivery_state(value: String) -> rusqlite::Result<DeliveryState> {
    match value.as_str() {
        "queued" => Ok(DeliveryState::Queued),
        "dispatching" => Ok(DeliveryState::Dispatching),
        "submitted" => Ok(DeliveryState::Submitted),
        "acknowledged" => Ok(DeliveryState::Acknowledged),
        "settled" => Ok(DeliveryState::Settled),
        "held" => Ok(DeliveryState::Held),
        "failed" => Ok(DeliveryState::Failed),
        "cancelled" => Ok(DeliveryState::Cancelled),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Text,
            format!("unknown delivery state {value}").into(),
        )),
    }
}

fn pinned_diff_evidence_paths(payload: &serde_json::Value) -> Vec<String> {
    let Some(evidence) = payload
        .get("git_state")
        .and_then(serde_json::Value::as_object)
        .and_then(|_| payload.get("evidence"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for pointer in evidence {
        if pointer.get("kind").and_then(serde_json::Value::as_str) != Some("diff") {
            continue;
        }
        let Some(path) = pointer
            .get("locator")
            .and_then(serde_json::Value::as_str)
            .filter(|path| *path != "[BLOCKED PATH]")
        else {
            continue;
        };
        paths.push(path.to_owned());
    }
    paths
}

fn parse_context_grant_state(value: String) -> rusqlite::Result<ContextGrantState> {
    match value.as_str() {
        "pending" => Ok(ContextGrantState::Pending),
        "active" => Ok(ContextGrantState::Active),
        "revoked" => Ok(ContextGrantState::Revoked),
        "expired" => Ok(ContextGrantState::Expired),
        _ => Err(invalid_text("Context Grant state", value)),
    }
}

fn parse_transition_entity_kind(value: String) -> rusqlite::Result<TransitionEntityKind> {
    match value.as_str() {
        "worktree" => Ok(TransitionEntityKind::Worktree),
        "pane_address" => Ok(TransitionEntityKind::PaneAddress),
        "integration_registration" => Ok(TransitionEntityKind::IntegrationRegistration),
        "collaboration_thread" => Ok(TransitionEntityKind::CollaborationThread),
        "collaboration_message" => Ok(TransitionEntityKind::CollaborationMessage),
        "mailbox_delivery" => Ok(TransitionEntityKind::MailboxDelivery),
        "delivery_attempt" => Ok(TransitionEntityKind::DeliveryAttempt),
        "agent_disposition" => Ok(TransitionEntityKind::AgentDisposition),
        "attention_item" => Ok(TransitionEntityKind::AttentionItem),
        "coordination_ownership" => Ok(TransitionEntityKind::CoordinationOwnership),
        _ => Err(invalid_text("transition entity kind", value)),
    }
}

fn parse_transition_kind(value: String) -> rusqlite::Result<TransitionKind> {
    match value.as_str() {
        "activated" => Ok(TransitionKind::Activated),
        "deactivated" => Ok(TransitionKind::Deactivated),
        "created" => Ok(TransitionKind::Created),
        "bound" => Ok(TransitionKind::Bound),
        "unbound" => Ok(TransitionKind::Unbound),
        "occupied" => Ok(TransitionKind::Occupied),
        "empty" => Ok(TransitionKind::Empty),
        "renamed" => Ok(TransitionKind::Renamed),
        "released" => Ok(TransitionKind::Released),
        "retired" => Ok(TransitionKind::Retired),
        "challenged" => Ok(TransitionKind::Challenged),
        "registered" => Ok(TransitionKind::Registered),
        "expired" => Ok(TransitionKind::Expired),
        "queued" => Ok(TransitionKind::Queued),
        "dispatching" => Ok(TransitionKind::Dispatching),
        "submitted" => Ok(TransitionKind::Submitted),
        "invoked" => Ok(TransitionKind::Invoked),
        "acknowledged" => Ok(TransitionKind::Acknowledged),
        "settled" => Ok(TransitionKind::Settled),
        "held" => Ok(TransitionKind::Held),
        "failed" => Ok(TransitionKind::Failed),
        "cancelled" => Ok(TransitionKind::Cancelled),
        "completed_reply" => Ok(TransitionKind::CompletedReply),
        "completed_no_reply" => Ok(TransitionKind::CompletedNoReply),
        "paused" => Ok(TransitionKind::Paused),
        "resumed" => Ok(TransitionKind::Resumed),
        "stopped" => Ok(TransitionKind::Stopped),
        "continued" => Ok(TransitionKind::Continued),
        "enabled" => Ok(TransitionKind::Enabled),
        "disabled" => Ok(TransitionKind::Disabled),
        "remote_commands_allowed" => Ok(TransitionKind::RemoteCommandsAllowed),
        "remote_commands_revoked" => Ok(TransitionKind::RemoteCommandsRevoked),
        "opened" => Ok(TransitionKind::Opened),
        "resolved" => Ok(TransitionKind::Resolved),
        "adopted" => Ok(TransitionKind::Adopted),
        _ => Err(invalid_text("transition kind", value)),
    }
}

fn parse_actor_kind(value: String) -> rusqlite::Result<ActorKind> {
    match value.as_str() {
        "human" => Ok(ActorKind::Human),
        "agent_session" => Ok(ActorKind::AgentSession),
        "plugin" => Ok(ActorKind::Plugin),
        "system" => Ok(ActorKind::System),
        _ => Err(invalid_text("actor kind", value)),
    }
}

fn parse_attention_source_kind(value: String) -> rusqlite::Result<crate::AttentionSourceKind> {
    match value.as_str() {
        "delivery_attempt" => Ok(crate::AttentionSourceKind::DeliveryAttempt),
        "automatic_reply_limit" => Ok(crate::AttentionSourceKind::AutomaticReplyLimit),
        "pane_binding" => Ok(crate::AttentionSourceKind::PaneBinding),
        "integration_registration" => Ok(crate::AttentionSourceKind::IntegrationRegistration),
        "agent_disposition" => Ok(crate::AttentionSourceKind::AgentDisposition),
        "context_grant" => Ok(crate::AttentionSourceKind::ContextGrant),
        "coordination" => Ok(crate::AttentionSourceKind::Coordination),
        _ => Err(invalid_text("attention source kind", value)),
    }
}

fn invalid_text(kind: &str, value: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        Type::Text,
        format!("unknown {kind} {value}").into(),
    )
}

fn bounded_redacted(value: &str, max_chars: usize) -> String {
    redact_disclosure_text(value)
        .chars()
        .take(max_chars)
        .collect()
}

fn verify_integrity(connection: &Connection, database_path: &Path) -> Result<(), StoreError> {
    let result = (|| -> rusqlite::Result<Vec<String>> {
        let mut statement = connection.prepare("PRAGMA integrity_check")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect()
    })();
    match result {
        Ok(rows) if rows == ["ok"] => Ok(()),
        Ok(rows) => Err(StoreError::integrity_failed(
            database_path,
            rows.join("; "),
            None,
        )),
        Err(error) => {
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ) {
                return Err(StoreError::from(error));
            }
            Err(StoreError::integrity_failed(
                database_path,
                error.to_string(),
                Some(Box::new(error)),
            ))
        }
    }
}

fn initialize_schema(
    connection: &mut Connection,
    _state_directory: &Path,
) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: u32 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::unsupported_schema(format!(
            "database schema {version} is not supported; expected {SCHEMA_VERSION}"
        )));
    }
    // Each step is additive and applies only to the versions that predate it.
    if version == 0 {
        transaction.execute_batch(SCHEMA)?;
    }
    for (introduced_in, schema) in [
        (2, groups::SCHEMA),
        (3, terminal::SCHEMA),
        (4, coordination::SCHEMA),
        (5, PERSONA_SCHEMA),
        (6, bridge::SCHEMA),
        (6, bridge::DELIVERY_SEATS_SCHEMA),
        (6, terminal::REMOTE_SCHEMA),
    ] {
        if version < introduced_in {
            transaction.execute_batch(schema)?;
        }
    }
    if version == SCHEMA_VERSION {
        for gap in pre_release_schema_6_gaps(&transaction)? {
            transaction.execute_batch(gap.ddl)?;
        }
    }
    if version < SCHEMA_VERSION {
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    transaction.commit()?;
    Ok(())
}

/// The public `experimental` branch wrote user_version 6 before 1.4.0 added
/// these two pieces to schema 6, so a database at version 6 may lack either.
fn pre_release_schema_6_gaps(connection: &Connection) -> Result<Vec<SchemaGap>, rusqlite::Error> {
    let present = |query: &str| {
        connection.query_row(query, [], |row| row.get::<_, i64>(0).map(|count| count > 0))
    };
    let mut gaps = Vec::new();
    if !present(
        "SELECT count(*) FROM pragma_table_info('pane_addresses')
          WHERE name = 'remote_commands_terminal_id'",
    )? {
        gaps.push(SchemaGap {
            name: "pane_addresses.remote_commands_terminal_id",
            ddl: terminal::REMOTE_SCHEMA,
        });
    }
    if !present(
        "SELECT count(*) FROM sqlite_master WHERE type = 'view' AND name = 'delivery_seats'",
    )? {
        gaps.push(SchemaGap {
            name: "delivery_seats",
            ddl: bridge::DELIVERY_SEATS_SCHEMA,
        });
    }
    Ok(gaps)
}

struct SchemaGap {
    name: &'static str,
    ddl: &'static str,
}

fn check_read_only_schema(connection: &Connection) -> Result<(), StoreError> {
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(StoreError::unsupported_schema(format!(
            "state has schema {version}; expected {SCHEMA_VERSION}"
        )));
    }
    let gaps = pre_release_schema_6_gaps(connection)?;
    if !gaps.is_empty() {
        let names: Vec<&str> = gaps.iter().map(|gap| gap.name).collect();
        return Err(StoreError::unsupported_schema(format!(
            "state has pre-release schema 6 without {}; activating Namedrop completes it",
            names.join(" and ")
        )));
    }
    Ok(())
}

const SCHEMA: &str = r#"
CREATE TABLE agent_session_observations (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    observation_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    status TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, observation_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE alias_claims (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    address_id TEXT NOT NULL,
    claim_state TEXT NOT NULL CHECK (claim_state IN ('active', 'tombstone')),
    replacement_alias TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, alias),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE alias_history (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    history_order INTEGER NOT NULL CHECK (history_order > 0),
    alias TEXT NOT NULL,
    address_id TEXT NOT NULL,
    transition_kind TEXT NOT NULL,
    replacement_alias TEXT,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, history_order),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE attention_items (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attention_item_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    summary TEXT NOT NULL,
    opened_at_ms INTEGER NOT NULL,
    resolved_at_ms INTEGER,
    PRIMARY KEY (session_socket_id, worktree_id, attention_item_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE collaboration_messages (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    thread_order INTEGER NOT NULL CHECK (thread_order > 0),
    in_reply_to TEXT,
    author_address_id TEXT NOT NULL,
    author_alias_snapshot TEXT NOT NULL,
    author_kind TEXT NOT NULL CHECK (author_kind IN ('human', 'agent_session')),
    author_session_snapshot TEXT,
    request_id TEXT NOT NULL,
    authored_source TEXT NOT NULL,
    body TEXT NOT NULL,
    grammar_version TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id),
    UNIQUE (session_socket_id, worktree_id, thread_id, thread_order),
    UNIQUE (session_socket_id, worktree_id, thread_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id, in_reply_to)
        REFERENCES collaboration_messages (
            session_socket_id, worktree_id, thread_id, message_id
        ),
    FOREIGN KEY (session_socket_id, worktree_id, author_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE collaboration_threads (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    title TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE context_grants (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    source_address_id TEXT NOT NULL,
    grantee_address_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    pane_binding_id TEXT,
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'active', 'revoked', 'expired')
        AND (
            state NOT IN ('pending', 'active')
            OR pane_binding_id IS NOT NULL
        )
    ),
    PRIMARY KEY (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, source_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, grantee_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (
        session_socket_id, worktree_id, pane_binding_id, source_address_id
    ) REFERENCES pane_bindings (
        session_socket_id, worktree_id, binding_id, address_id
    )
);

CREATE TABLE context_read_audit (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    audit_order INTEGER NOT NULL CHECK (audit_order > 0),
    grant_id TEXT NOT NULL,
    reader_address_id TEXT,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'agent_session')),
    actor_id TEXT NOT NULL,
    evidence_locator TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, audit_order),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, reader_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE context_references (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    reference_order INTEGER NOT NULL CHECK (reference_order > 0),
    address_id TEXT NOT NULL,
    alias_snapshot TEXT NOT NULL,
    binding_snapshot TEXT,
    session_snapshot TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, reference_order),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, binding_snapshot, address_id)
        REFERENCES pane_bindings (session_socket_id, worktree_id, binding_id, address_id)
);

CREATE TABLE context_snapshot_catalog (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    snapshot_kind TEXT NOT NULL CHECK (snapshot_kind IN ('working', 'thread')),
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE delivery_acknowledgements (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    acknowledgement_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    acknowledged_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, acknowledgement_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id)
        REFERENCES delivery_attempts (session_socket_id, worktree_id, attempt_id)
);

CREATE TABLE delivery_attempt_state (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('dispatching', 'submitted', 'acknowledged', 'settled',
                  'held', 'failed', 'cancelled')
    ),
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    PRIMARY KEY (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id, address_id)
        REFERENCES delivery_attempts (
            session_socket_id, worktree_id, attempt_id, address_id
        )
);

CREATE TABLE delivery_attempts (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    retry_of_attempt_id TEXT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, attempt_id),
    UNIQUE (session_socket_id, worktree_id, delivery_id, attempt_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id, address_id),
    UNIQUE (
        session_socket_id, worktree_id, attempt_id, delivery_id, address_id
    ),
    FOREIGN KEY (session_socket_id, worktree_id, delivery_id, address_id)
        REFERENCES mailbox_deliveries (
            session_socket_id, worktree_id, delivery_id, address_id
        ),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, binding_id, address_id)
        REFERENCES pane_bindings (
            session_socket_id, worktree_id, binding_id, address_id
        ),
    FOREIGN KEY (
        session_socket_id, worktree_id, retry_of_attempt_id, delivery_id, address_id
    ) REFERENCES delivery_attempts (
        session_socket_id, worktree_id, attempt_id, delivery_id, address_id
    )
);

CREATE TABLE delivery_dispositions (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    disposition_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    disposition_kind TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, disposition_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id)
        REFERENCES delivery_attempts (session_socket_id, worktree_id, attempt_id)
);

CREATE TABLE evidence_index (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    evidence_order INTEGER NOT NULL CHECK (evidence_order > 0),
    evidence_kind TEXT NOT NULL,
    locator TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id, evidence_order),
    FOREIGN KEY (session_socket_id, worktree_id, snapshot_id)
        REFERENCES context_snapshot_catalog (session_socket_id, worktree_id, snapshot_id)
);

CREATE TABLE idempotency_records (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    author_address_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, author_address_id, request_id),
    UNIQUE (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, author_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id)
);

CREATE TABLE integration_registrations (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    registration_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    runtime TEXT NOT NULL,
    protocol_version TEXT NOT NULL,
    capability_profile TEXT NOT NULL,
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    PRIMARY KEY (session_socket_id, worktree_id, registration_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE leases (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    acquired_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > acquired_at_ms),
    PRIMARY KEY (session_socket_id, worktree_id, address_id),
    UNIQUE (session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY (session_socket_id, worktree_id, attempt_id, address_id)
        REFERENCES delivery_attempts (
            session_socket_id, worktree_id, attempt_id, address_id
        )
);

CREATE TABLE mailbox_deliveries (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    mailbox_order INTEGER NOT NULL CHECK (mailbox_order > 0),
    message_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('queued', 'dispatching', 'submitted', 'acknowledged',
                  'settled', 'held', 'failed', 'cancelled')
    ),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, delivery_id),
    UNIQUE (session_socket_id, worktree_id, address_id, mailbox_order),
    UNIQUE (session_socket_id, worktree_id, delivery_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id, address_id)
        REFERENCES message_recipients (
            session_socket_id, worktree_id, message_id, address_id
        )
);

CREATE TABLE managed_tab_labels (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    tab_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    managed_label TEXT NOT NULL,
    previous_label TEXT NOT NULL,
    pending_label TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, tab_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE message_context_attachments (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, address_id),
    UNIQUE (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id)
);

CREATE TABLE message_context_requests (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, address_id),
    UNIQUE (session_socket_id, worktree_id, grant_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id)
);

CREATE TABLE message_recipients (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    recipient_order INTEGER NOT NULL CHECK (recipient_order > 0),
    address_id TEXT NOT NULL,
    alias_snapshot TEXT NOT NULL,
    session_snapshot TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, recipient_order),
    UNIQUE (session_socket_id, worktree_id, message_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE order_counters (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    stream_kind TEXT NOT NULL,
    stream_id TEXT NOT NULL,
    next_order INTEGER NOT NULL CHECK (next_order > 0),
    PRIMARY KEY (session_socket_id, worktree_id, stream_kind, stream_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE pane_addresses (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    role_id TEXT NOT NULL,
    retired INTEGER NOT NULL CHECK (retired IN (0, 1)),
    collaboration_disabled INTEGER NOT NULL CHECK (collaboration_disabled IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, role_id)
        REFERENCES pane_roles (session_socket_id, worktree_id, role_id)
);

CREATE TABLE pane_bindings (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    pane_id TEXT NOT NULL,
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    tab_id TEXT,
    bound_at_ms INTEGER NOT NULL,
    unbound_at_ms INTEGER,
    PRIMARY KEY (session_socket_id, worktree_id, binding_id),
    UNIQUE (session_socket_id, worktree_id, binding_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE pane_roles (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    role_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, role_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE reconciliation_epochs (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch >= 0),
    completed INTEGER NOT NULL CHECK (completed IN (0, 1)),
    PRIMARY KEY (session_socket_id, worktree_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE thread_context_snapshots (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    participant_address_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, snapshot_id)
        REFERENCES context_snapshot_catalog (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, participant_address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE thread_controls (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
    stopped INTEGER NOT NULL CHECK (stopped IN (0, 1)),
    reply_allowance INTEGER NOT NULL CHECK (reply_allowance >= 0),
    PRIMARY KEY (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id)
);

CREATE TABLE thread_participants (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    participant_order INTEGER NOT NULL CHECK (participant_order > 0),
    address_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, thread_id, participant_order),
    UNIQUE (session_socket_id, worktree_id, thread_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);

CREATE TABLE transition_facts (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    worktree_order INTEGER NOT NULL CHECK (worktree_order > 0),
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    transition_kind TEXT NOT NULL,
    actor_kind TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, worktree_order),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE working_context_snapshots (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, snapshot_id)
        REFERENCES context_snapshot_catalog (session_socket_id, worktree_id, snapshot_id),
    FOREIGN KEY (session_socket_id, worktree_id, grant_id)
        REFERENCES context_grants (session_socket_id, worktree_id, grant_id)
);

CREATE TABLE worktree_settings (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    settled_delay_ms INTEGER NOT NULL CHECK (settled_delay_ms >= 0),
    PRIMARY KEY (session_socket_id, worktree_id),
    FOREIGN KEY (session_socket_id, worktree_id)
        REFERENCES worktrees (session_socket_id, worktree_id)
);

CREATE TABLE worktrees (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    activated INTEGER NOT NULL CHECK (activated IN (0, 1)),
    paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
    deactivated INTEGER NOT NULL CHECK (deactivated IN (0, 1)),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (session_socket_id, worktree_id)
);

CREATE TRIGGER immutable_acknowledgements_update
BEFORE UPDATE ON delivery_acknowledgements BEGIN
    SELECT RAISE(ABORT, 'delivery acknowledgements are immutable');
END;

CREATE TRIGGER immutable_attempts_update
BEFORE UPDATE ON delivery_attempts BEGIN
    SELECT RAISE(ABORT, 'delivery attempts are immutable');
END;

CREATE TRIGGER immutable_dispositions_update
BEFORE UPDATE ON delivery_dispositions BEGIN
    SELECT RAISE(ABORT, 'delivery dispositions are immutable');
END;

CREATE TRIGGER immutable_messages_update
BEFORE UPDATE ON collaboration_messages BEGIN
    SELECT RAISE(ABORT, 'collaboration messages are immutable');
END;

CREATE TRIGGER immutable_recipients_update
BEFORE UPDATE ON message_recipients BEGIN
    SELECT RAISE(ABORT, 'message recipients are immutable');
END;

CREATE TRIGGER immutable_references_update
BEFORE UPDATE ON context_references BEGIN
    SELECT RAISE(ABORT, 'context references are immutable');
END;

CREATE TRIGGER immutable_thread_snapshots_update
BEFORE UPDATE ON thread_context_snapshots BEGIN
    SELECT RAISE(ABORT, 'thread context snapshots are immutable');
END;

CREATE TRIGGER immutable_transitions_update
BEFORE UPDATE ON transition_facts BEGIN
    SELECT RAISE(ABORT, 'transition facts are immutable');
END;

CREATE TRIGGER immutable_working_snapshots_update
BEFORE UPDATE ON working_context_snapshots BEGIN
    SELECT RAISE(ABORT, 'working context snapshots are immutable');
END;

CREATE UNIQUE INDEX one_active_alias_per_address
    ON alias_claims (session_socket_id, worktree_id, address_id)
    WHERE claim_state = 'active';

CREATE UNIQUE INDEX one_active_attempt_per_address
    ON delivery_attempt_state (session_socket_id, worktree_id, address_id)
    WHERE active = 1;

CREATE UNIQUE INDEX one_active_binding_per_address
    ON pane_bindings (session_socket_id, worktree_id, address_id) WHERE active = 1;

CREATE UNIQUE INDEX one_active_binding_per_pane
    ON pane_bindings (session_socket_id, worktree_id, pane_id) WHERE active = 1;

CREATE UNIQUE INDEX one_attention_item_per_unresolved_source
    ON attention_items (session_socket_id, worktree_id, source_kind, source_id)
    WHERE resolved_at_ms IS NULL;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "namedrop-store-test-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            crate::diagnostic_evidence::enable(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            crate::diagnostic_evidence::dump(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn channel_asks_use_stored_recipient_identity_and_operator_selections() {
        let directory = TestDirectory::new();
        let mut store = StateStore::open(&directory.0).unwrap();
        let ns = StateNamespace::for_space("socket", "workspace");
        let at = MutationAttribution::new(ActorKind::Human, "test", 1);
        store
            .transaction(|tx| {
                tx.activate_worktree(&ns, &at)?;
                tx.ensure_operator_author(&ns, &at)?;
                for alias in ["asker", "answerer", "replacement"] {
                    tx.create_pane_address(
                        &ns,
                        &PaneAddressDraft {
                            id: PaneAddressId::new(alias),
                            role_id: crate::PaneRoleId::new(alias),
                            role_name: alias.to_owned(),
                            role_description: alias.to_owned(),
                            alias: alias.to_owned(),
                        },
                        &at,
                    )?;
                }
                let root = MessageDraft {
                    thread_id: ThreadId::new("thread"),
                    message_id: MessageId::new("root"),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new("operator:local"),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "operator".to_owned(),
                    author_agent_session_snapshot: None,
                    request_id: "root".to_owned(),
                    authored_source: "A room statement.".to_owned(),
                    body: "A room statement.".to_owned(),
                    grammar_version: "channel/v1".to_owned(),
                    recipients: Vec::new(),
                    context_references: Vec::new(),
                };
                tx.create_thread(&ns, &root.thread_id, "test", &at)?;
                tx.append_message(&ns, &root, &at)?;
                assert!(matches!(
                    tx.deliver_channel_message(
                        &ns,
                        &root.message_id,
                        &["answerer".to_owned()],
                        &at
                    )?,
                    channel_routing::ChannelDelivery::Delivered(_)
                ));
                assert_eq!(
                    tx.message_route(&ns, &root.message_id)?
                        .asker_for(&PaneAddressId::new("answerer"))
                        .map(|asker| asker.address_id.as_str()),
                    Some("operator:local")
                );
                let recipient = crate::MessageRecipientDraft {
                    address_id: PaneAddressId::new("answerer"),
                    alias_snapshot: "answerer".to_owned(),
                    agent_session_snapshot: None,
                    delivery_id: crate::DeliveryId::new("followup-delivery"),
                };
                let followup = MessageDraft {
                    message_id: MessageId::new("followup"),
                    request_id: "followup".to_owned(),
                    in_reply_to: Some(root.message_id.clone()),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: vec![recipient.clone()],
                    ..root.clone()
                };
                tx.append_message(&ns, &followup, &at)?;
                assert!(
                    tx.channel_routing_state(&ns, &followup.message_id)?
                        .is_none()
                );
                assert_eq!(
                    tx.message_route(&ns, &followup.message_id)?
                        .asker_for(&PaneAddressId::new("answerer"))
                        .map(|asker| asker.address_id.as_str()),
                    Some("operator:local")
                );
                let mention_root = MessageDraft {
                    thread_id: ThreadId::new("mention-thread"),
                    message_id: MessageId::new("mention-root"),
                    request_id: "mention-root".to_owned(),
                    in_reply_to: None,
                    recipients: vec![crate::MessageRecipientDraft {
                        delivery_id: crate::DeliveryId::new("mention-root-delivery"),
                        ..recipient.clone()
                    }],
                    ..followup.clone()
                };
                tx.create_thread(&ns, &mention_root.thread_id, "mention test", &at)?;
                tx.append_message(&ns, &mention_root, &at)?;
                let routed_followup = MessageDraft {
                    message_id: MessageId::new("routed-followup"),
                    request_id: "routed-followup".to_owned(),
                    in_reply_to: Some(mention_root.message_id.clone()),
                    authored_source: "Please ask @replacement.".to_owned(),
                    body: "Please ask @replacement.".to_owned(),
                    recipients: Vec::new(),
                    ..mention_root
                };
                tx.append_pending_channel(&ns, &routed_followup, &at)?;
                let claim = tx.claim_channel(2)?.unwrap();
                assert!(tx.finish_channel(&claim, &[], false, 3)?);
                let route = tx.message_route(&ns, &routed_followup.message_id)?;
                assert_eq!(route.reply_mode, crate::DeliveryReplyMode::AgentThread);
                assert_eq!(
                    route
                        .asker_for(&PaneAddressId::new("answerer"))
                        .map(|asker| asker.address_id.as_str()),
                    Some("operator:local")
                );
                assert!(
                    route
                        .asker_for(&PaneAddressId::new("replacement"))
                        .is_none()
                );
                tx.replace_alias(&ns, &PaneAddressId::new("answerer"), "renamed", &at)?;
                tx.release_alias(&ns, "answerer", &at)?;
                tx.replace_alias(&ns, &PaneAddressId::new("replacement"), "answerer", &at)?;
                for (index, source, asked) in [
                    (0, "@operator Ask @answerer.", true),
                    (1, r"@operator Report from \@answerer.", false),
                    (2, "@operator Report from `@answerer`.", false),
                    (3, "@operator Report from #remote @answerer.", false),
                    (4, "@operator The number is 42.", false),
                    (5, "@operator Ask @replacement.", false),
                ] {
                    let message = MessageDraft {
                        message_id: MessageId::new(format!("question-{index}")),
                        request_id: format!("question-{index}"),
                        author_address_id: PaneAddressId::new("asker"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "asker".to_owned(),
                        authored_source: source.to_owned(),
                        body: source.to_owned(),
                        recipients: vec![crate::MessageRecipientDraft {
                            delivery_id: crate::DeliveryId::new(format!("delivery-{index}")),
                            ..recipient.clone()
                        }],
                        ..followup.clone()
                    };
                    tx.append_message(&ns, &message, &at)?;
                    let route = tx.message_route(&ns, &message.message_id)?;
                    assert_eq!(
                        route
                            .asker_for(&PaneAddressId::new("answerer"))
                            .map(|asker| asker.address_id.as_str()),
                        asked.then_some("asker"),
                        "{source}"
                    );
                    assert!(
                        route
                            .asker_for(&PaneAddressId::new("replacement"))
                            .is_none(),
                        "{source}"
                    );
                    assert!(
                        route.asker_for(&PaneAddressId::new("asker")).is_none(),
                        "{source}"
                    );
                }
                for (id, thread_id, in_reply_to, grammar) in [
                    ("new-ask", "thread", None, "delegated/v1"),
                    (
                        "agent-thread-ask",
                        "mention-thread",
                        Some("mention-root"),
                        "mention/v1",
                    ),
                ] {
                    let message = MessageDraft {
                        thread_id: ThreadId::new(thread_id),
                        message_id: MessageId::new(id),
                        request_id: id.to_owned(),
                        in_reply_to: in_reply_to.map(MessageId::new),
                        grammar_version: grammar.to_owned(),
                        author_address_id: PaneAddressId::new("asker"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "asker".to_owned(),
                        authored_source: "@answerer What is six times seven?".to_owned(),
                        body: "What is six times seven?".to_owned(),
                        recipients: vec![crate::MessageRecipientDraft {
                            delivery_id: crate::DeliveryId::new(format!("delivery-{id}")),
                            ..recipient.clone()
                        }],
                        ..followup.clone()
                    };
                    tx.append_message(&ns, &message, &at)?;
                    assert_eq!(
                        tx.message_route(&ns, &message.message_id)?
                            .asker_for(&PaneAddressId::new("answerer"))
                            .map(|asker| asker.address_id.as_str()),
                        Some("asker"),
                        "{id}"
                    );
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn operator_terminal_conversion_is_refused() {
        assert_operator_terminal_mutation_refused("set");
    }

    #[test]
    fn operator_allow_remote_is_refused() {
        assert_operator_terminal_mutation_refused("allow");
    }

    #[test]
    fn operator_deny_remote_is_refused() {
        assert_operator_terminal_mutation_refused("deny");
    }

    fn assert_operator_terminal_mutation_refused(operation: &str) {
        let directory = TestDirectory::new();
        let mut store = StateStore::open(&directory.0).unwrap();
        let ns = StateNamespace::for_space("socket", "workspace");
        let at = MutationAttribution::new(ActorKind::Human, "test", 1);
        let id = PaneAddressId::new("operator:local");
        let invoker = crate::DiscoveredPane {
            pane_id: "invoker".to_owned(),
            terminal_id: "terminal".to_owned(),
            tab_id: "tab".to_owned(),
            workspace_id: "workspace".to_owned(),
            cwd: None,
            pane_label: None,
            agent_session_id: None,
            agent: None,
            occupant_label: None,
            agent_status: None,
            idle_state_label: None,
        };
        store
            .transaction(|tx| {
                tx.activate_worktree(&ns, &at)?;
                tx.ensure_operator_author(&ns, &at)?;
                Ok(())
            })
            .unwrap();
        let error = store
            .transaction(|tx| match operation {
                "set" => tx.set_pane_type(&ns, &id, crate::PaneType::Terminal, &at),
                "allow" => tx
                    .allow_remote_commands(&ns, &id, &invoker, &[], &at)
                    .map(|_| ()),
                "deny" => tx.deny_remote_commands(&ns, &id, &at).map(|_| ()),
                _ => unreachable!(),
            })
            .expect_err(operation);
        assert_eq!(error.kind(), StoreErrorKind::InvalidInput, "{operation}");
        assert_eq!(
            error.message(),
            "the operator has no pane lifecycle",
            "{operation}"
        );
        store
            .transaction(|tx| {
                assert!(tx.is_operator_address(&ns, &id)?);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn status_reads_interrupt_long_queries_and_reject_writes() {
        use std::{sync::atomic::AtomicBool, time::Instant};
        let directory = TestDirectory::new();
        drop(StateStore::open(&directory.0).unwrap());
        let cancelled = AtomicBool::new(false);
        let value = StateStore::with_read_only_timeout(
            &directory.0,
            Duration::from_secs(1),
            &cancelled,
            |store| {
                let store = store.unwrap();
                assert!(
                    store
                        .connection
                        .execute("CREATE TABLE forbidden (id INTEGER)", [])
                        .is_err()
                );
                Ok(store
                    .connection
                    .query_row("SELECT 42", [], |row| row.get::<_, i64>(0))?)
            },
        )
        .unwrap();
        assert_eq!(value, 42);
        let start = Instant::now();
        let result = StateStore::with_read_only_timeout(
            &directory.0,
            Duration::from_millis(30),
            &cancelled,
            |store| {
                Ok(store.unwrap().connection.query_row(
                "WITH RECURSIVE numbers(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM numbers WHERE x<1000000000) SELECT sum(x) FROM numbers",
                [], |row| row.get::<_, i64>(0))?)
            },
        );
        assert!(result.is_err(), "a billion-row query must be interrupted");
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(
            StateStore::open_read_only(&directory.0)
                .unwrap()
                .unwrap()
                .schema_version()
                .unwrap(),
            SCHEMA_VERSION
        );
    }

    #[test]
    fn initializes_the_core_schema() {
        let directory = TestDirectory::new();
        let store = StateStore::open(&directory.0).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let tables = namespaced_tables(&store.connection, "main").unwrap();
        assert!(tables.contains(&"pane_addresses".to_owned()));
        assert!(tables.contains(&"collaboration_messages".to_owned()));
        assert!(!tables.iter().any(|table| table.contains("timeline")));
        assert!(
            !tables
                .iter()
                .any(|table| table.contains("conversation_workspace"))
        );
        assert!(!tables.iter().any(|table| table.contains("acp")));
    }

    fn schema_shape(connection: &Connection) -> String {
        let mut statement = connection
            .prepare(
                "SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL
                  ORDER BY type, name",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                let sql: String = row.get(2)?;
                let structure = sql
                    .lines()
                    .map(|line| line.split_once("--").map_or(line, |(code, _)| code))
                    .flat_map(str::split_whitespace)
                    .collect::<Vec<_>>()
                    .join(" ");
                Ok(format!(
                    "{} {}: {structure}\n",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?
                ))
            })
            .unwrap()
            .collect::<Result<String, _>>()
            .unwrap()
    }

    #[test]
    fn a_schema_change_without_a_version_bump_is_refused() {
        let directory = TestDirectory::new();
        let store = StateStore::open(&directory.0).unwrap();
        let shape = schema_shape(&store.connection);
        let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("src/store/schema-v{SCHEMA_VERSION}.txt"));
        if std::env::var_os("NAMEDROP_WRITE_SCHEMA_GOLDEN").is_some() {
            fs::write(&golden, &shape).unwrap();
        }
        let pinned = fs::read_to_string(&golden).unwrap_or_else(|_| {
            panic!(
                "schema {SCHEMA_VERSION} has no pinned shape at {}; write it with \
                 NAMEDROP_WRITE_SCHEMA_GOLDEN=1 once its migration step exists",
                golden.display()
            )
        });
        let actual: Vec<&str> = shape.lines().collect();
        let expected: Vec<&str> = pinned.lines().collect();
        if let Some(line) = (0..actual.len().max(expected.len()))
            .find(|&line| actual.get(line) != expected.get(line))
        {
            panic!(
                "a fresh database at schema {SCHEMA_VERSION} differs from {} at line {}:\n  \
                 now:    {}\n  pinned: {}\n\
                 Installed databases already at schema {SCHEMA_VERSION} would never receive \
                 this change. Bump SCHEMA_VERSION, add its step to initialize_schema, and \
                 write the new file with NAMEDROP_WRITE_SCHEMA_GOLDEN=1.",
                golden.display(),
                line + 1,
                actual.get(line).copied().unwrap_or("<nothing>"),
                expected.get(line).copied().unwrap_or("<nothing>")
            );
        }
    }
}
