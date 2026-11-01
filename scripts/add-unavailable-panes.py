#!/usr/bin/env python3
"""Migrate complete-discovery literals and reconciliation test callers."""
import argparse
from pathlib import Path
import re

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--write", action="store_true")
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
changed = []
for base in (Path("src"), Path("tests")):
    for path in sorted(base.rglob("*.rs")):
        old = path.read_text()
        new = re.sub(
            r"(\bDiscoveredWorktree \{\n)(\s*)(worktree_id:)",
            r"\1\2unavailable_panes: vec![],\n\2\3",
            old,
        )
        new = new.replace(
            "setup: CollaborationSetup {\n                worktree_state:",
            "setup: CollaborationSetup {\n                unavailable_panes: discovered.unavailable_panes,\n                worktree_state:",
        )
        if base == Path("tests"):
            pattern = r"(reconcile_discovered_panes\(\n[ \t]*transaction,\n[ \t]*&(?:scope|namespace),\n)([ \t]*)([^\n]*)"
            def migrate_call(match):
                if match[3] == "&[],":
                    return match[0]
                return match[1] + match[2] + "&[],\n" + match[2] + match[3]
            new = re.sub(pattern, migrate_call, new)
        if new != old:
            changed.append(str(path))
            print(path)
            if args.write:
                path.write_text(new)
raise SystemExit(bool(changed) if args.check else 0)
