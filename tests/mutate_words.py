"""Reintroduce host wording leaks; each must fail the Words test, then restore.

usage: python3 -I tests/mutate_words.py OUT [--combined] [LABEL...]

Each mutation runs `cargo test --test words` on its own. `--combined` also
applies the second review's ten at once (Fable's U11 review, finding 2), which
all passed the sweep before this round.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
env = {key: value for key, value in os.environ.items() if key not in {"REX_BLOCK", "REX_SESSION", "REX_SERVER", "REX_CLIENT"}}
# Each failure the Words test reports, by the words it uses.
FAILED_BY_WORDS = ("Words violation", "names Herdr's words", "no Rex surface the sweep checks printed",
                   "unclassified host word", "no surface printed", "stale ", "to give its Rex words", "AssertionError",
                   "panicked at tests/words.rs")
REVIEW = [
    ("terminal", "src/terminal.rs", '"terminal runner requires its current Rex pane"', '"terminal runner requires its current Herdr pane"'),
    ("naming space", "src/naming_ui.rs", '"This screen requires an active Rex session"', '"This popup requires an active Herdr workspace"'),
    ("doctor accepted", "src/health.rs", '"Rex accepted the prompt.', '"Herdr accepted the prompt.'),
    ("context", "src/context.rs", '"the current Rex pane is required for snapshot publication"', '"the current Herdr pane is required for snapshot publication"'),
    ("integration", "src/integration.rs", '"the current Rex pane is required for registration"', '"the current Herdr pane is required for registration"'),
    ("message", "src/message.rs", '"the current Rex pane is required to resolve the sender"', '"the current Herdr pane is required to resolve the sender"'),
    ("controls", "src/controls.rs", '"allow-remote must run from a Rex pane in this workspace"', '"allow-remote must run from a Herdr pane in this workspace."'),
    ("health identity", "src/health.rs", '"No Namedrop session identity for this Rex pane now."', '"No Herdr session identity for this Rex pane now."'),
    ("tour key", "src/tour.rs", '"Press Prefix, then Shift-S to see"', '"Press Prefix, then Shift-X to see"'),
    ("live peek", "src/dashboard/live_peek.rs", '"live peek is not available in Rex panes",\n', '"live peek works only in Herdr panes",\n'),
]
MUTATIONS = REVIEW + [
    # The first round's, kept.
    ("doctor", "src/health.rs", "Start Codex or Claude Code in a Rex pane, then check again.", "Start Codex or Claude Code in a Herdr pane, then check again."),
    ("activation", "src/output.rs", "Check Namedrop hooks with namedrop doctor.", "Check Herdr hooks with namedrop doctor."),
    ("tour", "src/tour.rs", '"this Rex session".to_owned()', '"this Herdr workspace".to_owned()'),
    ("package", "src/install.rs", '"agents or terminals in Rex"', '"agents or terminals in Herdr"'),
    ("help", "src/cli.rs", '"Name and @mention AI agents and terminal panes to collaborate inside Rex"', '"Name and @mention AI agents and terminal panes to collaborate inside Herdr"'),
    ("roster", "src/persona.rs", '"Current named agents in this Rex session;', '"Current named agents in this Herdr space;'),
    ("registration", "src/integration.rs", "rex_catalog_value(&mut operation.input_schema);", "let _ = &operation.input_schema;"),
    ("update notice", "src/update_check.rs", '"download Namedrop {} and run sh scripts/install.sh",', '"download Namedrop {} and run herdr plugin action invoke update",'),
    ("update doctor", "src/health.rs", 'format!("Update: {instruction}")', 'format!("Update outside Herdr: {instruction}")'),
    ("runtime", "scripts/runtime.py", "action = 'installation' if verify == 'install' else 'activation'", "action = 'activation'"),
    # This round's findings.
    ("F1 app-server summary", "src/codex_app_server.rs", "    kind.text(SUMMARY, REX_SUMMARY)\n", "    SUMMARY\n"),
    ("F1 app-server steps", "src/codex_app_server.rs", "            MuxKind::Rex => REX_STEPS,", "            MuxKind::Rex => STEPS,"),
    ("F1 doctor issue", "src/health.rs", "crate::codex_app_server::summary(kind),", "crate::codex_app_server::SUMMARY,"),
    ("F1 activation", "src/install.rs", ".advice(kind) {", ".advice(crate::mux::MuxKind::Herdr) {"),
    ("F1 name", "src/lifecycle.rs", "served.advice(runtime.mux.endpoint().kind)", "served.advice(crate::mux::MuxKind::Herdr)"),
    ("F1 doctor refusal", "src/health.rs", "crate::codex_app_server::summary(runtime.mux.endpoint().kind)", "crate::codex_app_server::SUMMARY"),
    ("busy gate popup", "scripts/runtime.py", "screen = 'screen' if standalone_owner(path.parent) else 'popup'", "screen = 'popup'"),
    ("help popup", "src/cli.rs", '"Open a naming, name-management, Doctor, agent status, or tour screen"', '"Open a naming, name-management, Doctor, agent status, or tour popup"'),
    ("help plugin", "src/cli.rs", '"Recover after a restart; ambiguous in-flight delivery attempts are held"', '"Recover after a plugin restart; ambiguous in-flight delivery attempts are held"'),
    ("screen popup", "src/naming_ui.rs", '"The invoking pane changed. Close this screen and open it again."', '"The invoking pane changed. Close this popup and open it again."'),
    ("status popup", "src/agent_status_ui.rs", '"This pane changed. Close this screen and open it again from your pane."', '"This pane changed. Close this popup and open it again from your pane."'),
    ("one noun", "src/hook.rs", '"No named agents in this Rex session."', '"No named agents in this Rex space."'),
    ("other sessions", "src/persona.rs", '"1 other Rex session has"', '"1 other Rex workspace has"'),
    ("tour repeats", "src/tour.rs", '"Run {} from Rex\'s palette."', '"Run {} from Rex\'s palette to open Namedrop Doctor."'),
    ("session repair", "src/health.rs", "            if rex {\n                add_operation(", "            if false {\n                add_operation("),
    ("session summary", "src/health.rs", 'kind.text("Herdr", "Namedrop")', 'kind.text("Herdr", "Rex")'),
    ("doctor prompt", "src/health.rs", 'kind.text("popup", "screen")', 'kind.text("popup", "popup")'),
    ("authored", "src/hook.rs", "is Namedrop-authored.\";", "is plugin-authored.\";"),
    ("new Herdr literal", "src/lifecycle.rs", "fn multiplexer_word<M, C, I>(runtime: &LifecycleRuntime<'_, M, C, I>) -> &'static str {\n", "fn multiplexer_word<M, C, I>(runtime: &LifecycleRuntime<'_, M, C, I>) -> &'static str {\n    let _ = \"Herdr says hello\";\n"),
    ("shared-home text moved", "src/health.rs", '"An older Herdr release updated the packages without updating the Rex module."', '"An older Herdr release updated the packages, not the Rex module."'),
]


def run(label, edits):
    originals = {}
    for _, file, before, after in edits:
        path = root / file
        originals.setdefault(path, path.read_bytes())
        text = path.read_bytes()
        assert text.count(before.encode()) == 1, (label, file, before, "mutation target must be unique")
        path.write_bytes(text.replace(before.encode(), after.encode()))
    try:
        log_path = out / (label.replace(" ", "-") + ".log")
        with log_path.open("w") as log:
            child = subprocess.Popen(["cargo", "test", "--locked", "--test", "words"], cwd=root, env=env,
                                     stdout=log, stderr=subprocess.STDOUT)
            print(label, "pid", child.pid, flush=True)
            code = child.wait()
        text = log_path.read_text()
        assert code != 0 and "test result: FAILED" in text, (label, code, "survived")
        reason = next((marker for marker in FAILED_BY_WORDS if marker in text), None)
        assert reason, (label, "failed for an unrelated reason", text[-4000:])
        print(label, "detected:", reason, flush=True)
        return {"mutation": label, "files": sorted({e[1] for e in edits}), "exit": code, "detected": reason}
    finally:
        for path, original in originals.items():
            path.write_bytes(original)


selected = [arg for arg in sys.argv[2:] if not arg.startswith("--")]
results = [run(label, [(label, file, before, after)]) for label, file, before, after in MUTATIONS
           if not selected or label in selected]
if "--combined" in sys.argv:
    results.append(run("review ten combined", REVIEW))
(out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
print(len(results), "mutations detected")
