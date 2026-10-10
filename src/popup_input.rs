use std::{
    io::{self, IsTerminal},
    process::{Command, Stdio},
};

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
            return Err("Could not enable interactive popup input".to_owned());
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
