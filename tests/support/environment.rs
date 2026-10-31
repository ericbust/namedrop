use std::{path::Path, process::Command};

pub trait CommandHome {
    fn isolated_home(&mut self, home: impl AsRef<Path>) -> &mut Self;
}

impl CommandHome for Command {
    fn isolated_home(&mut self, home: impl AsRef<Path>) -> &mut Self {
        self.env("HOME", home.as_ref())
    }
}
