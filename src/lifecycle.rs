use crate::integration::{onboard_settled_integrations, pane_agent_runtime};
use crate::mux::{
    AgentIdentity, AgentKind, AgentState, Applied, Depth, Effect, Gone, MuxAct, MuxError, MuxEvent,
    MuxRead, PaneFacts, PaneId, PaneSlot, PopupScreen, PopupSpec, Scope, StateTexts, Survey, TabId,
    TerminalId, resolve_session,
};
use crate::{
    ActorKind, AgentSessionId, AttentionItemRecord, Clock, IdGenerator, MutationAttribution,
    PaneAddressDraft, PaneAddressId, PaneAddressRecord, PaneBindingId, PaneRoleId, PaneRolePreset,
    StateNamespace, StateStore, StateTransaction, StoreError, WorktreeId, WorktreeState,
    normalize_alias, publish_status_metadata, reconcile_delivery_state, validate_alias,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredWorktree {
    pub worktree_id: WorktreeId,
    pub workspace_id: String,
    pub worktree_attached: bool,
    pub label: Option<String>,
    pub branch: Option<String>,
    pub panes: Vec<DiscoveredPane>,
}

impl DiscoveredWorktree {
    pub fn namespace(&self, session_socket_id: impl Into<String>) -> StateNamespace {
        StateNamespace::for_space(session_socket_id, self.workspace_id.clone())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPane {
    pub pane_id: String,
    pub terminal_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    pub cwd: Option<String>,
    pub pane_label: Option<String>,
    /// `None` exactly when the pane is a free terminal.
    pub agent: Option<AgentIdentity>,
    pub agent_status: Option<AgentState>,
    /// The label Herdr currently shows for this pane while it is idle.
    pub idle_state_label: Option<String>,
}

impl DiscoveredPane {
    pub fn agent_session_id(&self) -> Option<&AgentSessionId> {
        self.agent
            .as_ref()
            .and_then(|agent| resolve_session(&agent.session))
    }

    /// Whether the multiplexer names the agent running here, as Herdr's
    /// agent word did. A pane that reports only a session is still an agent
    /// pane, but it names no agent.
    pub fn reports_kind(&self) -> bool {
        self.agent
            .as_ref()
            .is_some_and(|agent| agent.kind != AgentKind::Unreported)
    }

    pub fn occupant_label(&self) -> Option<&str> {
        self.agent.as_ref()?.label.as_deref()
    }

    pub fn accepts_prompt(&self) -> bool {
        self.agent_status
            .as_ref()
            .is_some_and(AgentState::accepts_prompt)
    }

    /// The facts discovery keeps, for an effect that names its pane. It keeps
    /// neither the viewport rows nor whether a session was reported.
    pub fn facts(&self) -> PaneFacts {
        PaneFacts {
            id: PaneId::new(&self.pane_id),
            terminal: TerminalId::new(&self.terminal_id),
            tab: TabId::new(&self.tab_id),
            space: crate::SpaceId::new(&self.workspace_id),
            cwd: self.cwd.clone(),
            title: self.pane_label.clone(),
            agent: self.agent.clone(),
            state: self.agent_status.clone(),
            idle_label: self.idle_state_label.clone(),
            viewport_rows: None,
            reports_session: false,
        }
    }
}

#[derive(Clone, Copy)]
pub struct LifecycleRuntime<'a, M, C, I> {
    pub state_directory: &'a Path,
    /// The user directory holding the shared Agent Integration package targets
    /// every Worktree on this machine installs into. `None` when no user home
    /// directory could be resolved, which blocks installation instead of
    /// guessing a target.
    pub home_directory: Option<&'a Path>,
    pub agent_config: crate::AgentConfigDirectories<'a>,
    pub cwd: &'a Path,
    pub workspace_id: Option<&'a str>,
    pub current_pane_id: Option<&'a str>,
    pub session_socket_id: &'a str,
    pub mux: &'a M,
    pub clock: &'a C,
    pub ids: &'a I,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivateInput {
    #[serde(default)]
    confirmed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameInput {
    alias: String,
    pane_type: Option<crate::PaneType>,
    /// Omitted or null keeps whatever persona is already saved, so naming the
    /// same pane again never silently redefines the agent's role.
    persona: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PurgeInput {
    #[serde(alias = "typed_worktree_name")]
    typed_space_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressCreateInput {
    preset: Option<PaneRolePreset>,
    role_name: Option<String>,
    role_description: Option<String>,
    alias: Option<String>,
    pane_id: Option<String>,
}

struct ResolvedRoleInput {
    name: String,
    description: String,
    suggested_alias: String,
    alias_override: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressInput {
    address_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonaInput {
    address_id: String,
    /// Null clears. The field itself is required, checked on the raw input
    /// because serde cannot tell an absent field from an explicit null, so an
    /// edit that forgot the text cannot silently clear a saved persona.
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindInput {
    address_id: String,
    pane_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameInput {
    address_id: String,
    alias: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseAliasInput {
    alias: String,
}

pub fn execute_lifecycle_operation<M, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, M, C, I>,
) -> Result<Value, String>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let discovered = match runtime.workspace_id {
        Some(workspace_id) => discover_reminder_workspace(
            runtime.state_directory,
            &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
            runtime.mux,
        )?,
        None => discover_worktree(runtime.mux, runtime.cwd)?,
    };
    if discovered.worktree_attached && matches!(operation, "activate" | "name") {
        crate::install::codex_sandbox::check_worktree(
            runtime.state_directory,
            Path::new(discovered.worktree_id.as_str()),
        )?;
    }
    let namespace = discovered.namespace(runtime.session_socket_id);
    let (actor_kind, actor_id) = if operation.starts_with("event.") || operation == "reconcile" {
        (ActorKind::Plugin, multiplexer_word(runtime))
    } else {
        (ActorKind::Human, "local-human")
    };
    let attribution = MutationAttribution::new(actor_kind, actor_id, runtime.clock.now_unix_ms());
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;

    match operation {
        "name" => {
            let input: NameInput = parse_input(input)?;
            let supplied_alias = required_nonempty(input.alias, "alias")?;
            let alias = supplied_alias
                .trim()
                .strip_prefix('@')
                .unwrap_or(supplied_alias.trim())
                .to_owned();
            validate_alias(&alias)?;
            let persona = input.persona.as_deref().map(normalize_persona);
            let pane_id = runtime
                .current_pane_id
                .ok_or_else(|| "name must run from the Herdr pane being named".to_owned())?;
            let pane = discovered
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)
                .ok_or_else(|| "the current pane is not live in this space".to_owned())?;
            let agent_runtime = pane_agent_runtime(pane);
            let existing_type = store
                .transaction(|transaction| {
                    Ok(transaction
                        .list_pane_addresses(&namespace)?
                        .into_iter()
                        .find(|a| a.pane_id.as_deref() == Some(pane_id))
                        .map(|a| a.pane_type))
                })
                .map_err(|e| e.to_string())?;
            let pane_type = match input.pane_type.or(existing_type) {
                Some(kind) => kind,
                None => crate::detect_pane_type(runtime)?.ok_or_else(|| {
                    "pane type is uncertain; choose --type terminal or --type agent".to_owned()
                })?,
            };
            if pane_type == crate::PaneType::Agent && pane.reports_kind() && agent_runtime.is_none()
            {
                return Err(
                    "the current pane must contain a recognized Codex or Claude session".to_owned(),
                );
            }
            let registration_required = pane_type == crate::PaneType::Agent;
            let agent_session_id = pane.agent_session_id();
            let was_activated = store
                .transaction(|transaction| transaction.worktree_state(&namespace))
                .map_err(|error| error.to_string())?
                .is_some_and(|state| state.activated && !state.deactivated);
            let (_sandbox_package_guard, sandbox_current) =
                crate::installed_runtime::package_guard()?;
            let address_id = store
                .transaction(|transaction| {
                    if sandbox_current
                        && let Some(home) = runtime.home_directory
                        && !transaction
                            .worktree_state(&namespace)?
                            .is_some_and(|state| state.activated && !state.deactivated)
                    {
                        crate::install::codex_sandbox::activate(
                            &runtime.agent_config.codex(home),
                            runtime.state_directory,
                        )
                        .map_err(StoreError::invalid_input)?;
                    }
                    transaction.activate_worktree(&namespace, &attribution)?;
                    if let Some(binding) = transaction
                        .active_pane_bindings(&namespace)?
                        .into_iter()
                        .find(|binding| binding.pane_id == pane_id)
                    {
                        let current = transaction
                            .list_pane_addresses(&namespace)?
                            .into_iter()
                            .find(|address| address.id == binding.address_id)
                            .ok_or_else(|| {
                                StoreError::invalid_input(
                                    "the current pane's Pane Address is not active",
                                )
                            })?;
                        if current.alias != alias {
                            transaction.replace_alias(
                                &namespace,
                                &binding.address_id,
                                &alias,
                                &attribution,
                            )?;
                        }
                        transaction.mark_native_pane_address(
                            &namespace,
                            &binding.address_id,
                            &alias,
                        )?;
                        transaction.set_pane_type(
                            &namespace,
                            &binding.address_id,
                            pane_type,
                            &attribution,
                        )?;
                        if let Some(persona) = &persona {
                            transaction.set_address_persona(
                                &namespace,
                                &binding.address_id,
                                persona.as_deref(),
                            )?;
                        }
                        observe_discovered_occupant(
                            transaction,
                            &namespace,
                            &binding.address_id,
                            pane,
                            runtime.ids,
                            &attribution,
                        )?;
                        return Ok(binding.address_id);
                    }

                    let address_id = PaneAddressId::new(runtime.ids.next_id("pane-address"));
                    let draft = PaneAddressDraft {
                        id: address_id.clone(),
                        role_id: PaneRoleId::new(runtime.ids.next_id("pane-role")),
                        role_name: format!("@{alias}"),
                        role_description: format!("Native collaborator addressed as @{alias}."),
                        alias: alias.clone(),
                    };
                    transaction.create_pane_address(&namespace, &draft, &attribution)?;
                    transaction.set_pane_type(&namespace, &address_id, pane_type, &attribution)?;
                    if let Some(persona) = &persona {
                        transaction.set_address_persona(
                            &namespace,
                            &address_id,
                            persona.as_deref(),
                        )?;
                    }
                    transaction.bind_pane(
                        &namespace,
                        &PaneBindingId::new(runtime.ids.next_id("pane-binding")),
                        &address_id,
                        pane_id,
                        &attribution,
                    )?;
                    observe_discovered_occupant(
                        transaction,
                        &namespace,
                        &address_id,
                        pane,
                        runtime.ids,
                        &attribution,
                    )?;
                    Ok(address_id)
                })
                .map_err(|error| error.to_string())?;
            drop(_sandbox_package_guard);
            let reconciliation = reconcile_worktree(
                &mut store,
                &namespace,
                &discovered,
                runtime,
                &attribution,
                false,
            )?;
            let served =
                crate::codex_app_server::served_codex_panes(runtime, std::slice::from_ref(pane));
            publish_pane_address_metadata(
                runtime,
                pane_id,
                &alias,
                PaneLabel::for_pane(pane, served.as_ref()),
            )?;
            let addresses = store
                .transaction(|transaction| transaction.list_pane_addresses(&namespace))
                .map_err(|error| error.to_string())?;
            let tab_label = if tab_label_owner(&discovered.panes, &pane.tab_id, &addresses)
                .is_some_and(|owner| owner.pane_id == pane.pane_id)
            {
                apply_managed_tab_label(
                    &mut store,
                    &namespace,
                    &pane.tab_id,
                    &address_id,
                    &format!("@{alias}"),
                    runtime.mux,
                )
            } else {
                json!({"status": "unchanged", "reason": "tab_has_multiple_agents"})
            };
            let registered = if registration_required {
                if let Some(agent_session_id) = agent_session_id {
                    StateStore::open(runtime.state_directory)
                        .map_err(|error| error.to_string())?
                        .transaction(|transaction| {
                            transaction.active_integration_registration(
                                &namespace,
                                &address_id,
                                agent_session_id,
                            )
                        })
                        .map_err(|error| error.to_string())?
                        .is_some()
                } else {
                    false
                }
            } else {
                false
            };
            Ok(json!({
                "alias": format!("@{alias}"),
                "pane_type": pane_type,
                "type_override": "name <alias> --type terminal|agent",
                "pane_id": pane_id,
                "agent_session_id": agent_session_id,
                "native_session_setup": agent_runtime.filter(|_| agent_session_id.is_none() && served.is_none()).map(crate::install::native_session_setup),
                "codex_app_server": served.as_ref().map(crate::codex_app_server::ServedPanes::advice),
                "runtime": agent_runtime,
                "activated": true,
                "activation_was_needed": !was_activated,
                "registration_required": registration_required,
                "registered": registered,
                "ready": if pane_type == crate::PaneType::Terminal {
                    pane.agent.is_none()
                        && crate::detect_pane_type(runtime)? == Some(crate::PaneType::Terminal)
                } else { registered && agent_runtime.is_none_or(|runtime| !reconciliation.installation.failed(runtime)) },
                "readiness_scope": if pane_type == crate::PaneType::Terminal {"terminal shell availability; rechecked before execution"} else {"package_and_registration_only; runtime hooks remain unverified"},
                "integration_packages": reconciliation.installation.report(runtime.home_directory, runtime.state_directory, runtime.agent_config),
                "tab_label": tab_label,
                "drain": reconciliation.drain,
                "hook_activation": reconciliation.installation.hook_activation(runtime.home_directory, runtime.agent_config),
            }))
        }
        "activate" => {
            let input: ActivateInput = parse_input(input)?;
            if !input.confirmed {
                let hook_activation = crate::user_hook_activation_guidance(
                    runtime.home_directory,
                    runtime.agent_config,
                );
                return Err(format!(
                    "activation requires confirmation: Space={}, state_directory={}, shared_agent_integration_packages={{{}}} created or refreshed on activation and shared by every space on this machine, agent_integration_hook_activation={}, safe_defaults={{settled_delay_ms:500, ordinary_panes_auto_registered:false, routing_scope:space}}",
                    discovered.workspace_id,
                    runtime.state_directory.display(),
                    crate::disclosed_user_package_targets(
                        runtime.home_directory,
                        runtime.agent_config
                    ),
                    hook_activation
                ));
            }
            let shell_command = crate::install::install_shell_command(
                runtime
                    .home_directory
                    .ok_or("no HOME is available for shell command installation")?,
                Path::new(&crate::collaboration_executable()?),
                runtime.state_directory,
            )?;
            let (_sandbox_package_guard, sandbox_current) =
                crate::installed_runtime::package_guard()?;
            store
                .transaction(|transaction| {
                    if sandbox_current {
                        crate::install::codex_sandbox::activate(
                            &runtime
                                .agent_config
                                .codex(runtime.home_directory.ok_or_else(|| {
                                    StoreError::invalid_input("no HOME is available")
                                })?),
                            runtime.state_directory,
                        )
                        .map_err(StoreError::invalid_input)?;
                    }
                    transaction.activate_worktree(&namespace, &attribution)?;
                    Ok(())
                })
                .map_err(|error| error.to_string())?;
            drop(_sandbox_package_guard);
            let reconciliation = reconcile_worktree(
                &mut store,
                &namespace,
                &discovered,
                runtime,
                &attribution,
                false,
            )?;
            let shortcuts =
                crate::shortcuts::activate_shortcuts(runtime.home_directory, runtime.mux);
            // After the shortcuts, so the tour's reopen shortcut already works
            // when the popup mentions it. It reports instead of failing.
            let tour = crate::tour::show_for_first_agent(runtime, &discovered, None);
            Ok(json!({
                "space_id": discovered.workspace_id,
                "worktree_id": discovered.worktree_attached.then_some(discovered.worktree_id),
                "state_directory": runtime.state_directory,
                "shortcuts": shortcuts,
                "activated": true,
                "shell_command": shell_command,
                "codex_sandbox": {"writable_root": crate::install::codex_sandbox::root(runtime.state_directory)?, "guidance": "Start a new Codex session to load the deferred-commands spool writable root. Explicit permission profiles require their own spool-directory grant."},
                "integration_packages": reconciliation.installation.report(runtime.home_directory, runtime.state_directory, runtime.agent_config),
                "settled_delay_ms": 500,
                "agent_integration_hook_activation":
                    reconciliation.installation.hook_activation(runtime.home_directory, runtime.agent_config),
                "reconciliation_epoch": reconciliation.epoch,
                "drain": reconciliation.drain,
                "tour": tour
            }))
        }
        "deactivate" => {
            parse_input::<EmptyInput>(input)?;
            store
                .transaction(|transaction| {
                    transaction.deactivate_worktree(&namespace, &attribution)
                })
                .map_err(|error| error.to_string())?;
            let _ = reconcile_managed_tab_labels(&mut store, &namespace, &discovered, runtime);
            let permission_removal =
                crate::install::remove_claude_permissions(runtime.state_directory);
            let sandbox_removal = crate::install::codex_sandbox::remove(runtime.state_directory);
            let subagent_removal = crate::install::remove_subagent_hooks(
                runtime.state_directory,
                runtime.home_directory,
            );
            Ok(json!({
                "space_id": discovered.workspace_id,
                "worktree_id": discovered.worktree_attached.then_some(discovered.worktree_id),
                "codex_sandbox": {"removed_entries": sandbox_removal.as_ref().ok(), "error": sandbox_removal.err()},
                "subagent_hooks": {"removed_entries": subagent_removal.as_ref().ok(), "error": subagent_removal.err()},
                "claude_permissions": {"removed": permission_removal.as_ref().is_ok_and(|count| *count > 0), "removed_entries": permission_removal.as_ref().ok(), "error": permission_removal.err()},
                "deactivated": true,
                "state_preserved": true
            }))
        }
        "purge" => {
            let input: PurgeInput = parse_input(input)?;
            let Some(typed_space_id) = input.typed_space_id else {
                let summary = store
                    .purge_summary(&namespace)
                    .map_err(|error| error.to_string())?;
                return Ok(json!({
                    "space_id": summary.space_id,
                    "worktree_name": summary.space_id,
                    "deactivated": summary.deactivated,
                    "nonterminal_deliveries": summary.nonterminal_deliveries,
                    "affected_records": summary.affected_records,
                    "confirmation_required": true,
                    "purged": false
                }));
            };
            let result = store
                .purge_worktree(&namespace, &typed_space_id, &attribution)
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "space_id": result.summary.space_id,
                "worktree_name": result.summary.space_id,
                "deactivated": result.summary.deactivated,
                "nonterminal_deliveries": result.summary.nonterminal_deliveries,
                "affected_records": result.summary.affected_records,
                "purged": true,
                "forensic_erasure": false,
                "maintenance_complete": result.maintenance_complete,
                "maintenance_warnings": result.maintenance_warnings
            }))
        }
        "address.create" => {
            let input: AddressCreateInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let pane_id = input
                .pane_id
                .clone()
                .or_else(|| runtime.current_pane_id.map(str::to_owned))
                .ok_or_else(|| "pane_id is required outside a Herdr pane action".to_owned())?;
            let pane = discovered
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)
                .ok_or_else(|| "pane_id is not a live pane in the canonical Worktree".to_owned())?;
            let role = role_input(input)?;
            let alias = match role.alias_override {
                Some(alias) => alias,
                None => role.suggested_alias.trim_start_matches('@').to_owned(),
            };
            let draft = PaneAddressDraft {
                id: PaneAddressId::new(runtime.ids.next_id("pane-address")),
                role_id: PaneRoleId::new(runtime.ids.next_id("pane-role")),
                role_name: role.name,
                role_description: role.description,
                alias,
            };
            let binding_id = PaneBindingId::new(runtime.ids.next_id("pane-binding"));
            let record = store
                .transaction(|transaction| {
                    transaction.create_pane_address(&namespace, &draft, &attribution)?;
                    transaction.bind_pane(
                        &namespace,
                        &binding_id,
                        &draft.id,
                        &pane_id,
                        &attribution,
                    )?;
                    observe_discovered_occupant(
                        transaction,
                        &namespace,
                        &draft.id,
                        pane,
                        runtime.ids,
                        &attribution,
                    )?;
                    let record = transaction
                        .list_pane_addresses(&namespace)?
                        .into_iter()
                        .find(|record| record.id == draft.id)
                        .ok_or_else(|| {
                            StoreError::invalid_input("created Pane Address is not visible")
                        })?;
                    Ok(enrich_record(record, &discovered.panes))
                })
                .map_err(|error| error.to_string())?;
            onboard_settled_integrations(runtime, &discovered)?;
            serde_json::to_value(record).map_err(|error| error.to_string())
        }
        "address.list" => {
            let input: AddressListInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            match input.workspaces {
                None => address_rows(
                    &discovered,
                    runtime.session_socket_id,
                    &mut store,
                    runtime.home_directory,
                    runtime.agent_config,
                ),
                Some(AllWorkspaces::All) => {
                    every_workspace_addresses(runtime, &discovered.workspace_id, &mut store)
                }
            }
        }
        "address.bind" => {
            let input: BindInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let address_id = PaneAddressId::new(required_nonempty(input.address_id, "address_id")?);
            let pane_id = required_nonempty(input.pane_id, "pane_id")?;
            let pane = discovered
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)
                .ok_or_else(|| "pane_id is not a live pane in the canonical Worktree".to_owned())?;
            let binding_id = PaneBindingId::new(runtime.ids.next_id("pane-binding"));
            let record = mutate_address(
                &mut store,
                &namespace,
                &address_id,
                &discovered.panes,
                |transaction| {
                    transaction.rebind_pane(
                        &namespace,
                        &binding_id,
                        &address_id,
                        &pane_id,
                        &attribution,
                    )?;
                    observe_discovered_occupant(
                        transaction,
                        &namespace,
                        &address_id,
                        pane,
                        runtime.ids,
                        &attribution,
                    )
                },
            )?;
            onboard_settled_integrations(runtime, &discovered)?;
            let _ = reconcile_managed_tab_labels(&mut store, &namespace, &discovered, runtime);
            Ok(record)
        }
        "address.unbind" => {
            let input: AddressInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let address_id = PaneAddressId::new(required_nonempty(input.address_id, "address_id")?);
            let record = mutate_address(
                &mut store,
                &namespace,
                &address_id,
                &discovered.panes,
                |transaction| transaction.unbind_pane(&namespace, &address_id, &attribution),
            )?;
            let _ = reconcile_managed_tab_labels(&mut store, &namespace, &discovered, runtime);
            Ok(record)
        }
        "address.rename" => {
            let input: RenameInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let address_id = PaneAddressId::new(required_nonempty(input.address_id, "address_id")?);
            let alias = required_nonempty(input.alias, "alias")?;
            let record = mutate_address(
                &mut store,
                &namespace,
                &address_id,
                &discovered.panes,
                |transaction| {
                    transaction.replace_alias(&namespace, &address_id, &alias, &attribution)
                },
            )?;
            let _ = reconcile_managed_tab_labels(&mut store, &namespace, &discovered, runtime);
            Ok(record)
        }
        "address.persona" => {
            if input.get("description").is_none() {
                return Err(
                    "description is required; send null or blank text to clear the persona"
                        .to_owned(),
                );
            }
            let input: PersonaInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let address_id = PaneAddressId::new(required_nonempty(input.address_id, "address_id")?);
            let persona = input.description.as_deref().and_then(normalize_persona);
            mutate_address(
                &mut store,
                &namespace,
                &address_id,
                &discovered.panes,
                |transaction| {
                    transaction.set_address_persona(&namespace, &address_id, persona.as_deref())
                },
            )
        }
        "address.release-alias" => {
            let input: ReleaseAliasInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let alias = required_nonempty(input.alias, "alias")?;
            store
                .transaction(|transaction| {
                    transaction.release_alias(&namespace, &alias, &attribution)
                })
                .map_err(|error| error.to_string())?;
            Ok(json!({"alias": alias, "released": true}))
        }
        "address.remove" => {
            let input: AddressInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let address_id = PaneAddressId::new(required_nonempty(input.address_id, "address_id")?);
            let released_aliases = store
                .transaction(|transaction| {
                    transaction.remove_pane_address(&namespace, &address_id, &attribution)
                })
                .map_err(|error| error.to_string())?;
            let _ = reconcile_managed_tab_labels(&mut store, &namespace, &discovered, runtime);
            Ok(json!({
                "address_id": address_id,
                "released_aliases": released_aliases,
                "removed": true
            }))
        }
        "address.retire" => {
            let input: AddressInput = parse_input(input)?;
            require_activated(&mut store, &namespace)?;
            let address_id = PaneAddressId::new(required_nonempty(input.address_id, "address_id")?);
            store
                .transaction(|transaction| {
                    transaction.retire_pane_address(&namespace, &address_id, &attribution)
                })
                .map_err(|error| error.to_string())?;
            let _ = reconcile_managed_tab_labels(&mut store, &namespace, &discovered, runtime);
            Ok(json!({"address_id": address_id, "lifecycle": "retired"}))
        }
        "reconcile" => {
            parse_input::<EmptyInput>(input)?;
            require_activated(&mut store, &namespace)?;
            let reconciliation = reconcile_worktree(
                &mut store,
                &namespace,
                &discovered,
                runtime,
                &attribution,
                true,
            )?;
            Ok(json!({
                "space_id": discovered.workspace_id,
                "worktree_id": discovered.worktree_attached.then_some(discovered.worktree_id),
                "reconciliation_epoch": reconciliation.epoch,
                "drain": reconciliation.drain
            }))
        }
        "event.pane-lifecycle" | "event.agent-status-changed" => {
            let event = crate::mux::herdr::decode_operation_input(operation, input)?;
            handle_event(&mut store, &attribution, runtime, &event).map_err(String::from)
        }
        _ => Err(format!("{operation} is not a lifecycle operation")),
    }
}

/// The word Namedrop records as the actor of the changes a multiplexer's
/// events make: "herdr" or "rex", by the runtime's endpoint.
fn multiplexer_word<M, C, I>(runtime: &LifecycleRuntime<'_, M, C, I>) -> &'static str {
    crate::mux::Endpoint::parse(runtime.session_socket_id)
        .kind
        .word()
}

/// How the status worker treats a failure of `handle_mux_event`. Decided
/// from the `MuxError`, never from the text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventError {
    /// Gone, or a rejected input. Retrying cannot change it, so the marker
    /// is dropped.
    Definitive(String),
    /// Timeout, unavailable, storage. The marker stays and the worker backs
    /// off.
    Retry(String),
}

impl EventError {
    fn mux(error: MuxError) -> Self {
        match error {
            MuxError::Gone(..) => Self::Definitive(error.to_string()),
            MuxError::Timeout(_)
            | MuxError::TooLarge(_)
            | MuxError::Unavailable(_)
            | MuxError::Protocol(_) => Self::Retry(error.to_string()),
        }
    }

    fn retry(error: impl ToString) -> Self {
        Self::Retry(error.to_string())
    }
}

impl std::fmt::Display for EventError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Definitive(text) | Self::Retry(text)) = self;
        formatter.write_str(text)
    }
}

