use crate::{
    AgentRuntime, AttentionItemDraft, AttentionItemId, AttentionSourceKind, DiscoveredWorktree,
    LifecycleRuntime, MutationAttribution, StateStore, integration_package,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{ErrorKind, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use toml_edit::{DocumentMut, Item};

mod claude_permissions;
mod command;
pub(crate) use claude_permissions::remove_claude_permissions;
pub(crate) use command::install_shell_command;

/// The marker that records this plugin's ownership and installed version of one
/// package directory. A pre-existing target without it belongs to somebody else.
const OWNERSHIP_MARKER_FILE: &str = ".namedrop-package.json";
const PACKAGE_OWNER: &str = "namedrop";
const INSTRUCTIONS_FILE: &str = "SKILL.md";
const DESCRIPTOR_FILE: &str = "tool.json";
const CODEX_MANIFEST_FILE: &str = ".codex-plugin/plugin.json";
const CLAUDE_MANIFEST_FILE: &str = ".claude-plugin/plugin.json";
const HOOKS_FILE: &str = "hooks/hooks.json";
const HOOK_SCRIPT_FILE: &str = "scripts/user-prompt-submit";
const SESSION_START_SCRIPT_FILE: &str = "scripts/session-start";
const MARKETPLACE_FILE: &str = ".agents/plugins/marketplace.json";
const CODEX_RULES_MARKER: &str = "# Managed by Namedrop: collaboration command permissions.\n";
const INSTALLED_COMMAND_MARKER: &str = "<!-- namedrop-installed-command -->";
const PACKAGE_DIRECTORIES: [&str; 6] = [
    ".agents",
    ".agents/plugins",
    ".claude-plugin",
    ".codex-plugin",
    "hooks",
    "scripts",
];
const CODEX_SNAPSHOT_FILES: [&str; 6] = [
    CODEX_MANIFEST_FILE,
    INSTRUCTIONS_FILE,
    DESCRIPTOR_FILE,
    HOOKS_FILE,
    HOOK_SCRIPT_FILE,
    SESSION_START_SCRIPT_FILE,
];

/// Runtime configuration resolved at the CLI boundary. Library callers default
/// to directories under their explicit home, independent of process settings.
#[derive(Clone, Copy, Default)]
pub struct AgentConfigDirectories<'a> {
    pub claude: Option<&'a Path>,
    pub codex: Option<&'a Path>,
}
impl AgentConfigDirectories<'_> {
    fn claude(self, home: &Path) -> PathBuf {
        self.claude
            .map(Path::to_owned)
            .unwrap_or_else(|| home.join(".claude"))
    }
    pub(crate) fn codex(self, home: &Path) -> PathBuf {
        self.codex
            .map(Path::to_owned)
            .unwrap_or_else(|| home.join(".codex"))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CodexMarketplaceConfiguration {
    Missing,
    Matching,
    Conflicting,
}

/// The path components each supported runtime already discovers skills under,
/// relative to an installation root: the user's home directory in production and
/// an isolated root in tests.
fn package_components(runtime: AgentRuntime) -> [&'static str; 3] {
    match runtime {
        AgentRuntime::Codex => [".agents", "skills", "namedrop"],
        AgentRuntime::ClaudeCode => [".claude", "skills", "namedrop"],
    }
}

/// The dedicated package directory for one runtime under an installation root.
pub fn package_directory(root: &Path, runtime: AgentRuntime) -> PathBuf {
    package_components(runtime)
        .iter()
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

/// The two shared user-level package directories, in the order the activation
/// confirmation discloses them.
fn user_package_directories(home_directory: &Path) -> [(AgentRuntime, PathBuf); 2] {
    [AgentRuntime::Codex, AgentRuntime::ClaudeCode]
        .map(|runtime| (runtime, package_directory(home_directory, runtime)))
}

/// The disclosure the first Worktree activation must show before anything is
/// written, naming both shared user-level targets.
pub fn disclosed_user_package_targets(
    home_directory: Option<&Path>,
    config: AgentConfigDirectories<'_>,
) -> String {
    let Some(home_directory) = home_directory else {
        return "unavailable: no user home directory resolved".to_owned();
    };
    let packages = user_package_directories(home_directory)
        .iter()
        .map(|(runtime, directory)| format!("{}: {}", runtime.as_str(), directory.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let codex_home = config.codex(home_directory);
    let claude_settings = claude_permissions::settings_path(&config.claude(home_directory))
        .unwrap_or_else(|_| home_directory.join(".claude/settings.json"));
    format!(
        "{packages}, Claude collaboration permissions: {}, Codex collaboration permissions: {}, shell command: {}, PATH entries in zsh/bash startup files",
        claude_settings.display(),
        codex_home.join("rules/namedrop.rules").display(),
        home_directory.join(".local/bin/namedrop").display()
    )
}

/// Read-only repair guidance. The Herdr activation runner applies supported
/// Codex install steps automatically; hook trust remains a human action.
pub(crate) fn user_hook_activation_guidance(
    home_directory: Option<&Path>,
    config: AgentConfigDirectories<'_>,
) -> Value {
    let mut codex_cache_current = false;
    let mut codex_migration_ready = false;
    let codex_steps = match home_directory {
        Some(home_directory) => {
            let package = package_directory(home_directory, AgentRuntime::Codex);
            if let Some(package_text) = package.to_str() {
                let (marketplace, plugin_current, plugin_enabled) =
                    codex_plugin_configuration(home_directory, &package, config);
                codex_cache_current =
                    marketplace == CodexMarketplaceConfiguration::Matching && plugin_current;
                codex_migration_ready = codex_cache_current || plugin_enabled == Some(false);
                let mut steps = Vec::new();
                if marketplace == CodexMarketplaceConfiguration::Conflicting {
                    steps.push("codex plugin marketplace remove namedrop --json".to_owned());
                }
                if marketplace != CodexMarketplaceConfiguration::Matching {
                    steps.push(format!(
                        "codex plugin marketplace add {} --json",
                        shell_single_quote(package_text)
                    ));
                }
                if marketplace != CodexMarketplaceConfiguration::Matching || !plugin_current {
                    steps.push("codex plugin add namedrop@namedrop --json".to_owned());
                }
                steps.push("Start a new Codex session and approve Namedrop in /hooks if prompted. Verify a request and reply; doctor alone cannot verify hooks.".to_owned());
                json!(steps)
            } else {
                json!(["resolve an absolute UTF-8 HOME before activating the Codex plugin"])
            }
        }
        None => json!(["resolve an absolute HOME before activating the Codex plugin"]),
    };
    let mut guidance = json!({
        "codex": {
            "status": "human_action_required",
            "cache_current": codex_cache_current,
            "migration_ready": codex_migration_ready,
            "steps": codex_steps
        },
        "claude-code": {
            "status": "loads_next_session",
            "steps": ["Start a new Claude Code session or run /reload-plugins. Verify a request and reply; doctor alone cannot verify hooks."]
        }
    });
    if let Some(advice) = crate::codex_app_server::observe(home_directory, config).advice() {
        guidance["codex"]["app_server"] = advice;
    }
    guidance
}

pub(crate) fn native_session_setup(runtime: AgentRuntime) -> Value {
    let host_runtime = match runtime {
        AgentRuntime::Codex => "codex",
        AgentRuntime::ClaudeCode => "claude",
    };
    let explanation = match runtime {
        AgentRuntime::Codex => {
            "Herdr has not reported a native agent session identity. A fresh Codex can show its composer before running SessionStart; submit your first prompt in that pane, then check registration. Mentions stay queued until the real session is registered. If identity is still missing after a turn, install/update the Herdr native integration, restart Codex and approve its native hooks if requested."
        }
        AgentRuntime::ClaudeCode => {
            "Herdr has not reported a native agent session identity. For a new setup, install the Herdr native integration, then restart the agent and approve its native hooks if requested."
        }
    };
    json!({
        "status":"session_identity_missing",
        "command":["herdr","integration","install",host_runtime],
        "explanation": explanation,
    })
}

/// Reads Codex's persisted configuration and versioned plugin snapshot without
/// invoking Codex or mutating its shared state. Missing, unreadable, malformed,
/// stale, or mismatched state is treated conservatively as setup still being
/// required.
fn codex_plugin_configuration(
    home_directory: &Path,
    package: &Path,
    config: AgentConfigDirectories<'_>,
) -> (CodexMarketplaceConfiguration, bool, Option<bool>) {
    let codex_home = config.codex(home_directory);
    if !codex_home.is_absolute() {
        return (CodexMarketplaceConfiguration::Missing, false, None);
    }
    let config = match fs::read_to_string(codex_home.join("config.toml")) {
        Ok(config) => config,
        Err(error) => {
            return (
                CodexMarketplaceConfiguration::Missing,
                false,
                (error.kind() == ErrorKind::NotFound).then_some(false),
            );
        }
    };
    let Ok(document) = config.parse::<DocumentMut>() else {
        return (CodexMarketplaceConfiguration::Missing, false, None);
    };
    let marketplace = document
        .get("marketplaces")
        .and_then(|marketplaces| marketplaces.get(PACKAGE_OWNER));
    let marketplace = match marketplace {
        None => CodexMarketplaceConfiguration::Missing,
        Some(marketplace)
            if marketplace.get("source_type").and_then(Item::as_str) == Some("local")
                && marketplace
                    .get("source")
                    .and_then(Item::as_str)
                    .is_some_and(|source| {
                        paths_resolve_to_same_target(Path::new(source), package)
                    }) =>
        {
            CodexMarketplaceConfiguration::Matching
        }
        Some(_) => CodexMarketplaceConfiguration::Conflicting,
    };
    let plugin_enabled = match document.get("plugins") {
        None => Some(false),
        Some(plugins) if plugins.as_table_like().is_some() => plugins
            .get("namedrop@namedrop")
            .map_or(Some(false), |plugin| {
                plugin.get("enabled").and_then(Item::as_bool)
            }),
        Some(_) => None,
    };
    let current_snapshot = fs::read(package.join(CODEX_MANIFEST_FILE))
        .ok()
        .and_then(|manifest| {
            let version = serde_json::from_slice::<Value>(&manifest)
                .ok()?
                .get("version")?
                .as_str()?
                .to_owned();
            let cached_package = codex_home
                .join("plugins/cache")
                .join(PACKAGE_OWNER)
                .join(PACKAGE_OWNER)
                .join(version);
            codex_snapshot_matches(package, &cached_package).then_some(())
        })
        .is_some();
    (
        marketplace,
        plugin_enabled == Some(true) && current_snapshot,
        plugin_enabled,
    )
}

fn codex_snapshot_matches(installed: &Path, cached: &Path) -> bool {
    CODEX_SNAPSHOT_FILES.into_iter().all(|relative| {
        fs::read(installed.join(relative))
            .ok()
            .zip(fs::read(cached.join(relative)).ok())
            .is_some_and(|(installed, cached)| installed == cached)
    }) && [HOOK_SCRIPT_FILE, SESSION_START_SCRIPT_FILE]
        .into_iter()
        .all(|script| {
            fs::metadata(cached.join(script)).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o100 != 0
            })
        })
}

fn paths_resolve_to_same_target(configured: &Path, installed: &Path) -> bool {
    configured == installed
        || fs::canonicalize(configured)
            .ok()
            .zip(fs::canonicalize(installed).ok())
            .is_some_and(|(configured, installed)| configured == installed)
}

/// Installs one runtime's Agent Integration package under `root`, writing only
/// the Agent Integration and its owned dual-runtime plugin, and only where the
/// installed content differs from the packaged content.
///
/// A pre-existing target this plugin does not own is a conflict: a non-directory
/// target, a symbolic link anywhere along the owned path, or a directory without
/// the ownership marker is refused without being followed or modified. `root`
/// itself is a trusted base rather than part of the owned path: a user whose home
/// directory is a symbolic link must have the package installed where their
/// runtime resolves `~/.agents` and `~/.claude`, which is through that link, and
/// an attacker who could substitute `root` already controls this process's whole
/// environment. Everything the plugin claims to own below `root` is checked.
///
/// Atomicity is per file, which is the unit the specification names: each owned
/// file is replaced by an atomic rename, so no reader sees a half-written file.
/// The files are published in independent renames rather than one transaction,
/// so a crash mid-refresh can leave a mixed-version package. That state is
/// inert — only the marker's `owner` is ever read for a decision, and staleness
/// is detected by comparing content — and the marker is published first so the
/// package stays one this plugin owns and the next lifecycle event completes it.
pub fn install_integration_package(
    root: &Path,
    runtime: AgentRuntime,
    executable: &Path,
    state_directory: &Path,
) -> Result<PathBuf, String> {
    install_integration_package_with_config(
        root,
        runtime,
        executable,
        state_directory,
        AgentConfigDirectories::default(),
    )
}

fn install_integration_package_with_config(
    root: &Path,
    runtime: AgentRuntime,
    executable: &Path,
    state_directory: &Path,
    config: AgentConfigDirectories<'_>,
) -> Result<PathBuf, String> {
    let PreparedPackage {
        directory,
        files,
        codex_policy,
    } = prepare_integration_package(root, runtime, executable, state_directory, config)?;
    fs::create_dir_all(&directory)
        .map_err(|error| conflict(&directory, &format!("cannot be created: {error}")))?;
    for relative in PACKAGE_DIRECTORIES {
        let path = directory.join(relative);
        fs::create_dir(&path)
            .or_else(|error| {
                if error.kind() == ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|error| conflict(&path, &format!("cannot be created: {error}")))?;
    }
    // Publish ownership first so an interrupted refresh can be completed later.
    for (name, contents, mode) in files {
        replace_if_changed(&directory.join(name), &contents, mode)?;
    }
    if let Some((path, contents)) = codex_policy {
        fs::create_dir_all(path.parent().expect("policy has a parent"))
            .map_err(|error| conflict(&path, &format!("cannot create rules directory: {error}")))?;
        replace_if_changed(&path, &contents, Some(0o600))?;
    }
    if runtime == AgentRuntime::ClaudeCode
        && let Err(error) =
            claude_permissions::refresh(&config.claude(root), executable, state_directory)
    {
        eprintln!(
            "Namedrop skipped Claude Code permissions: {error}; {}",
            claude_permissions::REPAIR_GUIDANCE
        );
    }
    Ok(directory)
}

struct PreparedPackage {
    directory: PathBuf,
    files: [(&'static str, String, Option<u32>); 9],
    codex_policy: Option<(PathBuf, String)>,
}

/// The same ownership, content and permission contract feeds installation and
/// read-only diagnosis. Preparing it never creates directories or files.
fn prepare_integration_package(
    root: &Path,
    runtime: AgentRuntime,
    executable: &Path,
    state_directory: &Path,
    config: AgentConfigDirectories<'_>,
) -> Result<PreparedPackage, String> {
    if !executable.is_absolute() {
        return Err(format!(
            "the {} Agent Integration hook requires an absolute executable path, not {}",
            runtime.as_str(),
            executable.display()
        ));
    }
    if !state_directory.is_absolute() {
        return Err(format!(
            "the {} Agent Integration hook requires an absolute state directory, not {}",
            runtime.as_str(),
            state_directory.display()
        ));
    }
    if runtime == AgentRuntime::ClaudeCode {
        validate_claude_permission_paths(executable, state_directory)?;
    }
    let directory = verified_package_directory(root, runtime)?;
    let package = integration_package(runtime);
    let instructions =
        installed_instructions(package.instructions, runtime, executable, state_directory)?;
    let hooks = hooks_manifest(&directory)?;
    let hook = hook_script(runtime, executable, state_directory)?;
    let session_start = session_start_script(runtime, executable, state_directory)?;
    let codex_version = codex_cache_version(&[
        runtime.as_str(),
        &instructions,
        package.tool_descriptor,
        &hooks,
        &hook,
        &session_start,
    ]);
    let files = [
        (OWNERSHIP_MARKER_FILE, ownership_marker(), None),
        (INSTRUCTIONS_FILE, instructions, None),
        (DESCRIPTOR_FILE, package.tool_descriptor.to_owned(), None),
        (CODEX_MANIFEST_FILE, codex_manifest(&codex_version), None),
        (CLAUDE_MANIFEST_FILE, claude_manifest(), None),
        (HOOKS_FILE, hooks, None),
        (HOOK_SCRIPT_FILE, hook, Some(0o700)),
        (SESSION_START_SCRIPT_FILE, session_start, Some(0o700)),
        (MARKETPLACE_FILE, marketplace_manifest(), None),
    ];
    verify_owned_plugin_targets(&directory, &files)?;
    let codex_policy = if runtime == AgentRuntime::Codex {
        Some(prepare_codex_policy(
            &config.codex(root),
            executable,
            state_directory,
        )?)
    } else {
        None
    };
    Ok(PreparedPackage {
        directory,
        files,
        codex_policy,
    })
}

pub(crate) fn inspect_integration_package(
    home: Option<&Path>,
    runtime: AgentRuntime,
    state: &Path,
    config: AgentConfigDirectories<'_>,
) -> Result<(), String> {
    let home = home.ok_or("no absolute user home directory is available")?;
    let executable = PathBuf::from(crate::collaboration_executable()?);
    let prepared = prepare_integration_package(home, runtime, &executable, state, config)?;
    for (name, contents, mode) in prepared.files {
        inspect_installed_file(&prepared.directory.join(name), &contents, mode)?;
    }
    if let Some((path, contents)) = prepared.codex_policy {
        inspect_installed_file(&path, &contents, Some(0o600))?;
    }
    if runtime == AgentRuntime::ClaudeCode {
        claude_permissions::inspect(&config.claude(home), &executable, state)?;
    }
    Ok(())
}

fn inspect_installed_file(path: &Path, expected: &str, mode: Option<u32>) -> Result<(), String> {
    let actual =
        fs::read(path).map_err(|error| conflict(path, &format!("cannot be read: {error}")))?;
    if actual != expected.as_bytes() {
        return Err(conflict(
            path,
            "does not match the current Namedrop executable and state; activate again to refresh it",
        ));
    }
    if let Some(mode) = mode {
        let metadata = fs::metadata(path).map_err(|error| conflict(path, &error.to_string()))?;
        if metadata.permissions().mode() & 0o777 != mode {
            return Err(conflict(
                path,
                "has incorrect permissions; activate again to repair them",
            ));
        }
    }
    Ok(())
}

pub(crate) fn codex_configuration_current(home: &Path, config: AgentConfigDirectories<'_>) -> bool {
    let (marketplace, current, _) =
        codex_plugin_configuration(home, &package_directory(home, AgentRuntime::Codex), config);
    marketplace == CodexMarketplaceConfiguration::Matching && current
}

fn collaboration_permission_operations() -> impl Iterator<Item = Vec<String>> {
    let catalog = crate::operation_catalog();
    catalog
        .operations
        .iter()
        .chain(
            catalog
                .management_operations
                .iter()
                .filter(|operation| operation.automatic_permission),
        )
        .map(|operation| operation.argv.clone())
}

/// Complete argv prefixes are the shared data shape for host permission rules.
fn collaboration_permission_patterns(executable: &Path, state: &Path) -> Vec<Vec<String>> {
    collaboration_permission_operations()
        .map(|argv| {
            let mut pattern = vec![
                executable.to_string_lossy().into_owned(),
                "--plugin-state".to_owned(),
                state.to_string_lossy().into_owned(),
            ];
            pattern.extend(argv);
            pattern
        })
        .collect()
}

fn validate_claude_permission_paths(executable: &Path, state: &Path) -> Result<(), String> {
    if executable.to_string_lossy().contains('*') || state.to_string_lossy().contains('*') {
        return Err("Claude Code permission paths must not contain a wildcard (*)".to_owned());
    }
    Ok(())
}

fn claude_allow_rules(executable: &Path, state: &Path) -> Result<Vec<String>, String> {
    validate_claude_permission_paths(executable, state)?;
    Ok(collaboration_permission_patterns(executable, state)
        .into_iter()
        .map(|pattern| {
            format!(
                "Bash({} --plugin-state {} {} *)",
                shell_single_quote(&pattern[0]),
                shell_single_quote(&pattern[2]),
                pattern[3..].join(" ")
            )
        })
        .collect())
}

fn prepare_codex_policy(
    codex_home: &Path,
    executable: &Path,
    state: &Path,
) -> Result<(PathBuf, String), String> {
    if !codex_home.is_absolute() {
        return Err("CODEX_HOME must be absolute to install Namedrop rules".to_owned());
    }
    for directory in [codex_home, &codex_home.join("rules")] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if !metadata.is_dir() || metadata.is_symlink() => {
                return Err(conflict(directory, "is not a regular directory"));
            }
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(conflict(directory, &error.to_string()));
            }
            _ => {}
        }
    }
    let path = codex_home.join("rules/namedrop.rules");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata.is_symlink()
                || !fs::read_to_string(&path)
                    .is_ok_and(|text| text.is_empty() || text.starts_with(CODEX_RULES_MARKER))
            {
                return Err(conflict(&path, "is not owned by Namedrop"));
            }
        }
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(conflict(&path, &error.to_string()));
        }
        _ => {}
    }
    let mut contents = CODEX_RULES_MARKER.to_owned();
    for pattern in collaboration_permission_patterns(executable, state) {
        contents.push_str(&format!("prefix_rule(pattern={}, decision=\"allow\", justification=\"Namedrop local collaboration\")\n", serde_json::to_string(&pattern).map_err(|error| error.to_string())?));
    }
    Ok((path, contents))
}

