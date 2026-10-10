#!/usr/bin/env python3
"""Explicit, bounded live sampling; never invoked by the normal test suite.

Reads current-workspace panes and runs each same-host worker through the actual
library. It does not install a plugin, edit config, prompt an existing session,
or publish captured pane text. Usage is left unknown where the library has no
measurement. Real popup/TTY acceptance is a separate check.
"""
import argparse
import json
import math
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
MARKER = "AGENT_STATUS_PROBE_JSON="
MODELS = {"codex": "gpt-5.6-luna", "claude": "claude-haiku-4-5"}


def p90(values):
    return sorted(values)[math.ceil(len(values) * 0.9) - 1] if values else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", required=True)
    parser.add_argument("--hosts", default="codex,claude")
    parser.add_argument("--runs-per-host", type=int, default=10)
    parser.add_argument("--output", type=Path,
                        default=ROOT / ".unlazy/agent-status/reports/live-samples.json")
    args = parser.parse_args()
    hosts = args.hosts.split(",")
    if not 1 <= args.runs_per_host <= 10 or any(h not in MODELS for h in hosts):
        parser.error("use supported hosts and 1..10 runs per host")
    for name in ("HERDR_WORKSPACE_ID", "HERDR_SOCKET_PATH", "HERDR_PLUGIN_STATE_DIR"):
        if not os.environ.get(name):
            parser.error(f"{name} must identify the authorized workspace and state")
    response = subprocess.run(
        ["herdr", "pane", "list", "--workspace", os.environ["HERDR_WORKSPACE_ID"]],
        check=True, capture_output=True, text=True, timeout=5,
    )
    panes = json.loads(response.stdout)["result"]["panes"]
    chosen = {}
    for host in hosts:
        matches = [p for p in panes if p.get("agent") == host and p.get("agent_session")]
        if not matches:
            parser.error(f"no live {host} origin in this workspace")
        chosen[host] = matches[0]["pane_id"]

    report = {"scope": "live library calls; popup/TTY and usage acceptance separate",
              "workspace": os.environ["HERDR_WORKSPACE_ID"], "samples": [], "aggregates": {}}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for host, pane in chosen.items():
        for index in range(args.runs_per_host):
            environment = dict(os.environ, NAMEDROP_VERIFY_LIVE="1", NAMEDROP_VERIFY_PANE=pane)
            result = subprocess.run(
                ["cargo", "test", "--locked", "--test", "agent_status", "live_origin_probe",
                 "--", "--ignored", "--exact", "--nocapture"],
                cwd=ROOT, env=environment, capture_output=True, text=True, timeout=60,
            )
            observations = [line[len(MARKER):] for line in result.stdout.splitlines()
                            if line.startswith(MARKER)]
            if result.returncode or len(observations) != 1:
                print(result.stdout[-4000:] + result.stderr[-4000:], file=sys.stderr)
                raise RuntimeError(f"{host} sample {index + 1} did not produce valid evidence")
            sample = json.loads(observations[0])
            sample["run"] = index + 1
            report["samples"].append(sample)
            args.output.write_text(json.dumps(report, indent=2) + "\n")
            print(f"{host} {index + 1}/{args.runs_per_host}: facts {sample['facts_ms']}ms, "
                  f"cold {sample['cold_summary_ms']}ms, warm {sample['warm_summary_ms']}ms, "
                  f"error {sample['summary_error']}", flush=True)
    targets_met = True
    for host in hosts:
        samples = [s for s in report["samples"] if s["host"] == host]
        valid = all(s["model"] == MODELS[host] and s["cache_reused"]
                    and s["origin_unchanged"] and not s["summary_error"] for s in samples)
        metrics = {"facts_p90_ms": p90([s["facts_ms"] for s in samples]),
                   "cold_summary_p90_ms": p90([s["cold_summary_ms"] for s in samples]),
                   "warm_summary_p90_ms": p90([s["warm_summary_ms"] for s in samples
                                               if s["warm_summary_ms"] is not None]),
                   "valid": valid}
        report["aggregates"][host] = metrics
        targets_met &= valid and metrics["facts_p90_ms"] <= 500 and metrics["cold_summary_p90_ms"] <= 5000
    report["timing_targets_met"] = targets_met
    report["full_live_acceptance"] = False
    report["outstanding"] = ["real floating popup/TTY acceptance", "usage accounting",
                             "live dependency scenarios on a compatible state schema"]
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["aggregates"], indent=2))
    print("AGENT_STATUS_LIVE_SAMPLES_RECORDED")
    return 0 if targets_met else 1


if __name__ == "__main__":
    sys.exit(main())