impl From<EventError> for String {
    fn from(error: EventError) -> Self {
        error.to_string()
    }
}

/// Runs the handler for one multiplexer event. The status worker's Rex
/// markers enter here, and the changes they make are recorded as the
/// endpoint's multiplexer's.
pub fn handle_mux_event<M, C, I>(
    runtime: &LifecycleRuntime<'_, M, C, I>,
    event: &MuxEvent,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let attribution = MutationAttribution::new(
        ActorKind::Plugin,
        multiplexer_word(runtime),
        runtime.clock.now_unix_ms(),
    );
    let mut store = StateStore::open(runtime.state_directory).map_err(EventError::retry)?;
    handle_event(&mut store, &attribution, runtime, event)
}

/// Runs the handler for one multiplexer event. Each handler rediscovers the
/// spaces the event names, so the event carries only IDs.
fn handle_event<M, C, I>(
    store: &mut StateStore,
    attribution: &MutationAttribution,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    event: &MuxEvent,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    match event {
        MuxEvent::PaneAppeared(pane) => pane_appeared(store, runtime, pane),
        MuxEvent::PaneClosed(pane) => pane_closed(store, attribution, runtime, pane),
        MuxEvent::TabClosed { space, tab } => tab_closed(store, attribution, runtime, space, tab),
        MuxEvent::PaneMoved { from, to } => pane_moved(store, attribution, runtime, from, to),
        MuxEvent::AgentDetected(pane) => {
            agent_status_changed(store, attribution, runtime, pane, true)
        }
        MuxEvent::AgentStatusChanged(pane) => {
            agent_status_changed(store, attribution, runtime, pane, false)
        }
        MuxEvent::Resync(space) => resync(store, attribution, runtime, space),
        MuxEvent::SpaceClosed(space) => space_closed(store, attribution, runtime, space),
    }
}

