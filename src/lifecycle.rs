use crate::integration::{onboard_settled_integrations, pane_agent_runtime};
use crate::{
    ActorKind, AgentSessionId, AttentionItemRecord, Clock, HerdrCommandExecutor, IdGenerator,
    MutationAttribution, PaneAddressDraft, PaneAddressId, PaneAddressRecord, PaneBindingId,
    PaneRoleId, PaneRolePreset, StateNamespace, StateStore, StateTransaction, StoreError,
    WorktreeId, WorktreeState, normalize_alias, publish_status_metadata, reconcile_delivery_state,
    validate_alias,
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
    pub agent_session_id: Option<AgentSessionId>,
    pub agent: Option<String>,
    pub occupant_label: Option<String>,
    pub agent_status: Option<String>,
    /// The label Herdr currently shows for this pane while it is idle.
    pub idle_state_label: Option<String>,
}

#[derive(Clone, Copy)]
pub struct LifecycleRuntime<'a, E, C, I> {
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
    pub herdr: &'a E,
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

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum PaneLifecycleEvent {
    #[serde(rename = "pane_created")]
    Created { pane: PaneEventInfo },
    #[serde(rename = "pane_closed")]
    Closed {
        pane_id: String,
        workspace_id: String,
    },
    #[serde(rename = "pane_exited")]
    Exited {
        pane_id: String,
        workspace_id: String,
    },
    #[serde(rename = "tab_closed")]
    TabClosed {
        tab_id: String,
        workspace_id: String,
    },
    #[serde(rename = "pane_moved")]
    Moved {
        previous_pane_id: String,
        previous_workspace_id: String,
        #[serde(rename = "previous_tab_id")]
        _previous_tab_id: String,
        pane: PaneEventInfo,
        created_tab: Option<Value>,
        created_workspace: Option<Value>,
        closed_tab_id: Option<String>,
        closed_workspace_id: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStatusChangedEvent {
    #[serde(rename = "type")]
    event_type: AgentStatusEventType,
    pane_id: String,
    workspace_id: String,
    agent_status: Option<String>,
    released: Option<bool>,
    final_status: Option<String>,
    agent: Option<String>,
    display_agent: Option<String>,
    title: Option<String>,
    #[serde(default)]
    state_labels: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
enum AgentStatusEventType {
    #[serde(rename = "pane_agent_status_changed")]
    Changed,
    #[serde(rename = "pane_agent_detected")]
    Detected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PaneEventInfo {
    pane_id: String,
    workspace_id: String,
    terminal_id: String,
    tab_id: String,
    focused: bool,
    agent_status: String,
    revision: u64,
    agent: Option<String>,
    agent_session: Option<AgentSessionEntry>,
    cwd: Option<String>,
    display_agent: Option<String>,
    foreground_cwd: Option<String>,
    label: Option<String>,
    scroll: Option<Value>,
    #[serde(default)]
    state_labels: std::collections::BTreeMap<String, String>,
    terminal_title: Option<String>,
    terminal_title_stripped: Option<String>,
    title: Option<String>,
    #[serde(default)]
    tokens: std::collections::BTreeMap<String, String>,
}

pub fn execute_lifecycle_operation<E, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let discovered = match runtime.workspace_id {
        Some(workspace_id) => discover_worktree_for_workspace(runtime.herdr, workspace_id)?,
        None => discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    let namespace = discovered.namespace(runtime.session_socket_id);
    let (actor_kind, actor_id) = if operation.starts_with("event.") || operation == "reconcile" {
        (ActorKind::Plugin, "herdr")
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
            if pane_type == crate::PaneType::Agent
                && pane.agent.is_some()
                && agent_runtime.is_none()
            {
                return Err(
                    "the current pane must contain a recognized Codex or Claude session".to_owned(),
                );
            }
            let registration_required = pane_type == crate::PaneType::Agent;
            let agent_session_id = pane.agent_session_id.as_ref();
            let was_activated = store
                .transaction(|transaction| transaction.worktree_state(&namespace))
                .map_err(|error| error.to_string())?
                .is_some_and(|state| state.activated && !state.deactivated);
            let address_id = store
                .transaction(|transaction| {
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
                        transaction.set_pane_type(&namespace, &binding.address_id, pane_type)?;
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
                    transaction.set_pane_type(&namespace, &address_id, pane_type)?;
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
                    runtime.herdr,
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
                    pane.agent.is_none() && pane.agent_session_id.is_none()
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
            store
                .transaction(|transaction| transaction.activate_worktree(&namespace, &attribution))
                .map_err(|error| error.to_string())?;
            let reconciliation = reconcile_worktree(
                &mut store,
                &namespace,
                &discovered,
                runtime,
                &attribution,
                false,
            )?;
            let shortcuts =
                crate::shortcuts::activate_shortcuts(runtime.home_directory, runtime.herdr);
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
            Ok(json!({
                "space_id": discovered.workspace_id,
                "worktree_id": discovered.worktree_attached.then_some(discovered.worktree_id),
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
                .purge_worktree(&namespace, &typed_space_id)
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
            parse_input::<EmptyInput>(input)?;
            require_activated(&mut store, &namespace)?;
            // This is the refresh a coordinator is told to run immediately
            // before every selection, so it must answer availability itself:
            // a name alone cannot say whether that agent can take work now.
            // The same load the health check and the prompt roster use supplies
            // the two facts the record cannot carry — a live registration, and
            // a paused space — and the roster's own function turns them into
            // the same sentence, so there is one rule rather than two.
            let setup = load_collaboration_setup_from_store(
                &discovered,
                runtime.session_socket_id,
                &mut store,
            )?;
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
                    Ok(record)
                })
                .collect::<Result<Vec<Value>, serde_json::Error>>()
                .map_err(|error| error.to_string())?;
            serde_json::to_value(records).map_err(|error| error.to_string())
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
        "event.pane-lifecycle" => {
            let event: PaneLifecycleEvent = parse_input(input)?;
            match event {
                PaneLifecycleEvent::Created { pane } => {
                    if let Ok(event_worktree) =
                        discover_worktree_for_workspace(runtime.herdr, &pane.workspace_id)
                    {
                        let event_namespace = event_worktree.namespace(runtime.session_socket_id);
                        let event_runtime = LifecycleRuntime {
                            workspace_id: Some(&pane.workspace_id),
                            ..*runtime
                        };
                        let _ = reconcile_managed_tab_labels(
                            &mut store,
                            &event_namespace,
                            &event_worktree,
                            &event_runtime,
                        );
                    }
                    Ok(json!({"type": "pane_created", "handled": true}))
                }
                PaneLifecycleEvent::Closed {
                    pane_id,
                    workspace_id,
                }
                | PaneLifecycleEvent::Exited {
                    pane_id,
                    workspace_id,
                } => {
                    let event_worktree =
                        discover_worktree_for_workspace(runtime.herdr, &workspace_id)?;
                    let event_namespace = event_worktree.namespace(runtime.session_socket_id);
                    require_activated(&mut store, &event_namespace)?;
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
                                    &attribution,
                                )?;
                                return Ok(Some(binding.address_id));
                            }
                            Ok(None)
                        })
                        .map_err(|error| error.to_string())?;
                    let event_runtime = LifecycleRuntime {
                        workspace_id: Some(&workspace_id),
                        ..*runtime
                    };
                    let refreshed = discover_worktree_for_workspace(runtime.herdr, &workspace_id)?;
                    let _ = reconcile_managed_tab_labels(
                        &mut store,
                        &event_namespace,
                        &refreshed,
                        &event_runtime,
                    );
                    store
                        .transaction(|transaction| {
                            reconcile_delivery_state(
                                transaction,
                                &event_namespace,
                                &refreshed.panes,
                                false,
                                &attribution,
                            )?;
                            Ok(())
                        })
                        .map_err(|error| error.to_string())?;
                    let drain = crate::drain_mailboxes(&event_runtime)?;
                    Ok(json!({
                        "type": "pane_unavailable",
                        "handled": true,
                        "removed_address_id": removed_address_id,
                        "drain": drain
                    }))
                }
                PaneLifecycleEvent::TabClosed {
                    tab_id,
                    workspace_id,
                } => {
                    let event_worktree =
                        discover_worktree_for_workspace(runtime.herdr, &workspace_id)?;
                    let event_namespace = event_worktree.namespace(runtime.session_socket_id);
                    require_activated(&mut store, &event_namespace)?;
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
                                            address.agent_session_id.as_ref()
                                                == pane.agent_session_id.as_ref()
                                        })
                                });
                                if binding.tab_id.as_deref() == Some(tab_id.as_str())
                                    || (binding.tab_id.is_none() && !exact_pane_is_live)
                                {
                                    transaction.cancel_and_remove_pane_address(
                                        &event_namespace,
                                        &binding.address_id,
                                        &attribution,
                                    )?;
                                    removed.push(binding);
                                }
                            }
                            Ok(removed)
                        })
                        .map_err(|error| error.to_string())?;
                    let event_runtime = LifecycleRuntime {
                        workspace_id: Some(&workspace_id),
                        ..*runtime
                    };
                    let refreshed = discover_worktree_for_workspace(runtime.herdr, &workspace_id)?;
                    let _ = reconcile_managed_tab_labels(
                        &mut store,
                        &event_namespace,
                        &refreshed,
                        &event_runtime,
                    );
                    let drain = crate::drain_mailboxes(&event_runtime)?;
                    Ok(json!({
                        "type": "tab_unavailable",
                        "handled": true,
                        "tab_id": tab_id,
                        "removed_address_ids": removed_bindings
                            .into_iter()
                            .map(|binding| binding.address_id)
                            .collect::<Vec<_>>(),
                        "drain": drain
                    }))
                }
                PaneLifecycleEvent::Moved {
                    previous_pane_id,
                    previous_workspace_id,
                    _previous_tab_id: _,
                    pane,
                    created_tab,
                    created_workspace,
                    closed_tab_id,
                    closed_workspace_id,
                } => {
                    let _ = (
                        created_tab,
                        created_workspace,
                        closed_tab_id,
                        closed_workspace_id,
                    );
                    let source =
                        discover_worktree_for_workspace(runtime.herdr, &previous_workspace_id)?;
                    let source_namespace = source.namespace(runtime.session_socket_id);
                    require_activated(&mut store, &source_namespace)?;
                    let destination =
                        discover_worktree_for_workspace(runtime.herdr, &pane.workspace_id)?;
                    store
                        .transaction(|transaction| {
                            apply_pane_move(
                                transaction,
                                &source_namespace,
                                &previous_pane_id,
                                &pane.pane_id,
                                &pane.tab_id,
                                &destination.workspace_id,
                                &attribution,
                            )?;
                            if destination.workspace_id == source_namespace.space_id.as_str() {
                                reconcile_compacted_pane_bindings(
                                    transaction,
                                    &source_namespace,
                                    &destination.panes,
                                    &attribution,
                                )?;
                            }
                            Ok(())
                        })
                        .map_err(|error| error.to_string())?;
                    let event_runtime = LifecycleRuntime {
                        workspace_id: Some(&previous_workspace_id),
                        ..*runtime
                    };
                    let refreshed =
                        discover_worktree_for_workspace(runtime.herdr, &previous_workspace_id)?;
                    let _ = reconcile_managed_tab_labels(
                        &mut store,
                        &source_namespace,
                        &refreshed,
                        &event_runtime,
                    );
                    store
                        .transaction(|transaction| {
                            reconcile_delivery_state(
                                transaction,
                                &source_namespace,
                                &refreshed.panes,
                                false,
                                &attribution,
                            )?;
                            Ok(())
                        })
                        .map_err(|error| error.to_string())?;
                    let drain = crate::drain_mailboxes(&event_runtime)?;
                    Ok(json!({
                        "type": "pane_moved",
                        "handled": true,
                        "drain": drain
                    }))
                }
            }
        }
        "event.agent-status-changed" => {
            let event: AgentStatusChangedEvent = parse_input(input)?;
            let _ = (
                event.agent_status,
                event.released,
                event.final_status,
                event.agent,
                event.display_agent,
                event.title,
                event.state_labels,
            );
            let event_worktree =
                discover_worktree_for_workspace(runtime.herdr, &event.workspace_id)?;
            let event_namespace = event_worktree.namespace(runtime.session_socket_id);
            require_activated(&mut store, &event_namespace)?;
            if !event_worktree
                .panes
                .iter()
                .any(|pane| pane.pane_id == event.pane_id)
            {
                return Err("status event pane is not live in its Worktree".to_owned());
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
                        &attribution,
                    )?;
                    reconcile_delivery_state(
                        transaction,
                        &event_namespace,
                        &event_worktree.panes,
                        false,
                        &attribution,
                    )?;
                    Ok(())
                })
                .map_err(|error| error.to_string())?;
            let event_runtime = LifecycleRuntime {
                workspace_id: Some(&event.workspace_id),
                ..*runtime
            };
            onboard_settled_integrations(&event_runtime, &event_worktree)?;
            // Only a detection means an agent newly appeared; status changes
            // arrive constantly and must never open a popup.
            let tour = match event.event_type {
                AgentStatusEventType::Detected => Some(crate::tour::show_for_first_agent(
                    &event_runtime,
                    &event_worktree,
                    Some(&event.pane_id),
                )),
                AgentStatusEventType::Changed => None,
            };
            let drain = crate::drain_mailboxes(&event_runtime)?;
            Ok(json!({
                "type": "pane_agent_status_changed",
                "handled": true,
                "drain": drain,
                "tour": tour
            }))
        }
        _ => Err(format!("{operation} is not a lifecycle operation")),
    }
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
        } else if pane.agent.is_some() && pane.agent_session_id.is_none() {
            Self::AwaitingSession
        } else {
            Self::Ready
        }
    }

    /// The label that replaces the idle and unknown states, if any.
    pub(crate) fn hint(self, alias: &str) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::AwaitingSession => Some(format!("@{alias}: send a prompt to connect")),
            Self::SharedAppServer => Some(format!("@{alias}: see namedrop doctor")),
        }
    }
}

