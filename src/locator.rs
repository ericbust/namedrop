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

use serde_json::{Map, Value};
use std::{
    env,
    ffi::{OsStr, OsString},
    path::PathBuf,
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
    pub fn resolve<V: AsRef<OsStr>>(self, lookup: impl Fn(&str) -> Option<V>) -> Option<(Name, V)> {
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

    /// The value this process inherited, and which name held it. A value that
    /// is not Unicode counts as unset.
    pub fn inherited(self) -> Option<(Name, String)> {
        let (name, value) = self.resolve(|name| env::var_os(name))?;
        Some((name, value.into_string().ok()?))
    }

    pub fn env(self) -> Option<String> {
        self.inherited().map(|(_, value)| value)
    }

    pub fn env_os(self) -> Option<OsString> {
        self.resolve(|name| env::var_os(name))
            .map(|(_, value)| value)
    }

    /// The value a `collab/v1` tool environment carries, and the flag that
    /// passes it on.
    pub fn in_envelope(self, environment: &Map<String, Value>) -> Option<(&'static str, &str)> {
        self.resolve(|key| environment.get(key).and_then(Value::as_str))
            .map(|(name, value)| (self.flag_for(name), value))
    }

    /// Gives a child process `value` under the Herdr name, which a Herdr
    /// connection inherits, and clears the neutral name.
    pub fn pass(self, command: &mut Command, value: impl AsRef<OsStr>) {
        command
            .env(self.herdr_var(), value)
            .env_remove(self.neutral_var());
    }

    /// Clears both names from a child process, so it cannot take this value
    /// from its parent under either.
    pub fn clear(self, command: &mut Command) {
        command
            .env_remove(self.herdr_var())
            .env_remove(self.neutral_var());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Name {
    Herdr,
    Neutral,
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

/// The state directory: `flag`, then the inherited one. With neither, there
/// is no default: under Herdr a default could only be a second database
/// beside the plugin's, so step 4 adds one once the home is known to be Rex.
pub fn state_directory(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| Locator::StateDir.env_os().map(PathBuf::from))
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

    #[test]
    fn a_child_gets_the_herdr_name_and_loses_the_neutral_one() {
        let mut command = Command::new("true");
        Locator::Space.pass(&mut command, "w2");
        Locator::Pane.clear(&mut command);
        let envs: BTreeMap<_, _> = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect();
        assert_eq!(
            envs,
            BTreeMap::from([
                ("HERDR_PANE_ID".into(), None),
                ("HERDR_WORKSPACE_ID".into(), Some("w2".into())),
                ("NAMEDROP_PANE_ID".into(), None),
                ("NAMEDROP_SPACE_ID".into(), None),
            ])
        );
    }
}
