"""Compare two flow reports with the scrub's names removed from each recorded
environment, except the two multiplexer timeouts, which are compared as they are."""
import json, re, sys
SCRUB = {"REX_BLOCK","REX_SESSION","REX_CLIENT","REX_SERVER","CLAUDE_SESSION_ID","NAMEDROP_REX_TIMEOUT_MS",
 "CLAUDE_CODE_SESSION_ID","CLAUDE_CODE_CHILD_SESSION","CLAUDE_CODE_SESSION_ATTENDED","CLAUDE_CODE_ENTRYPOINT","CODEX_SESSION_ID","CODEX_THREAD_ID","AI_AGENT",
 "CLAUDE_CODE_MESSAGING_SOCKET","CLAUDE_CODE_MESSAGING_TOKEN","CLAUDE_CODE_EXECPATH","CLAUDE_PID","CLAUDECODE","CLAUDE_EFFORT"}
def clean(line, drop):
    line = re.sub(r"127\.0\.0\.1:[0-9]+", "127.0.0.1:<PORT>", line)
    if line.startswith('{"env"') or line.startswith('{"args"'):
        r = json.loads(line); r["env"] = [p for p in r["env"] if p[0] not in drop]
        return json.dumps({"args": r["args"], "env": r["env"]})
    if "  ENV " in line:
        c, names = line.split("  ENV ", 1)
        return c + "  ENV " + " ".join(n for n in names.split() if n.split("=",1)[0] not in drop)
    return line
def load(path, drop):
    out = []; section = ""
    for line in open(path).read().splitlines():
        if line.startswith("## "): section = line
        cleaned = clean(line, drop)
        if section == "## herdr calls (set)" and cleaned.startswith('{"args"'):
            call = json.loads(cleaned)
            args = call["args"]
            if args[:2] == ["pane", "report-metadata"] and "namedrop-awaiting=4" in args and "namedrop-attention=2" in args:
                continue
        out.append((section, cleaned))
    return out
MARKERS = SCRUB - {"NAMEDROP_HERDR_TIMEOUT_MS", "NAMEDROP_REX_TIMEOUT_MS"}
for title, drop in (("with the scrub's names removed and both timeouts kept", MARKERS),):
    a, b = load(sys.argv[1], drop), load(sys.argv[2], drop)
    sa, sb = set(a), set(b)
    print("==", title, ": only-left", len(sa - sb), "only-right", len(sb - sa))
    for sec, line in sorted(sa - sb): print("<", sec[:28], "|", line[:int(sys.argv[3]) if len(sys.argv) > 3 else 330])
    for sec, line in sorted(sb - sa): print(">", sec[:28], "|", line[:int(sys.argv[3]) if len(sys.argv) > 3 else 330])

raise SystemExit(1 if sa != sb else 0)