fn installed_instructions(
    instructions: &str,
    runtime: AgentRuntime,
    executable: &Path,
    state_directory: &Path,
) -> Result<String, String> {
    let (frontmatter, body) = instructions.split_once("\n---\n").ok_or_else(|| {
        format!(
            "the {} instructions have no frontmatter boundary",
            runtime.as_str()
        )
    })?;
    if !body.contains(INSTALLED_COMMAND_MARKER) {
        return Err(format!(
            "the {} instructions have no installed command marker",
            runtime.as_str()
        ));
    }
    let command_prefix = format!(
        "{} --plugin-state {}",
        shell_single_quote(&executable.to_string_lossy()),
        shell_single_quote(&state_directory.to_string_lossy())
    );
    let body = body.replace(
        INSTALLED_COMMAND_MARKER,
        &format!(
            "Use this exact installed command prefix whenever these instructions name `namedrop`: `{command_prefix}`."
        ),
    );
    let body = format!(
        "{body}\n## Invocation\n\nUse the installed executable directly with `--plugin-state <HERDR_PLUGIN_STATE_DIR>` before the bound operation. Append `--herdr-socket`, `--herdr-workspace`, and `--herdr-pane` with the exact `control.tool.environment` values. Keep operation tokens and flags as shown; single-quote literal values. For long JSON, write and verify a file in a separate tool call, then use `--input-json-file <absolute-path>` instead of `--input-json`; retain all bound fields. Use no environment prefix, shell substitution, pipes or wrappers. An OS sandbox access error is not an approval decision: request normal host approval for this exact invocation. If approval policy rejects it, report the blocker and keep the reply pending; do not broaden permissions or relay around the denial.\n"
    );
    if runtime != AgentRuntime::ClaudeCode {
        return Ok(format!(
            "{frontmatter}\n---\n{body}\nCodex: request elevated execution when required because Namedrop writes outside the workspace. Scoped installer rules load in the next session.\n"
        ));
    }
    Ok(format!(
        "{frontmatter}\n---\n{body}\nClaude Code: Activation installs scoped collaboration permissions in settings.json. Loading this skill grants no permissions.\n"
    ))
}

