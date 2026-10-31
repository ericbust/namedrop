use std::{path::Path, process::Command};

pub trait CommandHome {
    fn isolated_home(&mut self, home: impl AsRef<Path>) -> &mut Self;
}

impl CommandHome for Command {
    fn isolated_home(&mut self, home: impl AsRef<Path>) -> &mut Self {
        let home = home.as_ref();
        assert!(home.is_absolute(), "fixture home must be absolute");
        self.env("HOME", home)
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_RUNTIME_DIR", home.join(".runtime"))
    }
}
