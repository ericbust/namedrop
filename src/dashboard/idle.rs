//! When the Channels server stops because no browser is connected.

use super::server_files::ServerFiles;
use serde_json::{Value, json};
use std::{fs, num::NonZeroU32, path::Path, str::FromStr, time::Duration};

const DEFAULT: Duration = Duration::from_secs(10 * 60);
const FILE: &str = "idle";

/// The person's choice, as `namedrop channels --idle` takes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    Never,
    AfterMinutes(NonZeroU32),
}

impl FromStr for Stop {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "never" {
            return Ok(Self::Never);
        }
        text.parse()
            .map(Self::AfterMinutes)
            .map_err(|_| "expected `never` or a whole number of minutes, 1 or more".to_owned())
    }
}

impl Stop {
    fn stored(self) -> String {
        match self {
            Self::Never => "never".to_owned(),
            Self::AfterMinutes(minutes) => minutes.to_string(),
        }
    }

    fn limit(self) -> Option<Duration> {
        match self {
            Self::Never => None,
            Self::AfterMinutes(minutes) => Some(Duration::from_secs(u64::from(minutes.get()) * 60)),
        }
    }
}

pub(super) fn save(files: &ServerFiles, stop: Stop) -> Result<(), String> {
    files
        .write_atomic(FILE, stop.stored().as_bytes())
        .map_err(|error| format!("could not save the Channels idle setting: {error}"))
}

