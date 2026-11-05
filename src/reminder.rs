use crate::{AgentSessionId, DeliveryId, MessageId, PaneAddressId, PaneBindingId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ReminderOwner {
    pub address: PaneAddressId,
    pub session: AgentSessionId,
    pub binding: PaneBindingId,
    pub pane: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ReminderState {
    Scheduled {
        due_at_ms: u64,
        ordinal: u32,
    },
    AwaitingDelivery {
        delivery_id: DeliveryId,
        ordinal: u32,
    },
    Completed,
    Cancelled,
    Failed {
        reason: String,
    },
}

impl ReminderState {
    pub(crate) fn tag(&self) -> &'static str {
        match self {
            Self::Scheduled { .. } => "scheduled",
            Self::AwaitingDelivery { .. } => "awaiting_delivery",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ReminderSpec {
    pub body: String,
    pub first_due_at_ms: u64,
    pub in_reply_to: Option<MessageId>,
    pub repeat: Repeat,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Repeat {
    Once,
    Count { interval_ms: u64, total: u32 },
    Until { interval_ms: u64, until_ms: u64 },
}

impl Repeat {
    pub(crate) fn next(&self, ordinal: u32, delivered_at_ms: u64) -> Option<u64> {
        match *self {
            Self::Once => None,
            Self::Count { interval_ms, total } if ordinal < total => {
                delivered_at_ms.checked_add(interval_ms)
            }
            Self::Until {
                interval_ms,
                until_ms,
            } if ordinal < 100 => delivered_at_ms
                .checked_add(interval_ms)
                .filter(|due| *due <= until_ms),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Reminder {
    pub reminder_id: String,
    pub owner: ReminderOwner,
    pub spec: ReminderSpec,
    pub state: ReminderState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetInput {
    #[serde(rename = "in")]
    delay: Option<String>,
    at: Option<String>,
    body: String,
    every: Option<String>,
    count: Option<u32>,
    until: Option<String>,
    in_reply_to: Option<MessageId>,
    pub request_id: Option<String>,
}

pub(crate) fn validate_input(input: Value) -> Result<SetInput, String> {
    let input: SetInput = serde_json::from_value(input).map_err(crate::input_schema_error)?;
    if input.body.trim().is_empty() {
        return Err("body must not be empty".into());
    }
    if input.delay.is_some() == input.at.is_some() {
        return Err("provide exactly one of in or at".into());
    }
    if input.request_id.as_ref().is_some_and(|s| s.is_empty()) {
        return Err("request_id must not be empty".into());
    }
    Ok(input)
}

impl SetInput {
    pub(crate) fn resolve(self, now: u64) -> Result<ReminderSpec, String> {
        let due = match (self.delay, self.at) {
            (Some(delay), None) => now
                .checked_add(duration_ms(&delay)?)
                .ok_or("deadline overflow")?,
            (None, Some(at)) => timestamp_ms(&at)?,
            _ => return Err("provide exactly one of in or at".into()),
        };
        if due < now.saturating_add(60_000) {
            return Err("minimum reminder delay is 60 seconds".into());
        }
        let repeat = match (self.every, self.count, self.until) {
            (None, None, None) => Repeat::Once,
            (Some(every), Some(total), None) if (1..=100).contains(&total) => Repeat::Count {
                interval_ms: duration_ms(&every)?,
                total,
            },
            (Some(every), None, Some(until)) => {
                let interval_ms = duration_ms(&every)?;
                let until_ms = timestamp_ms(&until)?;
                if until_ms < due || (until_ms - due) / interval_ms >= 100 {
                    return Err("until must include the first occurrence and permit at most 100 total occurrences".into());
                }
                Repeat::Until {
                    interval_ms,
                    until_ms,
                }
            }
            _ => {
                return Err(
                    "every requires exactly one of count (1 to 100 total occurrences) or until"
                        .into(),
                );
            }
        };
        Ok(ReminderSpec {
            body: self.body,
            first_due_at_ms: due,
            in_reply_to: self.in_reply_to,
            repeat,
        })
    }
}

fn duration_ms(value: &str) -> Result<u64, String> {
    let (digits, multiplier) = match value.as_bytes().last() {
        Some(b's') => (&value[..value.len() - 1], 1_000),
        Some(b'm') => (&value[..value.len() - 1], 60_000),
        Some(b'h') => (&value[..value.len() - 1], 3_600_000),
        _ => return Err("duration must be an integer followed by s, m, or h".into()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid duration".into());
    }
    let ms = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or("invalid duration")?;
    if ms < 60_000 {
        return Err("minimum delay and interval is 60 seconds".into());
    }
    Ok(ms)
}

fn timestamp_ms(value: &str) -> Result<u64, String> {
    let invalid = || "at and until must be RFC 3339 timestamps".to_owned();
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(invalid());
    }
    if ![(0, 4), (5, 7), (8, 10), (11, 13), (14, 16), (17, 19)]
        .into_iter()
        .all(|(start, end)| bytes[start..end].iter().all(u8::is_ascii_digit))
    {
        return Err(invalid());
    }
    let number = |start, end| {
        value
            .get(start..end)
            .ok_or_else(invalid)?
            .parse::<i64>()
            .map_err(|_| invalid())
    };
    let (year, month, day, hour, minute, second) = (
        number(0, 4)?,
        number(5, 7)?,
        number(8, 10)?,
        number(11, 13)?,
        number(14, 16)?,
        number(17, 19)?,
    );
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month)
        || !(1..=days[(month - 1) as usize]).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(invalid());
    }
    let mut end = 19;
    let mut fraction = 0;
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if start == end {
            return Err(invalid());
        }
        for i in 0..3 {
            fraction = fraction * 10
                + bytes
                    .get(start + i)
                    .filter(|_| start + i < end)
                    .map_or(0, |b| i64::from(b - b'0'));
        }
    }
    let offset = match value.get(end..).ok_or_else(invalid)? {
        "Z" | "z" => 0,
        zone if zone.len() == 6
            && matches!(zone.as_bytes()[0], b'+' | b'-')
            && zone.as_bytes()[3] == b':' =>
        {
            let h = zone[1..3].parse::<i64>().map_err(|_| invalid())?;
            let m = zone[4..6].parse::<i64>().map_err(|_| invalid())?;
            if h > 23 || m > 59 {
                return Err(invalid());
            }
            (h * 3600 + m * 60) * if zone.starts_with('-') { -1 } else { 1 }
        }
        _ => return Err(invalid()),
    };
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + day - 1 - 719468;
    u64::try_from((days * 86400 + hour * 3600 + minute * 60 + second - offset) * 1000 + fraction)
        .map_err(|_| invalid())
}

pub fn execute_reminder_operation<
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
>(
    operation: &str,
    input: Value,
    runtime: &crate::LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String> {
    execute_with_caller(operation, input, runtime, None)
}

pub(crate) fn execute_with_caller<
    E: crate::HerdrCommandExecutor,
    C: crate::Clock,
    I: crate::IdGenerator,
>(
    operation: &str,
    input: Value,
    runtime: &crate::LifecycleRuntime<'_, E, C, I>,
    caller: Option<&crate::deferred_commands::Caller>,
) -> Result<Value, String> {
    let discovered = match runtime.workspace_id {
        Some(w) => crate::discover_worktree_for_workspace(runtime.herdr, w)?,
        None => crate::discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    let ns = discovered.namespace(runtime.session_socket_id);
    let pane = runtime
        .current_pane_id
        .ok_or("reminders require the calling pane")?;
    let now = runtime.clock.now_unix_ms();
    let mut store = crate::StateStore::open(runtime.state_directory).map_err(|e| e.to_string())?;
    let mut result = store
        .transaction(|tx| {
            if let Some(caller) = caller {
                caller.check(tx, &ns)?;
            }
            let owner = crate::context::registered_current_address(tx, &ns, pane, &discovered)?;
            let owner = ReminderOwner {
                address: owner.address_id,
                session: owner.agent_session_id.ok_or_else(|| {
                    crate::StoreError::invalid_input("reminder requires an Agent Session")
                })?,
                binding: owner.pane_binding_id.ok_or_else(|| {
                    crate::StoreError::invalid_input("reminder requires a pane binding")
                })?,
                pane: pane.to_owned(),
            };
            match operation {
                "reminder.set" => {
                    let parsed =
                        validate_input(input.clone()).map_err(crate::StoreError::invalid_input)?;
                    let request = parsed
                        .request_id
                        .clone()
                        .unwrap_or_else(|| runtime.ids.next_id("reminder-request"));
                    let fingerprint = serde_json::to_string(&input)
                        .map_err(|e| crate::StoreError::invalid_input(e.to_string()))?;
                    if let Some(existing) =
                        tx.replay_reminder(&ns, &owner.session, &request, &fingerprint)?
                    {
                        if existing.owner != owner {
                            return Err(crate::StoreError::invalid_input(
                                "reminder request belongs to a previous owner binding",
                            ));
                        }
                        return Ok(json!(existing));
                    }
                    let spec = parsed
                        .resolve(now)
                        .map_err(crate::StoreError::invalid_input)?;
                    let reminder = tx.set_reminder(
                        &ns,
                        runtime.ids.next_id("reminder"),
                        owner,
                        spec,
                        &request,
                        &fingerprint,
                    )?;
                    Ok(json!(reminder))
                }
                "reminder.list" if input == json!({}) => {
                    Ok(json!({"reminders":tx.list_reminders(&ns,&owner)?}))
                }
                "reminder.cancel" => {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Cancel {
                        reminder_id: String,
                    }
                    let cancel: Cancel = serde_json::from_value(input.clone()).map_err(|e| {
                        crate::StoreError::invalid_input(crate::input_schema_error(e))
                    })?;
                    tx.cancel_reminder(&ns, &owner, &cancel.reminder_id, now)
                }
                _ => Err(crate::StoreError::invalid_input(
                    "unsupported reminder operation or invalid input",
                )),
            }
        })
        .map_err(|e| e.to_string())?;
    if operation != "reminder.list"
        && let Err(error) = signal_clock(runtime.state_directory, runtime.session_socket_id)
    {
        result["clock_error"] = json!(error);
    }
    Ok(result)
}

mod clock;
use clock::signal_clock;
pub use clock::{clock_running, ensure_clock, pending_count, run_clock};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminder_rfc3339_rejects_signed_offset_components() {
        for offset in [
            "+-1:00", "+00:-1", "++1:00", "+00:+1", "--1:00", "-00:-1", "-+1:00", "-00:+1",
        ] {
            let value = format!("2099-01-01T00:00:00{offset}");
            assert!(
                timestamp_ms(&value).is_err(),
                "accepted malformed RFC3339 offset {value}"
            );
        }
    }

    #[test]
    fn reminder_schedule_limits_and_rfc3339_offsets() {
        let now = timestamp_ms("2026-11-05T12:00:00Z").unwrap();
        assert_eq!(timestamp_ms("2026-11-05T07:00:00-05:00").unwrap(), now);
        assert_eq!(timestamp_ms("2026-11-05T17:30:00+05:30").unwrap(), now);
        assert_eq!(
            timestamp_ms("2026-11-05T12:00:00.123456Z").unwrap(),
            now + 123
        );
        for input in [
            json!({"in":"59s","body":"x"}),
            json!({"in":"1m","at":"2026-11-05T13:00:00Z","body":"x"}),
            json!({"in":"1m","every":"59s","count":2,"body":"x"}),
            json!({"in":"1m","every":"1m","body":"x"}),
            json!({"in":"1m","every":"1m","count":101,"body":"x"}),
            json!({"in":"1m","count":2,"body":"x"}),
            json!({"in":"1m","every":"1m","until":"2026-11-05T13:41:00Z","body":"x"}),
            json!({"at":"2026-02-30T13:00:00Z","body":"x"}),
            json!({"in":"1m","body":" "}),
        ] {
            assert!(
                validate_input(input.clone())
                    .and_then(|i| i.resolve(now))
                    .is_err(),
                "accepted {input}"
            );
        }
        let spec = validate_input(
            json!({"in":"1m","every":"1m","until":"2026-11-05T13:40:00Z","body":"x"}),
        )
        .unwrap()
        .resolve(now)
        .unwrap();
        assert_eq!(spec.first_due_at_ms, now + 60_000);
        assert_eq!(
            spec.repeat.next(99, now + 99 * 60_000),
            Some(now + 100 * 60_000)
        );
        assert_eq!(spec.repeat.next(100, now + 100 * 60_000), None);
        assert_eq!(
            Repeat::Count {
                interval_ms: 60_000,
                total: 2
            }
            .next(1, now + 500_000),
            Some(now + 560_000)
        );
    }
}
