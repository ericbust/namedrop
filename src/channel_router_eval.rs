use super::*;
use serde::Deserialize;
use std::{fs, io::Write};

#[derive(Deserialize)]
struct Case {
    id: String,
    source: String,
    expected: Vec<String>,
    #[serde(default)]
    recap: Vec<String>,
}

#[test]
#[ignore = "makes real model calls; requires isolated credentials and explicit eval output"]
pub(super) fn real_host_router_corpus() {
    let source = env::var("NAMEDROP_ROUTER_EVAL_INPUT")
        .map(|path| fs::read_to_string(path).unwrap())
        .unwrap_or_else(|_| include_str!("../tests/fixtures/channel_router_cases.json").to_owned());
    let cases: Vec<Case> = serde_json::from_str(&source).unwrap();
    let output = env::var("NAMEDROP_ROUTER_EVAL_OUTPUT").expect("required eval output JSONL path");
    let mut output = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output)
        .unwrap();
    let hosts = env::var("NAMEDROP_ROUTER_EVAL_HOST").unwrap_or_else(|_| "both".to_owned());
    let formats = env::var("NAMEDROP_ROUTER_EVAL_FORMAT").unwrap_or_else(|_| "both".to_owned());
    let repetitions: u32 = env::var("NAMEDROP_ROUTER_EVAL_REPETITIONS")
        .unwrap_or_else(|_| "3".to_owned())
        .parse()
        .unwrap();
    let deadline_ms: u64 = env::var("NAMEDROP_ROUTER_EVAL_DEADLINE_MS")
        .unwrap_or_else(|_| "20000".to_owned())
        .parse()
        .unwrap();
    assert!((1..=20_000).contains(&deadline_ms));
    assert!((1..=100).contains(&repetitions));
    assert!(matches!(hosts.as_str(), "claude" | "codex" | "both"));
    assert!(matches!(formats.as_str(), "structured" | "plain" | "both"));
    let rows = tests::rows();
    for repetition in 0..repetitions {
        for (case_index, case) in cases.iter().enumerate() {
            let mentions = crate::message::context_mentions(&case.source);
            let candidates: Vec<_> = tests::candidates(&rows)
                .into_iter()
                .filter(|candidate| {
                    mentions.iter().any(|mention| {
                        mention.workspace.is_none() && mention.alias == candidate.alias
                    })
                })
                .collect();
            assert!(
                !candidates.is_empty(),
                "{} has no lexer candidates",
                case.id
            );
            for host in ["claude", "codex"] {
                if hosts != "both" && hosts != host {
                    continue;
                }
                let order = if (case_index + repetition as usize).is_multiple_of(2) {
                    ["structured", "plain"]
                } else {
                    ["plain", "structured"]
                };
                for format in order {
                    if formats != "both" && formats != format {
                        continue;
                    }
                    let mut input = build_input(RouterRequest {
                        host: Some(if host == "claude" {
                            AgentRuntime::ClaudeCode
                        } else {
                            AgentRuntime::Codex
                        }),
                        rows: &rows,
                        source: &case.source,
                        candidates: &candidates,
                    })
                    .unwrap();
                    if !case.recap.is_empty() {
                        let mut payload: Value = serde_json::from_str(&input.payload).unwrap();
                        payload["recap"] = json!(
                            case.recap
                                .iter()
                                .take(6)
                                .map(|line| bounded_text(line, 194))
                                .collect::<Vec<_>>()
                        );
                        input.payload = payload.to_string();
                        assert!(
                            input.payload.len() + input.schema.len() + INSTRUCTIONS.len()
                                <= MAX_INPUT_BYTES
                        );
                    }
                    let started = Instant::now();
                    let capture = capture_worker(
                        host,
                        &WorkerPrompt {
                            instructions: INSTRUCTIONS,
                            schema: input.schema,
                        },
                        &input.payload,
                        &AtomicBool::new(false),
                        Some(started + Duration::from_millis(deadline_ms)),
                        format == "structured",
                    );
                    let mut metrics = metrics(host, &capture.raw_stdout, capture.result.is_ok());
                    metrics.duration_ms = started.elapsed().as_millis().try_into().unwrap();
                    let worker_error = capture
                        .result
                        .as_ref()
                        .err()
                        .map(|error| bounded_text(error, 400));
                    let parsed = capture
                        .result
                        .and_then(|raw| extract_response(&raw, "to"))
                        .and_then(|response| validate_response(&response, &input.aliases));
                    let (actual, reason, error) = match parsed {
                        Ok((to, reason)) => (Some(to), Some(reason), None),
                        Err(error) => (None, None, Some(bounded_text(&error, 400))),
                    };
                    let outcome = if actual.is_some() {
                        "routed"
                    } else if worker_error.is_some() {
                        "worker_failed"
                    } else {
                        "invalid_response"
                    };
                    if actual.is_none() {
                        metrics.outcome = if worker_error.is_none() {
                            RouterOutcome::InvalidResponse
                        } else {
                            capture.failure.into()
                        };
                    }
                    let mut expected = case.expected.clone();
                    expected.sort();
                    let matches = actual.as_ref().is_some_and(|actual| {
                        let mut actual = actual.clone();
                        actual.sort();
                        actual == expected
                    });
                    let provider_envelope = serde_json::from_str::<Value>(&capture.raw_stdout).ok();
                    let provider_duration_api_ms = provider_envelope
                        .as_ref()
                        .and_then(|value| value["duration_api_ms"].as_u64());
                    let record = json!({
                        "id": case.id, "repetition": repetition + 1, "host": host, "format": format,
                        "candidates": input.aliases, "expected": case.expected, "actual": actual,
                        "matches": matches, "outcome": outcome, "reason": reason, "error": error,
                        "worker_error": worker_error, "error_category": metrics.outcome,
                        "metrics": metrics, "provider_usage": worker_usage(&capture.raw_stdout),
                        "provider_stdout": bounded_text(&capture.raw_stdout, 4096),
                        "provider_duration_api_ms": provider_duration_api_ms,
                        "provider_total_cost_usd": metrics.provider_usd,
                    });
                    serde_json::to_writer(&mut output, &record).unwrap();
                    output.write_all(b"\n").unwrap();
                    output.flush().unwrap();
                    output.sync_data().unwrap();
                    eprintln!(
                        "{} {} {} repetition {} matches={matches}",
                        case.id,
                        host,
                        format,
                        repetition + 1
                    );
                }
            }
        }
    }
}