/// The outcome of refreshing both shared user-level packages for one lifecycle
/// event. A runtime whose package failed must not be challenged.
pub(crate) struct IntegrationPackageInstallation {
    outcomes: Vec<(AgentRuntime, Result<PathBuf, String>)>,
}

impl IntegrationPackageInstallation {
    pub(crate) fn failed(&self, runtime: AgentRuntime) -> bool {
        self.outcomes
            .iter()
            .any(|(candidate, outcome)| *candidate == runtime && outcome.is_err())
    }

    pub(crate) fn report(
        &self,
        home_directory: Option<&Path>,
        state: &Path,
        config: AgentConfigDirectories<'_>,
    ) -> Value {
        let mut report = serde_json::Map::new();
        for (runtime, outcome) in &self.outcomes {
            let permissions = if *runtime == AgentRuntime::ClaudeCode {
                let inspection = home_directory
                    .ok_or_else(|| "no user home directory is available".to_owned())
                    .and_then(|home| {
                        let executable = PathBuf::from(crate::collaboration_executable()?);
                        claude_permissions::inspect(&config.claude(home), &executable, state)
                    });
                Some(
                    json!({"current": inspection.is_ok(), "repair": if inspection.is_ok() { "Re-run activation." } else { claude_permissions::REPAIR_GUIDANCE }, "error": inspection.err()}),
                )
            } else {
                None
            };
            report.insert(runtime.as_str().to_owned(), json!({
                "permissions": permissions,
                "installed": outcome.is_ok(),
                "path": home_directory.map(|home| package_directory(home, *runtime)),
                "error": outcome.as_ref().err(),
                "hook_verification": "unverified",
                "configuration": if outcome.is_err() { "installation_failed" } else if *runtime == AgentRuntime::Codex {
                    if outcome.is_ok() && home_directory.is_some_and(|home| codex_configuration_current(home, config)) { "current" } else { "setup_required" }
                } else { "loads_next_session" },
            }));
        }
        Value::Object(report)
    }

