"""Human-only wrapper around Herdr's managed reinstall and Namedrop activation."""

import json
import fcntl
import hashlib
import os
from pathlib import Path
import re
import select
import shlex
import sqlite3
import subprocess
import sys
import tempfile

SCHEMA_VERSION = 5


def prepare_target(before, directory):
    """Inspect the same ref native Herdr will install, without executing its code."""
    source = before['source']
    remote = f"https://github.com/{source['owner']}/{source['repo']}.git"
    checkout = directory / 'inspect'
    checkout.mkdir()
    def git(*args):
        result = subprocess.run(['git', '-C', str(checkout), *args],
                                capture_output=True, text=True, timeout=120)
        require(result.returncode == 0, 'Could not inspect the update source: ' + result.stderr.strip())
        return result.stdout.strip()
    git('init', '--quiet')
    git('fetch', '--quiet', '--depth', '1', remote, source['requested_ref'])
    revision = git('rev-parse', 'FETCH_HEAD^{commit}')
    require(re.fullmatch(r'[0-9a-f]{40,64}', revision), 'Update source returned an invalid revision.')
    subdir = Path(before['plugin_root']).resolve().relative_to(Path(source['managed_path']).resolve())
    prefix = subdir.as_posix() + '/' if subdir.parts else ''
    candidate = None
    live = False
    runtime = Path(os.environ['HERDR_PLUGIN_STATE_DIR']) / 'runtime'
    try:
        from runtime import contract, current, live_compatible, LAUNCHER
        (directory / 'runtime.json').write_text(git('show', f'{revision}:{prefix}runtime.json'))
        candidate = contract(directory)
        active = current(runtime)
        manifest = directory / 'herdr-plugin.toml'
        manifest.write_text(git('show', f'{revision}:{prefix}herdr-plugin.toml'))
        launcher = runtime / 'namedrop'
        checked = subprocess.run([str(launcher), '--validate-update-manifest', str(manifest)],
                                 capture_output=True, text=True, timeout=20)
        live = (active is not None and live_compatible(contract(runtime / active), candidate)
                and not launcher.is_symlink() and launcher.read_text() == LAUNCHER
                and checked.returncode == 0
                and git('show', f'{revision}:{prefix}scripts/run.sh') ==
                    (Path(__file__).parent / 'run.sh').read_text().strip())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        # Older/breaking releases still use the non-destructive readiness flow.
        live = False
    hooks = directory / 'git-hooks'
    hooks.mkdir()
    guard = hooks / 'post-checkout'
    guard.write_text('''#!/bin/sh
set -eu
if [ "$(git config --get remote.origin.url || :)" = "$NAMEDROP_UPDATE_SOURCE" ]; then
  if [ "$(git rev-parse HEAD)" != "$NAMEDROP_UPDATE_REVISION" ]; then
    printf '%s\\n' 'Namedrop update source changed after inspection. Nothing was replaced; reopen Update Namedrop.' >&2
    exit 1
  fi
fi
''')
    guard.chmod(0o700)
    environment = dict(os.environ)
    count = int(environment.get('GIT_CONFIG_COUNT', '0'))
    require(0 <= count < 1000, 'Invalid inherited Git configuration count.')
    environment.update({f'GIT_CONFIG_KEY_{count}': 'core.hooksPath',
                        f'GIT_CONFIG_VALUE_{count}': str(hooks), 'GIT_CONFIG_COUNT': str(count + 1),
                        'NAMEDROP_UPDATE_REVISION': revision, 'NAMEDROP_UPDATE_SOURCE': remote})
    return dict(revision=revision, contract=candidate, live=live, environment=environment)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def built_executable(root):
    """Source builds and archives stage bin/namedrop; older Mac checkouts have only Cargo's output."""
    staged = root / 'bin/namedrop'
    return staged if staged.is_file() else root / 'target/aarch64-apple-darwin/release/namedrop'


