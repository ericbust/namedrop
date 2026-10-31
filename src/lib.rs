mod agent_status;
mod agent_status_model;
pub mod cli;
pub use agent_status::collect_agent_status;
pub use agent_status_model::{suggest_agent_persona, summarize_agent_status};
mod codex_app_server;
mod context;
mod controls;
mod coordination;
pub mod dashboard;
mod delivery;
mod domain;
mod groups;
mod handoff;
mod health;
pub mod host;
pub use host::{
    Budget, CallerClaim, CallerIdentity, HcHost, Host, HostError, ListedWorkspace, PaneMetadata,
    PaneScope, Popup, Process, ProcessInfo, PromptFailure, RuntimeHost, SpaceLocator, TuiHost,
};
mod hook;
mod install;
mod installed_runtime;
mod integration;
mod lifecycle;
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub mod linux_sandbox;
mod message;
mod persona;
mod ports;
pub mod runtime;
mod shortcuts;
mod spaces;
mod store;
#[doc(hidden)]
pub use shortcuts::install_shortcuts;
mod terminal;
pub mod tour;
#[doc(hidden)]
pub mod update_check;
pub use terminal::{default_pane_type, detect_pane_type, execute_terminal};

pub(crate) const INTEGRATION_PACKAGE_ATTENTION_SOURCE_PREFIX: &str = "integration-package-";

pub use context::{execute_context_operation, execute_human_context_operation};
pub use controls::execute_control_operation;
pub use delivery::{DrainReport, GraceDeadline, drain_mailboxes, spaces_to_wake};
pub(crate) use delivery::{drain_receipt, publish_status_metadata, reconcile_delivery_state};
pub use domain::*;
pub use groups::execute_group_operation;
pub use handoff::execute_handoff_operation;
pub use health::{
    HealthIssue, HealthRepair, HealthRepairKind, HealthReport, HealthSeverity, HealthStatus,
    diagnose_collaboration, render_health_report_to_text, request_doctor_assistance,
};
pub use hook::{
    execute_codex_app_server_hook, execute_session_start_hook, execute_user_prompt_submit_hook,
};

#[cfg(test)]
pub(crate) use install::user_hook_activation_guidance;

#[doc(hidden)]
pub use install::hc_package_identity;

pub use install::{AgentConfigDirectories, install_integration_package, package_directory};
pub(crate) use install::{
    IntegrationPackageInstallation, integration_package_attention_source,
    refresh_user_integration_packages,
};
#[doc(hidden)]
pub use installed_runtime::command_guard as runtime_command_guard;
#[doc(hidden)]
pub use installed_runtime::executable as runtime_executable;
#[doc(hidden)]
pub use installed_runtime::validate_update_manifest;
pub use integration::{
    AgentRuntime, AttachedContext, CONTEXT_DELIVERY_OPERATIONS, CatalogExample, CatalogOperation,
    ContextRequest, DELIVERY_PERMITTED_OPERATIONS, DeliveryAddress, DeliveryControl,
    DeliveryControlDraft, DeliveryEnvelope, INTEGRATION_PROTOCOL_VERSION, InputTransport,
    IntegrationChallenge, IntegrationPackage, IntegrationRegistration, IntegrationToolInvocation,
    OperationCatalog, REPLY_BODY_PLACEHOLDER, ResultSetCompletion, ResultSetOutcome,
    ResultSetStatus, complete_capability_profile, describe_operations,
    execute_integration_operation, integration_package, issue_registration_challenge,
    operation_catalog, trusted_delivery_instruction,
};
pub(crate) use integration::{collaboration_executable, tool_environment};
pub use lifecycle::{
    DiscoveredPane, DiscoveredWorktree, LifecycleRuntime, execute_lifecycle_operation,
    locate_agent_session_pane, pane_workspace, reconcile_discovered_panes,
};
pub use message::{
    BodyMention, ParsedMessage, RecipientOutlook, RecipientRun, carries_leading_mentions,
    execute_message_operation, parse_mentions, parse_prompt_mentions, recipient_outlooks,
    validate_message_input,
};
pub use ports::{Clock, HerdrCommandExecutor, HerdrCommandOutput, IdGenerator, PromptFailureKind};
pub use runtime::{ProcessHerdr, ProcessIds, SystemClock};
use serde::Serialize;
pub use spaces::{SpaceScope, WorkspaceLabel};
mod output;
pub use output::render_command_result_to_text;
pub use store::{
    StateStore, StateTransaction, StoreError, StoreErrorKind, WorktreePurgeResult,
    WorktreePurgeSummary,
};

#[derive(Debug, PartialEq, Serialize)]
pub struct CommandResult {
    protocol_version: &'static str,
    status: &'static str,
    operation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<CommandError>,
}

#[derive(Debug, PartialEq, Serialize)]
struct CommandError {
    code: &'static str,
    message: String,
}

impl CommandResult {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            protocol_version: INTEGRATION_PROTOCOL_VERSION,
            status: "rejected",
            operation: None,
            data: None,
            error: Some(CommandError {
                code: "invalid_request",
                message: message.into(),
            }),
        }
    }

    pub fn rejected(
        operation: &'static str,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: INTEGRATION_PROTOCOL_VERSION,
            status: "rejected",
            operation: Some(operation),
            data: None,
            error: Some(CommandError {
                code,
                message: message.into(),
            }),
        }
    }

    pub fn success(operation: &'static str, data: serde_json::Value) -> Self {
        Self {
            protocol_version: INTEGRATION_PROTOCOL_VERSION,
            status: "ok",
            operation: Some(operation),
            data: Some(data),
            error: None,
        }
    }
}

pub fn is_lifecycle_operation(operation: &str) -> bool {
    matches!(
        operation,
        "name"
            | "activate"
            | "deactivate"
            | "purge"
            | "address.create"
            | "address.list"
            | "address.bind"
            | "address.unbind"
            | "address.rename"
            | "address.remove"
            | "address.retire"
            | "address.release-alias"
            | "address.persona"
            | "reconcile"
            | "event.pane-lifecycle"
            | "event.agent-status-changed"
    )
}

pub fn is_message_operation(operation: &str) -> bool {
    matches!(operation, "send" | "reply")
}

pub fn is_integration_operation(operation: &str) -> bool {
    matches!(
        operation,
        "register" | "ack" | "complete" | "delivery.inbox"
    )
}

pub fn is_context_operation(operation: &str) -> bool {
    matches!(
        operation,
        "context.grant"
            | "context.revoke"
            | "snapshot.publish"
            | "context.inspect"
            | "context.read"
    )
}

pub fn is_control_operation(operation: &str) -> bool {
    matches!(
        operation,
        "address.enable"
            | "address.disable"
            | "address.allow-remote"
            | "address.deny-remote"
            | "delivery.cancel-held"
            | "delivery.acknowledge"
            | "handoff.acknowledge"
            | "thread.pause"
            | "thread.resume"
            | "thread.stop"
            | "thread.continue"
            | "worktree.pause"
            | "worktree.resume"
    )
}

/// Tag JSON schema failures at the input boundary, before operational work.
pub(crate) fn input_schema_error(error: serde_json::Error) -> String {
    format!("invalid operation input: {error}")
}

#[doc(hidden)]
pub fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    #[test]
    fn lower_hex_matches_the_sha256_digests_earlier_releases_persisted() {
        assert_eq!(
            super::lower_hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
