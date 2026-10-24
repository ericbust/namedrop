use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "namedrop", version, about)]
pub struct Cli {
    /// Print the complete JSON response, including when running in a terminal.
    #[arg(long, global = true, conflicts_with = "human")]
    pub json: bool,

    /// Print readable output, including when piping or saving it to a file.
    #[arg(long, global = true, conflicts_with = "json")]
    pub human: bool,

    /// Structured input for agent-facing operations. Run `describe
    /// <operation-token>` for the schema and examples.
    #[arg(long, global = true, value_name = "JSON", default_value = "{}")]
    input_json: String,

    /// Read structured input from a file without shell substitution.
    #[arg(
        long,
        global = true,
        conflicts_with = "input_json",
        value_name = "PATH"
    )]
    input_json_file: Option<PathBuf>,

    #[command(flatten)]
    pub runtime: RuntimeOptions,

    #[command(subcommand)]
    command: Command,
}

/// Explicit transport context for permission-matchable direct invocations.
/// Omitted values retain the existing Herdr environment behavior.
#[derive(Debug, Default, Args)]
pub struct RuntimeOptions {
    #[arg(long, value_name = "PATH")]
    pub plugin_state: Option<PathBuf>,
    #[arg(long, global = true, value_name = "SOCKET")]
    pub herdr_socket: Option<String>,
    #[arg(long, global = true, value_name = "WORKSPACE")]
    pub herdr_workspace: Option<String>,
    #[arg(long, global = true, value_name = "PANE")]
    pub herdr_pane: Option<String>,
}

impl Cli {
    pub fn human_output(&self, terminal: bool) -> bool {
        self.human || (!self.json && terminal && !self.machine_operation())
    }

    fn machine_operation(&self) -> bool {
        matches!(
            self.operation(),
            "describe"
                | "register"
                | "ack"
                | "complete"
                | "send"
                | "reply"
                | "delivery.inbox"
                | "handoff.begin"
                | "handoff.resolve"
                | "handoff.adopt"
                | "handoff.status"
                | "handoff.results"
                | "handoff.preview"
        ) || matches!(
            self.command,
            Command::Hook { .. }
                | Command::Event { .. }
                | Command::Snapshot { .. }
                | Command::Context { .. }
        )
    }