/// Events for `space` may have been missed. A space the multiplexer no
/// longer has is closed. Otherwise the space is reconciled as a command's
/// `reconcile` would, except that it is no restart: an attempt whose session
/// still runs keeps going.
fn resync<M, C, I>(
    store: &mut StateStore,
    attribution: &MutationAttribution,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    space: &crate::SpaceId,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let discovered = match survey_space(runtime.mux, space.as_str()) {
        Err(MuxError::Gone(Gone::Space, _)) => {
            return space_closed(store, attribution, runtime, space);
        }
        surveyed => surveyed.map_err(EventError::mux)?,
    };
    let namespace = discovered.namespace(runtime.session_socket_id);
    check_activated(store, &namespace)?;
    let event_runtime = LifecycleRuntime {
        workspace_id: Some(space.as_str()),
        ..*runtime
    };
    let reconciliation = reconcile_worktree(
        store,
        &namespace,
        &discovered,
        &event_runtime,
        attribution,
        false,
    )
    .map_err(EventError::Retry)?;
    Ok(json!({
        "type": "space_resynced",
        "handled": true,
        "space_id": space.as_str(),
        "reconciliation_epoch": reconciliation.epoch,
        "drain": reconciliation.drain
    }))
}

/// The space is gone for good. Every pane address in it is cancelled and
/// removed, its reminders fail, and Namedrop is no longer activated for it,
/// so nothing waits on it again. Running it twice changes nothing more.
fn space_closed<M, C, I>(
    store: &mut StateStore,
    attribution: &MutationAttribution,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    space: &crate::SpaceId,
) -> Result<Value, EventError> {
    let namespace = StateNamespace::for_space(runtime.session_socket_id, space.as_str());
    let removed = store
        .transaction(|transaction| {
            let Some(state) = transaction.worktree_state(&namespace)? else {
                return Ok(Vec::new());
            };
            for binding in transaction.active_pane_bindings(&namespace)? {
                transaction.end_moves_into(
                    runtime.session_socket_id,
                    &binding.pane_id,
                    attribution,
                )?;
            }
            let mut removed = Vec::new();
            for address in transaction.list_pane_addresses(&namespace)? {
                if transaction.is_operator_address(&namespace, &address.id)? {
                    continue;
                }
                transaction.cancel_and_remove_pane_address(&namespace, &address.id, attribution)?;
                removed.push(address.id);
            }
            transaction.fail_reminder_workspace(&namespace)?;
            if !state.deactivated {
                transaction.deactivate_worktree(&namespace, attribution)?;
            }
            Ok(removed)
        })
        .map_err(EventError::retry)?;
    Ok(json!({
        "type": "space_closed",
        "handled": true,
        "space_id": space.as_str(),
        "removed_address_ids": removed
    }))
}

