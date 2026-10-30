use crate::{DiscoveredPane, DiscoveredWorktree, PromptFailureKind};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

pub mod tui;
pub use tui::TuiHost;

#[derive(Debug)]
pub struct HostError {
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for HostError {}
impl From<String> for HostError {
    fn from(message: String) -> Self {
        Self {
            code: None,
            message,
        }
    }
}
impl From<HostError> for String {
    fn from(error: HostError) -> Self {
        error.message
    }
}

#[derive(Debug)]
pub struct PromptFailure {
    pub kind: PromptFailureKind,
    pub message: String,
}
impl std::fmt::Display for PromptFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl From<PromptFailure> for String {
    fn from(error: PromptFailure) -> Self {
        error.message
    }
}

pub trait Budget {
    fn remaining(&self) -> Result<Duration, String>;
    fn cancelled(&self) -> &AtomicBool;
    fn max_output_bytes(&self) -> usize;
}

pub enum SpaceLocator<'a> {
    Checkout(&'a Path),
    Workspace(&'a str),
}
pub enum PaneScope<'a> {
    Workspace(&'a str),
    All,
}

pub enum PaneMetadata {
    Identity {
        title: String,
        ready: String,
        working: String,
        blocked: String,
        unknown: String,
    },
    Counters {
        awaiting: u64,
        attention: u64,
    },
    Relay {
        text: String,
        ttl_ms: u64,
    },
}

pub struct Popup<'a> {
    pub entrypoint: &'a str,
    pub width: &'a str,
    pub height: &'a str,
    pub env: &'a [(&'a str, String)],
}

pub struct CallerClaim {
    pub workspace: Option<String>,
    pub pane: Option<String>,
    pub inherited_pane: Option<String>,
    pub agent_sessions: Vec<String>,
}
pub struct CallerIdentity {
    pub workspace: Option<String>,
    pub pane: Option<String>,
}

pub struct ListedWorkspace {
    pub id: String,
    pub label: String,
    /// The repository a workspace's checkout belongs to, when Herdr stamped one.
    pub repo: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct ProcessInfo {
    pub shell_pid: u64,
    pub foreground_process_group_id: u64,
    #[serde(default)]
    pub foreground_processes: Vec<Process>,
}
#[derive(serde::Deserialize)]
pub struct Process {
    pub pid: u32,
}

pub trait Host {
    fn space(
        &self,
        at: SpaceLocator<'_>,
        budget: Option<&dyn Budget>,
    ) -> Result<DiscoveredWorktree, HostError>;
    fn spaces(&self) -> Result<Vec<ListedWorkspace>, HostError>;
    fn panes(&self, scope: PaneScope<'_>) -> Result<Vec<DiscoveredPane>, HostError>;
    fn process_info(&self, pane: &str) -> Option<ProcessInfo>;
    fn read(
        &self,
        pane: &str,
        lines: usize,
        budget: Option<&dyn Budget>,
    ) -> Result<String, HostError>;
    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptFailure>;
    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), PromptFailure>;
    fn interrupt(&self, pane: &str) -> Result<(), HostError>;
    fn report_metadata(&self, pane: &str, metadata: &PaneMetadata) -> Result<(), HostError>;
    fn notify(&self, title: &str, body: &str, sound: bool) -> Result<(), HostError>;
    fn tab_label(&self, tab: &str) -> Option<String>;
    fn rename_tab(&self, tab: &str, label: &str) -> Result<(), HostError>;
    fn open_popup(&self, popup: &Popup<'_>) -> Result<serde_json::Value, HostError>;
    fn reload_config(&self) -> Result<(), HostError>;
    fn plugin_root(&self) -> Option<PathBuf>;
    fn socket_id(&self) -> Option<String>;
    fn caller_identity(&self, claim: &CallerClaim) -> Result<CallerIdentity, HostError>;
}