def structured(*args, socket=None):
    environment = dict(os.environ)
    if socket is not None:
        environment['HERDR_SOCKET_PATH'] = socket
    result = subprocess.run(["herdr", *args], env=environment, capture_output=True, text=True, timeout=20)
    require(result.returncode == 0, f"herdr {shlex.join(args)} failed: {result.stderr.strip()}")
    value = json.loads(result.stdout)
    require(isinstance(value, dict) and "error" not in value, "Herdr returned an invalid response.")
    return value


def registration():
    result = structured("plugin", "list", "--plugin", "namedrop", "--json").get("result", {})
    require(isinstance(result, dict), "Invalid plugin registry response.")
    plugins = result.get("plugins")
    require(result.get("type") == "plugin_list" and isinstance(plugins, list)
            and len(plugins) == 1, "Expected exactly one installed Namedrop registration.")
    plugin = plugins[0]
    require(isinstance(plugin, dict) and plugin.get("plugin_id") == "namedrop", "Invalid Namedrop registration.")
    source = plugin.get("source", {})
    require(isinstance(source, dict) and source.get("kind") == "github",
            "Only GitHub-managed installations support this update. For local installs, update your checkout and run its setup; for archives, follow the manual upgrade steps in docs/operations.md. No source was converted.")
    for key in ("owner", "repo"):
        require(isinstance(source.get(key), str) and re.fullmatch(r"[A-Za-z0-9_.-]+", source[key])
                and source[key] not in (".", "..") and not source[key].startswith("-"), f"Invalid installed GitHub {key}.")
    ref = source.get("requested_ref")
    require(isinstance(ref, str) and ref and not ref.startswith("-")
            and all(ord(c) >= 32 and ord(c) != 127 for c in ref),
            "The registration has no safe explicit requested_ref. Resolve the intended pin with Herdr before updating.")
    root = plugin.get("plugin_root")
    require(isinstance(root, str) and Path(root).is_absolute() and Path(root).is_dir(), "Installed root is missing or invalid.")
    managed = source.get("managed_path")
    require(isinstance(managed, str) and Path(managed).is_absolute() and Path(managed).is_dir()
            and Path(root).resolve().is_relative_to(Path(managed).resolve()), "Managed checkout path is missing or inconsistent.")
    subdir = Path(root).resolve().relative_to(Path(managed).resolve()).as_posix()
    require(subdir == "." or re.fullmatch(r"[A-Za-z0-9_./-]+", subdir), "Invalid installed subdirectory.")
    require(source.get("subdir") in (None, "" if subdir == "." else subdir),
            "Installed subdirectory metadata disagrees with the registered root; inspect the registration before updating.")
    require(isinstance(source.get("resolved_commit"), str)
            and re.fullmatch(r"[0-9a-fA-F]{40,64}", source["resolved_commit"]), "Installed revision is unknown.")
    return plugin


def origin(plugin):
    source = plugin["source"]
    subdir = Path(plugin["plugin_root"]).resolve().relative_to(Path(source["managed_path"]).resolve())
    return source["owner"] + "/" + source["repo"] + ("/" + subdir.as_posix() if subdir.parts else "")