    pub(crate) fn hook_activation(
        &self,
        home_directory: Option<&Path>,
        config: AgentConfigDirectories<'_>,
    ) -> Value {
        let mut guidance = user_hook_activation_guidance(home_directory, config);
        for (runtime, outcome) in &self.outcomes {
            if let Err(error) = outcome {
                guidance[runtime.as_str()] = json!({
                    "status": "installation_failed",
                    "error": error,
                    "steps": ["Resolve the reported installation conflict, then activate Namedrop again."],
                });
            }
        }
        guidance
    }
}

/// Refreshes both shared user-level Agent Integration packages before any
/// registration challenge is issued. A failed package does not fail the
/// lifecycle event: it blocks challenge issuance for that runtime and leaves one
/// durable Integration Registration Attention Item naming the failed target. A
/// later successful refresh resolves that item.
pub(crate) fn refresh_user_integration_packages<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    discovered: &DiscoveredWorktree,
) -> Result<IntegrationPackageInstallation, String>
where
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let (_package_guard, current) = crate::installed_runtime::package_guard()?;
    let executable = crate::collaboration_executable().map(PathBuf::from);
    let outcomes = [AgentRuntime::Codex, AgentRuntime::ClaudeCode].map(|agent_runtime| {
        let outcome = match (runtime.home_directory, &executable) {
            (Some(home_directory), _) if !current => {
                verified_package_directory(home_directory, agent_runtime)
            }
            (Some(home_directory), Ok(executable)) => install_integration_package_with_config(
                home_directory,
                agent_runtime,
                executable,
                runtime.state_directory,
                runtime.agent_config,
            ),
            (None, _) => Err(
                "no user home directory is available for the shared Agent Integration package"
                    .to_owned(),
            ),
            (_, Err(error)) => Err(error.clone()),
        };
        (agent_runtime, outcome)
    });
    let namespace = discovered.namespace(runtime.session_socket_id);
    let attribution = MutationAttribution::new(
        crate::ActorKind::Plugin,
        "namedrop-integration-install",
        runtime.clock.now_unix_ms(),
    );
    StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| {
            for (agent_runtime, outcome) in &outcomes {
                let source_id = integration_package_attention_source(*agent_runtime);
                match outcome {
                    Ok(_) => {
                        transaction.resolve_attention_source(
                            &namespace,
                            AttentionSourceKind::IntegrationRegistration,
                            &source_id,
                            &attribution,
                        )?;
                    }
                    Err(error) => {
                        transaction.open_attention_item(
                            &namespace,
                            &AttentionItemDraft {
                                id: AttentionItemId::new(format!("attention-{source_id}")),
                                source_kind: AttentionSourceKind::IntegrationRegistration,
                                source_id: source_id.clone(),
                                summary: format!(
                                    "The {} Agent Integration package could not be installed, so this runtime cannot be challenged to register and its queued work stays queued: {error}",
                                    agent_runtime.as_str()
                                ),
                            },
                            &attribution,
                        )?;
                    }
                }
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    Ok(IntegrationPackageInstallation {
        outcomes: outcomes.into_iter().collect(),
    })
}

/// The durable per-runtime readiness record a failed refresh opens and a
/// successful refresh resolves. Delivery dispatch reads the same source, so
/// package readiness gates both challenge issuance and delivery.
pub(crate) fn integration_package_attention_source(runtime: AgentRuntime) -> String {
    format!(
        "{}{}",
        crate::INTEGRATION_PACKAGE_ATTENTION_SOURCE_PREFIX,
        runtime.as_str()
    )
}

fn ownership_marker() -> String {
    format!(
        "{}\n",
        json!({
            "owner": PACKAGE_OWNER,
            "protocol_version": crate::INTEGRATION_PROTOCOL_VERSION,
            "package_version": env!("CARGO_PKG_VERSION"),
        })
    )
}

fn codex_manifest(version: &str) -> String {
    format!(
        "{}\n",
        json!({
            "name": PACKAGE_OWNER,
            "version": version,
            "description": "Route @mention prompts through Namedrop.",
            "author": {"name": "Namedrop"},
            "interface": {
                "displayName": "Namedrop",
                "shortDescription": "Route @mentions to named agents.",
                "longDescription": "Namedrop routes column-zero @mentions directly and lets agents delegate through embedded @mentions.",
                "developerName": "Namedrop",
                "category": "Productivity",
                "capabilities": ["Hooks"],
                "defaultPrompt": "Ask @reviewer to inspect this change."
            }
        })
    )
}

/// Codex snapshots installed plugins by manifest version. The generated wrapper
/// contains the absolute executable and state paths, so a stable Cargo version
/// would let Codex reuse an obsolete cached hook after Herdr refreshes it.
/// Deriving build metadata from the actual generated files makes identical
/// packages stable while any routing-relevant change gets a new cache key.
fn codex_cache_version(contents: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"namedrop-codex-plugin-v1");
    for content in contents {
        digest.update((content.len() as u64).to_le_bytes());
        digest.update(content.as_bytes());
    }
    let digest = crate::lower_hex(&digest.finalize());
    format!("{}+codex.{}", env!("CARGO_PKG_VERSION"), &digest[..16])
}

