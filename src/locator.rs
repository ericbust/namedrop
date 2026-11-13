//! Where a Namedrop command runs: its state directory and the endpoint, space,
//! and pane of the multiplexer it serves. Each has a neutral `NAMEDROP_*` name
//! and the Herdr name accepted beside it, in the environment, on the command
//! line, and in a `collab/v1` tool environment.
//!
//! One rule holds everywhere. An explicit flag wins for its own value. The
//! endpoint, space, and pane otherwise come from one set of names, chosen once
//! by [`home`]: Herdr's under Herdr, the neutral ones elsewhere. The state
//! directory comes from `HERDR_PLUGIN_STATE_DIR` when it is set, and otherwise
//! from the same set, so under Herdr `NAMEDROP_STATE_DIR` is ignored.

use crate::mux::{Endpoint, MuxKind};
use serde_json::{Map, Value};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Locator {
    StateDir,
    Endpoint,
    Space,
    Pane,
}

impl Locator {
    pub const ALL: [Self; 4] = [Self::StateDir, Self::Endpoint, Self::Space, Self::Pane];

    pub const fn herdr_var(self) -> &'static str {
        match self {
            Self::StateDir => "HERDR_PLUGIN_STATE_DIR",
            Self::Endpoint => "HERDR_SOCKET_PATH",
            Self::Space => "HERDR_WORKSPACE_ID",
            Self::Pane => "HERDR_PANE_ID",
        }
    }

    pub const fn neutral_var(self) -> &'static str {
        match self {
            Self::StateDir => "NAMEDROP_STATE_DIR",
            Self::Endpoint => "NAMEDROP_ENDPOINT",
            Self::Space => "NAMEDROP_SPACE_ID",
            Self::Pane => "NAMEDROP_PANE_ID",
        }
    }

    /// The flag that passes a value read under `variable` on to a command.
    pub const fn flag_for(self, variable: Name) -> &'static str {
        match (self, variable) {
            (Self::StateDir, _) => "--plugin-state",
            (Self::Endpoint, Name::Herdr) => "--herdr-socket",
            (Self::Space, Name::Herdr) => "--herdr-workspace",
            (Self::Pane, Name::Herdr) => "--herdr-pane",
            (Self::Endpoint, Name::Neutral) => "--endpoint",
            (Self::Space, Name::Neutral) => "--space",
            (Self::Pane, Name::Neutral) => "--pane",
        }
    }

    pub const fn var(self, name: Name) -> &'static str {
        match name {
            Name::Herdr => self.herdr_var(),
            Name::Neutral => self.neutral_var(),
        }
    }

    /// The value `lookup` gives, and which name gave it. A Herdr value counts
    /// even when empty, as it always has; an empty neutral value counts as
    /// unset.
    pub(crate) fn resolve<V: AsRef<OsStr>>(
        self,
        lookup: impl Fn(&str) -> Option<V>,
    ) -> Option<(Name, V)> {
        let name = match self {
            Self::StateDir if lookup(self.herdr_var()).is_some() => Name::Herdr,
            Self::StateDir | Self::Endpoint | Self::Space | Self::Pane => home(&lookup),
        };
        let value = lookup(self.var(name));
        match name {
            Name::Herdr => value,
            Name::Neutral => value.filter(|value| !value.as_ref().is_empty()),
        }
        .map(|value| (name, value))
    }

    /// The value a `collab/v1` tool environment carries, and the flag that
    /// passes it on.
    pub fn in_envelope(self, environment: &Map<String, Value>) -> Option<(&'static str, &str)> {
        self.resolve(|key| environment.get(key).and_then(Value::as_str))
            .map(|(name, value)| (self.flag_for(name), value))
    }

    /// Gives a child process `value` under its name in `set` and clears the
    /// other. For an endpoint's kind, [`Name::of`] gives Herdr's name, which a
    /// Herdr connection inherits, for Herdr, and the neutral name for Rex, so
    /// a Rex endpoint never becomes `HERDR_SOCKET_PATH`.
    pub fn pass(self, command: &mut Command, set: Name, value: impl AsRef<OsStr>) {
        let (name, other) = match set {
            Name::Herdr => (Name::Herdr, Name::Neutral),
            Name::Neutral => (Name::Neutral, Name::Herdr),
        };
        command
            .env(self.var(name), value)
            .env_remove(self.var(other));
    }

    /// Clears both names from a child process, so it cannot take this value
    /// from its parent under either.
    pub fn clear(self, command: &mut Command) {
        command
            .env_remove(self.herdr_var())
            .env_remove(self.neutral_var());
    }
}

