#!/bin/sh
set -eu

operation=install
output_format=human
operation_set=
keep_open=
for argument in "$@"; do
  case "$argument" in
    -h|--help) printf '%s\n' 'Usage: sh scripts/setup.sh [install|activate|check] [--human|--json] [--keep-open]' 'Run in a terminal in the desired Herdr workspace. install links this directory and activates Namedrop, activate only activates an already registered installation, and check only runs doctor. --keep-open keeps the result visible until Enter is pressed.'; exit 0 ;;
    --json) output_format=json ;;
    --human) output_format=human ;;
    --keep-open) keep_open=yes ;;
    install|activate|check)
      if [ -n "$operation_set" ]; then printf '%s\n' 'Choose one of install, activate or check.' >&2; exit 64; fi
      operation=$argument; operation_set=yes ;;
    *) printf '%s\n' 'Usage: sh scripts/setup.sh [install|activate|check] [--human|--json] [--keep-open]' >&2; exit 64 ;;
  esac
done
if [ -n "$keep_open" ]; then
  # Plugin tabs retain the result until the user has read it. Preserve the
  # setup status, including failures caused by `set -e` below.
  trap 'namedrop_setup_status=$?; trap - EXIT; printf "\nPress Enter when you are done reading.\n" >&2; IFS= read -r namedrop_setup_reply || :; exit "$namedrop_setup_status"' EXIT
fi
progress() {
  if [ "$output_format" = json ]; then printf '%s\n' "$@" >&2; else printf '%s\n' "$@"; fi
}
if [ -z "${HERDR_WORKSPACE_ID:-}" ] || [ -z "${HERDR_PANE_ID:-}" ] || [ -z "${HERDR_SOCKET_PATH:-}" ]; then
  printf '%s\n' 'Open a terminal in the desired Herdr workspace, then run this command again. HERDR_WORKSPACE_ID, HERDR_PANE_ID and HERDR_SOCKET_PATH must identify that workspace.' >&2
  exit 64
fi
if ! command -v herdr >/dev/null 2>&1; then
  printf '%s\n' 'Herdr is missing from PATH. Install Herdr and open a new terminal inside it.' >&2
  exit 69
fi
if ! command -v python3 >/dev/null 2>&1; then
  printf '%s\n' 'Herdr native runtime hooks require Python 3 on PATH to report Agent Session IDs. Install Python 3, open a new Herdr terminal, and retry. The Namedrop executable itself does not require Python.' >&2
  exit 69
fi
action=doctor
# Empty unless this run is allowed to (re)link a local directory. Herdr owns a
# GitHub-installed checkout, so linking one would replace its managed origin
# with a local link.
link_root=
if [ "$operation" != check ]; then
  action=activate
fi
if [ "$operation" = install ]; then
  # Any unreadable or unrecognized answer must report `unreadable`, never crash:
  # a traceback here would exit with the wrong status and no guidance.
  registration=$(python3 -c 'import json, sys
try:
    plugins = json.loads(sys.argv[1])["result"]["plugins"]
    # Only a real list is a registration. An object or a string also iterates,
    # yielding no kinds, so either would otherwise read as `none` and authorize
    # the build-and-relink branch below. A non-object envelope, or one missing
    # result.plugins, raises on the line above and reports the same way.
    if isinstance(plugins, list):
        kinds = [p.get("source", {}).get("kind") for p in plugins
                 if p.get("plugin_id") == "namedrop"]
    else:
        kinds = None
except (AttributeError, IndexError, KeyError, TypeError, ValueError):
    kinds = None
if kinds is None:
    print("unreadable")
elif not kinds:
    print("none")
elif "github" in kinds:
    print("github")
elif all(kind == "local" for kind in kinds):
    print("local")
else:
    print("unsupported:" + ", ".join(repr(kind) if isinstance(kind, str)
                                     else "no source kind" for kind in kinds))' \
    "$(herdr plugin list --plugin namedrop --json || true)")
  case "$registration" in
    github)
      progress 'Using the installed GitHub revision; activating this workspace.'
      ;;
    none|local)
      link_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
      # A source checkout checks this machine's platform while it builds. A
      # release archive has no build step, so run its bundled executable before
      # linking to prove the archive was built for this machine. Unlike a shell,
      # execv never reinterprets a foreign executable as a script.
      if [ -f "$link_root/scripts/build.sh" ]; then
        if [ "$output_format" = json ]; then sh "$link_root/scripts/build.sh" >&2; else sh "$link_root/scripts/build.sh"; fi
      else
        case "$(python3 -c 'import os, sys; os.execv(sys.argv[1], sys.argv[1:])' "$link_root/bin/namedrop" --version 2>/dev/null || :)" in
          'namedrop '*) ;;
          *)
            printf '%s\n' \
              "The bundled Namedrop executable does not run on this machine ($(uname -s) $(uname -m)); nothing was linked." \
              'Install from source instead: herdr plugin install ericbust/namedrop --ref main' >&2
            exit 69 ;;
        esac
      fi
      ;;
    unsupported:*)
      printf '%s\n' "Namedrop is registered with Herdr from a source this helper cannot keep (${registration#unsupported:}). Inspect herdr plugin list --plugin namedrop --json, then run sh scripts/setup.sh activate instead of relinking it." >&2
      exit 2
      ;;
    *)
      printf '%s\n' 'Could not read how Namedrop is registered with Herdr, so this run stopped rather than risk replacing a managed installation with a local link. Inspect herdr plugin list --plugin namedrop --json, then run sh scripts/setup.sh activate if it is already installed.' >&2
      exit 2
      ;;
  esac
