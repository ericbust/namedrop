use std::{path::Path, process::Command};

pub trait CommandHome {
    fn isolated_home(&mut self, home: impl AsRef<Path>) -> &mut Self;
    #[allow(dead_code)]
    fn isolated_env(&mut self, home: impl AsRef<Path>) -> &mut Self;
}

impl CommandHome for Command {
    fn isolated_env(&mut self, home: impl AsRef<Path>) -> &mut Self {
        self.env_clear().isolated_home(home)
    }

    fn isolated_home(&mut self, home: impl AsRef<Path>) -> &mut Self {
        let home = home.as_ref();
        assert!(home.is_absolute(), "fixture home must be absolute");
        self.env("HC_SOCKET", "")
            .env("HOME", home)
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_RUNTIME_DIR", home.join(".runtime"));
        assert_private_hc_routing(self, home);
        self
    }
}

pub fn assert_private_hc_routing(command: &Command, home: &Path) {
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == "HC_SOCKET" && value == Some(std::ffi::OsStr::new(""))),
        "fixture command must explicitly clear HC_SOCKET before its endpoint override"
    );
    for (key, expected) in [
        ("HOME", home.to_path_buf()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_RUNTIME_DIR", home.join(".runtime")),
    ] {
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == key && value == Some(expected.as_os_str())),
            "fixture command must assign private {key} before launch"
        );
    }
}
