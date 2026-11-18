use namedrop::mux::MuxKind;
use std::{
    io::{self, IsTerminal, Read, Write},
    process::{Command, Stdio},
    sync::OnceLock,
};

/// The host this process draws its one screen for, whose words the screen
/// uses: a Herdr popup or a Rex layer. Decided once, by the first caller.
static HOST: OnceLock<MuxKind> = OnceLock::new();

pub(crate) fn host_for(
    options: &namedrop::cli::RuntimeOptions,
    inherited: &dyn namedrop::mux::home::Inherited,
) -> MuxKind {
    set_host(
        namedrop::mux::home::resolve(&options.flags(), inherited)
            .map_or(MuxKind::Herdr, |home| home.kind()),
    )
}

/// Records `kind` as the screen's host unless one was recorded already, and
/// returns the one recorded.
pub(crate) fn set_host(kind: MuxKind) -> MuxKind {
    *HOST.get_or_init(|| kind)
}

/// [`host_for`]'s answer, or Herdr before any screen asked.
pub(crate) fn host() -> MuxKind {
    HOST.get().copied().unwrap_or(MuxKind::Herdr)
}

pub(crate) fn is_rex(
    options: &namedrop::cli::RuntimeOptions,
    inherited: &dyn namedrop::mux::home::Inherited,
) -> bool {
    host_for(options, inherited) == MuxKind::Rex
}

pub(crate) fn wait_for_key() -> Result<(), String> {
    eprint!("\nPress any key to close: ");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let _mode = InputMode::enter()?;
    io::stdin().read_exact(&mut [0]).map_err(|e| e.to_string())
}

/// Restore the popup's terminal settings on every normal or error return.
pub(crate) struct InputMode(Option<String>);

impl InputMode {
    pub(crate) fn enter() -> Result<Self, String> {
        Self::configure("1", "0")
    }

    pub(crate) fn polling() -> Result<Self, String> {
        Self::configure("0", "1")
    }

    fn configure(minimum: &str, timeout: &str) -> Result<Self, String> {
        if !io::stdin().is_terminal() {
            return Ok(Self(None));
        }
        let output = Command::new("/bin/stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err("Could not read terminal input settings".to_owned());
        }
        let saved = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
        let guard = Self(Some(saved.trim().to_owned()));
        if !Command::new("/bin/stty")
            .args(["-icanon", "-echo", "min", minimum, "time", timeout])
            .status()
            .map_err(|error| error.to_string())?
            .success()
        {
            return Err(host()
                .text(
                    "Could not enable interactive popup input",
                    "Could not enable interactive screen input",
                )
                .to_owned());
        }
        Ok(guard)
    }
}

impl Drop for InputMode {
    fn drop(&mut self) {
        if let Some(saved) = &self.0 {
            let _ = Command::new("/bin/stty").arg(saved).status();
        }
    }
}