fi
# Herdr actions are asynchronous. An invocation acknowledgement is not the
# action's result: wait only for the exact returned log ID, never a nearby log.
python3 - "$action" "$link_root" "$HERDR_WORKSPACE_ID" "$output_format" <<'PY'
import json
import os
import shlex
import socket
import subprocess
import sys
import time
import textwrap

action = sys.argv[1]
action_id = "check" if action == "doctor" else action
workspace = sys.argv[3]
human = sys.argv[4] == "human"
progress_stream = sys.stdout if human else sys.stderr
deadline = time.monotonic() + 60
log_id = "not assigned"

def run(arguments):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise RuntimeError("60-second deadline exceeded; inspect the exact action log before retrying")
    result = subprocess.run(["herdr", *arguments], capture_output=True, text=True,
                            timeout=min(10, remaining), check=False)
    if result.returncode:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise RuntimeError(f"Herdr command exited {result.returncode}")
    return result.stdout

def query(arguments):
    return json.loads(run(arguments))["result"]

def invoke():
    # Herdr's action CLI follows UI focus. Its socket API accepts an explicit
    # context, so a background updater must bind the originating pane here.
    pane = os.environ['HERDR_PANE_ID']
    panes = query(['pane', 'list'])['panes']
    if not any(p.get('pane_id') == pane and p.get('workspace_id') == workspace for p in panes):
        raise RuntimeError('The originating pane is no longer in this workspace; reopen setup')
    request = {'id': 'namedrop-setup-' + str(os.getpid()), 'method': 'plugin.action.invoke',
               'params': {'plugin_id': 'namedrop', 'action_id': action_id,
                          'context': {'workspace_id': workspace, 'focused_pane_id': pane,
                                      'invocation_source': 'cli'}}}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(max(.01, min(10, deadline - time.monotonic())))
        connection.connect(os.environ['HERDR_SOCKET_PATH'])
        connection.sendall((json.dumps(request) + '\n').encode())
        with connection.makefile('rb') as stream:
            response = json.loads(stream.readline(1024 * 1024))
    if response.get('id') != request['id'] or 'error' in response:
        raise RuntimeError('Herdr did not accept the bound setup action: ' + str(response.get('error')))
    return response['result']

def safe(value):
    return "".join(c if c.isprintable() else ascii(c)[1:-1] for c in str(value))

def step(value, prefix="  • "):
    if str(value).startswith(("codex ", "herdr ", "claude ")):
        print("    " + safe(value))
        return
    print(textwrap.fill(safe(value), width=92, initial_indent=prefix, subsequent_indent="    "))