fn pane_appeared<M, C, I>(
    store: &mut StateStore,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    pane: &PaneSlot,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let workspace_id = pane.space.as_str();
    if let Ok(event_worktree) = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
        runtime.mux,
    ) {
        let event_namespace = event_worktree.namespace(runtime.session_socket_id);
        let event_runtime = LifecycleRuntime {
            workspace_id: Some(workspace_id),
            ..*runtime
        };
        let _ =
            reconcile_managed_tab_labels(store, &event_namespace, &event_worktree, &event_runtime);
    }
    Ok(json!({"type": "pane_created", "handled": true}))
}

fn pane_closed<M, C, I>(
    store: &mut StateStore,
    attribution: &MutationAttribution,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    pane: &PaneSlot,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let (workspace_id, pane_id) = (pane.space.as_str(), pane.pane.as_str());
    let event_worktree = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
        runtime.mux,
    )?;
    let event_namespace = event_worktree.namespace(runtime.session_socket_id);
    store
        .transaction(|transaction| {
            transaction.end_moves_into(runtime.session_socket_id, pane_id, attribution)
        })
        .map_err(EventError::retry)?;
    check_activated(store, &event_namespace)?;
    let removed_address_id = store
        .transaction(|transaction| {
            let binding = transaction
                .active_pane_bindings(&event_namespace)?
                .into_iter()
                .find(|binding| binding.pane_id == pane_id);
            if let Some(binding) = binding {
                transaction.cancel_and_remove_pane_address(
                    &event_namespace,
                    &binding.address_id,
                    attribution,
                )?;
                return Ok(Some(binding.address_id));
            }
            Ok(None)
        })
        .map_err(EventError::retry)?;
    let event_runtime = LifecycleRuntime {
        workspace_id: Some(workspace_id),
        ..*runtime
    };
    let refreshed = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
        runtime.mux,
    )?;
    let _ = reconcile_managed_tab_labels(store, &event_namespace, &refreshed, &event_runtime);
    store
        .transaction(|transaction| {
            reconcile_delivery_state(
                transaction,
                &event_namespace,
                &refreshed.panes,
                false,
                runtime.state_directory,
                attribution,
            )?;
            Ok(())
        })
        .map_err(EventError::retry)?;
    let drain = crate::drain_mailboxes(&event_runtime).map_err(EventError::Retry)?;
    Ok(json!({
        "type": "pane_unavailable",
        "handled": true,
        "removed_address_id": removed_address_id,
        "drain": drain
    }))
}

fn tab_closed<M, C, I>(
    store: &mut StateStore,
    attribution: &MutationAttribution,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    space: &crate::SpaceId,
    tab: &TabId,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let workspace_id = space.as_str();
    let event_worktree = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
        runtime.mux,
    )?;
    let event_namespace = event_worktree.namespace(runtime.session_socket_id);
    check_activated(store, &event_namespace)?;
    let removed_bindings = store
        .transaction(|transaction| {
            let addresses = transaction.list_pane_addresses(&event_namespace)?;
            let bindings = transaction.active_pane_bindings(&event_namespace)?;
            let mut removed = Vec::new();
            for binding in bindings {
                let address = addresses
                    .iter()
                    .find(|address| address.id == binding.address_id);
                let exact_pane_is_live = event_worktree.panes.iter().any(|pane| {
                    pane.pane_id == binding.pane_id
                        && address.is_some_and(|address| {
                            address.agent_session_id.as_ref() == pane.agent_session_id()
                        })
                });
                if binding.tab_id.as_deref() == Some(tab.as_str())
                    || (binding.tab_id.is_none() && !exact_pane_is_live)
                {
                    transaction.cancel_and_remove_pane_address(
                        &event_namespace,
                        &binding.address_id,
                        attribution,
                    )?;
                    removed.push(binding);
                }
            }
            Ok(removed)
        })
        .map_err(EventError::retry)?;
    let event_runtime = LifecycleRuntime {
        workspace_id: Some(workspace_id),
        ..*runtime
    };
    let refreshed = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
        runtime.mux,
    )?;
    let _ = reconcile_managed_tab_labels(store, &event_namespace, &refreshed, &event_runtime);
    let drain = crate::drain_mailboxes(&event_runtime).map_err(EventError::Retry)?;
    Ok(json!({
        "type": "tab_unavailable",
        "handled": true,
        "tab_id": tab.as_str(),
        "removed_address_ids": removed_bindings
            .into_iter()
            .map(|binding| binding.address_id)
            .collect::<Vec<_>>(),
        "drain": drain
    }))
}

