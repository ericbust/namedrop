use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
};

/// One process-boundary capture shared by host selection, paths, and child commands.
#[derive(Clone)]
pub struct EnvironmentSnapshot(Arc<Snapshot>);

struct Snapshot {
    variables: BTreeMap<OsString, OsString>,
    home: Option<PathBuf>,
    state: Option<PathBuf>,
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    runtime: Option<PathBuf>,
    claude: Option<PathBuf>,
    codex: Option<PathBuf>,
    herdr_config: Option<PathBuf>,
    hc_endpoint: OnceLock<Result<PathBuf, String>>,
}

impl std::fmt::Debug for EnvironmentSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EnvironmentSnapshot { .. }")
    }
}

impl Default for EnvironmentSnapshot {
    fn default() -> Self {
        Self::capture()
    }
}

impl EnvironmentSnapshot {
    pub fn capture() -> Self {
        Self::from_variables(std::env::vars_os())
    }

    pub fn from_variables(variables: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        let variables: BTreeMap<_, _> = variables.into_iter().collect();
        let value = |key: &str| variables.get(OsStr::new(key)).cloned();
        let home = value("HOME").map(PathBuf::from);
        let root = |key: &str, suffix: &str| {
            value(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(suffix)))
        };
        let config = value("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home.as_ref().map(|home| home.join(".config")));
        Self(Arc::new(Snapshot {
            state: root("XDG_STATE_HOME", ".local/state"),
            data: root("XDG_DATA_HOME", ".local/share"),
            runtime: value("XDG_RUNTIME_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            claude: value("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            codex: value("CODEX_HOME").map(PathBuf::from),
            herdr_config: value("HERDR_CONFIG_PATH")
                .map(PathBuf::from)
                .or_else(|| config.as_ref().map(|root| root.join("herdr/config.toml"))),
            hc_endpoint: OnceLock::new(),
            home,
            config,
            variables,
        }))
    }

    pub fn var_os(&self, key: &str) -> Option<OsString> {
        self.0.variables.get(OsStr::new(key)).cloned()
    }

    pub fn var(&self, key: &str) -> Option<String> {
        self.var_os(key)?.into_string().ok()
    }

    pub fn home(&self) -> Option<&Path> {
        self.0.home.as_deref()
    }
    pub fn hc_endpoint(&self) -> Result<&Path, String> {
        self.0
            .hc_endpoint
            .get_or_init(|| {
                crate::runtime::hc_socket_path_os(|key| {
                    let root = match key {
                        "HOME" => &self.0.home,
                        "XDG_STATE_HOME" => &self.0.state,
                        "XDG_CONFIG_HOME" => &self.0.config,
                        "XDG_DATA_HOME" => &self.0.data,
                        "XDG_RUNTIME_DIR" => &self.0.runtime,
                        _ => return self.var_os(key),
                    };
                    root.as_ref().map(|path| path.as_os_str().to_owned())
                })
            })
            .as_deref()
            .map_err(Clone::clone)
    }
    pub fn hc_state_directory(&self) -> Result<PathBuf, String> {
        Ok(self
            .0
            .state
            .as_ref()
            .ok_or("Set XDG_STATE_HOME, HOME, or --plugin-state for hc.")?
            .join("namedrop/hc")
            .join(crate::runtime::hc_session(|key| self.var(key))?))
    }
    pub fn agent_config(&self) -> crate::AgentConfigDirectories<'_> {
        crate::AgentConfigDirectories {
            claude: self.0.claude.as_deref(),
            codex: self.0.codex.as_deref(),
            state_root: self.0.state.as_deref(),
            config_root: self.0.config.as_deref(),
            data_root: self.0.data.as_deref(),
            runtime_root: self.0.runtime.as_deref(),
            herdr_config: self.0.herdr_config.as_deref(),
        }
    }
    pub(crate) fn apply_roots(&self, command: &mut Command) {
        for key in [
            "HOME",
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
        ] {
            match self.var_os(key) {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
    }
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        self.apply_roots(&mut command);
        // Only routing and root values are materialized: Command's Debug output
        // must never contain unrelated credentials from the captured environment.
        for key in [
            "PATH",
            "HC_BIN_PATH",
            "HERDR_BIN_PATH",
            "NAMEDROP_HERDR_TIMEOUT_MS",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "HC_SOCKET",
            "HC_TERMINAL_SOCKET",
            "HC_SESSION",
            "HC_TERMINAL_ID",
            "HERDR_SOCKET_PATH",
            "HERDR_PLUGIN_STATE_DIR",
            "HERDR_PANE_ID",
            "HERDR_WORKSPACE_ID",
            "HERDR_CONFIG_PATH",
            "NAMEDROP_STATE_DIR",
            "NAMEDROP_HC_SPACE",
        ] {
            match self.var_os(key) {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        if let Some(Ok(endpoint)) = self.0.hc_endpoint.get() {
            command.env("HC_SOCKET", endpoint);
        }
        command
    }
    pub(crate) fn hc_command(&self, program: impl AsRef<OsStr>) -> Command {
        use std::os::unix::ffi::OsStrExt;
        let mut command = self.command(program);
        for key in self
            .0
            .variables
            .keys()
            .cloned()
            .chain(std::env::vars_os().map(|(key, _)| key))
        {
            if key.as_bytes().starts_with(b"HERDR_")
                || key.as_bytes().starts_with(b"HC_TERMINAL_")
                || key == "HC_SCOPE"
                || key == "HC_RUNTIME"
            {
                command.env_remove(key);
            }
        }
        if let Ok(endpoint) = self.hc_endpoint() {
            command.env("HC_SOCKET", endpoint);
        }
        command
    }
}