def durable_state(panes, other_servers=False):
    sessions = [p.get("agent_session") for p in panes if p.get("agent") or p.get("agent_session")]
    identified = not other_servers and all(isinstance(s, dict) and isinstance(s.get("value"), str) and s["value"] for s in sessions)
    live = {(p['_socket'], p['agent_session']['value']) for p in panes
            if p.get('agent') or p.get('agent_session')} if identified else set()

    def session_gone(socket, session):
        # Only conclude absence after inspecting every running Herdr server.
        return identified and bool(socket) and bool(session) and (socket, session) not in live

    path = Path(os.environ["HERDR_PLUGIN_STATE_DIR"]) / "namedrop.sqlite3"
    require(path.is_file(), "Cannot verify durable collaboration state. Activate/check Namedrop before updating.")
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=2) as db:
        db.execute('BEGIN')
        schema = db.execute("PRAGMA user_version").fetchone()[0]
        require(schema in (3, 4, SCHEMA_VERSION),
                "Unknown collaboration database schema; use the matching Namedrop release to inspect unfinished work.")
        deliveries = db.execute("""SELECT session_socket_id, worktree_id, delivery_id, state FROM mailbox_deliveries d
            WHERE state NOT IN ('settled', 'failed', 'cancelled') OR
            (state = 'settled' AND NOT EXISTS (
                SELECT 1 FROM delivery_attempts a JOIN delivery_dispositions p
                USING (session_socket_id, worktree_id, attempt_id)
                WHERE a.session_socket_id=d.session_socket_id AND a.worktree_id=d.worktree_id
                AND a.delivery_id=d.delivery_id))""").fetchall()
        historical = 0
        historical_deliveries = set()
        for socket, space, delivery, state in deliveries:
            if state != "settled":
                continue
            attempt = db.execute("""SELECT a.agent_session_id, s.state, s.active
                FROM delivery_attempts a LEFT JOIN delivery_attempt_state s
                USING (session_socket_id, worktree_id, attempt_id, address_id)
                WHERE a.session_socket_id=? AND a.worktree_id=? AND a.delivery_id=?
                ORDER BY a.rowid DESC LIMIT 1""", (socket, space, delivery)).fetchone()
            if attempt and attempt[1:] == ("settled", 0) and session_gone(socket, attempt[0]):
                historical += 1
                historical_deliveries.add((socket, space, delivery))
        unfinished = len(deliveries) - historical
        joins = db.execute("""SELECT m.session_socket_id, m.author_session_snapshot, m.worktree_id, m.message_id FROM collaboration_messages m
            JOIN pane_addresses a ON a.session_socket_id=m.session_socket_id
                 AND a.worktree_id=m.worktree_id AND a.address_id=m.author_address_id
            WHERE m.author_kind='agent_session' AND m.grammar_version='delegated/v1'
            AND a.retired=0 AND a.collaboration_disabled=0 AND m.author_session_snapshot IS NOT NULL
            AND (SELECT count(*) FROM message_recipients r
                 WHERE r.session_socket_id=m.session_socket_id AND r.worktree_id=m.worktree_id
                 AND r.message_id=m.message_id) > 1
            AND NOT EXISTS (SELECT 1 FROM collaboration_messages result
                 WHERE result.session_socket_id=m.session_socket_id AND result.worktree_id=m.worktree_id
                 AND result.in_reply_to=m.message_id AND result.grammar_version='result-set/v1')""").fetchall()
        historical_joins = sum(session_gone(socket, session) for socket, session, _, _ in joins)
        pending_join = len(joins) - historical_joins
        # Owner loss cannot resolve open work or an unfinished report delivery.
        # A terminal/disposed delivery permits updating; saved text remains and
        # this does not certify that the human saw it. An open handoff whose
        # lead is retired can never resolve; once a human acknowledges it, only
        # its unfinished deliveries, counted above, still block.
        coordinations = db.execute("""SELECT session_socket_id,worktree_id,coordination_id,deliverable,outcome
            FROM coordinations c WHERE (outcome='open' AND NOT (
                EXISTS (SELECT 1 FROM pane_addresses p
                    WHERE p.session_socket_id=c.session_socket_id AND p.worktree_id=c.worktree_id
                    AND p.address_id=c.owner_address_id AND p.retired=1)
                AND EXISTS (SELECT 1 FROM attention_items a JOIN transition_facts f
                    ON f.session_socket_id=a.session_socket_id AND f.worktree_id=a.worktree_id
                    AND f.entity_id=a.attention_item_id
                    WHERE a.session_socket_id=c.session_socket_id AND a.worktree_id=c.worktree_id
                    AND a.source_kind='coordination' AND a.source_id=c.coordination_id
                    AND f.entity_kind='attention_item' AND f.transition_kind='acknowledged'
                    AND f.actor_kind='human')))
            OR (report IS NOT NULL AND NOT EXISTS (
                SELECT 1 FROM mailbox_deliveries d
                WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                AND d.message_id=c.report_message_id AND (d.state IN ('failed','cancelled') OR
                    (d.state='settled' AND EXISTS (
                        SELECT 1 FROM delivery_attempts a JOIN delivery_dispositions p
                        USING (session_socket_id,worktree_id,attempt_id)
                        WHERE a.session_socket_id=d.session_socket_id AND a.worktree_id=d.worktree_id
                        AND a.delivery_id=d.delivery_id)))))""").fetchall() if schema == SCHEMA_VERSION else []
        unfinished += len(coordinations)
        details = []
        for socket, space, delivery, status in deliveries:
            if (socket, space, delivery) in historical_deliveries:
                continue
            target = db.execute("""SELECT r.alias_snapshot, t.title FROM mailbox_deliveries d
                JOIN message_recipients r USING (session_socket_id,worktree_id,message_id,address_id)
                JOIN collaboration_messages m USING (session_socket_id,worktree_id,message_id)
                JOIN collaboration_threads t USING (session_socket_id,worktree_id,thread_id)
                WHERE d.session_socket_id=? AND d.worktree_id=? AND d.delivery_id=?""",
                (socket, space, delivery)).fetchone()
            details.append((socket, space, target[0] if target else delivery,
                            target[1] if target else 'Handoff', status))
        details.extend((socket, space, coordination, deliverable, "coordination:" + outcome)
                       for socket, space, coordination, deliverable, outcome in coordinations)
        terminals = []
        if db.execute("SELECT 1 FROM sqlite_master WHERE name='terminal_executions'").fetchone():
            terminals = db.execute("""SELECT t.session_socket_id, t.terminal_id FROM terminal_executions t
                JOIN delivery_attempt_state s USING (session_socket_id,worktree_id,attempt_id)
                WHERE t.completed_at_ms IS NULL
                AND (t.started_at_ms IS NOT NULL OR s.state NOT IN ('settled','failed','cancelled'))""").fetchall()
        return dict(paused=db.execute("SELECT session_socket_id, worktree_id, paused FROM worktrees ORDER BY 1,2").fetchall(),
                    pause_orders=db.execute('SELECT session_socket_id,worktree_id,max(worktree_order) FROM transition_facts GROUP BY 1,2').fetchall(),
                    unfinished=unfinished, joins=pending_join,
                    details=details, terminals=terminals)


