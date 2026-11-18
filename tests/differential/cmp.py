"""Compare two flow reports with the scrub's names removed from each recorded
environment, except the two multiplexer timeouts, which are compared as they are.
Steps compare in order, each line under its stream; the "(set)" sections
compare as sets."""
import difflib, json, re, sys
SCRUB = {"REX_BLOCK","REX_SESSION","REX_CLIENT","REX_SERVER","CLAUDE_SESSION_ID","NAMEDROP_REX_TIMEOUT_MS",
 "CLAUDE_CODE_SESSION_ID","CLAUDE_CODE_CHILD_SESSION","CLAUDE_CODE_SESSION_ATTENDED","CLAUDE_CODE_ENTRYPOINT","CODEX_SESSION_ID","CODEX_THREAD_ID","AI_AGENT",
 "CLAUDE_CODE_MESSAGING_SOCKET","CLAUDE_CODE_MESSAGING_TOKEN","CLAUDE_CODE_EXECPATH","CLAUDE_PID","CLAUDECODE","CLAUDE_EFFORT"}
def clean(line, drop):
    line = re.sub(r"127\.0\.0\.1:[0-9]+", "127.0.0.1:<PORT>", line)
    line = re.sub(r'\\"port\\":[0-9]+', r'\\"port\\":<PORT>', line)
    if line.startswith('{"env"') or line.startswith('{"args"'):
        r = json.loads(line); r["env"] = [p for p in r["env"] if p[0] not in drop]
        return json.dumps({"args": r["args"], "env": r["env"]})
    if "  ENV " in line:
        c, names = line.split("  ENV ", 1)
        return c + "  ENV " + " ".join(n for n in names.split() if n.split("=",1)[0] not in drop)
    return line
PIN = json.dumps({"mode": "0o600", "path": "<ROOT>/home/.config/namedrop/packages.lock", "text": ""}, sort_keys=True)
def load(path, drop):
    """A report's steps, in order and each line under its stream, and its
    "(set)" sections as one set."""
    steps = []; sets = set(); section = ""; stream = ""
    for line in open(path).read().splitlines():
        if line.startswith("## "): section = line; stream = ""
        elif line in ("--stdout", "--stderr"): stream = line
        cleaned = clean(line, drop)
        if not section.endswith("(set)"):
            steps.append((section, stream, cleaned))
            continue
        if "--allow-package-lock" in sys.argv and section == "## files (set)" and cleaned == PIN:
            continue
        if section == "## herdr calls (set)" and cleaned.startswith('{"args"'):
            call = json.loads(cleaned)
            args = call["args"]
            if args[:2] == ["pane", "report-metadata"] and "namedrop-awaiting=4" in args and "namedrop-attention=2" in args:
                continue
        sets.add((section, cleaned))
    return steps, sets
OWNER = ("## files (set)", json.dumps({"mode": "0o600", "path": "<STATE>-runtime/runtime/owner", "text": "herdr\n"}, sort_keys=True))
def show(row):
    return " | ".join(part[:28] if index < len(row) - 1 else part[:330] for index, part in enumerate(row))
MARKERS = SCRUB - {"NAMEDROP_HERDR_TIMEOUT_MS", "NAMEDROP_REX_TIMEOUT_MS"}
for title, drop in (("with the scrub's names removed and both timeouts kept", MARKERS),):
    (steps_a, sa), (steps_b, sb) = load(sys.argv[1], drop), load(sys.argv[2], drop)
    missing = "--allow-runtime-owner" in sys.argv and OWNER not in sb
    if "--allow-runtime-owner" in sys.argv:
        # U4's pin for a base from before U4: the right report must write the
        # file, and the left one, which is compared as usual, must not.
        sb.discard(OWNER)
    print("== steps in order:", "equal" if steps_a == steps_b else "differ")
    for line in difflib.unified_diff([show(row) for row in steps_a], [show(row) for row in steps_b], "left", "right", n=1, lineterm=""):
        print(line)
    print("== sets", title, ": only-left", len(sa - sb), "only-right", len(sb - sa))
    for sec, line in sorted(sa - sb): print("<", sec[:28], "|", line[:330])
    for sec, line in sorted(sb - sa): print(">", sec[:28], "|", line[:330])
    if missing: print("> missing the pinned", OWNER[1])

raise SystemExit(1 if sa != sb or steps_a != steps_b or missing else 0)