    pub fn operation(&self) -> &'static str {
        self.command.operation()
    }

    pub fn input_is_file(&self) -> bool {
        self.input_json_file.is_some()
    }

    pub fn input(&self) -> Result<Value, String> {
        let text = match &self.input_json_file {
            Some(path) => {
                std::fs::read_to_string(path).map_err(|error| format!("input file: {error}"))?
            }
            None => self.input_json.clone(),
        };
        serde_json::from_str(&text).map_err(crate::input_schema_error)
    }

    pub fn described_operation(&self) -> Option<&str> {
        match &self.command {
            Command::Describe(args) => args.operation.as_deref(),
            _ => None,
        }
    }

    pub fn user_prompt_submit_hook_state_directory(&self) -> Option<&Path> {
        match &self.command {
            Command::Hook {
                command: HookCommand::UserPromptSubmit(args),
            } => Some(&args.state_directory),
            _ => None,
        }
    }

    pub fn mailbox_wake_hook_state_directory(&self) -> Option<&Path> {
        match &self.command {
            Command::Hook {
                command: HookCommand::MailboxWake(args),
            } => Some(&args.state_directory),
            _ => None,
        }
    }

    pub fn session_start_hook_state_directory(&self) -> Option<&Path> {
        match &self.command {
            Command::Hook {
                command: HookCommand::SessionStart(args),
            } => Some(&args.state_directory),
            _ => None,
        }
    }

    pub fn is_agent_status_changed_hook(&self) -> bool {
        matches!(
            self.command,
            Command::Hook {
                command: HookCommand::AgentStatusChanged
            }
        )
    }

    pub fn is_agent_status_worker_hook(&self) -> bool {
        matches!(
            self.command,
            Command::Hook {
                command: HookCommand::AgentStatusWorker
            }
        )
    }

    /// The name supplied by the one-step setup command. Both `reviewer` and
    /// `@reviewer` are accepted; the stored name never includes the at-sign.
    pub fn agent_name(&self) -> Option<&str> {
        match &self.command {
            Command::Name(args) => Some(&args.alias),
            _ => None,
        }
    }

    pub fn name_pane_type(&self) -> Option<crate::PaneType> {
        match &self.command {
            Command::Name(args) => args.pane_type,
            _ => None,
        }
    }

    /// The persona to save while naming. `None` keeps the saved one; `--generic`
    /// is the empty description that clears it.
    pub fn name_persona(&self) -> Option<&str> {
        match &self.command {
            Command::Name(args) if args.generic => Some(""),
            Command::Name(args) => args.persona.as_deref(),
            _ => None,
        }
    }
    pub fn terminal_attempt(&self) -> Option<&str> {
        match &self.command {
            Command::TerminalExecute { attempt_id } => Some(attempt_id),
            _ => None,
        }
    }

    pub fn naming_ui(&self) -> Option<(bool, UiMode)> {
        match self.command {
            Command::Ui {
                command: UiCommand::Launch { mode },
            } => Some((true, mode)),
            Command::Ui {
                command: UiCommand::Run { mode },
            } => Some((false, mode)),
            _ => None,
        }
    }

    pub fn shortcuts_config(&self) -> Option<Option<&Path>> {
        match &self.command {
            Command::Ui {
                command: UiCommand::InstallShortcuts { config },
            } => Some(config.as_deref()),
            _ => None,
        }
    }

    pub fn is_reset_tour(&self) -> bool {
        matches!(
            self.command,
            Command::Ui {
                command: UiCommand::ResetTour
            }
        )
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open a naming, name-management, Doctor, agent status, or tour popup.
    Ui {
        #[command(subcommand)]
        command: UiCommand,
    },
    #[command(hide = true)]
    TerminalExecute { attempt_id: String },
    /// Inspect handoff progress, partial results, or a proposed send.
    Handoff {
        #[command(subcommand)]
        command: HandoffCommand,
    },
    /// Manage saved groups of agent mentions.
    Group {
        #[command(subcommand)]
        command: GroupCommand,
    },
    /// Name this pane; detect a terminal or agent, or choose --type explicitly.
    Name(NameArgs),
    /// Activate Namedrop for this Herdr space after confirmation.
    Activate,
    /// Deactivate Namedrop for this Herdr space while preserving its data.
    Deactivate,
    /// Inspect collaboration health without changing state.
    Doctor,
    /// Permanently purge this space's Namedrop data after confirmation.
    Purge,
    /// Describe agent-facing operations, input schemas, and examples.
    Describe(DescribeArgs),
    /// Create and manage agent names in this Herdr space.
    Address {
        #[command(subcommand)]
        command: AddressCommand,
    },
    /// Send a collaboration message; leading mentions are its recipients.
    Send,
    /// Reply to a collaboration message.
    Reply,
    /// Answer a registration challenge for the agent in this pane.
    Register,
    /// Acknowledge a delivered attempt before substantive work.
    Ack,
    /// Finish a delivery attempt with an explicit no-reply disposition.
    Complete(CompleteArgs),
    /// Recover delivery envelopes this agent session still owes.
    Delivery {
        #[command(subcommand)]
        command: DeliveryCommand,
    },
    /// Publish a bounded working-context snapshot for named collaborators.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    /// Grant, revoke, inspect, and read bounded collaboration context.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Pause, resume, stop, or continue one collaboration thread.
    Thread {
        #[command(subcommand)]
        command: ThreadCommand,
    },
    /// Pause or resume all collaboration in this Herdr space.
    Space {
        #[command(subcommand)]
        command: SpaceCommand,
    },
    /// Recover after a plugin restart; ambiguous in-flight delivery attempts are held.
    Reconcile,
    /// Handle one Herdr lifecycle event.
    Event {
        #[command(subcommand)]
        command: EventCommand,
    },
    /// Runtime hooks installed for supported agents.
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
}

