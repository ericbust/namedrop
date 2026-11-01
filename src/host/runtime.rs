use super::*;
use crate::{HerdrCommandExecutor, runtime::HostKind};

#[derive(Clone, Copy)]
pub enum RuntimeHost<E: HerdrCommandExecutor> {
    Tui(TuiHost<E>),
    Hc(HcHost<'static, E>),
}

impl<E: HerdrCommandExecutor> RuntimeHost<E> {
    pub fn new(kind: HostKind, executor: E) -> Self {
        match kind {
            HostKind::Tui => Self::Tui(TuiHost::new(executor)),
            HostKind::Hc => Self::Hc(HcHost::new(executor)),
        }
    }
}

impl<E: HerdrCommandExecutor> Host for RuntimeHost<E> {
    fn kind(&self) -> HostKind {
        match self {
            Self::Tui(_) => HostKind::Tui,
            Self::Hc(_) => HostKind::Hc,
        }
    }
    fn environment_snapshot(&self) -> Option<&crate::environment::EnvironmentSnapshot> {
        match self {
            Self::Tui(host) => host.environment_snapshot(),
            Self::Hc(host) => host.environment_snapshot(),
        }
    }
    fn space(
        &self,
        at: SpaceLocator<'_>,
        budget: Option<&dyn Budget>,
    ) -> Result<DiscoveredWorktree, HostError> {
        match self {
            Self::Tui(host) => host.space(at, budget),
            Self::Hc(host) => host.space(at, budget),
        }
    }
    fn spaces(&self) -> Result<Vec<ListedWorkspace>, HostError> {
        match self {
            Self::Tui(host) => host.spaces(),
            Self::Hc(host) => host.spaces(),
        }
    }
    fn panes(&self, scope: PaneScope<'_>) -> Result<Vec<DiscoveredPane>, HostError> {
        match self {
            Self::Tui(host) => host.panes(scope),
            Self::Hc(host) => host.panes(scope),
        }
    }
    fn process_info(&self, pane: &str) -> Option<ProcessInfo> {
        match self {
            Self::Tui(host) => host.process_info(pane),
            Self::Hc(host) => host.process_info(pane),
        }
    }
    fn read(
        &self,
        pane: &str,
        lines: usize,
        budget: Option<&dyn Budget>,
    ) -> Result<String, HostError> {
        match self {
            Self::Tui(host) => host.read(pane, lines, budget),
            Self::Hc(host) => host.read(pane, lines, budget),
        }
    }
    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptFailure> {
        match self {
            Self::Tui(host) => host.prompt(pane, text),
            Self::Hc(host) => host.prompt(pane, text),
        }
    }
    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), PromptFailure> {
        match self {
            Self::Tui(host) => host.run_in_pane(pane, command),
            Self::Hc(host) => host.run_in_pane(pane, command),
        }
    }
    fn interrupt(&self, pane: &str) -> Result<(), HostError> {
        match self {
            Self::Tui(host) => host.interrupt(pane),
            Self::Hc(host) => host.interrupt(pane),
        }
    }
    fn report_metadata(&self, pane: &str, metadata: &PaneMetadata) -> Result<(), HostError> {
        match self {
            Self::Tui(host) => host.report_metadata(pane, metadata),
            Self::Hc(host) => host.report_metadata(pane, metadata),
        }
    }
    fn notify(&self, title: &str, body: &str, sound: bool) -> Result<(), HostError> {
        match self {
            Self::Tui(host) => host.notify(title, body, sound),
            Self::Hc(host) => host.notify(title, body, sound),
        }
    }
    fn supports_tab_labels(&self) -> bool {
        match self {
            Self::Tui(host) => host.supports_tab_labels(),
            Self::Hc(host) => host.supports_tab_labels(),
        }
    }
    fn tab_label(&self, tab: &str) -> Option<String> {
        match self {
            Self::Tui(host) => host.tab_label(tab),
            Self::Hc(host) => host.tab_label(tab),
        }
    }
    fn rename_tab(&self, tab: &str, label: &str) -> Result<(), HostError> {
        match self {
            Self::Tui(host) => host.rename_tab(tab, label),
            Self::Hc(host) => host.rename_tab(tab, label),
        }
    }
    fn open_popup(&self, popup: &Popup<'_>) -> Result<serde_json::Value, HostError> {
        match self {
            Self::Tui(host) => host.open_popup(popup),
            Self::Hc(host) => host.open_popup(popup),
        }
    }
    fn reload_config(&self) -> Result<(), HostError> {
        match self {
            Self::Tui(host) => host.reload_config(),
            Self::Hc(host) => host.reload_config(),
        }
    }
    fn plugin_root(&self) -> Option<PathBuf> {
        match self {
            Self::Tui(host) => host.plugin_root(),
            Self::Hc(host) => host.plugin_root(),
        }
    }
    fn socket_id(&self) -> Option<String> {
        match self {
            Self::Tui(host) => host.socket_id(),
            Self::Hc(host) => host.socket_id(),
        }
    }
    fn caller_identity(&self, claim: &CallerClaim) -> Result<CallerIdentity, HostError> {
        match self {
            Self::Tui(host) => host.caller_identity(claim),
            Self::Hc(host) => host.caller_identity(claim),
        }
    }
}