def verify_pauses(previous, setup, allowed_since=None):
    # New setup may migrate the database; never rerun the old delivery preflight.
    path = Path(os.environ["HERDR_PLUGIN_STATE_DIR"]) / "namedrop.sqlite3"
    guidance = "Pause preservation is unverified. Run the new installation's check: " + shlex.join(["sh", str(setup), "check", "--human"]) + "; inspect pause state before resuming work."
    try:
        with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=2) as db:
            db.execute('BEGIN')
            version = db.execute("PRAGMA user_version").fetchone()[0]
            current = db.execute("SELECT session_socket_id, worktree_id, paused FROM worktrees").fetchall()
            allowed = set()
            if allowed_since is not None and version == SCHEMA_VERSION:
                orders = {(socket, space): order for socket, space, order in allowed_since}
                for socket, space, paused in current:
                    latest = db.execute('''SELECT worktree_order,transition_kind,actor_kind FROM transition_facts
                        WHERE session_socket_id=? AND worktree_id=? AND entity_kind='worktree'
                        AND transition_kind IN ('paused','resumed','deactivated')
                        ORDER BY worktree_order DESC LIMIT 1''', (socket, space)).fetchone()
                    if (latest and latest[0] > orders.get((socket, space), 0) and latest[2] == 'human'
                            and latest[1] in (('paused',) if paused else ('resumed', 'deactivated'))):
                        allowed.add((socket, space))
    except sqlite3.Error:
        return guidance
    require(all(row in current or row[:2] in allowed for row in previous),
            "Installation and activation succeeded, but saved pause flags changed or disappeared. Pause verification failed; inspect the new installation's state before resuming work.")
    return "Pause preservation verified." if version == SCHEMA_VERSION else guidance