impl Command {
    fn operation(&self) -> &'static str {
        match self {
            Self::Ui {
                command: UiCommand::Launch { .. },
            } => "ui.launch",
            Self::Ui {
                command: UiCommand::Run { .. },
            } => "ui.run",
            Self::Ui {
                command: UiCommand::InstallShortcuts { .. },
            } => "ui.install-shortcuts",
            Self::Ui {
                command: UiCommand::ResetTour,
            } => "ui.reset-tour",
            Self::TerminalExecute { .. } => "terminal.execute",
            Self::Handoff { command } => command.operation(),
            Self::Group { command } => command.operation(),
            Self::Name(_) => "name",
            Self::Activate => "activate",
            Self::Deactivate => "deactivate",
            Self::Doctor => "doctor",
            Self::Purge => "purge",
            Self::Describe(_) => "describe",
            Self::Address { command } => command.operation(),
            Self::Send => "send",
            Self::Reply => "reply",
            Self::Register => "register",
            Self::Ack => "ack",
            Self::Complete(args) => {
                let _ = args.no_reply;
                "complete"
            }
            Self::Delivery { command } => command.operation(),
            Self::Snapshot { command } => command.operation(),
            Self::Context { command } => command.operation(),
            Self::Thread { command } => command.operation(),
            Self::Space { command } => command.operation(),
            Self::Reconcile => "reconcile",
            Self::Event { command } => command.operation(),
            Self::Hook { command } => command.operation(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum UiCommand {
    /// Add Namedrop's popup shortcuts without replacing any existing binding.
    #[command(alias = "install-status-shortcut")]
    InstallShortcuts {
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    Launch {
        #[arg(value_enum)]
        mode: UiMode,
    },
    Run {
        #[arg(value_enum, default_value = "name")]
        mode: UiMode,
    },
    /// Show the Namedrop tour again the next time an agent pane is detected.
    ResetTour,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum UiMode {
    Name,
    Manage,
    Doctor,
    Status,
    Tour,
}

#[derive(Debug, Subcommand)]
enum HandoffCommand {
    Begin,
    Resolve,
    Adopt,
    /// Acknowledge a resolved warning after human review; preserves the outcome and report.
    Acknowledge,
    Status,
    Results,
    Preview,
}
impl HandoffCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::Begin => "handoff.begin",
            Self::Resolve => "handoff.resolve",
            Self::Adopt => "handoff.adopt",
            Self::Acknowledge => "handoff.acknowledge",
            Self::Status => "handoff.status",
            Self::Results => "handoff.results",
            Self::Preview => "handoff.preview",
        }
    }
}

#[derive(Debug, Subcommand)]
enum GroupCommand {
    List,
    Set,
    Remove,
}
impl GroupCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::List => "group.list",
            Self::Set => "group.set",
            Self::Remove => "group.remove",
        }
    }
}

#[derive(Debug, Args)]
struct NameArgs {
    /// The name to use in prompts, with or without a leading @.
    #[arg(value_name = "@NAME")]
    alias: String,
    /// Recipient type, independent of its name. Omit to detect or retain it.
    #[arg(long = "type", value_enum)]
    pane_type: Option<crate::PaneType>,
    /// What this agent specializes in. Omit to keep the saved persona.
    #[arg(long, value_name = "TEXT")]
    persona: Option<String>,
    /// Clear any saved persona; a Generic agent still takes ordinary work.
    #[arg(long, conflicts_with = "persona")]
    generic: bool,
}

#[derive(Debug, Args)]
struct DescribeArgs {
    /// One operation token, such as `send`; omit it to describe all operations.
    #[arg(value_name = "OPERATION-TOKEN")]
    operation: Option<String>,
}

#[derive(Debug, Args)]
struct CompleteArgs {
    /// Record the explicit no-reply disposition.
    #[arg(long, required = true)]
    no_reply: bool,
}

#[derive(Debug, Subcommand)]
enum AddressCommand {
    /// Create an agent name.
    Create,
    /// List this space's names and live agents.
    List,
    /// Bind a name to a live Herdr pane.
    Bind,
    /// Unbind a name from its pane.
    Unbind,
    /// Rename an agent.
    Rename,
    /// Remove an agent name and release its aliases.
    Remove,
    /// Retire a name while retaining message history.
    Retire,
    /// Set or clear an agent's persona; blank or null text means Generic.
    Persona,
    /// Release a reserved alias for reuse.
    ReleaseAlias,
    /// Enable delivery to a named agent.
    Enable,
    /// Disable delivery to a named agent.
    Disable,
}

impl AddressCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::Create => "address.create",
            Self::List => "address.list",
            Self::Bind => "address.bind",
            Self::Unbind => "address.unbind",
            Self::Rename => "address.rename",
            Self::Remove => "address.remove",
            Self::Retire => "address.retire",
            Self::Persona => "address.persona",
            Self::ReleaseAlias => "address.release-alias",
            Self::Enable => "address.enable",
            Self::Disable => "address.disable",
        }
    }
}

#[derive(Debug, Subcommand)]
enum DeliveryCommand {
    /// List outstanding envelopes this agent session still owes.
    Inbox,
    /// Cancel one held delivery after human confirmation; never retry or stop its agent.
    CancelHeld,
    /// Acknowledge a settled attempt's missing disposition after human confirmation, once its recipient is retired or its session was replaced.
    Acknowledge,
}