fn pane_moved<M, C, I>(
    store: &mut StateStore,
    attribution: &MutationAttribution,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    from: &PaneSlot,
    to: &PaneSlot,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let previous_workspace_id = from.space.as_str();
    let destination_tab = to.tab.as_ref().ok_or_else(|| {
        EventError::Definitive("a moved pane event names no destination tab".to_owned())
    })?;
    let source = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, previous_workspace_id),
        runtime.mux,
    )?;
    let source_namespace = source.namespace(runtime.session_socket_id);
    check_activated(store, &source_namespace)?;
    let destination = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, to.space.as_str()),
        runtime.mux,
    )?;
    store
        .transaction(|transaction| {
            apply_pane_move(
                transaction,
                &source_namespace,
                from.pane.as_str(),
                to.pane.as_str(),
                destination_tab.as_str(),
                &destination.workspace_id,
                attribution,
            )?;
            if destination.workspace_id == source_namespace.space_id.as_str() {
                reconcile_compacted_pane_bindings(
                    transaction,
                    &source_namespace,
                    &destination.panes,
                    attribution,
                )?;
            }
            Ok(())
        })
        .map_err(EventError::retry)?;
    let event_runtime = LifecycleRuntime {
        workspace_id: Some(previous_workspace_id),
        ..*runtime
    };
    let refreshed = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, previous_workspace_id),
        runtime.mux,
    )?;
    let _ = reconcile_managed_tab_labels(store, &source_namespace, &refreshed, &event_runtime);
    store
        .transaction(|transaction| {
            reconcile_delivery_state(
                transaction,
                &source_namespace,
                &refreshed.panes,
                false,
                runtime.state_directory,
                attribution,
            )?;
            Ok(())
        })
        .map_err(EventError::retry)?;
    let drain = crate::drain_mailboxes(&event_runtime).map_err(EventError::Retry)?;
    Ok(json!({
        "type": "pane_moved",
        "handled": true,
        "drain": drain
    }))
}

fn agent_status_changed<M, C, I>(
    store: &mut StateStore,
    attribution: &MutationAttribution,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    pane: &PaneSlot,
    detected: bool,
) -> Result<Value, EventError>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let (workspace_id, pane_id) = (pane.space.as_str(), pane.pane.as_str());
    let event_worktree = discover_event_space(
        runtime.state_directory,
        &StateNamespace::for_space(runtime.session_socket_id, workspace_id),
        runtime.mux,
    )?;
    let event_namespace = event_worktree.namespace(runtime.session_socket_id);
    check_activated(store, &event_namespace)?;
    if !event_worktree
        .panes
        .iter()
        .any(|pane| pane.pane_id == pane_id)
    {
        return Err(EventError::Definitive(
            "status event pane is not live in its Worktree".to_owned(),
        ));
    }
    store
        .transaction(|transaction| {
            // More than one session may restart before the next status event.
            // Refresh every binding before onboarding inspects any of them.
            reconcile_discovered_panes(
                transaction,
                &event_namespace,
                &event_worktree.panes,
                runtime.ids,
                attribution,
            )?;
            reconcile_delivery_state(
                transaction,
                &event_namespace,
                &event_worktree.panes,
                false,
                runtime.state_directory,
                attribution,
            )?;
            Ok(())
        })
        .map_err(EventError::retry)?;
    let event_runtime = LifecycleRuntime {
        workspace_id: Some(workspace_id),
        ..*runtime
    };
    onboard_settled_integrations(&event_runtime, &event_worktree).map_err(EventError::Retry)?;
    // Only a detection means an agent newly appeared; status changes
    // arrive constantly and must never open a popup.
    let tour = detected
        .then(|| crate::tour::show_for_first_agent(&event_runtime, &event_worktree, Some(pane_id)));
    let drain = crate::drain_mailboxes(&event_runtime).map_err(EventError::Retry)?;
    Ok(json!({
        "type": "pane_agent_status_changed",
        "handled": true,
        "drain": drain,
        "tour": tour
    }))
}

/// What a named pane's idle label tells the human.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaneLabel {
    Ready,
    /// Herdr sees a native agent but has not reported its session. A fresh
    /// Codex can show its composer before running SessionStart, so the human
    /// must submit a first prompt before the pane can register.
    AwaitingSession,
    /// Codex's shared app-server serves this pane, so a prompt cannot connect
    /// it; Doctor explains the fix.
    SharedAppServer,
}

impl PaneLabel {
    pub(crate) const HINTS: [Self; 2] = [Self::AwaitingSession, Self::SharedAppServer];

    pub(crate) fn for_pane(
        pane: &DiscoveredPane,
        served: Option<&crate::codex_app_server::ServedPanes>,
    ) -> Self {
        if served.is_some_and(|served| served.serves(&pane.pane_id)) {
            Self::SharedAppServer
        } else if pane.agent.is_some() && pane.agent_session_id().is_none() {
            Self::AwaitingSession
        } else {
            Self::Ready
        }
    }

    pub(crate) fn hint(self, alias: &str) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::AwaitingSession => Some(format!("@{alias}: send a prompt to connect")),
            Self::SharedAppServer => Some(format!("@{alias}: see namedrop doctor")),
        }
    }

    /// Whether the pane's labels must be published again: its hint changed, or
    /// a hint that no longer applies is still showing. A label the person set
    /// themselves is left alone.
    pub(crate) fn replaces(self, alias: &str, showing: Option<&str>) -> bool {
        let wanted = self.hint(alias);
        if wanted.as_deref() == showing {
            return false;
        }
        wanted.is_some()
            || Self::HINTS
                .iter()
                .any(|hint| hint.hint(alias).as_deref() == showing)
    }
}