/// A missing or unreadable setting is no setting, so the default applies.
fn saved(state: &Path) -> Option<Stop> {
    fs::read_to_string(state.join("dashboard").join(FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The test override. It is read once by the server that it applies to.
pub(super) fn environment() -> Option<Duration> {
    std::env::var("NAMEDROP_DASHBOARD_IDLE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_millis)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Source {
    Tailscale,
    Environment,
    Setting,
    Default,
}

impl Source {
    const ALL: [Self; 4] = [
        Self::Tailscale,
        Self::Environment,
        Self::Setting,
        Self::Default,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Tailscale => "tailscale",
            Self::Environment => "environment",
            Self::Setting => "setting",
            Self::Default => "default",
        }
    }
}

/// What the server does now, and which rule decided it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Policy {
    limit: Option<Duration>,
    pub source: Source,
}

/// A Tailscale handler forwards to this server's port, so a server that owns
/// one never stops for idleness, whatever the setting says.
pub(crate) fn policy(state: &Path, tailnet_owned: bool, environment: Option<Duration>) -> Policy {
    if tailnet_owned {
        return Policy {
            limit: None,
            source: Source::Tailscale,
        };
    }
    if let Some(limit) = environment {
        return Policy {
            limit: Some(limit),
            source: Source::Environment,
        };
    }
    match saved(state) {
        Some(stop) => Policy {
            limit: stop.limit(),
            source: Source::Setting,
        },
        None => Policy {
            limit: Some(DEFAULT),
            source: Source::Default,
        },
    }
}

impl Policy {
    pub fn reached(&self, idle_for: Duration) -> bool {
        self.limit.is_some_and(|limit| idle_for >= limit)
    }

    pub fn json(&self) -> Value {
        json!({
            "stop_after_ms": self.limit.map(|limit| limit.as_millis() as u64),
            "source": self.source.name(),
        })
    }

    pub fn sentence(&self) -> String {
        let after = self.limit.map(|limit| match limit.as_millis() {
            60_000 => "1 minute".to_owned(),
            ms if ms % 60_000 == 0 => format!("{} minutes", ms / 60_000),
            ms => format!("{ms} ms"),
        });
        match (self.source, after) {
            (Source::Tailscale, _) => {
                "The server keeps running while Tailscale access is on.".to_owned()
            }
            (Source::Setting, None) => {
                "The server keeps running with no tab open, as set with `--idle never`.".to_owned()
            }
            (Source::Setting, Some(after)) => {
                format!("The server stops {after} after the last tab closes, as set with `--idle`.")
            }
            (_, None) => "The server keeps running with no tab open.".to_owned(),
            (_, Some(after)) => format!("The server stops {after} after the last tab closes."),
        }
    }
}

/// The sentence for the `idle` object of a command result, if it is one.
pub(crate) fn sentence(idle: &Value) -> Option<String> {
    let name = idle["source"].as_str()?;
    let source = Source::ALL
        .into_iter()
        .find(|source| source.name() == name)?;
    let limit = match &idle["stop_after_ms"] {
        Value::Null => None,
        value => Some(Duration::from_millis(value.as_u64()?)),
    };
    Some(Policy { limit, source }.sentence())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(name: &str) -> ServerFiles {
        let root = std::env::temp_dir().join(format!(
            "namedrop-idle-{name}-{}",
            super::super::server_files::random_hex(8).unwrap()
        ));
        ServerFiles::at(&root).unwrap()
    }

    #[test]
    fn the_setting_takes_never_or_whole_minutes_from_one() {
        assert_eq!("never".parse(), Ok(Stop::Never));
        assert_eq!(
            "45".parse(),
            Ok(Stop::AfterMinutes(NonZeroU32::new(45).unwrap()))
        );
        for refused in ["0", "-5", "1.5", "", "Never", "10m", "ten"] {
            assert!(refused.parse::<Stop>().is_err(), "{refused:?}");
        }
    }

    #[test]
    fn tailscale_wins_then_the_test_override_then_the_setting_then_ten_minutes() {
        let files = files("order");
        let state = files.state();
        let minutes = |n: u64| Some(Duration::from_secs(n * 60));

        let default = policy(state, false, None);
        assert_eq!(
            (default.limit, default.source),
            (minutes(10), Source::Default)
        );

        save(&files, Stop::AfterMinutes(NonZeroU32::new(45).unwrap())).unwrap();
        let set = policy(state, false, None);
        assert_eq!((set.limit, set.source), (minutes(45), Source::Setting));
        assert!(!set.reached(Duration::from_secs(45 * 60 - 1)));
        assert!(set.reached(Duration::from_secs(45 * 60)));

        let overridden = policy(state, false, Some(Duration::from_millis(300)));
        assert_eq!(
            (overridden.limit, overridden.source),
            (Some(Duration::from_millis(300)), Source::Environment)
        );

        let tailnet = policy(state, true, Some(Duration::from_millis(300)));
        assert_eq!((tailnet.limit, tailnet.source), (None, Source::Tailscale));
        assert!(!tailnet.reached(Duration::from_secs(365 * 24 * 60 * 60)));

        save(&files, Stop::Never).unwrap();
        let never = policy(state, false, None);
        assert_eq!((never.limit, never.source), (None, Source::Setting));

        fs::write(state.join("dashboard/idle"), "soon").unwrap();
        assert_eq!(policy(state, false, None), default);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn each_rule_reads_as_one_sentence() {
        let files = files("sentence");
        let state = files.state();
        assert_eq!(
            policy(state, false, None).sentence(),
            "The server stops 10 minutes after the last tab closes."
        );
        assert_eq!(
            policy(state, true, None).sentence(),
            "The server keeps running while Tailscale access is on."
        );
        save(&files, Stop::AfterMinutes(NonZeroU32::new(1).unwrap())).unwrap();
        assert_eq!(
            policy(state, false, None).sentence(),
            "The server stops 1 minute after the last tab closes, as set with `--idle`."
        );
        save(&files, Stop::Never).unwrap();
        assert_eq!(
            policy(state, false, None).sentence(),
            "The server keeps running with no tab open, as set with `--idle never`."
        );
        assert_eq!(
            policy(state, false, Some(Duration::from_millis(300))).sentence(),
            "The server stops 300 ms after the last tab closes."
        );
        for rule in [
            policy(state, false, None),
            policy(state, true, None),
            policy(state, false, Some(Duration::from_millis(300))),
        ] {
            assert_eq!(sentence(&rule.json()), Some(rule.sentence()));
        }
        assert_eq!(sentence(&json!(null)), None);
        assert_eq!(
            sentence(&json!({"source": "later", "stop_after_ms": 5})),
            None
        );
        let _ = fs::remove_dir_all(state);
    }
}
