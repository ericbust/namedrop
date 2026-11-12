//! The sound Channels plays when an agent messages the person.
//!
//! The choices live on the server so every browser the person opens Channels
//! in, a laptop and a phone over Tailscale alike, plays the same sounds.

use super::server_files::ServerFiles;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};

const FILE: &str = "sounds.json";
const MAX_AGENTS: usize = 512;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Sound {
    #[default]
    Chime,
    Ping,
    Pop,
    Knock,
    Bell,
    Blip,
    Marimba,
    Soft,
    None,
}

/// An agent's stable pane address id, such as `pane-address-1-2-3`.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct AgentKey(String);

impl TryFrom<String> for AgentKey {
    type Error = &'static str;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        if (1..=128).contains(&key.len())
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            Ok(Self(key))
        } else {
            Err("an agent key is 1 to 128 letters, digits, '_' or '-'")
        }
    }
}

impl From<AgentKey> for String {
    fn from(key: AgentKey) -> Self {
        key.0
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub default: Sound,
    pub muted: bool,
    pub agents: BTreeMap<AgentKey, Sound>,
}

/// One edit the page posts, so two browsers changing different fields never
/// overwrite each other.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum Change {
    Default {
        sound: Sound,
    },
    Muted {
        muted: bool,
    },
    Agent {
        agent: AgentKey,
        /// Required, so only an explicit `null` returns the agent to the default.
        #[serde(deserialize_with = "Option::deserialize")]
        sound: Option<Sound>,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Rejected {
    TooManyAgents,
}

impl Settings {
    pub(crate) fn apply(mut self, change: Change) -> Result<Self, Rejected> {
        match change {
            Change::Default { sound } => self.default = sound,
            Change::Muted { muted } => self.muted = muted,
            Change::Agent { agent, sound: None } => {
                self.agents.remove(&agent);
            }
            Change::Agent {
                agent,
                sound: Some(sound),
            } => {
                if self.agents.len() >= MAX_AGENTS && !self.agents.contains_key(&agent) {
                    return Err(Rejected::TooManyAgents);
                }
                self.agents.insert(agent, sound);
            }
        }
        Ok(self)
    }
}

/// A missing, unreadable or unknown file reads as the factory default, and the
/// next change replaces it.
pub(crate) fn read(files: &ServerFiles) -> Settings {
    fs::read(files.state().join("dashboard").join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|mut value| {
            let fields = value.as_object_mut()?;
            (fields.remove("version")? == json!(1)).then_some(())?;
            serde_json::from_value(value).ok()
        })
        .unwrap_or_default()
}

pub(crate) fn write(files: &ServerFiles, settings: &Settings) -> Result<(), String> {
    let mut value = serde_json::to_value(settings).map_err(|error| error.to_string())?;
    value
        .as_object_mut()
        .expect("settings serialize as an object")
        .insert("version".to_owned(), json!(1));
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    files.write_atomic(FILE, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(body: Value) -> Result<Change, serde_json::Error> {
        serde_json::from_value(body)
    }

    fn agent(index: usize) -> AgentKey {
        AgentKey::try_from(format!("pane-address-{index}")).unwrap()
    }

    #[test]
    fn the_factory_default_is_an_unmuted_chime_for_everyone() {
        assert_eq!(
            serde_json::to_value(Settings::default()).unwrap(),
            json!({"default": "chime", "muted": false, "agents": {}})
        );
    }

    #[test]
    fn each_change_sets_only_its_own_field() {
        let settings = Settings::default()
            .apply(change(json!({"kind": "default", "sound": "bell"})).unwrap())
            .unwrap()
            .apply(change(json!({"kind": "muted", "muted": true})).unwrap())
            .unwrap()
            .apply(
                change(json!({"kind": "agent", "agent": "pane-address-1-2-3", "sound": "pop"}))
                    .unwrap(),
            )
            .unwrap()
            .apply(
                change(json!({"kind": "agent", "agent": "pane-address-4-5-6", "sound": "none"}))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(&settings).unwrap(),
            json!({
                "default": "bell",
                "muted": true,
                "agents": {"pane-address-1-2-3": "pop", "pane-address-4-5-6": "none"},
            })
        );
        let cleared = settings
            .apply(
                change(json!({"kind": "agent", "agent": "pane-address-1-2-3", "sound": null}))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(&cleared).unwrap(),
            json!({"default": "bell", "muted": true, "agents": {"pane-address-4-5-6": "none"}})
        );
    }

    #[test]
    fn a_full_list_refuses_a_new_agent_but_still_changes_or_clears_a_listed_one() {
        let mut full = Settings::default();
        for index in 0..MAX_AGENTS {
            full.agents.insert(agent(index), Sound::Ping);
        }
        let set = |key: AgentKey, sound: Option<Sound>| Change::Agent { agent: key, sound };
        assert_eq!(
            full.clone().apply(set(agent(MAX_AGENTS), Some(Sound::Pop))),
            Err(Rejected::TooManyAgents)
        );
        let changed = full.clone().apply(set(agent(0), Some(Sound::Pop))).unwrap();
        assert_eq!(changed.agents[&agent(0)], Sound::Pop);
        let cleared = full.apply(set(agent(0), None)).unwrap();
        assert_eq!(cleared.agents.len(), MAX_AGENTS - 1);
        cleared
            .apply(set(agent(MAX_AGENTS), Some(Sound::Pop)))
            .expect("a freed place takes a new agent");
    }

    #[test]
    fn a_change_with_a_bad_agent_sound_or_shape_does_not_parse() {
        let long = "a".repeat(129);
        for body in [
            json!({"kind": "agent", "agent": "", "sound": "pop"}),
            json!({"kind": "agent", "agent": long, "sound": "pop"}),
            json!({"kind": "agent", "agent": "pane/address", "sound": "pop"}),
            json!({"kind": "agent", "agent": "pane address", "sound": "pop"}),
            json!({"kind": "agent", "agent": "pane-address-1", "sound": "trumpet"}),
            json!({"kind": "agent", "agent": "pane-address-1"}),
            json!({"kind": "default", "sound": "Chime"}),
            json!({"kind": "default", "sound": null}),
            json!({"kind": "muted", "muted": "yes"}),
            json!({"kind": "muted", "muted": true, "sound": "pop"}),
            json!({"kind": "volume", "volume": 3}),
            json!({"sound": "pop"}),
        ] {
            assert!(change(body.clone()).is_err(), "{body}");
        }
        for key in ["a".repeat(128), "Pane_address-9".to_owned()] {
            assert!(
                change(json!({"kind": "agent", "agent": key, "sound": "soft"})).is_ok(),
                "{key}"
            );
        }
    }

    fn files() -> (std::path::PathBuf, ServerFiles) {
        let state = std::env::temp_dir().join(format!(
            "namedrop-sounds-{}",
            super::super::server_files::random_hex(12).unwrap()
        ));
        let files = ServerFiles::at(&state).unwrap();
        (state, files)
    }

    #[test]
    fn written_settings_read_back_and_carry_a_version() {
        let (state, files) = files();
        assert_eq!(read(&files), Settings::default());
        let settings = Settings::default()
            .apply(Change::Agent {
                agent: agent(7),
                sound: Some(Sound::Marimba),
            })
            .unwrap();
        write(&files, &settings).unwrap();
        assert_eq!(read(&files), settings);
        let stored: Value =
            serde_json::from_slice(&fs::read(state.join("dashboard").join(FILE)).unwrap()).unwrap();
        assert_eq!(stored["version"], json!(1));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_corrupt_or_unknown_file_reads_as_the_factory_default() {
        let (state, files) = files();
        let path = state.join("dashboard").join(FILE);
        for bytes in [
            b"not json".as_slice(),
            b"[]",
            br#"{"default":"bell","muted":true,"agents":{}}"#,
            br#"{"version":2,"default":"bell","muted":true,"agents":{}}"#,
            br#"{"version":1,"default":"trumpet","muted":true,"agents":{}}"#,
            br#"{"version":1,"default":"bell","muted":true,"agents":{"bad key":"pop"}}"#,
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(
                read(&files),
                Settings::default(),
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
        fs::write(
            &path,
            br#"{"version":1,"default":"bell","muted":true,"agents":{}}"#,
        )
        .unwrap();
        assert_eq!(read(&files).default, Sound::Bell);
        fs::remove_dir_all(state).unwrap();
    }
}