/// The state directory a worker inherited from the hook or command that
/// started it. The endpoint, space, and pane have no such reader: only
/// `mux::home` reads them, from its `Inherited`.
pub fn inherited_state_directory() -> Option<PathBuf> {
    Locator::StateDir
        .resolve(|name| env::var_os(name))
        .map(|(_, value)| PathBuf::from(value))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Name {
    Herdr,
    Neutral,
}

impl Name {
    /// The set Namedrop writes for an endpoint of `kind`, into a child's
    /// environment, an envelope, or a command line: Herdr's names for Herdr,
    /// and the neutral ones for Rex.
    pub const fn of(kind: MuxKind) -> Self {
        match kind {
            MuxKind::Herdr => Self::Herdr,
            MuxKind::Rex => Self::Neutral,
        }
    }

    /// The set for the endpoint `value` names, by its kind.
    pub fn for_endpoint(value: &str) -> Self {
        Self::of(Endpoint::parse(value).kind)
    }
}

/// The names the endpoint, space, pane, and (without `HERDR_PLUGIN_STATE_DIR`)
/// state directory come from: Herdr's when
/// `lookup` holds any of `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID`, or
/// `HERDR_PANE_ID`, even empty, and the neutral ones otherwise. Herdr names
/// every pane and plugin process afresh, so under Herdr a neutral name
/// inherited from elsewhere never fills a value Herdr left unset. Step 4
/// replaces this with the multiplexer `mux::home::resolve` chooses.
pub fn home<V>(lookup: impl Fn(&str) -> Option<V>) -> Name {
    if [Locator::Endpoint, Locator::Space, Locator::Pane]
        .iter()
        .any(|locator| lookup(locator.herdr_var()).is_some())
    {
        Name::Herdr
    } else {
        Name::Neutral
    }
}

/// The state directory for a home of `kind`, from `flag`, then what `lookup`
/// holds.
///
/// - Herdr: step 3's rule, with no default: `HERDR_PLUGIN_STATE_DIR`, else
///   the directory of the location's set of names, so a Herdr set never reads
///   `NAMEDROP_STATE_DIR`. A default could only be a second database beside
///   the plugin's.
/// - Rex: `NAMEDROP_STATE_DIR`, then the install record, then
///   `$XDG_STATE_HOME/namedrop` or `$HOME/.local/state/namedrop`. Never
///   `HERDR_PLUGIN_STATE_DIR`, which a Rex daemon may still inherit.
pub fn state_directory(
    flag: Option<PathBuf>,
    kind: MuxKind,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    flag.or_else(|| match kind {
        MuxKind::Herdr => Locator::StateDir
            .resolve(&lookup)
            .map(|(_, value)| PathBuf::from(value)),
        MuxKind::Rex => {
            let set = |name: &str| lookup(name).filter(|value| !value.is_empty());
            let absolute = |name: &str| {
                set(name)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
            };
            set(Locator::StateDir.neutral_var())
                .map(PathBuf::from)
                .or_else(|| {
                    let config = absolute("XDG_CONFIG_HOME")
                        .or_else(|| absolute("HOME").map(|home| home.join(".config")))?;
                    read_install_record(&install_record(&config))
                })
                .or_else(|| {
                    absolute("XDG_STATE_HOME")
                        .or_else(|| absolute("HOME").map(|home| home.join(".local/state")))
                        .map(|state| state.join("namedrop"))
                })
        }
    })
}

/// The install record under a config directory, `$XDG_CONFIG_HOME` or
/// `$HOME/.config`: one line holding the state directory the installer bakes
/// into the hook wrappers, so a Rex block's CLI opens the directory its hooks
/// write.
pub fn install_record(config: &Path) -> PathBuf {
    config.join("namedrop/state-directory")
}

/// The record's absolute directory. A missing, unreadable, or relative
/// record is none.
pub fn read_install_record(record: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(record).ok()?;
    let directory = PathBuf::from(text.strip_suffix('\n').unwrap_or(&text));
    directory.is_absolute().then_some(directory)
}

/// Writes the record through a temporary file and a rename, so a reader sees
/// the old line or the new one. An unchanged record is not written.
pub fn write_install_record(record: &Path, state: &Path) -> io::Result<()> {
    if read_install_record(record).as_deref() == Some(state) {
        return Ok(());
    }
    let parent = record
        .parent()
        .ok_or_else(|| io::Error::other("the install record has no directory"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".state-directory.{}.tmp", std::process::id()));
    let mut line = state.as_os_str().to_owned();
    line.push("\n");
    fs::write(&temporary, line.as_encoded_bytes())?;
    fs::rename(&temporary, record).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: BTreeMap<String, OsString> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn resolved(locator: Locator, pairs: &[(&str, &str)]) -> Option<(Name, String)> {
        locator
            .resolve(env(pairs))
            .map(|(name, value)| (name, value.to_string_lossy().into_owned()))
    }

    #[test]
    fn the_herdr_name_outranks_the_neutral_one_for_every_locator() {
        for locator in Locator::ALL {
            let both = [
                (locator.herdr_var(), "herdr"),
                (locator.neutral_var(), "neutral"),
            ];
            assert_eq!(
                resolved(locator, &both),
                Some((Name::Herdr, "herdr".to_owned())),
                "{locator:?}"
            );
            assert_eq!(
                resolved(locator, &[(locator.neutral_var(), "neutral")]),
                Some((Name::Neutral, "neutral".to_owned())),
                "{locator:?}"
            );
            assert_eq!(resolved(locator, &[]), None, "{locator:?}");
        }
    }

    #[test]
    fn an_empty_herdr_value_still_counts_and_an_empty_neutral_one_does_not() {
        let pane = Locator::Pane;
        assert_eq!(
            resolved(
                pane,
                &[("HERDR_PANE_ID", ""), ("NAMEDROP_PANE_ID", "w1:p2")]
            ),
            Some((Name::Herdr, String::new()))
        );
        assert_eq!(resolved(pane, &[("NAMEDROP_PANE_ID", "")]), None);
    }

    #[test]
    fn any_herdr_location_name_hides_every_neutral_location_name() {
        let neutral = [
            ("NAMEDROP_STATE_DIR", "/n"),
            ("NAMEDROP_ENDPOINT", "/n.sock"),
            ("NAMEDROP_SPACE_ID", "n1"),
            ("NAMEDROP_PANE_ID", "n1:p1"),
        ];
        for herdr in [Locator::Endpoint, Locator::Space, Locator::Pane] {
            for value in ["h", ""] {
                let mut pairs = neutral.to_vec();
                pairs.push((herdr.herdr_var(), value));
                for locator in [Locator::Endpoint, Locator::Space, Locator::Pane] {
                    let expected = (locator == herdr).then(|| (Name::Herdr, value.to_owned()));
                    assert_eq!(resolved(locator, &pairs), expected, "{herdr:?} {locator:?}");
                }
                assert_eq!(resolved(Locator::StateDir, &pairs), None, "{herdr:?}");
            }
        }
        let state = [("HERDR_PLUGIN_STATE_DIR", "/h")];
        assert_eq!(
            [Locator::Endpoint, Locator::Space, Locator::Pane]
                .map(|locator| resolved(locator, &[state[0], neutral[1], neutral[2], neutral[3]])),
            [
                Some((Name::Neutral, "/n.sock".to_owned())),
                Some((Name::Neutral, "n1".to_owned())),
                Some((Name::Neutral, "n1:p1".to_owned())),
            ]
        );
    }

    #[test]
    fn an_envelope_gives_each_value_with_the_flag_of_the_key_that_held_it() {
        let herdr = json!({
            "HERDR_PLUGIN_STATE_DIR": "/s",
            "HERDR_SOCKET_PATH": "/h.sock",
            "HERDR_WORKSPACE_ID": "w1",
            "HERDR_PANE_ID": "w1:p1",
        });
        let neutral = json!({
            "NAMEDROP_STATE_DIR": "/s",
            "NAMEDROP_ENDPOINT": "/h.sock",
            "NAMEDROP_SPACE_ID": "w1",
            "NAMEDROP_PANE_ID": "w1:p1",
        });
        fn flags(environment: &Value) -> [Option<(&'static str, &str)>; 4] {
            Locator::ALL.map(|locator| locator.in_envelope(environment.as_object().unwrap()))
        }
        assert_eq!(
            flags(&herdr),
            [
                Some(("--plugin-state", "/s")),
                Some(("--herdr-socket", "/h.sock")),
                Some(("--herdr-workspace", "w1")),
                Some(("--herdr-pane", "w1:p1")),
            ]
        );
        assert_eq!(
            flags(&neutral),
            [
                Some(("--plugin-state", "/s")),
                Some(("--endpoint", "/h.sock")),
                Some(("--space", "w1")),
                Some(("--pane", "w1:p1")),
            ]
        );
        let both = json!({"HERDR_PANE_ID": "herdr", "NAMEDROP_PANE_ID": "neutral"});
        assert_eq!(
            Locator::Pane.in_envelope(both.as_object().unwrap()),
            Some(("--herdr-pane", "herdr"))
        );
        let mixed = json!({"HERDR_SOCKET_PATH": "/h.sock", "NAMEDROP_PANE_ID": "neutral"});
        assert_eq!(Locator::Pane.in_envelope(mixed.as_object().unwrap()), None);
    }

    fn child_environment(command: &Command) -> BTreeMap<OsString, Option<OsString>> {
        command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect()
    }

    #[test]
    fn a_herdr_child_gets_the_herdr_name_and_loses_the_neutral_one() {
        let mut command = Command::new("true");
        Locator::Space.pass(&mut command, Name::of(MuxKind::Herdr), "w2");
        Locator::Pane.clear(&mut command);
        assert_eq!(
            child_environment(&command),
            BTreeMap::from([
                ("HERDR_PANE_ID".into(), None),
                ("HERDR_WORKSPACE_ID".into(), Some("w2".into())),
                ("NAMEDROP_PANE_ID".into(), None),
                ("NAMEDROP_SPACE_ID".into(), None),
            ])
        );
    }

    #[test]
    fn a_rex_child_gets_every_neutral_name_and_loses_every_herdr_one() {
        let mut command = Command::new("true");
        for (locator, value) in [
            (Locator::StateDir, "/s"),
            (Locator::Endpoint, "rex:/tmp/r.sock"),
            (Locator::Space, "s1"),
            (Locator::Pane, "b1"),
        ] {
            locator.pass(&mut command, Name::of(MuxKind::Rex), value);
        }
        assert_eq!(
            child_environment(&command),
            BTreeMap::from([
                ("HERDR_PANE_ID".into(), None),
                ("HERDR_PLUGIN_STATE_DIR".into(), None),
                ("HERDR_SOCKET_PATH".into(), None),
                ("HERDR_WORKSPACE_ID".into(), None),
                ("NAMEDROP_ENDPOINT".into(), Some("rex:/tmp/r.sock".into())),
                ("NAMEDROP_PANE_ID".into(), Some("b1".into())),
                ("NAMEDROP_SPACE_ID".into(), Some("s1".into())),
                ("NAMEDROP_STATE_DIR".into(), Some("/s".into())),
            ])
        );
    }

    #[test]
    fn an_endpoint_s_kind_picks_the_set_namedrop_writes() {
        for (value, name) in [
            ("", Name::Herdr),
            ("default", Name::Herdr),
            ("/tmp/h.sock", Name::Herdr),
            ("/tmp/rex:h.sock", Name::Herdr),
            ("rex:/tmp/r.sock", Name::Neutral),
        ] {
            assert_eq!(Name::for_endpoint(value), name, "{value:?}");
        }
    }

    fn state(kind: MuxKind, pairs: &[(&str, &str)]) -> Option<PathBuf> {
        state_directory(None, kind, env(pairs))
    }

    #[test]
    fn a_herdr_home_reads_step_3_s_state_directory_with_no_default() {
        let home = [("HOME", "/home/u"), ("XDG_STATE_HOME", "/home/u/state")];
        assert_eq!(state(MuxKind::Herdr, &home), None);
        assert_eq!(
            state_directory(Some("/flag".into()), MuxKind::Herdr, env(&home)),
            Some("/flag".into())
        );
        let herdr = [
            ("HERDR_SOCKET_PATH", "/h.sock"),
            ("NAMEDROP_STATE_DIR", "/n"),
            ("HOME", "/home/u"),
        ];
        assert_eq!(
            state(MuxKind::Herdr, &herdr),
            None,
            "a Herdr set never reads NAMEDROP_STATE_DIR"
        );
        let mut plugin = herdr.to_vec();
        plugin.push(("HERDR_PLUGIN_STATE_DIR", "/p"));
        assert_eq!(state(MuxKind::Herdr, &plugin), Some("/p".into()));
        assert_eq!(
            state(MuxKind::Herdr, &[("NAMEDROP_STATE_DIR", "/n")]),
            Some("/n".into()),
            "the neutral set reads its own name, as on 17c580d"
        );
    }

    #[test]
    fn a_rex_home_reads_its_variable_then_the_install_record_then_a_default() {
        let root = std::env::temp_dir().join(format!("nd-locator-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let home = home.to_str().unwrap();
        let default = [("HOME", home), ("HERDR_PLUGIN_STATE_DIR", "/herdr")];
        assert_eq!(
            state(MuxKind::Rex, &default),
            Some(Path::new(home).join(".local/state/namedrop")),
            "never HERDR_PLUGIN_STATE_DIR"
        );
        let xdg = [("HOME", home), ("XDG_STATE_HOME", "/xdg/state")];
        assert_eq!(
            state(MuxKind::Rex, &xdg),
            Some("/xdg/state/namedrop".into())
        );

        let record = install_record(&Path::new(home).join(".config"));
        write_install_record(&record, Path::new("/recorded")).unwrap();
        assert_eq!(state(MuxKind::Rex, &xdg), Some("/recorded".into()));
        let config = root.join("config");
        let elsewhere = [
            ("HOME", home),
            ("XDG_CONFIG_HOME", config.to_str().unwrap()),
        ];
        assert_eq!(
            state(MuxKind::Rex, &elsewhere),
            Some(Path::new(home).join(".local/state/namedrop")),
            "the record is read from XDG_CONFIG_HOME when it is set"
        );
        write_install_record(&install_record(&config), Path::new("/configured")).unwrap();
        assert_eq!(state(MuxKind::Rex, &elsewhere), Some("/configured".into()));

        let mut named = xdg.to_vec();
        named.push(("NAMEDROP_STATE_DIR", "/n"));
        assert_eq!(state(MuxKind::Rex, &named), Some("/n".into()));
        assert_eq!(
            state_directory(Some("/flag".into()), MuxKind::Rex, env(&named)),
            Some("/flag".into())
        );
        fs::write(&record, "relative/state\n").unwrap();
        assert_eq!(
            state(MuxKind::Rex, &xdg),
            Some("/xdg/state/namedrop".into()),
            "a relative record is no record"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_install_record_round_trips_and_is_not_rewritten_unchanged() {
        use std::os::unix::fs::MetadataExt;
        let root = std::env::temp_dir().join(format!("nd-record-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let record = install_record(&root);
        assert_eq!(read_install_record(&record), None);
        write_install_record(&record, Path::new("/state/a b")).unwrap();
        assert_eq!(fs::read_to_string(&record).unwrap(), "/state/a b\n");
        assert_eq!(read_install_record(&record), Some("/state/a b".into()));
        let inode = fs::metadata(&record).unwrap().ino();
        write_install_record(&record, Path::new("/state/a b")).unwrap();
        assert_eq!(fs::metadata(&record).unwrap().ino(), inode);
        write_install_record(&record, Path::new("/state/c")).unwrap();
        assert_eq!(read_install_record(&record), Some("/state/c".into()));
        assert_eq!(
            fs::read_dir(record.parent().unwrap()).unwrap().count(),
            1,
            "no temporary file is left"
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