fn claude_manifest() -> String {
    format!(
        "{}\n",
        json!({
            "$schema": "https://anthropic.com/claude-code/plugin.schema.json",
            "name": PACKAGE_OWNER,
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Route @mention prompts through Namedrop.",
            "author": {"name": "Namedrop"},
            "skills": ["./"]
        })
    )
}

/// Both hosts read this file. Each entry launches the wrapper from the plugin
/// root the host exports, or from the installed path when neither is set.
/// SessionStart carries no matcher, so it runs for every source the host
/// reports: a new session, a resume, a clear, a compaction, a fork. Each of
/// those rebuilds the context the policy was in.
fn hooks_manifest(package_directory: &Path) -> Result<String, String> {
    let launch = |script: &str| -> Result<String, String> {
        let fallback = package_directory.join(script);
        let fallback = fallback.to_str().ok_or_else(|| {
            format!(
                "the Agent Integration hook path is not valid UTF-8: {}",
                fallback.display()
            )
        })?;
        Ok(format!(
            "if [ -n \"${{PLUGIN_ROOT:-}}\" ]; then \"${{PLUGIN_ROOT}}/{script}\"; elif [ -n \"${{CLAUDE_PLUGIN_ROOT:-}}\" ]; then \"${{CLAUDE_PLUGIN_ROOT}}/{script}\"; else {}; fi",
            shell_single_quote(fallback)
        ))
    };
    Ok(format!(
        "{}\n",
        json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": launch(SESSION_START_SCRIPT_FILE)?,
                        "timeout": 10
                    }]
                }],
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": launch(HOOK_SCRIPT_FILE)?,
                        "timeout": 10
                    }]
                }]
            }
        })
    ))
}