def show_report(payload):
    if payload.get("error"):
        print("Error: " + safe(payload["error"]["message"]), file=sys.stderr)
        return
    data = payload["data"]
    if action == "doctor":
        print({"healthy": "Namedrop checks passed.", "needs_attention": "Namedrop needs attention.",
               "unavailable": "Namedrop could not complete its health check."}[data["status"]])
        counts = [(data.get(key, 0), noun) for key, noun in
                  (("available_agents", "agent"), ("available_terminals", "terminal"), ("waiting_messages", "message"))]
        agents, terminals, waiting = [f"{n} {noun}{'' if n == 1 else 's'}" for n, noun in counts]
        print(f"{agents}, {terminals} available; {waiting} waiting.")
        for issue in data.get("issues", []):
            print(safe(issue["code"]) + ": " + safe(issue["summary"]))
            print("  " + safe(issue["explanation"]))
            diagnostic = issue.get("diagnostic", {})
            targets = [f"{safe(k)}={safe(v)}" for k, v in diagnostic.items()
                       if v is not None and (k.endswith("_id") or k in ("runtime", "path"))]
            if targets:
                print("  Target: " + ", ".join(targets))
            repair = issue["repair"]
            print("  " + safe(repair["prompt"]))
            if repair.get("command") and all(arg.isprintable() for arg in repair["command"]):
                print("  Command: " + shlex.join(repair["command"]))
            for instruction in diagnostic.get("steps", []):
                print("  " + safe(instruction))
        print("A health check does not verify loaded hooks; confirm a request and reply between agents.")
        return
    print("\nStatus")
    print("  Namedrop activated in space " + safe(data["space_id"]) + ".")
    for runtime, package in data["integration_packages"].items():
        print("  " + safe(runtime) + ": " + ("installed" if package.get("installed") else "needs attention"))
        if package.get("error"):
            step(package["error"])
    shortcuts = data.get("shortcuts", {})
    if shortcuts.get("error"):
        step("Shortcuts need attention: " + safe(shortcuts["error"]))
    for binding in shortcuts.get("bindings", []):
        if binding.get("error"):
            step(safe(binding["label"]) + " shortcut needs attention: " + safe(binding["error"]))
        elif binding.get("key"):
            print("  " + safe(binding["label"]) + " shortcut: " + safe(binding["key"]))
    drain = data.get("drain", {})
    if drain.get("error"):
        print("\nNeeds attention")
        step("Delivery error: " + safe(drain["error"]))
    for state in ("held", "failed", "expired", "requeued", "submitted", "terminal_running", "terminal_waiting"):
        if drain.get(state):
            step(f"Deliveries {state.replace('_', ' ')}: {len(drain[state])}. Run namedrop doctor for details.")
    live_update = data.get("runtime", {}).get("live_update") and all(
            package.get("installed") for package in data["integration_packages"].values()) and all(
            hook.get("status") not in ("setup_required", "setup_failed")
            for hook in data.get("agent_integration_hook_activation", {}).values())
    if live_update:
        print("\n  Your current agents can keep working.")
        commands = [instruction for hook in data.get("agent_integration_hook_activation", {}).values()
                    for instruction in hook.get("steps", []) if instruction.startswith("codex ")]
        if commands:
            print("  For new Codex sessions, refresh the installed plugin:")
            for instruction in commands:
                step(instruction)
        else:
            print("  New sessions load the updated instructions.")
            if "codex" in data.get("agent_integration_hook_activation", {}):
                step("Start a new Codex session and review Namedrop in /hooks if prompted.")
        if data.get("runtime", {}).get("codex_migration_pending"):
            step("After Codex setup, reopen Activate Namedrop before starting fresh Codex sessions. This completes the one-time migration.")
        return
    for line in native.splitlines():
        if line.startswith(("codex:", "claude:")):
            step("Herdr " + line)
    print("\nNext steps — for the runtimes you use")
    step("Start fresh agent sessions after setup. Installation alone does not verify loaded hooks; confirm a request and reply between agents.")
    for runtime, hook in data.get("agent_integration_hook_activation", {}).items():
        steps = hook.get("steps", [])
        if steps:
            print("  " + safe(runtime))
            for instruction in steps:
                step(instruction, "    • ")
    if data.get("runtime", {}).get("codex_migration_pending"):
        step("After Codex setup, reopen Activate Namedrop before starting fresh Codex sessions. This completes the one-time migration.")
    for instruction in data.get("setup_steps", []):
        step(instruction)
    print("  Native hooks, if setup is needed (choose your runtime):")
    print("    herdr integration install codex\n    herdr integration install claude")
    command = data.get("shell_command")
    if command:
        step(command["next_step"])
        print("  For this terminal: " + safe(command["current_shell"]))
    print("\nDetails")
    print("  State: " + safe(data["state_directory"]))
    if command:
        print("  Command: " + safe(command["path"]))