pub(crate) fn publish_pane_address_metadata<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    pane_id: &str,
    alias: &str,
    label: PaneLabel,
) -> Result<(), String>
where
    E: HerdrCommandExecutor,
{
    let title = format!("@{alias}");
    let hint = label.hint(alias);
    let ready = hint.clone().unwrap_or_else(|| format!("@{alias} ready"));
    let working = format!("@{alias} working");
    let blocked = format!("@{alias} needs attention");
    let unknown = hint.unwrap_or_else(|| format!("@{alias} status unknown"));
    runtime.herdr.execute(&[
        "pane",
        "report-metadata",
        pane_id,
        "--source",
        "namedrop-identity",
        "--title",
        &title,
        "--state-label",
        &format!("idle={ready}"),
        "--state-label",
        &format!("done={ready}"),
        "--state-label",
        &format!("working={working}"),
        "--state-label",
        &format!("blocked={blocked}"),
        "--state-label",
        &format!("unknown={unknown}"),
    ])?;
    Ok(())
}

struct WorktreeReconciliation {
    epoch: u64,
    drain: crate::DrainReport,
    installation: crate::IntegrationPackageInstallation,
}

fn reconcile_worktree<E, C, I>(
    store: &mut StateStore,
    namespace: &StateNamespace,
    discovered: &DiscoveredWorktree,
    runtime: &LifecycleRuntime<'_, E, C, I>,
    attribution: &MutationAttribution,
    after_restart: bool,
) -> Result<WorktreeReconciliation, String>
where
    E: HerdrCommandExecutor,
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
    herdr: &impl HerdrCommandExecutor,
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
            if herdr
                .execute(&["tab", "rename", tab_id, desired_label])
                .is_err()
            {
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
            && (pane.agent.is_some()
                || addresses.iter().any(|address| {
                    !address.retired && address.pane_id.as_deref() == Some(pane.pane_id.as_str())
                }))
    });
    let owner = candidates.next()?;
    candidates.next().is_none().then_some(owner)
}