fn marketplace_manifest() -> String {
    format!(
        "{}\n",
        json!({
            "name": PACKAGE_OWNER,
            "interface": {"displayName": "Namedrop"},
            "plugins": [{
                "name": PACKAGE_OWNER,
                "source": {"source": "local", "path": "."},
                "policy": {
                    "installation": "AVAILABLE",
                    "authentication": "ON_INSTALL"
                },
                "category": "Productivity"
            }]
        })
    )
}

fn hook_script(
    runtime: AgentRuntime,
    executable: &Path,
    state_directory: &Path,
) -> Result<String, String> {
    let executable = executable.to_str().ok_or_else(|| {
        format!(
            "collaboration executable path is not valid UTF-8: {}",
            executable.display()
        )
    })?;
    let state_directory = state_directory.to_str().ok_or_else(|| {
        format!(
            "collaboration state directory path is not valid UTF-8: {}",
            state_directory.display()
        )
    })?;
    let mut safe_context = crate::hook::failed_handoff_hook_outputs()
        .iter()
        .map(|output| {
            format!(
                "if [ \"$output\" = {} ]; then\n  printf '%s' \"$output\"\nfi\n",
                shell_single_quote(&output.to_string())
            )
        })
        .collect::<String>();
    // The workspace persona roster is plugin-authored but changes every turn, so
    // it cannot be an exact literal. The wrapper recognizes it by the serialized
    // text either side of its fixed marker, which no other output carries. The
    // delivery attestation stays Claude-only under the same rule.
    let mut markers = vec![crate::persona::ROSTER_MARKER];
    if runtime == AgentRuntime::ClaudeCode {
        markers.push(crate::hook::DELIVERY_MARKER);
    }
    for marker in markers {
        safe_context.push_str(&forward_marked_context("UserPromptSubmit", marker));
    }
    Ok(format!(
        "#!/bin/sh\noutput=$({} hook user-prompt-submit --state-directory {} 2>/dev/null) || exit 0\nif [ \"$output\" = '{{\"decision\":\"block\",\"reason\":\"Collaboration message sent.\"}}' ]; then\n  {} hook mailbox-wake --state-directory {} </dev/null >/dev/null 2>&1 &\n  printf '%s' \"$output\"\n  exit 0\nfi\n{}exit 0\n",
        shell_single_quote(executable),
        shell_single_quote(state_directory),
        shell_single_quote(executable),
        shell_single_quote(state_directory),
        safe_context,
    ))
}

