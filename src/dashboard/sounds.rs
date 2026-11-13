//! The sound Channels plays when an agent messages the person.
//!
//! The choices live on the server so every browser the person opens Channels
//! in, a laptop and a phone over Tailscale alike, plays the same sounds.

use super::server_files::ServerFiles;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
};

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
/// overwrite each other. `was` is the value the page showed before the edit,
/// and the edit applies only while the stored value still matches it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum Change {
    Default {
        sound: Sound,
        was: Sound,
    },
    Muted {
        muted: bool,
        was: bool,
    },
    Agent {
        agent: AgentKey,
        /// Required, so only an explicit `null` returns the agent to the default.
        #[serde(deserialize_with = "Option::deserialize")]
        sound: Option<Sound>,
        /// Required; `null` means the agent had no sound of its own.
        #[serde(deserialize_with = "Option::deserialize")]
        was: Option<Sound>,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Rejected {
    TooManyAgents,
    /// The stored value is no longer the one the page showed.
    Stale,
}

impl Settings {
    /// Applies `change` and keeps every other pick. Only a new pick that
    /// would pass the cap first drops the picks of agents that are no longer
    /// known, so a full list refuses it only when every listed agent is known.
    /// The agent the change sets is kept either way.
    pub(crate) fn change(
        self,
        change: Change,
        known: Option<&HashSet<String>>,
    ) -> Result<Self, Rejected> {
        match (self.clone().apply(change.clone()), known) {
            (Err(Rejected::TooManyAgents), Some(known)) => self.prune(known).apply(change),
            (result, _) => result,
        }
    }

    fn prune(mut self, known: &HashSet<String>) -> Self {
        self.agents.retain(|agent, _| known.contains(&agent.0));
        self
    }

    pub(crate) fn apply(mut self, change: Change) -> Result<Self, Rejected> {
        let current = match &change {
            Change::Default { was, .. } => self.default == *was,
            Change::Muted { was, .. } => self.muted == *was,
            Change::Agent { agent, was, .. } => self.agents.get(agent) == was.as_ref(),
        };
        if !current {
            return Err(Rejected::Stale);
        }
        match change {
            Change::Default { sound, .. } => self.default = sound,
            Change::Muted { muted, .. } => self.muted = muted,
            Change::Agent {
                agent, sound: None, ..
            } => {
                self.agents.remove(&agent);
            }
            Change::Agent {
                agent,
                sound: Some(sound),
                ..
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
            .apply(change(json!({"kind": "default", "sound": "bell", "was": "chime"})).unwrap())
            .unwrap()
            .apply(change(json!({"kind": "muted", "muted": true, "was": false})).unwrap())
            .unwrap()
            .apply(
                change(json!({"kind": "agent", "agent": "pane-address-1-2-3",
                              "sound": "pop", "was": null}))
                .unwrap(),
            )
            .unwrap()
            .apply(
                change(json!({"kind": "agent", "agent": "pane-address-4-5-6",
                              "sound": "none", "was": null}))
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
                change(json!({"kind": "agent", "agent": "pane-address-1-2-3",
                              "sound": null, "was": "pop"}))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(&cleared).unwrap(),
            json!({"default": "bell", "muted": true, "agents": {"pane-address-4-5-6": "none"}})
        );
    }

    #[test]
    fn a_change_applies_only_over_the_value_the_page_showed() {
        let stored = Settings {
            default: Sound::Bell,
            muted: true,
            agents: BTreeMap::from([(agent(1), Sound::Pop)]),
        };
        for (body, applies) in [
            (
                json!({"kind": "default", "sound": "ping", "was": "bell"}),
                true,
            ),
            (
                json!({"kind": "default", "sound": "ping", "was": "chime"}),
                false,
            ),
            (
                json!({"kind": "default", "sound": "bell", "was": "ping"}),
                false,
            ),
            (json!({"kind": "muted", "muted": false, "was": true}), true),
            (
                json!({"kind": "muted", "muted": false, "was": false}),
                false,
            ),
            (
                json!({"kind": "agent", "agent": "pane-address-1", "sound": "soft", "was": "pop"}),
                true,
            ),
            (
                json!({"kind": "agent", "agent": "pane-address-1", "sound": null, "was": "pop"}),
                true,
            ),
            (
                json!({"kind": "agent", "agent": "pane-address-1", "sound": "soft", "was": "knock"}),
                false,
            ),
            (
                json!({"kind": "agent", "agent": "pane-address-1", "sound": "soft", "was": null}),
                false,
            ),
            (
                json!({"kind": "agent", "agent": "pane-address-2", "sound": "soft", "was": null}),
                true,
            ),
            (
                json!({"kind": "agent", "agent": "pane-address-2", "sound": "soft", "was": "pop"}),
                false,
            ),
        ] {
            let result = stored.clone().apply(change(body.clone()).unwrap());
            if applies {
                assert_ne!(result.unwrap(), stored, "{body}");
            } else {
                assert_eq!(result, Err(Rejected::Stale), "{body}");
            }
        }
    }

    #[test]
    fn a_full_list_refuses_a_new_agent_but_still_changes_or_clears_a_listed_one() {
        let mut full = Settings::default();
        for index in 0..MAX_AGENTS {
            full.agents.insert(agent(index), Sound::Ping);
        }
        let set = |from: &Settings, key: AgentKey, sound: Option<Sound>| Change::Agent {
            was: from.agents.get(&key).copied(),
            agent: key,
            sound,
        };
        assert_eq!(
            full.clone()
                .apply(set(&full, agent(MAX_AGENTS), Some(Sound::Pop))),
            Err(Rejected::TooManyAgents)
        );
        let changed = full
            .clone()
            .apply(set(&full, agent(0), Some(Sound::Pop)))
            .unwrap();
        assert_eq!(changed.agents[&agent(0)], Sound::Pop);
        let clear = set(&full, agent(0), None);
        let cleared = full.apply(clear).unwrap();
        assert_eq!(cleared.agents.len(), MAX_AGENTS - 1);
        let fill = set(&cleared, agent(MAX_AGENTS), Some(Sound::Pop));
        cleared
            .apply(fill)
            .expect("a freed place takes a new agent");
    }

    #[test]
    fn a_save_keeps_every_pick_and_drops_unknown_agents_only_to_fit_a_new_one() {
        let set = |key: AgentKey| Change::Agent {
            agent: key,
            sound: Some(Sound::Pop),
            was: None,
        };
        let mut few = Settings::default();
        few.agents.insert(agent(1), Sound::Bell);
        let known: HashSet<String> = HashSet::new();
        let kept = few.change(set(agent(2)), Some(&known)).unwrap();
        assert_eq!(
            kept.agents.keys().cloned().collect::<Vec<_>>(),
            [agent(1), agent(2)],
            "an agent that is not a direct message peer keeps its pick"
        );
        let mut full = Settings::default();
        for index in 0..MAX_AGENTS {
            full.agents.insert(agent(index), Sound::Ping);
        }
        let known: HashSet<String> = (0..MAX_AGENTS - 1)
            .map(|index| String::from(agent(index)))
            .collect();
        let fits = full
            .clone()
            .change(set(agent(MAX_AGENTS)), Some(&known))
            .unwrap();
        assert_eq!(fits.agents[&agent(MAX_AGENTS)], Sound::Pop);
        assert_eq!(fits.agents.len(), MAX_AGENTS);
        assert!(!fits.agents.contains_key(&agent(MAX_AGENTS - 1)));
        assert_eq!(
            full.clone().change(set(agent(MAX_AGENTS)), None),
            Err(Rejected::TooManyAgents),
            "with no listing nothing is dropped"
        );
        let all: HashSet<String> = (0..MAX_AGENTS).map(|index| String::from(agent(index))).collect();
        assert_eq!(
            full.change(set(agent(MAX_AGENTS)), Some(&all)),
            Err(Rejected::TooManyAgents)
        );
    }

    #[test]
    fn a_change_with_a_bad_agent_sound_or_shape_does_not_parse() {
        let long = "a".repeat(129);
        for body in [
            json!({"kind": "agent", "agent": "", "sound": "pop", "was": null}),
            json!({"kind": "agent", "agent": long, "sound": "pop", "was": null}),
            json!({"kind": "agent", "agent": "pane/address", "sound": "pop", "was": null}),
            json!({"kind": "agent", "agent": "pane address", "sound": "pop", "was": null}),
            json!({"kind": "agent", "agent": "pane-address-1", "sound": "trumpet", "was": null}),
            json!({"kind": "agent", "agent": "pane-address-1", "was": null}),
            json!({"kind": "agent", "agent": "pane-address-1", "sound": "pop"}),
            json!({"kind": "agent", "agent": "pane-address-1", "sound": "pop", "was": "trumpet"}),
            json!({"kind": "default", "sound": "Chime", "was": "bell"}),
            json!({"kind": "default", "sound": null, "was": "bell"}),
            json!({"kind": "default", "sound": "bell"}),
            json!({"kind": "default", "sound": "bell", "was": null}),
            json!({"kind": "muted", "muted": "yes", "was": false}),
            json!({"kind": "muted", "muted": true}),
            json!({"kind": "muted", "muted": true, "was": false, "sound": "pop"}),
            json!({"kind": "volume", "volume": 3}),
            json!({"sound": "pop"}),
        ] {
            assert!(change(body.clone()).is_err(), "{body}");
        }
        for key in ["a".repeat(128), "Pane_address-9".to_owned()] {
            assert!(
                change(json!({"kind": "agent", "agent": key, "sound": "soft", "was": null}))
                    .is_ok(),
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
                was: None,
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