fn current_tab_label(herdr: &impl HerdrCommandExecutor, tab_id: &str) -> Option<String> {
    let output = herdr.execute(&["tab", "get", tab_id]).ok()?;
    let value: Value = serde_json::from_str(&output.stdout).ok()?;
    value
        .pointer("/result/label")
        .or_else(|| value.pointer("/result/tab/label"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn reconcile_managed_tab_labels<E, C, I>(
    store: &mut StateStore,
    namespace: &StateNamespace,
    discovered: &DiscoveredWorktree,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<(), String>
where
    E: HerdrCommandExecutor,
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
        let Some(current) = current_tab_label(runtime.herdr, &label.tab_id) else {
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
                if runtime
                    .herdr
                    .execute(&["tab", "rename", &label.tab_id, &pending])
                    .is_err()
                {
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
        if runtime
            .herdr
            .execute(&["tab", "rename", &label.tab_id, &replacement])
            .is_err()
        {
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
    let state = store
        .transaction(|transaction| transaction.worktree_state(namespace))
        .map_err(|error| error.to_string())?;
    if state.is_some_and(|state| state.activated && !state.deactivated) {
        Ok(())
    } else {
        Err("Namedrop is not activated for this space".to_owned())
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

pub fn discover_worktree(
    herdr: &impl HerdrCommandExecutor,
    cwd: &Path,
) -> Result<DiscoveredWorktree, String> {
    let cwd = cwd
        .to_str()
        .ok_or_else(|| "Worktree path must be valid UTF-8".to_owned())?;
    let worktree_output = herdr.execute(&["worktree", "list", "--cwd", cwd, "--json"])?;
    let worktrees: WorktreeListEnvelope =
        serde_json::from_str(&worktree_output.stdout).map_err(|error| error.to_string())?;
    let WorktreeListResult { source, worktrees } = worktrees.result;
    let worktree = worktrees
        .into_iter()
        .find(|worktree| Path::new(&worktree.path) == Path::new(cwd))
        .ok_or_else(|| format!("Herdr did not resolve {cwd} as a Worktree"))?;
    let workspace_id = worktree
        .open_workspace_id
        .clone()
        .ok_or_else(|| format!("Worktree {} is not open in Herdr", worktree.path))?;
    let repo_key = source
        .as_ref()
        .and_then(|source| source.repo_key.as_deref());
    let (owners, unknown) = spaces_owning_checkout(herdr, &worktree, repo_key, &workspace_id)?;
    if owners.len() > 1 || !unknown.is_empty() {
        let open_in = match owners.as_slice() {
            [owner] => format!("Herdr space {owner}"),
            owners => format!("more than one Herdr space ({})", owners.join(", ")),
        };
        let suspects = if unknown.is_empty() {
            String::new()
        } else {
            format!(
                " and may be open in {}, whose checkout Herdr does not report",
                unknown.join(", ")
            )
        };
        return Err(format!(
            "Checkout {} is open in {open_in}{suspects}, so Namedrop can't tell which one \
             this command is for. Pass --herdr-workspace <space-id> (or --herdr-pane \
             <pane-id>).",
            worktree.path
        ));
    }
    discover_panes(
        herdr,
        worktree.path,
        workspace_id,
        true,
        worktree.label,
        worktree.branch,
    )
}

// `worktree list --cwd` names only the first opener. Returns the spaces proven
// to own `checkout`, and, for a linked checkout, the spaces in the same
// repository whose checkout Herdr does not report.
fn spaces_owning_checkout(
    herdr: &impl HerdrCommandExecutor,
    checkout: &WorktreeEntry,
    repo_key: Option<&str>,
    opener: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let output = herdr.execute(&["workspace", "list"])?;
    let workspaces: WorkspaceListEnvelope =
        serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
    let mut owners = vec![opener.to_owned()];
    let mut unknown = Vec::new();
    for workspace in workspaces.result.workspaces {
        if workspace.workspace_id == opener {
            continue;
        }
        // Herdr stamps `worktree` only through `worktree open` and
        // `worktree create`, and then it names the space's own checkout.
        if let Some(stamped) = workspace.worktree {
            if Path::new(&stamped.checkout_path) == Path::new(&checkout.path) {
                owners.push(workspace.workspace_id);
            }
            continue;
        }
        let listing = match worktree_list_for_workspace(herdr, &workspace.workspace_id) {
            Ok(listing) => listing,
            Err(error) if herdr_error_code(&error).as_deref() == Some("workspace_not_found") => {
                None
            }
            Err(error) => return Err(error),
        };
        let Some(result) = listing else {
            continue;
        };
        let other_repo = result
            .source
            .as_ref()
            .and_then(|source| source.repo_key.as_deref())
            .zip(repo_key)
            .is_some_and(|(theirs, ours)| theirs != ours);
        if other_repo {
            continue;
        }
        match owned_worktree_index(&result, &workspace.workspace_id) {
            Some(index)
                if Path::new(&result.worktrees[index].path) == Path::new(&checkout.path) =>
            {
                owners.push(workspace.workspace_id);
            }
            Some(_) => {}
            // A space on the repository root is always placed, as opener or
            // through `source`, so only a checkout not known to be the root
            // can hide an owner.
            None if checkout.is_linked_worktree != Some(false) => {
                unknown.push(workspace.workspace_id)
            }
            None => {}
        }
    }
    owners.sort();
    unknown.sort();
    Ok((owners, unknown))
}

fn worktree_list_for_workspace(
    herdr: &impl HerdrCommandExecutor,
    workspace_id: &str,
) -> Result<Option<WorktreeListResult>, String> {
    let output = match herdr.execute(&["worktree", "list", "--workspace", workspace_id, "--json"]) {
        Ok(output) => output,
        Err(error) if herdr_error_code(&error).as_deref() == Some("not_git_worktree") => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let worktrees: WorktreeListEnvelope =
        serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
    Ok(Some(worktrees.result))
}

fn owned_worktree_index(result: &WorktreeListResult, workspace_id: &str) -> Option<usize> {
    result
        .worktrees
        .iter()
        .position(|worktree| worktree.open_workspace_id.as_deref() == Some(workspace_id))
        .or_else(|| {
            // Herdr lists each checkout path once with its first opener as
            // open_workspace_id, so a second workspace on the same path never
            // matches by id. `source` names the requester only when it sits on
            // the repo root, and then source_checkout_path is its checkout.
            let source = result
                .source
                .as_ref()
                .filter(|source| source.source_workspace_id.as_deref() == Some(workspace_id))?;
            let checkout = source.source_checkout_path.as_deref()?;
            result
                .worktrees
                .iter()
                .position(|worktree| Path::new(&worktree.path) == Path::new(checkout))
        })
}

pub fn discover_worktree_for_workspace(
    herdr: &impl HerdrCommandExecutor,
    workspace_id: &str,
) -> Result<DiscoveredWorktree, String> {
    let Some(mut result) = worktree_list_for_workspace(herdr, workspace_id)? else {
        return discover_panes(
            herdr,
            format!("herdr-space:{workspace_id}"),
            workspace_id.to_owned(),
            false,
            None,
            None,
        );
    };
    let index = owned_worktree_index(&result, workspace_id);
    match index.map(|index| result.worktrees.swap_remove(index)) {
        Some(worktree) => discover_panes(
            herdr,
            worktree.path,
            workspace_id.to_owned(),
            true,
            worktree.label,
            worktree.branch,
        ),
        None => Err(format!(
            "Herdr returned a Worktree snapshot that does not contain space {workspace_id}"
        )),
    }
}

/// Asks Herdr which pane runs `agent_session_id` and returns its (space ID,
/// pane ID). For agents whose command environment lost the Herdr pane
/// variables. Only an exact, unique match counts, so it never guesses.
pub fn locate_agent_session_pane(
    herdr: &impl HerdrCommandExecutor,
    agent_session_id: &str,
) -> Option<(String, String)> {
    let output = herdr.execute(&["pane", "list"]).ok()?;
    let panes: PaneListEnvelope = serde_json::from_str(&output.stdout).ok()?;
    let mut matches = panes.result.panes.into_iter().filter(|pane| {
        pane.agent_session
            .as_ref()
            .is_some_and(|session| session.value == agent_session_id)
    });
    let pane = matches.next()?;
    matches
        .next()
        .is_none()
        .then_some((pane.workspace_id, pane.pane_id))
}

pub fn pane_workspace(herdr: &impl HerdrCommandExecutor, pane_id: &str) -> Result<String, String> {
    let output = herdr.execute(&["pane", "list"])?;
    let panes: PaneListEnvelope =
        serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
    panes
        .result
        .panes
        .into_iter()
        .find(|pane| pane.pane_id == pane_id)
        .map(|pane| pane.workspace_id)
        .ok_or_else(|| {
            format!(
                "Herdr has no pane {pane_id}, so its space is unknown. Pass \
                 --herdr-workspace <space-id>."
            )
        })
}

fn herdr_error_code(error: &str) -> Option<String> {
    serde_json::from_str::<Value>(error).ok().and_then(|value| {
        value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

fn discover_panes(
    herdr: &impl HerdrCommandExecutor,
    worktree_path: String,
    workspace_id: String,
    worktree_attached: bool,
    label: Option<String>,
    branch: Option<String>,
) -> Result<DiscoveredWorktree, String> {
    let pane_output = herdr.execute(&["pane", "list", "--workspace", &workspace_id])?;
    let panes: PaneListEnvelope =
        serde_json::from_str(&pane_output.stdout).map_err(|error| error.to_string())?;
    let panes = panes
        .result
        .panes
        .into_iter()
        .map(|pane| DiscoveredPane {
            pane_id: pane.pane_id,
            terminal_id: pane.terminal_id,
            tab_id: pane.tab_id,
            workspace_id: pane.workspace_id,
            cwd: pane.cwd,
            pane_label: pane.label.or(pane.title).or(pane.terminal_title_stripped),
            agent_session_id: pane
                .agent_session
                .map(|session| AgentSessionId::new(session.value)),
            agent: pane.agent.clone(),
            occupant_label: pane.display_agent.or(pane.agent),
            agent_status: pane.agent_status,
            idle_state_label: pane
                .state_labels
                .and_then(|mut labels| labels.remove("idle")),
        })
        .collect();
    Ok(DiscoveredWorktree {
        worktree_id: WorktreeId::new(worktree_path),
        workspace_id,
        worktree_attached,
        label,
        branch,
        panes,
    })
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
                            && pane.agent_session_id.as_ref() == Some(agent_session_id)
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
    for binding in transaction.active_pane_bindings(namespace)? {
        let mut matches = panes.iter().filter(|pane| pane.pane_id == binding.pane_id);
        let first = matches.next();
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
            .agent_session_id
            .clone()
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
            pane.agent_status.as_deref().unwrap_or("empty"),
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
            .agent_session_id
            .clone()
            .unwrap_or_else(|| AgentSessionId::new("")),
        pane.agent_status.as_deref().unwrap_or("empty"),
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
        record.occupant_label.clone_from(&pane.occupant_label);
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
        return Ok(());
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
        transaction.unbind_pane(namespace, &binding.address_id, attribution)
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
            .filter(|pane| pane.agent_session_id.as_ref() == Some(expected_session));
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

#[derive(Deserialize)]
struct WorktreeListEnvelope {
    result: WorktreeListResult,
}

#[derive(Deserialize)]
struct WorktreeListResult {
    source: Option<WorktreeListSource>,
    worktrees: Vec<WorktreeEntry>,
}

#[derive(Deserialize)]
struct WorktreeListSource {
    repo_key: Option<String>,
    source_checkout_path: Option<String>,
    source_workspace_id: Option<String>,
}

#[derive(Deserialize)]
struct WorktreeEntry {
    path: String,
    is_linked_worktree: Option<bool>,
    open_workspace_id: Option<String>,
    label: Option<String>,
    branch: Option<String>,
}

#[derive(Deserialize)]
struct WorkspaceListEnvelope {
    result: WorkspaceListResult,
}

#[derive(Deserialize)]
struct WorkspaceListResult {
    workspaces: Vec<WorkspaceEntry>,
}

#[derive(Deserialize)]
struct WorkspaceEntry {
    workspace_id: String,
    worktree: Option<WorkspaceWorktree>,
}

#[derive(Deserialize)]
struct WorkspaceWorktree {
    checkout_path: String,
}

#[derive(Deserialize)]
struct PaneListEnvelope {
    result: PaneListResult,
}

#[derive(Deserialize)]
struct PaneListResult {
    panes: Vec<PaneEntry>,
}

#[derive(Deserialize)]
struct PaneEntry {
    pane_id: String,
    terminal_id: String,
    tab_id: String,
    workspace_id: String,
    cwd: Option<String>,
    label: Option<String>,
    title: Option<String>,
    terminal_title_stripped: Option<String>,
    agent: Option<String>,
    display_agent: Option<String>,
    agent_session: Option<AgentSessionEntry>,
    agent_status: Option<String>,
    state_labels: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Clone, Deserialize)]
struct AgentSessionEntry {
    value: String,
}