/// The shell that forwards one dynamic context, recognized by the serialized
/// text either side of its fixed marker, which no other output carries.
fn forward_marked_context(event: &str, marker: &str) -> String {
    let (opening, closing) = crate::hook::context_bounds(event, marker);
    format!(
        "case \"$output\" in\n  {}*{}) printf '%s' \"$output\" ;;\nesac\n",
        shell_single_quote(&opening),
        shell_single_quote(&closing),
    )
}

/// The session-start wrapper. It forwards only what the plugin produced: the
/// selection policy, and for Codex the fixed shared app-server notice, so a
/// host never receives anything else under this event from Namedrop.
fn session_start_script(
    runtime: AgentRuntime,
    executable: &Path,
    state_directory: &Path,
) -> Result<String, String> {
    let executable = executable.to_str().ok_or_else(|| {
        format!(
            "collaboration executable path is not valid UTF-8: {}",
            executable.display()
        )
    })?;
    let state_directory = state_directory.to_str().ok_or_else(|| {
        format!(
            "collaboration state directory path is not valid UTF-8: {}",
            state_directory.display()
        )
    })?;
    let app_server_notice = match runtime {
        AgentRuntime::Codex => format!(
            "if [ \"$output\" = {} ]; then\n  printf '%s' \"$output\"\n  exit 0\nfi\n",
            shell_single_quote(&crate::hook::codex_app_server_hook_output().to_string())
        ),
        AgentRuntime::ClaudeCode => String::new(),
    };
    Ok(format!(
        "#!/bin/sh\noutput=$({} hook session-start --state-directory {} 2>/dev/null) || exit 0\n{app_server_notice}{}exit 0\n",
        shell_single_quote(executable),
        shell_single_quote(state_directory),
        forward_marked_context("SessionStart", crate::persona::POLICY_MARKER),
    ))
}

pub(crate) fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