pub(crate) fn publish_pane_address_metadata<M, C, I>(
    runtime: &LifecycleRuntime<'_, M, C, I>,
    pane_id: &str,
    alias: &str,
    label: PaneLabel,
) -> Result<(), String>
where
    M: crate::mux::Mux,
{
    let hint = label.hint(alias);
    let texts = StateTexts {
        ready: hint.clone().unwrap_or_else(|| format!("@{alias} ready")),
        working: format!("@{alias} working"),
        attention: format!("@{alias} needs attention"),
        unknown: hint.unwrap_or_else(|| format!("@{alias} status unknown")),
    };
    runtime
        .mux
        .apply(&Effect::Identify {
            pane: PaneId::new(pane_id),
            alias: alias.to_owned(),
            texts,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Opens `screen` for `target` and returns the multiplexer's reply. The
/// popup reconnects to the runtime's own `session_socket_id`.
pub fn open_popup<M, C, I>(
    runtime: &LifecycleRuntime<'_, M, C, I>,
    screen: PopupScreen,
    target: PaneFacts,
) -> Result<serde_json::Value, MuxError>
where
    M: MuxAct,
{
    let popup = PopupSpec {
        screen,
        target,
        state_dir: runtime.state_directory.to_owned(),
        socket: runtime.session_socket_id.to_owned(),
    };
    match runtime.mux.apply(&Effect::OpenPopup(Box::new(popup)))? {
        Applied::Opened(reply) => Ok(reply),
        Applied::Done | Applied::Unsupported => Ok(serde_json::Value::Null),
    }
}

fn rename_tab(mux: &impl MuxAct, tab_id: &str, label: &str) -> Result<Applied, MuxError> {
    mux.apply(&Effect::RenameTab {
        tab: TabId::new(tab_id),
        label: label.to_owned(),
    })
}

struct WorktreeReconciliation {
    epoch: u64,
    drain: crate::DrainReport,
    installation: crate::IntegrationPackageInstallation,
}

fn reconcile_worktree<M, C, I>(
    store: &mut StateStore,
    namespace: &StateNamespace,
    discovered: &DiscoveredWorktree,
    runtime: &LifecycleRuntime<'_, M, C, I>,
    attribution: &MutationAttribution,
    after_restart: bool,
) -> Result<WorktreeReconciliation, String>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    store
        .transaction(|transaction| {
            reconcile_compacted_pane_bindings(
                transaction,
                namespace,
                &discovered.panes,
                attribution,
            )?;
            reconcile_discovered_panes(
                transaction,
                namespace,
                &discovered.panes,
                runtime.ids,
                attribution,
            )?;
            reconcile_delivery_state(
                transaction,
                namespace,
                &discovered.panes,
                after_restart,
                runtime.state_directory,
                attribution,
            )?;
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    publish_status_metadata(runtime, discovered)?;
    reconcile_managed_tab_labels(store, namespace, discovered, runtime)?;
    let epoch = store
        .transaction(|transaction| transaction.complete_reconciliation(namespace))
        .map_err(|error| error.to_string())?;
    let installation = onboard_settled_integrations(runtime, discovered)?;
    let drain = crate::drain_mailboxes(runtime)?;
    Ok(WorktreeReconciliation {
        epoch,
        drain,
        installation,
    })
}

fn apply_managed_tab_label(
    store: &mut StateStore,
    namespace: &StateNamespace,
    tab_id: &str,
    address_id: &PaneAddressId,
    desired_label: &str,
    herdr: &impl MuxAct,
) -> Value {
    let warning = || {
        json!({
            "status": "warning",
            "message": "Pane named, but its tab label could not be updated.",
        })
    };
    let Some(current_label) = current_tab_label(herdr, tab_id) else {
        return warning();
    };
    let intent = store.transaction(|transaction| {
        let existing = transaction
            .managed_tab_labels(namespace)?
            .into_iter()
            .find(|managed| managed.tab_id == tab_id);
        if existing.is_none() && current_label == desired_label {
            return Ok(false);
        }
        let managed_label = existing.as_ref().map_or_else(
            || current_label.clone(),
            |managed| managed.managed_label.clone(),
        );
        let previous_label =
            existing.map_or_else(|| current_label.clone(), |managed| managed.previous_label);
        transaction.put_managed_tab_label(
            namespace,
            &crate::store::ManagedTabLabel {
                tab_id: tab_id.to_owned(),
                address_id: address_id.clone(),
                managed_label,
                previous_label,
                pending_label: Some(desired_label.to_owned()),
            },
        )?;
        Ok(true)
    });
    match intent {
        Ok(false) => json!({"status": "applied"}),
        Err(_) => warning(),
        Ok(true) => {
            if rename_tab(herdr, tab_id, desired_label).is_err() {
                return warning();
            }
            let finalized = store.transaction(|transaction| {
                let Some(mut label) = transaction
                    .managed_tab_labels(namespace)?
                    .into_iter()
                    .find(|managed| managed.tab_id == tab_id)
                else {
                    return Ok(());
                };
                label.managed_label = desired_label.to_owned();
                label.pending_label = None;
                transaction.put_managed_tab_label(namespace, &label)
            });
            if finalized.is_ok() {
                json!({"status": "applied"})
            } else {
                json!({
                    "status": "warning",
                    "message": "Pane named, but its tab label ownership will be reconciled later.",
                })
            }
        }
    }
}

/// The one pane a tab's label can name: its only agent or named pane. Plain
/// shell panes beside it do not compete for the label.
fn tab_label_owner<'a>(
    panes: &'a [DiscoveredPane],
    tab_id: &str,
    addresses: &[PaneAddressRecord],
) -> Option<&'a DiscoveredPane> {
    let mut candidates = panes.iter().filter(|pane| {
        pane.tab_id == tab_id
            && (pane.reports_kind()
                || addresses.iter().any(|address| {
                    !address.retired && address.pane_id.as_deref() == Some(pane.pane_id.as_str())
                }))
    });
    let owner = candidates.next()?;
    candidates.next().is_none().then_some(owner)
}

fn current_tab_label(mux: &impl MuxRead, tab_id: &str) -> Option<String> {
    mux.tab_label(&TabId::new(tab_id)).ok().flatten()
}

fn reconcile_managed_tab_labels<M, C, I>(
    store: &mut StateStore,
    namespace: &StateNamespace,
    discovered: &DiscoveredWorktree,
    runtime: &LifecycleRuntime<'_, M, C, I>,
) -> Result<(), String>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let managed = store
        .transaction(|transaction| transaction.managed_tab_labels(namespace))
        .map_err(|error| error.to_string())?;
    if managed.is_empty() {
        return Ok(());
    }
    let (addresses, active) = store
        .transaction(|transaction| {
            Ok((
                transaction.list_pane_addresses(namespace)?,
                transaction
                    .worktree_state(namespace)?
                    .is_some_and(|state| state.activated && !state.deactivated),
            ))
        })
        .map_err(|error| error.to_string())?;
    for mut label in managed {
        let panes = discovered
            .panes
            .iter()
            .filter(|pane| pane.tab_id == label.tab_id)
            .collect::<Vec<_>>();
        if panes.is_empty() {
            store
                .transaction(|transaction| {
                    transaction.remove_managed_tab_label(namespace, &label.tab_id)
                })
                .map_err(|error| error.to_string())?;
            continue;
        }
        let address = addresses
            .iter()
            .find(|address| !address.retired && address.id == label.address_id);
        let bound_pane = address.and_then(|address| address.pane_id.as_deref());
        // While the named pane stays in this tab, keep the record so its alias
        // returns once the tab again holds no other agent.
        let retained = active
            && bound_pane.is_some_and(|pane_id| panes.iter().any(|pane| pane.pane_id == pane_id));
        let desired = if retained {
            tab_label_owner(&discovered.panes, &label.tab_id, &addresses)
                .filter(|owner| Some(owner.pane_id.as_str()) == bound_pane)
                .and(address)
                .map(|address| format!("@{}", address.alias))
        } else {
            None
        };
        let Some(current) = current_tab_label(runtime.mux, &label.tab_id) else {
            continue;
        };
        if let Some(pending) = label.pending_label.clone() {
            if current != pending {
                if current != label.managed_label {
                    store
                        .transaction(|transaction| {
                            transaction.remove_managed_tab_label(namespace, &label.tab_id)
                        })
                        .map_err(|error| error.to_string())?;
                    continue;
                }
                if rename_tab(runtime.mux, &label.tab_id, &pending).is_err() {
                    continue;
                }
            }
            if desired.is_none() && !retained && pending == label.previous_label {
                store
                    .transaction(|transaction| {
                        transaction.remove_managed_tab_label(namespace, &label.tab_id)
                    })
                    .map_err(|error| error.to_string())?;
            } else {
                label.managed_label = pending;
                label.pending_label = None;
                store
                    .transaction(|transaction| transaction.put_managed_tab_label(namespace, &label))
                    .map_err(|error| error.to_string())?;
            }
            continue;
        }
        if desired.as_deref() == Some(label.managed_label.as_str()) {
            continue;
        }
        if current != label.managed_label {
            store
                .transaction(|transaction| {
                    transaction.remove_managed_tab_label(namespace, &label.tab_id)
                })
                .map_err(|error| error.to_string())?;
            continue;
        }
        if desired.is_none() && label.managed_label == label.previous_label {
            // The tab already shows the label the alias replaced.
            if !retained {
                store
                    .transaction(|transaction| {
                        transaction.remove_managed_tab_label(namespace, &label.tab_id)
                    })
                    .map_err(|error| error.to_string())?;
            }
            continue;
        }
        let replacement = desired
            .clone()
            .unwrap_or_else(|| label.previous_label.clone());
        label.pending_label = Some(replacement.clone());
        store
            .transaction(|transaction| transaction.put_managed_tab_label(namespace, &label))
            .map_err(|error| error.to_string())?;
        if rename_tab(runtime.mux, &label.tab_id, &replacement).is_err() {
            continue;
        }
        store
            .transaction(|transaction| {
                if desired.is_some() || retained {
                    transaction.put_managed_tab_label(
                        namespace,
                        &crate::store::ManagedTabLabel {
                            managed_label: replacement.clone(),
                            pending_label: None,
                            ..label.clone()
                        },
                    )
                } else {
                    transaction.remove_managed_tab_label(namespace, &label.tab_id)
                }
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn mutate_address(
    store: &mut StateStore,
    namespace: &StateNamespace,
    address_id: &PaneAddressId,
    panes: &[DiscoveredPane],
    mutation: impl FnOnce(&mut StateTransaction<'_>) -> Result<(), StoreError>,
) -> Result<Value, String> {
    let record = store
        .transaction(|transaction| {
            mutation(transaction)?;
            transaction
                .list_pane_addresses(namespace)?
                .into_iter()
                .find(|record| &record.id == address_id)
                .ok_or_else(|| StoreError::invalid_input("Pane Address is not active"))
        })
        .map_err(|error| error.to_string())?;
    serde_json::to_value(enrich_record(record, panes)).map_err(|error| error.to_string())
}

fn require_activated(store: &mut StateStore, namespace: &StateNamespace) -> Result<(), String> {
    check_activated(store, namespace).map_err(String::from)
}

fn check_activated(store: &mut StateStore, namespace: &StateNamespace) -> Result<(), EventError> {
    let state = store
        .transaction(|transaction| transaction.worktree_state(namespace))
        .map_err(EventError::retry)?;
    if state.is_some_and(|state| state.activated && !state.deactivated) {
        Ok(())
    } else {
        Err(EventError::Definitive(
            "Namedrop is not activated for this space".to_owned(),
        ))
    }
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T, String> {
    serde_json::from_value(input).map_err(crate::input_schema_error)
}

/// Blank text is how Generic is chosen, so an emptied editor field and a
/// deliberate clear are the same input rather than two shapes to keep in step.
fn normalize_persona(description: &str) -> Option<String> {
    let trimmed = description.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn required_nonempty(value: String, field: &str) -> Result<String, String> {
    if value.is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(value)
    }
}

fn role_input(input: AddressCreateInput) -> Result<ResolvedRoleInput, String> {
    if let Some(preset) = input.preset {
        if input.role_name.is_some() || input.role_description.is_some() {
            return Err("preset cannot be combined with a custom role".to_owned());
        }
        let role = preset.role();
        return Ok(ResolvedRoleInput {
            name: role.name.to_owned(),
            description: role.description.to_owned(),
            suggested_alias: role.suggested_alias.to_owned(),
            alias_override: input.alias,
        });
    }
    let name = required_nonempty(
        input
            .role_name
            .ok_or_else(|| "role_name is required for a custom role".to_owned())?,
        "role_name",
    )?;
    let description = required_nonempty(
        input
            .role_description
            .ok_or_else(|| "role_description is required for a custom role".to_owned())?,
        "role_description",
    )?;
    let suggested = normalize_alias(&name)?;
    Ok(ResolvedRoleInput {
        name,
        description,
        suggested_alias: suggested,
        alias_override: input.alias,
    })
}

/// The refresh a coordinator is told to run immediately before every
/// selection, so it must answer availability itself.
fn address_rows(
    discovered: &DiscoveredWorktree,
    session_socket_id: &str,
    store: &mut StateStore,
    home: Option<&Path>,
    agent_config: crate::AgentConfigDirectories<'_>,
) -> Result<Value, String> {
    // A name alone cannot say whether that agent can take work now. The same
    // load the health check and the prompt roster use supplies the two facts
    // the record cannot carry, a live registration and a paused space, and the
    // roster's own function turns them into the same sentence, so there is one
    // rule rather than two.
    let setup = load_collaboration_setup_from_store(discovered, session_socket_id, store)?;
    let paused = setup
        .worktree_state
        .as_ref()
        .is_some_and(|state| state.paused);
    let records = setup
        .addresses
        .iter()
        .map(|address| {
            let registered = setup.registered_address_ids.contains(&address.id);
            let mut record = serde_json::to_value(address)?;
            record["registered"] = Value::Bool(registered);
            record["selection"] =
                crate::persona::selection_note(address, registered, paused).into();
            // Only a registered address is proven to hold the live pane's
            // own session, so only its transcript is read.
            if registered
                && !address.retired
                && let Some(session) = &address.agent_session_id
                && let Some(pane) = setup
                    .panes
                    .iter()
                    .find(|pane| address.pane_id.as_deref() == Some(pane.pane_id.as_str()))
                && let Some(host) = pane_agent_runtime(pane)
                && let Some(tokens) = crate::transcript_usage::context_tokens(
                    host,
                    session,
                    pane.cwd.as_deref(),
                    home,
                    agent_config,
                )
            {
                record["context_tokens"] = tokens.into();
            }
            Ok(record)
        })
        .collect::<Result<Vec<Value>, serde_json::Error>>()
        .map_err(|error| error.to_string())?;
    serde_json::to_value(records).map_err(|error| error.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressListInput {
    workspaces: Option<AllWorkspaces>,
}

#[derive(Deserialize)]
enum AllWorkspaces {
    #[serde(rename = "all")]
    All,
}

fn every_workspace_addresses<M, C, I>(
    runtime: &LifecycleRuntime<'_, M, C, I>,
    here: &str,
    store: &mut StateStore,
) -> Result<Value, String>
where
    M: crate::mux::Mux,
    C: Clock,
    I: IdGenerator,
{
    let spaces: Vec<(crate::SpaceId, crate::WorkspaceLabel)> = workspace_labels(runtime.mux)?
        .into_iter()
        .map(|workspace| {
            (
                crate::SpaceId::new(workspace.id),
                crate::WorkspaceLabel::from_herdr(&workspace.label),
            )
        })
        .collect();
    let mut workspaces = Vec::new();
    let mut here_label = None;
    for (space, label) in &spaces {
        let namespace = StateNamespace::new(runtime.session_socket_id, space.clone());
        let Some(state) = store
            .transaction(|transaction| transaction.worktree_state(&namespace))
            .map_err(|error| error.to_string())?
            .filter(|state| state.activated && !state.deactivated)
        else {
            continue;
        };
        let label = crate::spaces::workspace_token(
            space,
            Some(label),
            spaces.iter().map(|(other, label)| (other, Some(label))),
        );
        let discovered = discover_worktree_for_workspace(runtime.mux, space.as_str())?;
        let groups = store
            .transaction(|transaction| transaction.list_mention_groups(&namespace))
            .map_err(|error| error.to_string())?;
        let current = space.as_str() == here;
        if current {
            here_label = Some(label.clone());
        }
        workspaces.push(serde_json::json!({
            "label": label,
            "current": current,
            "paused": state.paused,
            "addresses": address_rows(
                &discovered,
                runtime.session_socket_id,
                store,
                runtime.home_directory,
                runtime.agent_config,
            )?,
            "groups": groups,
        }));
    }
    Ok(serde_json::json!({"here": here_label, "workspaces": workspaces}))
}

pub fn discover_worktree(
    mux: &(impl MuxRead + ?Sized),
    cwd: &Path,
) -> Result<DiscoveredWorktree, String> {
    surveyed(mux.survey(Scope::CheckoutOf(cwd), Depth::Panes)).map_err(|error| error.to_string())
}

pub(crate) struct ListedWorkspace {
    pub id: String,
    pub label: String,
    /// The repository a workspace's checkout belongs to, when Herdr stamped one.
    pub repo: Option<String>,
}

pub(crate) fn workspace_labels(mux: &impl MuxRead) -> Result<Vec<ListedWorkspace>, String> {
    let mut spaces = mux
        .survey(Scope::All, Depth::Spaces)
        .map_err(|error| error.to_string())?
        .spaces;
    spaces.sort_by_key(|space| space.number);
    Ok(spaces
        .into_iter()
        .map(|space| ListedWorkspace {
            label: space.label.unwrap_or_else(|| space.id.as_str().to_owned()),
            id: space.id.as_str().to_owned(),
            repo: space.checkout.and_then(|checkout| checkout.repo_name),
        })
        .collect())
}

pub(crate) fn discover_reminder_workspace(
    state: &Path,
    namespace: &StateNamespace,
    mux: &impl MuxRead,
) -> Result<DiscoveredWorktree, String> {
    discover_event_space(state, namespace, mux).map_err(String::from)
}

/// The space `namespace` names, as an event handler finds it. A space that
/// is gone fails its reminders.
fn discover_event_space(
    state: &Path,
    namespace: &StateNamespace,
    mux: &impl MuxRead,
) -> Result<DiscoveredWorktree, EventError> {
    let result = survey_space(mux, namespace.space_id.as_str());
    if let Err(MuxError::Gone(Gone::Space, _)) = result {
        StateStore::open(state)
            .map_err(EventError::retry)?
            .transaction(|tx| tx.fail_reminder_workspace(namespace))
            .map_err(EventError::retry)?;
    }
    result.map_err(EventError::mux)
}

pub fn discover_worktree_for_workspace(
    mux: &(impl MuxRead + ?Sized),
    workspace_id: &str,
) -> Result<DiscoveredWorktree, String> {
    survey_space(mux, workspace_id).map_err(|error| error.to_string())
}

fn survey_space(
    mux: &(impl MuxRead + ?Sized),
    workspace_id: &str,
) -> Result<DiscoveredWorktree, MuxError> {
    let space = crate::SpaceId::new(workspace_id);
    surveyed(mux.survey(Scope::Space(&space), Depth::Panes))
}

/// Asks the multiplexer which pane runs `agent_session_id` and returns its
/// (space ID, pane ID). For agents whose command environment lost the Herdr
/// pane variables. Only an exact, unique match counts, so it never guesses.
pub fn locate_agent_session_pane(
    mux: &impl MuxRead,
    agent_session_id: &str,
) -> Option<(String, String)> {
    let wanted = crate::mux::SessionClaim::Reported(AgentSessionId::new(agent_session_id));
    let survey = mux.survey(Scope::All, Depth::Panes).ok()?;
    let mut matches = survey
        .spaces
        .into_iter()
        .flat_map(|space| space.panes)
        .filter(|pane| {
            pane.agent
                .as_ref()
                .is_some_and(|agent| agent.session == wanted)
        });
    let pane = matches.next()?;
    matches
        .next()
        .is_none()
        .then(|| (pane.space.as_str().to_owned(), pane.id.as_str().to_owned()))
}

pub fn pane_workspace(mux: &impl MuxRead, pane_id: &str) -> Result<String, String> {
    mux.survey(Scope::All, Depth::Panes)
        .map_err(|error| error.to_string())?
        .spaces
        .into_iter()
        .flat_map(|space| space.panes)
        .find(|pane| pane.id.as_str() == pane_id)
        .map(|pane| pane.space.as_str().to_owned())
        .ok_or_else(|| {
            format!(
                "Herdr has no pane {pane_id}, so its space is unknown. Pass \
                 --herdr-workspace <space-id>."
            )
        })
}

pub(crate) fn herdr_error_code(error: &str) -> Option<String> {
    serde_json::from_str::<Value>(error).ok().and_then(|value| {
        value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

/// The one space a `Depth::Panes` survey of one space or checkout returns.
fn surveyed(survey: Result<Survey, MuxError>) -> Result<DiscoveredWorktree, MuxError> {
    let space = survey?.spaces.into_iter().next().ok_or_else(|| {
        MuxError::Protocol("the multiplexer returned no space for a discovery".to_owned())
    })?;
    let worktree_id = space.worktree.ok_or_else(|| {
        MuxError::Protocol(format!(
            "the multiplexer named no worktree for space {}",
            space.id.as_str()
        ))
    })?;
    let (label, branch) = space.checkout.as_ref().map_or((None, None), |checkout| {
        (checkout.label.clone(), checkout.branch.clone())
    });
    Ok(DiscoveredWorktree {
        worktree_id,
        workspace_id: space.id.as_str().to_owned(),
        worktree_attached: space.checkout.is_some(),
        label,
        branch,
        panes: space.panes.into_iter().map(discovered_pane).collect(),
    })
}

fn discovered_pane(pane: PaneFacts) -> DiscoveredPane {
    DiscoveredPane {
        pane_id: pane.id.as_str().to_owned(),
        terminal_id: pane.terminal.as_str().to_owned(),
        tab_id: pane.tab.as_str().to_owned(),
        workspace_id: pane.space.as_str().to_owned(),
        cwd: pane.cwd,
        pane_label: pane.title,
        agent: pane.agent,
        agent_status: pane.state,
        idle_state_label: pane.idle_label,
    }
}

/// The current space state used by the read-only health check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CollaborationSetup {
    pub worktree_state: Option<WorktreeState>,
    pub panes: Vec<DiscoveredPane>,
    pub addresses: Vec<PaneAddressRecord>,
    pub registered_address_ids: Vec<PaneAddressId>,
    pub attention_items: Vec<AttentionItemRecord>,
}

pub(crate) fn load_collaboration_setup_from_store(
    discovered: &DiscoveredWorktree,
    session_socket_id: &str,
    store: &mut StateStore,
) -> Result<CollaborationSetup, String> {
    let namespace = discovered.namespace(session_socket_id);
    let (worktree_state, addresses, registered_address_ids, attention_items) = store
        .transaction(|transaction| {
            let worktree_state = transaction.worktree_state(&namespace)?;
            let addresses = transaction
                .list_pane_addresses(&namespace)?
                .into_iter()
                .map(|record| enrich_record(record, &discovered.panes))
                .collect::<Vec<_>>();
            let mut registered = Vec::new();
            for address in &addresses {
                if let Some(agent_session_id) = &address.agent_session_id
                    && discovered.panes.iter().any(|pane| {
                        address.pane_id.as_deref() == Some(pane.pane_id.as_str())
                            && pane.agent_session_id() == Some(agent_session_id)
                            && pane_agent_runtime(pane).is_some()
                    })
                    && transaction
                        .active_integration_registration(&namespace, &address.id, agent_session_id)?
                        .is_some()
                {
                    registered.push(address.id.clone());
                }
            }
            Ok((
                worktree_state,
                addresses,
                registered,
                transaction.unresolved_attention_items(&namespace)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    Ok(CollaborationSetup {
        worktree_state,
        panes: discovered.panes.clone(),
        addresses,
        registered_address_ids,
        attention_items,
    })
}

pub(crate) fn terminal_safe_text(value: &str) -> String {
    let mut safe = String::new();
    for character in value.chars() {
        if character.is_control() || crate::domain::is_invisible_format(character) {
            safe.extend(character.escape_default());
        } else {
            safe.push(character);
        }
    }
    safe
}

pub fn reconcile_discovered_panes(
    transaction: &mut StateTransaction<'_>,
    namespace: &StateNamespace,
    panes: &[DiscoveredPane],
    ids: &impl IdGenerator,
    attribution: &MutationAttribution,
) -> Result<(), StoreError> {
    // A Rex block ID is never reused and a block never leaves its session,
    // so a bound block the survey no longer lists has closed, and its name
    // is retired as its close would retire it. Herdr reuses pane IDs.
    let closed_for_good =
        crate::mux::Endpoint::parse(&namespace.session_socket_id).kind == crate::mux::MuxKind::Rex;
    for binding in transaction.active_pane_bindings(namespace)? {
        let mut matches = panes.iter().filter(|pane| pane.pane_id == binding.pane_id);
        let first = matches.next();
        if first.is_none() && closed_for_good {
            transaction.cancel_and_remove_pane_address(
                namespace,
                &binding.address_id,
                attribution,
            )?;
            continue;
        }
        if first.is_none() {
            transaction.unbind_pane(namespace, &binding.address_id, attribution)?;
            continue;
        }
        if matches.next().is_some() {
            transaction.unbind_pane(namespace, &binding.address_id, attribution)?;
            transaction.open_attention_item(
                namespace,
                &crate::AttentionItemDraft {
                    id: crate::AttentionItemId::new(format!(
                        "attention-pane-binding-{}",
                        binding.address_id.as_str()
                    )),
                    source_kind: crate::AttentionSourceKind::PaneBinding,
                    source_id: binding.address_id.as_str().to_owned(),
                    summary:
                        "Pane Address binding recovery is ambiguous and requires an explicit rebind."
                            .to_owned(),
                },
                attribution,
            )?;
            continue;
        }
        let pane = first.expect("one matching pane was established");
        transaction.observe_pane_binding_location(
            namespace,
            &binding.address_id,
            &pane.pane_id,
            &pane.tab_id,
        )?;
        let session_id = pane
            .agent_session_id()
            .cloned()
            .unwrap_or_else(|| AgentSessionId::new(""));
        transaction.observe_agent_session(
            namespace,
            &format!(
                "{}-{}",
                ids.next_id("observation"),
                binding.address_id.as_str()
            ),
            &binding.address_id,
            &session_id,
            pane.agent_status
                .as_ref()
                .map_or("empty", AgentState::as_str),
            attribution,
        )?;
    }
    Ok(())
}

fn observe_discovered_occupant(
    transaction: &mut StateTransaction<'_>,
    namespace: &StateNamespace,
    address_id: &PaneAddressId,
    pane: &DiscoveredPane,
    ids: &impl IdGenerator,
    attribution: &MutationAttribution,
) -> Result<(), StoreError> {
    transaction.observe_pane_binding_location(
        namespace,
        address_id,
        &pane.pane_id,
        &pane.tab_id,
    )?;
    transaction.observe_agent_session(
        namespace,
        &format!("{}-{}", ids.next_id("observation"), address_id.as_str()),
        address_id,
        &pane
            .agent_session_id()
            .cloned()
            .unwrap_or_else(|| AgentSessionId::new("")),
        pane.agent_status
            .as_ref()
            .map_or("empty", AgentState::as_str),
        attribution,
    )
}

fn enrich_record(
    mut record: crate::PaneAddressRecord,
    panes: &[DiscoveredPane],
) -> crate::PaneAddressRecord {
    if let Some(pane) = record
        .pane_id
        .as_ref()
        .and_then(|pane_id| panes.iter().find(|pane| &pane.pane_id == pane_id))
    {
        record.pane_label.clone_from(&pane.pane_label);
        record.occupant_label = pane.occupant_label().map(str::to_owned);
    }
    record
}

fn apply_pane_move(
    transaction: &mut StateTransaction<'_>,
    namespace: &StateNamespace,
    previous_pane_id: &str,
    replacement_pane_id: &str,
    replacement_tab_id: &str,
    destination_space_id: &str,
    attribution: &MutationAttribution,
) -> Result<(), StoreError> {
    let binding = transaction
        .active_pane_bindings(namespace)?
        .into_iter()
        .find(|binding| binding.pane_id == previous_pane_id);
    let Some(binding) = binding else {
        return transaction.follow_moved_pane(
            &namespace.session_socket_id,
            previous_pane_id,
            replacement_pane_id,
        );
    };
    if destination_space_id == namespace.space_id.as_str() {
        transaction.move_pane_binding(
            namespace,
            previous_pane_id,
            replacement_pane_id,
            attribution,
        )?;
        transaction.observe_pane_binding_location(
            namespace,
            &binding.address_id,
            replacement_pane_id,
            replacement_tab_id,
        )
    } else {
        transaction.unbind_moved_pane(namespace, &binding, replacement_pane_id, attribution)
    }
}

fn reconcile_compacted_pane_bindings(
    transaction: &mut StateTransaction<'_>,
    namespace: &StateNamespace,
    panes: &[DiscoveredPane],
    attribution: &MutationAttribution,
) -> Result<(), StoreError> {
    let bindings = transaction.active_pane_bindings(namespace)?;
    let addresses = transaction.list_pane_addresses(namespace)?;
    let mut claimed_panes = bindings
        .iter()
        .filter(|binding| panes.iter().any(|pane| pane.pane_id == binding.pane_id))
        .map(|binding| binding.pane_id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    for binding in bindings
        .iter()
        .filter(|binding| !panes.iter().any(|pane| pane.pane_id == binding.pane_id))
    {
        let Some(expected_session) = addresses
            .iter()
            .find(|address| address.id == binding.address_id)
            .and_then(|address| address.agent_session_id.as_ref())
        else {
            continue;
        };
        if addresses
            .iter()
            .filter(|address| address.agent_session_id.as_ref() == Some(expected_session))
            .count()
            != 1
        {
            continue;
        }
        let mut matches = panes
            .iter()
            .filter(|pane| pane.agent_session_id() == Some(expected_session));
        let Some(replacement) = matches.next() else {
            continue;
        };
        if matches.next().is_some() || claimed_panes.contains(&replacement.pane_id) {
            continue;
        }
        transaction.move_pane_binding(
            namespace,
            &binding.pane_id,
            &replacement.pane_id,
            attribution,
        )?;
        claimed_panes.insert(replacement.pane_id.clone());
    }
    Ok(())
}

#[cfg(test)]
mod event_tests;