try:
    if action == "activate":
        print("Activate Namedrop\n  Checking integrations and activating…", file=progress_stream, flush=True)
        native = run(["integration", "status"])
        if not human:
            sys.stderr.write(native)
        if sys.argv[2]:
            linked = run(["plugin", "link", sys.argv[2], "--enabled"])
            if not human:
                sys.stderr.write(linked)
    invoked = invoke()
    if invoked.get("type") != "plugin_action_invoked":
        raise ValueError("unexpected action acknowledgement")
    log_id = invoked["log"]["log_id"]
    if not isinstance(log_id, str) or not log_id:
        raise ValueError("missing action log ID")
    while True:
        response = query(["plugin", "log", "list", "--plugin", "namedrop", "--limit", "100"])
        if response.get("type") != "plugin_log_list" or not isinstance(response.get("logs"), list):
            raise ValueError("unexpected action log response")
        matches = [entry for entry in response["logs"] if entry.get("log_id") == log_id]
        if len(matches) > 1:
            raise ValueError("duplicate action log ID")
        if matches and matches[0].get("status") in ("succeeded", "failed", "cancelled"):
            result = matches[0]
            if result.get("plugin_id") != "namedrop" or result.get("action_id") != action_id:
                raise ValueError("action log identity does not match")
            code = result.get("exit_code")
            if type(code) is not int or not 0 <= code <= 255:
                raise ValueError("completed action has no valid exit code")
            for key in ("stdout", "stderr"):
                if not isinstance(result[key], str):
                    raise ValueError("invalid action output")
            sys.stderr.write(result["stderr"])
            if not human:
                sys.stdout.write(result["stdout"])
                sys.stdout.flush()
            if code:
                if human:
                    try:
                        show_report(json.loads(result["stdout"]))
                    except (ValueError, KeyError, TypeError, AttributeError):
                        print(f"Namedrop {action} failed (exit {code}): " + safe(result["stdout"]), file=sys.stderr)
                sys.exit(code)
            payload = json.loads(result["stdout"])
            if result["status"] != "succeeded":
                raise ValueError("action failed without a nonzero exit code")
            if action == "doctor" and payload["data"]["status"] != "healthy":
                raise ValueError("zero-exit doctor did not report healthy")
            if action == "activate":
                data = payload["data"]
                # The action carries this terminal's Herdr context, so a result
                # for another Space would mean the wrong StateNamespace.
                if data.get("space_id") != workspace:
                    raise ValueError(f"activation reported Space {data.get('space_id')!r} instead of "
                                     f"this terminal's {workspace!r}")
                state_directory = data.get("state_directory")
                if not isinstance(state_directory, str) or not state_directory:
                    raise ValueError("activation reported no state directory")
                packages = data["integration_packages"]
                if not isinstance(packages, dict) or not packages:
                    raise ValueError("activation has no integration installation report")
                failed = [name for name, package in packages.items() if package.get("installed") is not True]
                if failed:
                    if human:
                        show_report(payload)
                    print("Namedrop setup needs attention for: " + ", ".join(failed) +
                          ". Resolve the reported conflicts before retrying.", file=sys.stderr)
                    sys.exit(1)
            if human:
                show_report(payload)
            sys.exit(0)
        time.sleep(min(0.25, max(0, deadline - time.monotonic())))
except (OSError, ValueError, KeyError, TypeError, AttributeError, RuntimeError, subprocess.TimeoutExpired) as error:
    print(f"Namedrop could not verify {action} action {log_id}: {error}. "
          "Inspect herdr plugin log list --plugin namedrop --limit 100; do not assume the action failed or retry automatically.",
          file=sys.stderr)
    sys.exit(2)
PY