def session_panes():
    sessions = structured("session", "list", "--json").get("sessions")
    require(isinstance(sessions, list), "Cannot inspect other Herdr sessions.")
    current_socket = os.environ['HERDR_SOCKET_PATH']
    sockets = {current_socket}
    unverified = []
    for session in sessions:
        require(isinstance(session, dict) and isinstance(session.get("running"), bool), "Unknown Herdr session state.")
        if session["running"]:
            if isinstance(session.get('socket_path'), str) and session['socket_path']:
                sockets.add(session['socket_path'])
            else:
                unverified.append("Unknown Herdr session")
    panes = []
    for socket in sorted(sockets):
        try:
            result = structured("pane", "list", socket=socket).get("result", {})
            require(isinstance(result, dict), "Invalid pane list response.")
            items = result.get('panes')
            require(isinstance(items, list), "Cannot inspect panes across workspaces.")
            for pane in items:
                require(isinstance(pane, dict) and isinstance(pane.get('pane_id'), str), "Unknown pane state.")
                pane['_socket'] = socket
            panes.extend(items)
        except (RuntimeError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
            if socket == current_socket:
                raise
            unverified.append(socket)
    return panes, unverified


def preflight(live=False):
    panes, unverified = session_panes()
    current_socket = os.environ['HERDR_SOCKET_PATH']
    require(any(p.get("pane_id") == os.environ["HERDR_PANE_ID"]
                and p.get("workspace_id") == os.environ["HERDR_WORKSPACE_ID"]
                and p['_socket'] == current_socket for p in panes),
            "The source pane/workspace is not present on this server.")
    state = durable_state(panes, bool(unverified))
    state['other_servers'] = unverified
    agents = [p for p in panes if p.get("agent") or p.get("agent_session")
              or (p['_socket'], p.get('terminal_id')) in state['terminals']]
    state['live_update'] = live
    state['session_count'] = sum(bool(p.get('agent') or p.get('agent_session')) for p in panes)
    if live:
        legacy = json.loads((Path(os.environ['HERDR_PLUGIN_STATE_DIR']) / 'runtime/legacy-sessions.json').read_text())
        require(isinstance(legacy, dict) and type(legacy.get('codex_ready')) is bool
                and type(legacy.get('packages_ready')) is bool
                and isinstance(legacy.get('sessions'), list)
                and all(isinstance(item, list) and len(item) == 2
                and all(isinstance(part, str) and part for part in item) for item in legacy['sessions']),
                'Cannot verify sessions from the one-time runtime migration.')
        state['codex_migration_pending'] = not legacy['codex_ready']
        state['activation_pending'] = not legacy['packages_ready']
        agents = [p for p in agents if (p.get('agent') or p.get('agent_session'))
                  and (not isinstance(p.get('agent_session'), dict)
                       or not p['agent_session'].get('value')
                       or [p['_socket'], p['agent_session']['value']] in legacy['sessions']
                       or not legacy['packages_ready']
                       or not legacy['codex_ready'] and is_codex(p))]
    return state, agents


def is_codex(pane):
    agent = pane.get('agent') or (pane.get('agent_session') or {}).get('agent')
    return agent not in ('claude', 'claude-code')


def labels(panes):
    # Labels are cosmetic; unavailable metadata must not hide an open session.
    labels = {}
    sockets = {os.environ['HERDR_SOCKET_PATH']} | {p['_socket'] for p in panes}
    for kind, plural in (("workspace", "workspaces"), ("tab", "tabs")):
        labels[kind] = {}
        for socket in sorted(sockets):
            try:
                items = structured(kind, "list", socket=socket)["result"][plural]
                labels[kind].update({(socket, item[kind + "_id"]): item for item in items})
            except (RuntimeError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
                pass
    return labels


def safe(value):
    return "".join(c if c.isprintable() else " " for c in str(value))


def show_agents(panes, metadata):
    print(f"\n  Open sessions ({len(panes)})")
    for pane in panes:
        workspace = metadata["workspace"].get((pane['_socket'], pane.get("workspace_id")), {})
        tab = metadata["tab"].get((pane['_socket'], pane.get("tab_id")), {})
        name = pane.get("label") or pane.get("title") or pane.get("terminal_title_stripped") or "Unnamed pane"
        session = pane.get("agent_session") or {}
        agent = pane.get("agent") or (session.get("agent") if isinstance(session, dict) else None) or "terminal command"
        line = (f"  - {workspace.get('label') or pane.get('workspace_id', 'Unknown workspace')}"
                f" → tab {tab.get('number') or pane.get('tab_id', '?')} {tab.get('label', '')}"
                f" → {name} ({agent}, {pane.get('agent_status') or 'unknown'}; {pane['pane_id']})")
        if pane['_socket'] != os.environ['HERDR_SOCKET_PATH']:
            line += f" [Herdr: {pane['_socket']}]"
        # Do not let terminal titles inject control sequences into the prompt.
        print(safe(line))


def ready(state, agents):
    return not (agents or state['other_servers'] or
                not state.get('live_update') and (state['unfinished'] or state['joins']))


def confirm_when_ready(before, live=False):
    """Observe work until the user can install; never stop or settle it here."""
    waiting = False
    previous = None
    while True:
        require(registration() == before, "Installed source/ref changed. Reopen Update Namedrop to review it.")
        state, agents = preflight(live)
        if ready(state, agents):
            print("\n  Ready to install.")
            if live:
                print(f"  {state['session_count']} open sessions can keep working. Pending handoffs stay in place.")
            else:
                print("  During installation, keep new agents and handoffs stopped.")
                print("  Finish standalone agents and work in other profiles first.")
            if input("Install update now? [y/N] ").strip().lower() not in ('y', 'yes'):
                return None
            require(registration() == before, "Installed source/ref changed. Reopen Update Namedrop to review it.")
            state, agents = preflight(live)
            if ready(state, agents):
                return state
            print("\n  Work resumed. Installation will wait.")
            waiting = True
            previous = None
        # Reprint only when blockers change, not on every readiness poll.
        current = ([{k: pane.get(k) for k in ('_socket', 'pane_id', 'workspace_id', 'tab_id',
                    'agent', 'agent_session', 'agent_status', 'label', 'title', 'terminal_title_stripped')}
                    for pane in agents], state['details'], state['joins'], state['other_servers'])
        changed = current != previous
        if changed:
            metadata = labels(agents)
            print("\n  Waiting for current work. Your panes and handoffs stay as they are.")
            if agents:
                show_agents(agents, metadata)
            if not live and (state['unfinished'] or state['joins']):
                print(f"\n  Handoffs: {state['unfinished']} unfinished; {state['joins']} combined results pending.")
                for socket, space, recipient, title, status in state['details']:
                    workspace = metadata['workspace'].get((socket, space), {}).get('label') or space
                    print(f"  - {safe(workspace)} / @{safe(recipient)}: {safe(title)} ({status})")
                print("  Held or uncertain handoffs need review in handoff status/inbox.")
            for server in state['other_servers']:
                print("  Could not check another Herdr session: " + safe(server))
            if live:
                if state.get('activation_pending'):
                    print("\n  Complete Activate Namedrop before starting fresh sessions; initial setup has not finished.")
                elif state.get('codex_migration_pending'):
                    print("\n  Finish Codex's one-time setup, then run Activate Namedrop again before starting fresh Codex sessions.")
                    print("  Its cached plugin has not yet been verified for seamless updates.")
                else:
                    print("\n  These sessions started before seamless updates were installed.")
                    print("  Finish them when convenient and exit to their shells. New sessions can stay open.")
            else:
                print("\n  Keep working. When finished, exit agents to their shells; keep the panes open.")
                print("  This release needs a session refresh. Nothing is stopped automatically.")
            previous = current
        if not waiting:
            print("\n  1) Wait here\n  2) Later")
            while True:
                choice = input("Choose [1/2, default 2]: ").strip() or '2'
                if choice in ('1', '2'):
                    break
                print("Choose 1 or 2, or press Enter for later.")
            if choice == '2':
                return None
            waiting = True
        if changed:
            print("  Leave this tab open; it will ask before installing. Press Enter to do this later.", flush=True)
        if select.select([sys.stdin], [], [], 2)[0]:
            sys.stdin.readline()
            return None


def main():
    require(sys.version_info >= (3, 9), "Update Namedrop requires Python 3.9 or newer. Install it and reopen this tab.")
    require(sys.argv[1:] in ([], ["--open"]), "Usage: sh scripts/update.sh [--open]")
    for key in ("HERDR_SOCKET_PATH", "HERDR_WORKSPACE_ID", "HERDR_PANE_ID", "HERDR_PLUGIN_STATE_DIR"):
        require(os.environ.get(key), f"Missing {key}; open Update Namedrop from Herdr's workspace actions.")
    require(Path(os.environ["HERDR_PLUGIN_STATE_DIR"]).is_absolute(), "State directory must be absolute.")
    if sys.argv[1:] == ["--open"]:
        result = subprocess.run(["herdr", "plugin", "pane", "open", "--plugin", "namedrop",
                                 "--entrypoint", "update", "--placement", "tab",
                                 "--workspace", os.environ["HERDR_WORKSPACE_ID"], "--focus"])
        return result.returncode
    require(sys.stdin.isatty() and sys.stdout.isatty(), "A human terminal is required; piped/noninteractive updates do not install anything.")
    before = registration()
    root = Path(before["plugin_root"]).resolve()
    managed = Path(before["source"]["managed_path"]).resolve()
    require(Path(__file__).resolve().parent.parent == root,
            "This updater is not from the currently registered root. Reopen Update Namedrop from Herdr.")
    state = Path(os.environ["HERDR_PLUGIN_STATE_DIR"]).resolve()
    require(not state.is_relative_to(managed), "Durable state is inside the replaceable checkout; move it through supported configuration before updating.")
    for key in ("HOME", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME"):
        if os.environ.get(key):
            require(Path(os.environ[key]).is_absolute() and not Path(os.environ[key]).resolve().is_relative_to(managed),
                    f"{key} is inside the replaceable checkout or relative. Reopen the updater with a stable external profile.")
    install = ["herdr", "plugin", "install", origin(before), "--ref", before["source"]["requested_ref"], "--yes"]
    recover = ["herdr", "plugin", "install", origin(before), "--ref", before["source"]["resolved_commit"]]
    print("Update Namedrop\n==============")
    print(f"  Source   {origin(before)} @ {safe(before['source']['requested_ref'])}")
    print(f"  Installed {before['source']['resolved_commit'][:12]}")
    runtime = state / 'runtime'
    binary = built_executable(root)
    try:
        activated = (json.loads((runtime/'legacy-sessions.json').read_text()).get('packages_ready') is True
                     and json.loads((runtime/'activation.json').read_text()).get('revision') ==
                     (runtime/'current').resolve().name
                     and not (runtime/'migration.json').exists())
    except (OSError, ValueError, AttributeError):
        activated = False
    selected_matches = (activated and binary.is_file() and (runtime/'current/namedrop').is_file()
                        and hashlib.sha256(binary.read_bytes()).digest() ==
                            hashlib.sha256((runtime/'current/namedrop').read_bytes()).digest())
    try:
        from runtime import contract, current
        active = current(runtime)
        active_schema = contract(runtime / active)['state_schema'] if active else None
        print('  Active    ' + ('matches installed version' if selected_matches else
              f'schema {active_schema}; installed version awaits activation' if active_schema else
              'not activated'))
    except (OSError, ValueError, RuntimeError):
        print('  Active    could not verify; setup will inspect it')
    if (before['source']['requested_ref'].lower() == before['source']['resolved_commit'].lower()
            and (selected_matches or not (runtime/'namedrop').exists())):
        print("\nAlready at your pinned revision. Nothing to install; keep working.")
        return 0
    print("\n[1/3] Check update", flush=True)
    # The Python program is already loaded. Neither it nor any child uses the
    # managed checkout as cwd while native Herdr removes and replaces that tree.
    with tempfile.TemporaryDirectory(prefix="namedrop-update-", dir=state) as working_directory, \
            os.fdopen(os.open(state / 'update.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600), 'r+') as update_lock:
        try:
            fcntl.flock(update_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise RuntimeError('Another update is already open. Continue in that update tab.')
        target = prepare_target(before, Path(working_directory))
        if target['revision'] == before['source']['resolved_commit'] and selected_matches:
            print('\nAlready up to date. Keep working; nothing to install.')
            return 0
        print(f"  Update   {target['revision'][:12]}")
        print('  Sessions and handoffs can continue; any supported schema migration runs automatically.' if target['live'] else
              '  This release needs a session refresh; the updater will wait for work to finish.')
        try:
            reviewed = confirm_when_ready(before, target['live'])
        except (EOFError, KeyboardInterrupt):
            reviewed = None
        if reviewed is None:
            print("\nUpdate postponed. Your work is unchanged; nothing installed.")
            return 0
        require(registration() == before, "Registration changed; reopen Update Namedrop to review it.")
        os.chdir(working_directory)
        log = state / 'last-update.log'
        if target['revision'] == before['source']['resolved_commit']:
            print("\n[2/3] Branch already installed — finishing activation.", flush=True)
            log.write_text('Installed branch matches the inspected target; no download or rebuild needed.\n')
        else:
            print("\n[2/3] Install — downloading and building; this may take a few minutes…", flush=True)
            with log.open('w') as output:
                output.write(shlex.join(install) + '\n')
                output.flush()
                result = subprocess.run(install, env=target['environment'], stdout=output, stderr=subprocess.STDOUT)
            require(result.returncode == 0,
                    f"Reinstall failed (exit {result.returncode}); activation was not attempted.\n  Build log: {log}\n  Recovery: {shlex.join(recover)}\n  Recovery pins the old commit; no source rollback was attempted.")
        after = registration()
        require(origin(after) == origin(before) and after["source"]["requested_ref"] == before["source"]["requested_ref"],
                "Reinstall returned a different source/ref; activation was not attempted. Inspect the registration and recover the intended pin manually.")
        require(after['source']['resolved_commit'] == target['revision'],
                'Installed revision differs from the inspected update. Activation was not attempted; the previous stable runtime is unchanged.')
        setup = Path(after["plugin_root"]) / "scripts" / "setup.sh"
        require(setup.is_file(), f"Reinstalled root has no setup script: {setup}. Inspect plugin logs; activation was not attempted.")
        print(f"  Installed {after['source']['resolved_commit'][:12]}", flush=True)
        print("\n[3/3] Activate", flush=True)
        result = subprocess.run(["sh", str(setup), "activate", "--human"], capture_output=True, text=True)
        with log.open('a') as output:
            output.write(result.stdout + result.stderr)
        print(result.stdout.removeprefix("Activate Namedrop\n  Checking integrations and activating…\n"), end='')
        if result.stderr:
            print(result.stderr, end='', file=sys.stderr)
        require(result.returncode == 0,
                f"Reinstall succeeded but activation did not complete (exit {result.returncode}).\n  Log: {log}\n  Inspect: herdr plugin log list --plugin namedrop --limit 20\n  After resolving the cause: {shlex.join(['sh', str(setup), 'activate', '--human'])}\n  Recovery: {shlex.join(recover)}\n  Recovery pins the old commit; no source rollback was attempted.")
        pause_result = verify_pauses(reviewed['paused'], setup, reviewed['pause_orders'] if target['live'] else None)
        os.chdir(Path(working_directory).parent)
    print("\nUpdate complete. " + pause_result)
    print("Keep working in your current sessions." if target['live'] else
          "Continue in your existing panes; start fresh agents there when ready.")
    return 0


if __name__ == "__main__":
    code = 2
    try:
        code = main()
    except (RuntimeError, OSError, ValueError, KeyError, TypeError, sqlite3.Error, subprocess.SubprocessError) as error:
        print(f"\nUpdate stopped\n  {error}", file=sys.stderr)
        print("\nInspect: herdr plugin list --plugin namedrop --json\nHelp: Namedrop README → upgrade guide → docs/operations.md#manual-upgrade", file=sys.stderr)
    except KeyboardInterrupt:
        print("Update interrupted. Inspect registration and plugin logs before retrying; an install already started may still be running.", file=sys.stderr)
        code = 130
    finally:
        if sys.argv[1:] != ["--open"] and sys.stdin.isatty() and sys.stdout.isatty():
            try:
                input("Press Enter to close this update tab… ")
            except (EOFError, KeyboardInterrupt):
                pass
    sys.exit(code)