fn verify_owned_plugin_targets(
    directory: &Path,
    files: &[(&str, String, Option<u32>)],
) -> Result<(), String> {
    for relative in PACKAGE_DIRECTORIES {
        let path = directory.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(conflict(&path, "is a symbolic link"));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(conflict(&path, "is not a directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(conflict(&path, &format!("cannot be inspected: {error}"))),
        }
    }
    for (relative, _, _) in files {
        let path = directory.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(conflict(&path, "is a symbolic link"));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(conflict(&path, "is not a regular file"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(conflict(&path, &format!("cannot be inspected: {error}"))),
        }
    }
    Ok(())
}

/// Refuses every unowned target before anything is written, so a conflict never
/// follows or modifies what it found.
fn verified_package_directory(root: &Path, runtime: AgentRuntime) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err(format!(
            "the {} Agent Integration package requires an absolute installation root, not {}",
            runtime.as_str(),
            root.display()
        ));
    }
    let mut path = root.to_path_buf();
    for component in package_components(runtime) {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(conflict(&path, "is a symbolic link"));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(conflict(&path, "is not a directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => return Err(conflict(&path, &format!("cannot be inspected: {error}"))),
        }
    }
    let directory = package_directory(root, runtime);
    if fs::symlink_metadata(&directory).is_ok() {
        verify_ownership(&directory)?;
    }
    Ok(directory)
}

/// A pre-existing package directory is only refreshed when it carries this
/// plugin's ownership marker as a regular file naming this plugin as owner.
fn verify_ownership(directory: &Path) -> Result<(), String> {
    let marker = directory.join(OWNERSHIP_MARKER_FILE);
    let owned = fs::symlink_metadata(&marker).is_ok_and(|metadata| metadata.is_file())
        && fs::read_to_string(&marker)
            .ok()
            .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())
            .and_then(|marker| {
                marker
                    .get("owner")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|owner| owner == PACKAGE_OWNER);
    if !owned {
        return Err(conflict(
            directory,
            &format!("already exists without the {PACKAGE_OWNER} ownership marker"),
        ));
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_DIRECTORY_SYNC: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[derive(Debug)]
struct ReplacementError {
    message: String,
    applied: bool,
}
impl From<String> for ReplacementError {
    fn from(message: String) -> Self {
        Self {
            message,
            applied: false,
        }
    }
}
impl std::fmt::Display for ReplacementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
/// Replaces one owned file by staging it and renaming it over the target, and
/// only when its content differs, so a reader of the package never observes a
/// half-written file and repeated lifecycle events rewrite nothing.
fn replace_if_changed(target: &Path, contents: &str, mode: Option<u32>) -> Result<(), String> {
    replace_with_status(target, contents, mode).map_err(|error| error.message)
}

fn replace_with_status(
    target: &Path,
    contents: &str,
    mode: Option<u32>,
) -> Result<(), ReplacementError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_symlink() => {
            return Err(conflict(target, "is a symbolic link").into());
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(conflict(target, "is not a regular file").into());
        }
        _ => {}
    }
    if fs::read(target).is_ok_and(|installed| installed == contents.as_bytes()) {
        if let Some(mode) = mode {
            let permissions = fs::metadata(target)
                .map_err(|error| conflict(target, &format!("cannot be inspected: {error}")))?
                .permissions();
            if permissions.mode() & 0o7777 != mode {
                fs::set_permissions(target, fs::Permissions::from_mode(mode)).map_err(|error| {
                    conflict(target, &format!("permissions cannot be updated: {error}"))
                })?;
            }
        }
        return Ok(());
    }
    let staged = staging_path(target)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    if let Some(mode) = mode {
        options.mode(mode);
    }
    let staged_result = options
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            if let Some(mode) = mode {
                file.set_permissions(fs::Permissions::from_mode(mode))?;
            }
            file.sync_all()
        })
        .and_then(|()| fs::rename(&staged, target));
    if let Err(error) = staged_result {
        let _ = fs::remove_file(&staged);
        return Err(conflict(target, &format!("cannot be replaced: {error}")).into());
    }
    let sync_result = (|| {
        #[cfg(test)]
        if FAIL_DIRECTORY_SYNC.with(|path| path.borrow().as_deref() == Some(target)) {
            return Err(std::io::Error::other("injected directory sync failure"));
        }
        fs::File::open(target.parent().expect("target parent"))?.sync_all()
    })();
    sync_result.map_err(|error| ReplacementError {
        message: conflict(
            target,
            &format!("was replaced but directory sync failed: {error}"),
        ),
        applied: true,
    })
}

/// A staging name inside the package directory, so the replacement rename is
/// atomic and two concurrent installers cannot stage over each other.
fn staging_path(target: &Path) -> Result<PathBuf, String> {
    static NEXT_STAGED: AtomicU64 = AtomicU64::new(1);
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| conflict(target, "has no file name"))?;
    Ok(target.with_file_name(format!(
        ".{name}.{}-{}.staged",
        std::process::id(),
        NEXT_STAGED.fetch_add(1, Ordering::Relaxed)
    )))
}

fn conflict(path: &Path, reason: &str) -> String {
    format!("{} {reason}", path.display())
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    #[test]
    fn collaboration_allow_rules_have_the_exact_installed_prefix() {
        let actual =
            claude_allow_rules(Path::new("/opt/namedrop"), Path::new("/opt/state")).unwrap();
        let expected = [
            "Bash('/opt/namedrop' --plugin-state '/opt/state' describe *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' register *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' ack *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' send *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' reply *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' complete --no-reply *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' delivery inbox *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' snapshot publish *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' context inspect *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' context read *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' handoff status *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' handoff results *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' handoff preview *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' group list *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' handoff begin *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' handoff resolve *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' handoff adopt *)",
            "Bash('/opt/namedrop' --plugin-state '/opt/state' address list *)",
        ];
        assert_eq!(actual, expected);
        for operation in crate::operation_catalog()
            .management_operations
            .iter()
            .filter(|operation| !operation.automatic_permission)
        {
            assert!(!actual.contains(&format!(
                "Bash('/opt/namedrop' --plugin-state '/opt/state' {} *)",
                operation.argv.join(" ")
            )));
        }
    }
}
