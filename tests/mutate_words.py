"""Reintroduce host wording leaks; each must fail the U11 sweep, then restore."""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
env = {key: value for key, value in os.environ.items() if key not in {"REX_BLOCK", "REX_SESSION", "REX_SERVER", "REX_CLIENT"}}
mutations = [
    ("doctor", "src/health.rs", "Start Codex or Claude Code in a Rex pane, then check again.", "Start Codex or Claude Code in a Herdr pane, then check again.", "rex_words_sweep"),
    ("activation", "src/output.rs", "Check Namedrop hooks with namedrop doctor.", "Check Herdr hooks with namedrop doctor.", "rex_words_sweep"),
    ("tour", "src/tour.rs", '\"this Rex session\"', '\"this Herdr workspace\"', "rex_words_sweep"),
    ("package", "src/install.rs", "agents or terminals in Rex", "agents or terminals in Herdr", "rex_words_sweep"),
    ("help", "src/cli.rs", "collaborate inside Rex", "collaborate inside Herdr", "rex_words_sweep"),
    ("roster", "src/persona.rs", "Current named agents in this {} space", "Current named agents in this Herdr {} space", "rex_words_sweep"),
    ("registration", "src/integration.rs", "rex_catalog_value(&mut operation.input_schema);", "let _ = &operation.input_schema;", "rex_words_sweep"),
    ("runtime", "scripts/runtime.py", "action = 'installation' if verify == 'install' else 'activation'", "action = 'activation'", "rex_words_screens_and_runtime_failures"),
]
results = []
for label, file, before, after, test in mutations:
    path = root / file
    original = path.read_bytes()
    assert original.count(before.encode()) == 1, (label, "mutation target must be unique")
    try:
        path.write_bytes(original.replace(before.encode(), after.encode()))
        with (out / (label + ".log")).open("w") as log:
            child = subprocess.Popen(["cargo", "test", "--locked", "--test", "words", test, "--", "--exact", "--nocapture"], cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
            print(label, "pid", child.pid, flush=True)
            code = child.wait()
        text = (out / (label + ".log")).read_text()
        assert code != 0 and "test result: FAILED" in text, (label, code, text)
        assert "Words violation" in text or label == "runtime" and "AssertionError" in text, (label, "failed for an unrelated reason", text)
        results.append({"mutation": label, "file": file, "test": test, "pid": child.pid, "exit": code, "detected": True})
        print(label + " detected", flush=True)
    finally:
        path.write_bytes(original)
(out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