impl DeliveryCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::Inbox => "delivery.inbox",
            Self::CancelHeld => "delivery.cancel-held",
            Self::Acknowledge => "delivery.acknowledge",
        }
    }
}

#[derive(Debug, Subcommand)]
enum SnapshotCommand {
    /// Publish a working-context snapshot to named grantees.
    Publish,
}

impl SnapshotCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::Publish => "snapshot.publish",
        }
    }
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    Grant,
    Revoke,
    Inspect,
    Read,
}

impl ContextCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::Grant => "context.grant",
            Self::Revoke => "context.revoke",
            Self::Inspect => "context.inspect",
            Self::Read => "context.read",
        }
    }
}

#[derive(Debug, Subcommand)]
enum ThreadCommand {
    Pause,
    Resume,
    Stop,
    Continue,
}

impl ThreadCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::Pause => "thread.pause",
            Self::Resume => "thread.resume",
            Self::Stop => "thread.stop",
            Self::Continue => "thread.continue",
        }
    }
}

#[derive(Debug, Subcommand)]
enum SpaceCommand {
    Pause,
    Resume,
}

impl SpaceCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::Pause => "worktree.pause",
            Self::Resume => "worktree.resume",
        }
    }
}

#[derive(Debug, Subcommand)]
enum EventCommand {
    PaneLifecycle,
    AgentStatusChanged,
}

impl EventCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::PaneLifecycle => "event.pane-lifecycle",
            Self::AgentStatusChanged => "event.agent-status-changed",
        }
    }
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    AgentStatusChanged,
    AgentStatusWorker,
    MailboxWake(HookArgs),
    SessionStart(HookArgs),
    UserPromptSubmit(HookArgs),
}

impl HookCommand {
    fn operation(&self) -> &'static str {
        match self {
            Self::AgentStatusChanged => "hook.agent-status-changed",
            Self::AgentStatusWorker => "hook.agent-status-worker",
            Self::MailboxWake(_) => "hook.mailbox-wake",
            Self::SessionStart(_) => "hook.session-start",
            Self::UserPromptSubmit(_) => "hook.user-prompt-submit",
        }
    }
}

#[derive(Debug, Args)]
struct HookArgs {
    /// Plugin-owned state directory for this installed hook.
    #[arg(long, value_name = "PATH")]
    state_directory: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;
    use toml_edit::DocumentMut;

    #[test]
    fn removed_view_commands_stay_out_of_the_cli() {
        for command in ["timeline", "conversation"] {
            assert!(
                Cli::try_parse_from(["namedrop", command]).is_err(),
                "removed command {command} was accepted"
            );
        }
    }

    #[test]
    fn plugin_manifest_declares_setup_update_and_maintenance_popups() {
        let source = include_str!("../herdr-plugin.toml");
        let manifest = source.parse::<DocumentMut>().unwrap();
        let panes = manifest["panes"].as_array_of_tables().unwrap();
        assert_eq!(
            panes
                .iter()
                .map(|pane| (pane["id"].as_str(), pane["placement"].as_str()))
                .collect::<Vec<_>>(),
            [
                (Some("setup"), Some("tab")),
                (Some("update"), Some("tab")),
                (Some("naming"), Some("popup")),
                (Some("management"), Some("popup")),
                (Some("doctor"), Some("popup")),
                (Some("agent-status"), Some("popup")),
                (Some("tour"), Some("popup"))
            ]
        );
        assert!(Cli::try_parse_from(["namedrop", "ui", "launch", "name"]).is_ok());
        assert!(Cli::try_parse_from(["namedrop", "ui", "launch", "manage"]).is_ok());
        assert!(Cli::try_parse_from(["namedrop", "ui", "launch", "doctor"]).is_ok());
        assert!(Cli::try_parse_from(["namedrop", "ui", "launch", "status"]).is_ok());
        assert!(Cli::try_parse_from(["namedrop", "ui", "launch", "tour"]).is_ok());
        assert!(Cli::try_parse_from(["namedrop", "ui", "run", "tour"]).is_ok());
        let reset = Cli::try_parse_from(["namedrop", "ui", "reset-tour"]).unwrap();
        assert!(reset.is_reset_tour());
        assert!(Cli::try_parse_from(["namedrop", "ui", "install-shortcuts"]).is_ok());
        assert!(Cli::try_parse_from(["namedrop", "ui", "install-status-shortcut"]).is_ok());
    }
}
