"""Run both archive-built binaries three times and compare all reports."""
import itertools
import os
from pathlib import Path
import subprocess
import sys

tools = Path(__file__).resolve().parent
ALLOW = {"--allow-package-lock", "--allow-runtime-owner"}
if len(sys.argv) < 4 or not set(sys.argv[4:]) <= ALLOW:
    raise SystemExit("usage: run.py BASE_BINARY HEAD_BINARY OUT [--allow-package-lock] [--allow-runtime-owner]")
root = Path(sys.argv[3]).resolve()
root.mkdir(parents=True, exist_ok=True)
env = {k: v for k, v in os.environ.items() if k not in {"REX_BLOCK", "REX_SESSION", "REX_SERVER", "REX_CLIENT"}}
reports = []
for label, binary in zip(("base", "head"), sys.argv[1:3]):
    for trial in range(3):
        out = root / (label + str(trial))
        with (root / (out.name + ".log")).open("w") as log:
            subprocess.run([sys.executable, "-I", str(tools / "flow.py"), str(Path(binary).resolve()), str(out)], env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
        reports.append(out / "report")
failed = False
for left, right in itertools.combinations(reports, 2):
    with (root / (left.parent.name + "-" + right.parent.name + ".diff")).open("w") as log:
        # An allowance describes base against head, so only those pairs take it.
        crossed = left.parent.name.startswith("base") and right.parent.name.startswith("head")
        compared = subprocess.run([sys.executable, "-I", str(tools / "cmp.py"), str(left), str(right), *(sys.argv[4:] if crossed else [])], env=env, stdout=log, stderr=subprocess.STDOUT)
    failed |= compared.returncode != 0
raise SystemExit(1 if failed else 0)
