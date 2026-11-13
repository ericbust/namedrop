"""Test a fresh Namedrop install in its own Herdr profile and server."""

import sys

sys.dont_write_bytecode = True

import argparse
from collections import namedtuple
import contextlib
import datetime
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rex_lane
import source as sources

KIT = Path(__file__).resolve().parent.parent
VERSION = 1
AGENTS = ("claude", "codex")
REQUIRED_TOOLS = ("herdr", "git", "cargo")
OPTIONAL_TOOLS = ("node", "rustc", "cc")
TOOLS = REQUIRED_TOOLS + AGENTS + OPTIONAL_TOOLS + ("python3",)
VERSION_MANAGERS = frozenset(("mise",))
NAME_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
SHELL_PS1 = "[testbed {}] \\w $ "

PaneSpec = namedtuple("PaneSpec", "alias kind variable default placements")
PANES = (
    PaneSpec("shell", "terminal", None, None, ()),
    PaneSpec("claude", "agent", "TESTBED_CLAUDE_CMD", "claude", (("shell", "right"),)),
    PaneSpec("codex", "agent", "TESTBED_CODEX_CMD", "codex --no-daemon",
             (("claude", "down"), ("shell", "right"))),
)


def fail(message):
    raise RuntimeError(message)


def say(message):
    print(message, flush=True)


def write_if_changed(path, content):
    if path.is_symlink():
        fail("refusing to write a symlink: " + str(path))
    if not path.exists() or path.read_text() != content:
        temporary = path.with_name(path.name + ".new")
        if temporary.exists() or temporary.is_symlink():
            fail("remove stale temporary file after inspecting it: " + str(temporary))
        temporary.write_text(content)
        temporary.replace(path)


def copy_seed(seed, target):
    target.mkdir(parents=True, exist_ok=True)
    if any(target.iterdir()):
        raise ValueError("seed target must be empty: " + str(target))
    for item in sorted(seed.rglob("*")):
        if item.is_file() and "__pycache__" not in item.parts and item.name != ".DS_Store":
            destination = target / item.relative_to(seed)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(item, destination)
            destination.chmod(0o644)


# gpt-6.1-sol answers only 5 requests per 10 minutes on a Pro account, fewer than one run
# sends; gpt-6-sol took 15 in a row. Pinning also keeps runs alike when Codex's default moves.
CODEX_SETTINGS = (("approvals_reviewer", "auto_review"), ("model", "gpt-6-sol"),
                  ("model_reasoning_effort", "high"))


def ensure_codex_settings(config):
    if config.is_symlink():
        fail("Codex config must not be a symlink: " + str(config))
    content = config.read_bytes() if config.exists() else b""
    # Root TOML keys must precede the tables activation appends.
    top_level = re.split(rb"(?m)^[ \t]*\[", content, maxsplit=1)[0]
    missing = b""
    for key, value in CODEX_SETTINGS:
        name = re.escape(key.encode())
        if not re.search(rb'''(?m)^[ \t]*(?:%s|"%s"|'%s')[ \t]*=''' % (name, name, name), top_level):
            missing += '{} = "{}"\n'.format(key, value).encode()
    if missing:
        write_if_changed(config, (missing + content).decode("utf-8"))


def clean_environment(base, socket, toolchain):
    return {
        "PATH": ":".join([str(base / "bin"), str(base / "home/.local/bin"), "/opt/homebrew/bin",
                          "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]),
        "TERM": "xterm-256color", "LANG": "en_US.UTF-8" if sys.platform == "darwin" else "C.UTF-8",
        "USER": os.environ.get("USER", str(os.getuid())), "SHELL": "/bin/bash",
        "HOME": str(base / "home"), "XDG_CONFIG_HOME": str(base / "config"),
        "XDG_STATE_HOME": str(base / "state"), "XDG_DATA_HOME": str(base / "data"),
        "XDG_CACHE_HOME": str(base / "cache"), "XDG_RUNTIME_DIR": str(base / "run"),
        "XDG_CONFIG_DIRS": str(base / "config-dirs"), "XDG_DATA_DIRS": str(base / "data-dirs"),
        "CODEX_HOME": str(base / "codex"),
        "HERDR_SOCKET_PATH": str(socket),
        "HERDR_CONFIG_PATH": str(base / "config/herdr/config.toml"),
        "RUSTUP_HOME": toolchain["RUSTUP_HOME"], "CARGO_HOME": toolchain["CARGO_HOME"],
    }


CLAUDE_TOKEN_FILE_VARIABLE = "NAMEDROP_TESTBED_CLAUDE_TOKEN_FILE"
# Claude Code's own map of an OAuth profile's organization_type to its subscription type.
CLAUDE_PLANS = {"claude_max": "max", "claude_pro": "pro", "claude_team": "team",
                "claude_enterprise": "enterprise"}
PERSONAL_PLANS = frozenset(("max", "pro"))
OAUTH_PROFILE_URL = "https://api.anthropic.com/api/oauth/profile"
TOKEN_PATTERN = re.compile(r"^[A-Za-z0-9._~+/=-]+$")
# A token from `claude setup-token` holds only the user:inference scope, and the profile endpoint
# refuses it with 403 permission_error. A revoked or mistyped token gets 401 instead.
SCOPE_REFUSAL = 403
REJECTED_TOKEN = 401

ClaudeLogin = namedtuple("ClaudeLogin", "token_file default_login org")
"""How a run signs Claude Code in. default_login allows the machine's own login when token_file
is missing; org is the organization --claude-org allows."""


def claude_login(default_login=False, org=None):
    named = os.environ.get(CLAUDE_TOKEN_FILE_VARIABLE)
    token_file = Path(named).expanduser() if named else testbed_home() / "claude-oauth-token"
    return ClaudeLogin(token_file.absolute(), default_login, org)


def missing_token(token_file):
    return ("no Claude Code token at {0}. The testbed signs Claude Code in with a token, never "
            "with this machine's default login. Create one under the account to bill:\n"
            "  claude setup-token\n"
            "  mkdir -p {1} && (umask 077 && read -rs token && printf '%s\\n' \"$token\" > {0})\n"
            "  chmod 600 {0}\n"
            "Paste the token at the silent prompt. {2} names another file. To use the default "
            "login instead, pass --claude-default-login.").format(
                shlex.quote(str(token_file)), shlex.quote(str(token_file.parent)),
                CLAUDE_TOKEN_FILE_VARIABLE)


def claude_auth(login):
    """"token" when the token file signs Claude Code in, or "default" for the machine's login."""
    if login.token_file.exists():
        mode = login.token_file.stat()
        if not login.token_file.is_file() or not mode.st_size:
            fail("the Claude Code token file is empty or not a file: " + str(login.token_file))
        if mode.st_mode & 0o077:
            fail("other users can read the Claude Code token; run chmod 600 " +
                 shlex.quote(str(login.token_file)))
        read_token(login.token_file)
        return "token"
    if login.default_login:
        return "default"
    fail(missing_token(login.token_file))


def token_wrapper(token_file, program):
    """A `claude` that reads the token inside the launch, so it is never on a command line, in
    env.sh, or in the profile's state."""
    return ("#!/bin/sh\n"
            "token_file={0}\n"
            'if [ ! -s "$token_file" ]; then\n'
            '  echo "testbed: no Claude Code token at $token_file" >&2\n'
            "  exit 1\n"
            "fi\n"
            'CLAUDE_CODE_OAUTH_TOKEN=$(cat "$token_file") || exit 1\n'
            "export CLAUDE_CODE_OAUTH_TOKEN\n"
            'exec {1} "$@"\n').format(shlex.quote(str(token_file)), shlex.quote(str(program)))


def read_token(token_file):
    """The token, as the wrapper's $(cat) passes it: trailing newlines dropped, nothing else. One
    token on one line, so no error message can echo it from a malformed header."""
    token = token_file.read_text().rstrip("\n")
    if not TOKEN_PATTERN.match(token):
        fail("the Claude Code token file must hold the token alone on one line: " + str(token_file))
    return token


class NoRedirects(urllib.request.HTTPRedirectHandler):
    """urllib sends a request's headers, Authorization included, to whatever host a redirect
    names. A redirect reaches the caller as an HTTPError instead."""

    def redirect_request(self, *args, **kwargs):
        return None


OAUTH_OPENER = urllib.request.build_opener(NoRedirects)


def token_account(token_file):
    """The plan and organization name of the token's account, or None when the endpoint refuses
    the token's scope. Claude Code's `auth status` shows neither for a token, only its authMethod,
    so this asks the OAuth profile endpoint Claude Code itself reads."""
    request = urllib.request.Request(OAUTH_PROFILE_URL, headers={
        "Authorization": "Bearer " + read_token(token_file),
        "Content-Type": "application/json", "User-Agent": "namedrop-testbed"})
    try:
        with OAUTH_OPENER.open(request, timeout=20) as response:
            profile = json.loads(response.read().decode())
    except urllib.error.HTTPError as error:
        if error.code == SCOPE_REFUSAL and b'"permission_error"' in error.read():
            return None
        if error.code == REJECTED_TOKEN:
            fail("the testbed token was rejected (HTTP 401 from {}); create a new one with "
                 "claude setup-token and save it to {}".format(OAUTH_PROFILE_URL, token_file))
        fail("the Claude Code token's account lookup at {} answered HTTP {}".format(
            OAUTH_PROFILE_URL, error.code))
    except (urllib.error.URLError, OSError, ValueError, http.client.HTTPException) as error:
        fail("could not look up the Claude Code token's account at {} ({})".format(
            OAUTH_PROFILE_URL, type(error).__name__))
    organization = profile.get("organization") if isinstance(profile, dict) else None
    if not isinstance(organization, dict):
        organization = {}
    return CLAUDE_PLANS.get(organization.get("organization_type")), organization.get("name")


def account_line(plan, org, allowed):
    """The report's account line. A personal plan's organization is named after its email, so its
    name is never shown; any other plan runs only when --claude-org names its organization."""
    if plan in PERSONAL_PLANS:
        return "{} plan, personal account".format(plan)
    if not org or org != allowed:
        shown = "organization {!r}".format(org) if plan in CLAUDE_PLANS.values() else (
            "an organization whose name is not shown, because a personal organization's name holds "
            "its email" if org else "no organization")
        fail("Claude Code is signed in to the {} plan of {}. A run that bills an organization needs "
             "--claude-org naming it.".format(plan or "unknown", shown))
    return "{} plan, organization {} (allowed by --claude-org)".format(plan or "unknown", org)


def env_command(env, command):
    return ["env", "-i"] + [key + "=" + value for key, value in sorted(env.items())] + list(command)


def real_tool(name, search_path):
    """The file a tool name should run from PATH, or None. mise's wrappers and shims are symlinks
    to mise itself, which in a profile's clean environment hangs instead of starting the tool. An
    agent CLI prefers a file of its own name, but a native or npm install resolves to a file like
    versions/2.1.310 or cli.js, so another name is still accepted when nothing better is on PATH."""
    found = [Path(hit).resolve() for hit in
             (shutil.which(name, path=entry) for entry in search_path.split(os.pathsep) if entry) if hit]
    found = [path for path in found if path.name not in VERSION_MANAGERS]
    exact = [path for path in found if path.name == name] if name in AGENTS else []
    return str((exact or found)[0]) if found else None


def host_tools(herdr_bin=None, search_path=None):
    """Resolve host launchers to real files so profile runs bypass version-manager shims."""
    search_path = os.environ.get("PATH", "") if search_path is None else search_path
    tools = {}
    for name in TOOLS:
        located = sys.executable if name == "python3" else real_tool(name, search_path)
        if name == "herdr" and herdr_bin:
            located = herdr_bin
            if not os.access(located, os.X_OK):
                fail("HERDR_BIN is not an executable file: " + located)
        if located:
            tools[name] = str(Path(located).resolve())
    return tools


def host_toolchain():
    home = Path.home()
    return {"RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(home / ".rustup")),
            "CARGO_HOME": os.environ.get("CARGO_HOME", str(home / ".cargo"))}


def link_tools(bin_dir, tools):
    for name in TOOLS:
        destination = bin_dir / name
        if destination.exists() and not destination.is_symlink():
            fail("profile executable must be its managed symlink: " + str(destination))
        current = os.readlink(destination) if destination.is_symlink() else None
        if current != tools.get(name):
            if current is not None:
                destination.unlink()
            if name in tools:
                destination.symlink_to(tools[name])


def first_version(text):
    found = re.search(r"\d+\.\d+[0-9A-Za-z.+-]*", text or "")
    return found.group(0) if found else "unknown"


def testbed_home():
    return Path(os.environ.get("TESTBED_HOME") or str(Path.home() / ".namedrop-testbed")).expanduser()


def profiles_dir():
    return testbed_home() / "profiles"


def socket_for(directory):
    digest = hashlib.sha256(str(directory).encode()).hexdigest()[:16]
    return Path("/tmp/nd-tb-{}-{}.sock".format(os.getuid(), digest))


class Profile:
    def __init__(self, name):
        if not NAME_PATTERN.match(name):
            fail("profile names use letters, digits, '.', '_' and '-': " + repr(name))
        self.name = name
        real_home = Path.home().resolve()
        requested = profiles_dir().absolute() / name
        if requested.is_symlink():
            fail("profile must not be a symlink")
        self.dir = requested.resolve()
        if self.dir == real_home or self.dir in real_home.parents:
            fail("profile must be a dedicated directory, never HOME or its ancestors")
        protected = [real_home / name for name in
                     (".config", ".local", ".claude", ".codex", ".cargo", ".rustup", "Library")]
        protected.extend(Path(os.environ[key]).expanduser().resolve() for key in
                         ("XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME", "CODEX_HOME",
                          "CLAUDE_CONFIG_DIR", "CARGO_HOME", "RUSTUP_HOME") if os.environ.get(key))
        if any(self.dir == p or p in self.dir.parents or self.dir in p.parents for p in protected):
            fail("profile overlaps a real configuration, credential, or toolchain directory: " +
                 str(self.dir))
        self.socket = socket_for(self.dir)
        real_sockets = [real_home / ".config/herdr/herdr.sock",
                        Path(os.environ.get("XDG_CONFIG_HOME", str(real_home / ".config"))) /
                        "herdr/herdr.sock"]
        if os.environ.get("HERDR_SOCKET_PATH"):
            real_sockets.append(Path(os.environ["HERDR_SOCKET_PATH"]))
        if any(self.socket.resolve() == p.expanduser().resolve() for p in real_sockets):
            fail("profile socket equals a default or real Herdr socket")
        if len(os.fsencode(str(self.socket))) >= 100:
            fail("profile socket path is too long for macOS")
        self.marker = self.dir / ".namedrop-testbed"
        self.identity = {"version": VERSION, "profile": str(self.dir), "socket": str(self.socket)}
        if self.dir.exists():
            if not self.dir.is_dir():
                fail("profile is not a directory: " + str(self.dir))
            if self.marker.is_symlink():
                fail("profile marker must not be a symlink")
            if self.marker.exists():
                if json.loads(self.marker.read_text()) != self.identity:
                    fail("profile marker does not match this profile and socket")
            elif any(self.dir.iterdir()):
                fail("refusing an unmarked non-empty profile: " + str(self.dir))
        self.state_path = self.dir / "state.json"
        if self.state_path.is_symlink():
            fail("profile state must not be a symlink")
        self.state = json.loads(self.state_path.read_text()) if self.state_path.exists() else {}
        self.repo = self.dir / "repo"
        self.login = claude_login()

    @property
    def env(self):
        if "toolchain" not in self.state:
            fail("profile {} never finished setup; run ./testbed up --profile {}".format(
                self.name, self.name))
        return clean_environment(self.dir, self.socket, self.state["toolchain"])

    def save(self):
        write_if_changed(self.state_path, json.dumps(self.state, indent=2, sort_keys=True) + "\n")

    def owned(self):
        if not self.marker.is_file() or self.marker.is_symlink():
            fail("profile {} is not initialized; run ./testbed up first".format(self.name))
        if json.loads(self.marker.read_text()) != self.identity:
            fail("profile marker changed")

    def create(self):
        if not self.marker.exists():
            if self.socket.exists():
                fail("unowned profile socket already exists: " + str(self.socket))
            self.dir.mkdir(parents=True, mode=0o700, exist_ok=True)
            write_if_changed(self.marker, json.dumps(self.identity, sort_keys=True) + "\n")

    @contextlib.contextmanager
    def locked(self):
        # One profile has one writer, even when two commands arrive together.
        lock = self.dir / "testbed.lock"
        if lock.is_symlink():
            fail("profile lock must not be a symlink")
        with lock.open("a") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            yield

    def prepare(self, agents):
        for name in ("home", "home/.claude", "config", "state", "data", "cache", "run", "codex",
                     "bin", "logs", "config-dirs", "data-dirs"):
            directory = self.dir / name
            if directory.is_symlink():
                fail("profile directory must not be a symlink: " + str(directory))
            directory.mkdir(exist_ok=True, mode=0o700)
        herdr_bin = os.environ.get("HERDR_BIN") or self.state.get("herdr_bin")
        tools = host_tools(herdr_bin)
        missing = [name for name in REQUIRED_TOOLS + tuple(agents) if name not in tools]
        if missing:
            fail("required executables are missing from PATH: " + ", ".join(missing))
        # A profile has Claude Code, and a login for it, only when its run starts Claude Code.
        auth = claude_auth(self.login) if "claude" in agents else None
        if "claude" in self.state.get("tools", {}) and self.state.get("claude_auth", "legacy") != auth \
                and self.running():
            # A Claude Code already running keeps its login, and launch never restarts it.
            fail("profile {} runs Claude Code, which keeps the login it started with. Run ./testbed "
                 "stop --profile {} first; the next start launches Claude Code again.".format(
                     self.name, self.name))
        self.state.pop("claude_account", None)
        if auth == "token":
            tools["claude"] = self.claude_wrapper(tools["claude"])
        elif auth is None:
            tools.pop("claude", None)
        link_tools(self.dir / "bin", tools)
        self.state.update(tools=tools, toolchain=host_toolchain(), herdr_bin=herdr_bin,
                          claude_auth=auth, agents=list(agents))
        self.save()
        self.auth_links(auth)
        if auth:
            try:
                self.check_claude_account(auth)
            except BaseException:
                self.auth_links(None)
                raise
        write_if_changed(self.dir / "env.sh",
                         "# Source this file to enter a shell with only the testbed environment.\n" +
                         "testbed_shell() {\n  " + shlex.join(self.shell_command()) + ";\n}\n"
                         "testbed_shell\n")
        bashrc = self.dir / "home/.bashrc"
        prompt = "export PS1='{}'\n".format(SHELL_PS1.format(self.name))
        # A pane's login shell runs /etc/profile, whose path_helper puts the system directories
        # ahead of bin, where another claude can sign in some other way.
        head = prompt + "PATH={}:$PATH\n".format(shlex.quote(str(self.dir / "bin")))
        kept = bashrc.read_text() if bashrc.exists() else ""
        if not kept.startswith(head):
            # Namedrop's activation appends its own lines, which stay.
            write_if_changed(bashrc, head + kept.removeprefix(prompt))
        bash_profile = self.dir / "home/.bash_profile"
        if not bash_profile.exists():
            write_if_changed(bash_profile, 'if [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc"; fi\n')

    def shell_command(self):
        return env_command(self.env, ["/bin/bash", "--noprofile", "--rcfile",
                                      str(self.dir / "home/.bashrc"), "-i"])

    def run(self, command, cwd=None, check=True, capture=True):
        full = env_command(self.env, command)
        result = subprocess.run(full, cwd=cwd, text=True,
                                stdout=subprocess.PIPE if capture else None,
                                stderr=subprocess.STDOUT if capture else None)
        if check and result.returncode != 0:
            shown = shlex.join(full).replace("\r", "\\r")
            fail("command failed: " + shown + "\n" + (result.stdout or "see output above"))
        return result

    def herdr(self, *args, check=True):
        result = self.run(["herdr"] + list(args), check=check)
        if result.returncode != 0:
            return None
        try:
            return json.loads(result.stdout)["result"]
        except (ValueError, KeyError, TypeError):
            if not check:
                return None
            fail("Herdr returned unexpected JSON for " + shlex.join(args) + ": " + result.stdout)

    def require_claude_login(self):
        """Refuses a profile that can start Claude Code when no account check accepted it: one whose
        check refused it, or an older testbed's profile, which links the default login and an
        unwrapped claude whatever its agents."""
        reachable = [self.dir / "bin/claude"] + [link for _, link in self.default_login_links()]
        if "claude_account" not in self.state and any(path.is_symlink() for path in reachable):
            fail("profile {0} can start Claude Code with no accepted account check. Run ./testbed "
                 "up --profile {0} first, or add --agents codex for a profile without Claude "
                 "Code.".format(self.name))

    def claude_wrapper(self, program):
        """Installs the token wrapper and returns its path. The wrapper starts Claude Code through
        a link named claude, so the process keeps that name."""
        libexec = self.dir / "libexec"
        if libexec.is_symlink():
            fail("profile directory must not be a symlink: " + str(libexec))
        libexec.mkdir(exist_ok=True, mode=0o700)
        link_tools(libexec, {"claude": program})
        wrapper = libexec / "claude-with-token"
        write_if_changed(wrapper, token_wrapper(self.login.token_file, libexec / "claude"))
        wrapper.chmod(0o700)
        return str(wrapper)

    def check_claude_account(self, auth):
        """Asks the profile's Claude Code which account it signs in to, before any prompt, and
        records the report's account line. Refuses an organization --claude-org does not name."""
        try:
            result = subprocess.run(env_command(self.env, ["claude", "auth", "status", "--json"]),
                                    cwd=str(self.dir), text=True, capture_output=True, timeout=60)
        except subprocess.TimeoutExpired:
            fail("claude auth status did not answer in 60 seconds in profile " + self.name)
        try:
            status = json.loads(result.stdout)
        except ValueError:
            status = {}
        if not status.get("loggedIn"):
            fail("Claude Code in profile {} is not signed in (claude auth status exited {}): {}".format(
                self.name, result.returncode, (result.stderr or result.stdout).strip()[-500:]))
        method = status.get("authMethod")
        if auth == "token":
            if method != "oauth_token":
                fail("Claude Code in profile {} signs in with {}, not the testbed token".format(
                    self.name, method))
            account = token_account(self.login.token_file)
            if account is None:
                self.state["claude_account"] = ("plan and organization unreadable with this token's "
                                                "scope, signed in with the testbed token")
                self.save()
                return
            plan, org = account
        else:
            plan, org = status.get("subscriptionType"), status.get("orgName")
        self.state["claude_account"] = "{}, signed in with {}".format(
            account_line(plan, org, self.login.org),
            "the testbed token" if auth == "token" else "the default login")
        self.save()

    def default_login_links(self):
        """The (target, link) pairs that give the profile's Claude Code this machine's login."""
        real_home = Path.home()
        links = [(real_home / ".claude/.credentials.json", self.dir / "home/.claude/.credentials.json")]
        if sys.platform == "darwin":
            # Claude Code on macOS keeps its login in the credentials file or the login keychain.
            links.append((real_home / "Library/Keychains", self.dir / "home/Library/Keychains"))
        return links

    def auth_links(self, claude_auth):
        """Links the Codex login, and the default Claude Code login only when the run asked for it.
        Otherwise a Claude link an earlier run made is removed."""
        real_home = Path.home()
        claude = self.default_login_links()
        logins = [("Codex", "codex login",
                   [(Path(os.environ.get("CODEX_HOME", str(real_home / ".codex"))) / "auth.json",
                     self.dir / "codex/auth.json")])]
        if claude_auth == "default":
            logins.append(("Claude", "claude auth login", claude))
        else:
            for target, destination in claude:
                if destination.is_symlink():
                    if os.readlink(destination) != str(target):
                        fail("unexpected authentication link: " + str(destination))
                    destination.unlink()
        for label, login, links in logins:
            linked = False
            for target, destination in links:
                if destination.is_symlink() or destination.exists():
                    if destination.is_symlink() and os.readlink(destination) != str(target):
                        fail("unexpected authentication link: " + str(destination))
                    linked = True
                elif target.exists():
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    destination.symlink_to(target, target_is_directory=target.is_dir())
                    linked = True
            if not linked:
                say("WARNING: {} login missing. Run ./testbed shell --profile {}, then {}".format(
                    label, self.name, login))

    def running(self):
        return self.socket.exists() and self.herdr("workspace", "list", check=False) is not None

    def start_server(self):
        if self.running():
            return
        if self.socket.exists():
            fail("profile socket exists but cannot be reached; inspect it before retrying: " +
                 str(self.socket))
        log = self.dir / "logs/server.log"
        say("Starting the profile's Herdr server on " + str(self.socket))
        lane = rex_lane.Lane(self, fail, say) if self.state.get("inside_rex") else None
        if lane:
            lane.start_herdr(sys.executable, log)
            process = None
        else:
            with log.open("a") as output:
                process = subprocess.Popen(env_command(self.env, ["herdr", "server"]),
                                           cwd=str(self.dir), stdout=output,
                                           stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.running():
                if lane:
                    say(lane.prove_herdr_in_block())
                # Restored panes come back as new shells: agents must start again and
                # naming again restores the pane titles.
                for key in list(self.state):
                    if key.startswith(("launched_", "named_")):
                        del self.state[key]
                self.save()
                return
            if process and process.poll() is not None:
                fail("profile server exited ({})\n{}".format(process.returncode, log.read_text()))
            time.sleep(0.1)
        fail("profile server did not become ready\n" + log.read_text())

    def stop_server(self):
        if self.socket.exists():
            self.run(["herdr", "server", "stop"], capture=False)
            deadline = time.monotonic() + 10
            while self.socket.exists() and time.monotonic() < deadline:
                time.sleep(0.1)
            if self.socket.exists():
                fail("profile socket remains; inspect it before retrying: " + str(self.socket))
            say("Stopped the server of " + self.name)
        else:
            say("Server already stopped: " + self.name)
        rex_lane.Lane(self, fail, say).stop()
        self.kill_leftovers()

    def leftover_pids(self):
        """Processes that still run under this profile: agent CLIs, Codex's daemons."""
        marker = str(self.dir)
        pids = set()
        listing = subprocess.run(["ps", "-eww", "-o", "pid=,command="], capture_output=True,
                                 text=True, check=False)
        for line in listing.stdout.splitlines():
            parts = line.split(None, 1)
            if len(parts) == 2 and parts[0].isdigit() and marker in parts[1]:
                pids.add(int(parts[0]))
        proc = Path("/proc")
        if proc.is_dir():
            home = ("HOME=" + marker).encode()
            for entry in proc.iterdir():
                if entry.name.isdigit():
                    try:
                        variables = (entry / "environ").read_bytes().split(b"\0")
                    except OSError:
                        continue
                    if any(v == home or v.startswith(home + b"/") for v in variables):
                        pids.add(int(entry.name))
        return pids - {os.getpid(), os.getppid()}

    def kill_leftovers(self):
        pids = self.leftover_pids()
        if not pids:
            return
        say("Stopping {} leftover processes of {}".format(len(pids), self.name))
        for number in (signal.SIGTERM, signal.SIGKILL):
            for pid in pids:
                try:
                    os.kill(pid, number)
                except ProcessLookupError:
                    pass
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                pids = self.leftover_pids()
                if not pids:
                    return
                time.sleep(0.1)
        fail("processes still run under the profile: " + ", ".join(map(str, sorted(pids))))

    def resolve(self, request):
        if request["kind"] == "ref":
            return dict(request)
        if request["kind"] == "pr":
            say("Resolving {} PR #{} on GitHub".format(request["repo"], request["number"]))
            pull = sources.resolve_pull(request["repo"], request["number"],
                                        os.environ.get("GITHUB_TOKEN"))
            if pull["state"] != "open":
                say("WARNING: PR #{} is {}".format(pull["number"], pull["state"]))
            return pull
        path = request["path"]
        commit = self.run(["git", "-C", path, "rev-parse", "--verify", "HEAD^{commit}"]).stdout.strip()
        if self.run(["git", "--no-optional-locks", "-C", path, "status", "--porcelain"]).stdout.strip():
            say("WARNING: {} has uncommitted changes; the testbed uses commit {} only".format(
                path, commit[:7]))
        return {"kind": "link", "path": path, "commit": commit}

    def plugins(self):
        return self.herdr("plugin", "list", "--plugin", "namedrop", "--json")["plugins"]

    def install_plugin(self, source):
        if not self.plugins():
            plan = sources.install_plan(source)
            say("Installing Namedrop from " + sources.describe(source) + ". The first build can "
                "take several minutes.")
            {"install": self.install_from_github, "link": self.link_from_clone}[plan["method"]](plan)
        plugins = self.plugins()
        if len(plugins) != 1:
            fail("expected one Namedrop installation in the profile")
        plugin = plugins[0]
        root = Path(plugin["plugin_root"]).resolve()
        if self.dir not in root.parents:
            fail("plugin root is outside the profile: " + str(root))
        if not plugin.get("enabled", True):
            self.herdr("plugin", "enable", "namedrop")
        self.state["plugin_root"] = str(root)
        self.save()

    def install_from_github(self, plan):
        self.run(["herdr", "plugin", "install", plan["repo"], "--ref", plan["ref"], "--yes"],
                 capture=False)

    def link_from_clone(self, plan):
        local = self.dir / "namedrop-source"
        if not local.exists():
            # Build the clone aside so a crash never leaves a half-made source behind.
            partial = self.dir / "namedrop-source.partial"
            if partial.is_symlink():
                fail("refusing a symlinked clone directory: " + str(partial))
            if partial.exists():
                shutil.rmtree(str(partial))
            self.run(["git", "clone", "--quiet", "--no-local", "--no-hardlinks",
                      plan["clone"], str(partial)], capture=False)
            if plan["fetch"]:
                self.run(["git", "fetch", "--quiet", "origin", plan["fetch"]], cwd=str(partial),
                         capture=False)
            self.run(["git", "-c", "advice.detachedHead=false", "checkout", "--quiet",
                      plan["checkout"]], cwd=str(partial), capture=False)
            partial.rename(local)
        if local.is_symlink():
            fail("refusing a symlinked clone directory: " + str(local))
        if not (local / "bin/namedrop").is_file():
            self.run(["sh", "scripts/build.sh"], cwd=str(local), capture=False)
        self.herdr("plugin", "link", str(local))

    def install_integrations(self, agents):
        installed = self.state.get("integrations") or []
        for kind in agents:
            if kind not in installed:
                self.run(["herdr", "integration", "install", kind], capture=False)
                installed.append(kind)
                self.state["integrations"] = installed
                self.save()

    def seed(self):
        if self.repo.is_symlink():
            fail("profile repo must not be a symlink")
        if self.state.get("seeding") and not self.state.get("seed_commit") and self.repo.exists():
            shutil.rmtree(str(self.repo))
        if not self.repo.exists() or not any(self.repo.iterdir()):
            self.state["seeding"] = True
            self.save()
            copy_seed(KIT / "seed", self.repo)
            cwd = str(self.repo)
            self.run(["git", "init", "--quiet", "--initial-branch=main", "--template="], cwd=cwd)
            self.run(["git", "config", "user.name", "Namedrop Testbed"], cwd=cwd)
            self.run(["git", "config", "user.email", "testbed@example.invalid"], cwd=cwd)
            self.run(["git", "add", "."], cwd=cwd)
            self.run(["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                      "commit", "--quiet", "-m", "Add the Namedrop testbed seed"], cwd=cwd)
            self.state["seed_commit"] = self.run(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
            self.state.pop("seeding", None)
            self.save()
        if not self.state.get("seed_commit"):
            fail("repo exists without a recorded seed commit; inspect it before retrying")
        if (self.repo / ".git").is_symlink():
            fail("repo Git directory must not be a symlink")
        top = self.run(["git", "rev-parse", "--show-toplevel"], cwd=str(self.repo)).stdout.strip()
        if Path(top).resolve() != self.repo:
            fail("profile repo is not its own Git repository")

    def workspace(self):
        workspace = self.state.get("workspace")
        if workspace and self.herdr("workspace", "get", workspace, check=False) is not None:
            return
        result = self.herdr("worktree", "open", "--cwd", str(self.repo), "--path", str(self.repo),
                            "--label", "testbed " + self.name, "--no-focus")
        workspace = result["workspace"]["workspace_id"]
        self.clear_panes()
        self.state["workspace"] = workspace
        self.save()
        info = self.herdr("workspace", "get", workspace)["workspace"]
        worktree = info.get("worktree") or {}
        if Path(worktree.get("checkout_path", "")).resolve() != self.repo:
            fail("workspace is not attached to the profile repo as a Git worktree")

    def clear_panes(self):
        self.state.update(workspace=None, panes={}, activated=False)
        for key in list(self.state):
            if key.startswith(("launched_", "named_")):
                del self.state[key]
        self.save()

    def in_pane(self, pane, command, label, timeout=90):
        output = self.dir / "logs" / (label + ".log")
        done = self.dir / "logs" / (label + ".exit")
        if done.exists():
            done.unlink()
        # The poll below stops once .exit exists, so the code lands in a temp
        # file first and the rename makes .exit appear already written.
        written = self.dir / "logs" / (label + ".exit.tmp")
        shell = "{ " + command + "\n} > " + shlex.quote(str(output)) + \
                ' 2>&1; printf "%s" "$?" > ' + shlex.quote(str(written)) + \
                " && mv -f " + shlex.quote(str(written)) + " " + shlex.quote(str(done)) + "\n"
        # Sourced from a file so the typed line stays short: macOS drops a typed line
        # past the tty's 1024-byte canonical limit, which a fresh pane still has.
        script = self.dir / "logs" / (label + ".sh")
        script.write_text(shell)
        self.type_line(pane, ". " + shlex.quote(str(script)))
        deadline = time.monotonic() + timeout
        while not done.exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        if not done.exists():
            fail("pane command timed out; inspect " + str(output) + " before retrying")
        text = output.read_text() if output.exists() else ""
        return int(done.read_text()), text

    def panes(self, agents, launch):
        live = self.herdr("pane", "list", "--workspace", self.state["workspace"])["panes"]
        ids = {pane["pane_id"] for pane in live}
        panes = self.state["panes"]
        if panes.get("shell") not in ids:
            if not live:
                fail("workspace has no root shell pane")
            panes["shell"] = live[0]["pane_id"]
            self.save()
        self.activate(panes["shell"])
        wanted = [spec for spec in PANES if spec.kind == "terminal" or spec.alias in agents]
        for spec in PANES:
            if spec in wanted or spec.kind == "terminal":
                continue
            if panes.get(spec.alias) in ids:
                self.herdr("pane", "close", panes[spec.alias])
                ids.discard(panes[spec.alias])
            panes.pop(spec.alias, None)
            self.state.pop("named_" + spec.alias, None)
            self.state.pop("launched_" + spec.alias, None)
            self.save()
        for spec in wanted:
            if spec.kind == "terminal" or panes.get(spec.alias) in ids:
                continue
            parent, direction = next((p, d) for p, d in spec.placements if panes.get(p) in ids)
            result = self.herdr("pane", "split", panes[parent], "--direction", direction,
                                "--cwd", str(self.repo), "--no-focus")
            panes[spec.alias] = result["pane"]["pane_id"]
            ids.add(panes[spec.alias])
            self.state.pop("launched_" + spec.alias, None)
            self.state.pop("named_" + spec.alias, None)
            self.save()
        for spec in wanted:
            if not self.state.get("named_" + spec.alias):
                command = ["namedrop", "name", "@" + spec.alias, "--human", "--type", spec.kind]
                code, text = self.in_pane(panes[spec.alias], shlex.join(command), "name-" + spec.alias)
                print(text, end="", flush=True)
                if code:
                    fail("naming failed; inspect profile pane " + panes[spec.alias])
                self.state["named_" + spec.alias] = True
                self.save()
        if "codex" in agents:
            ensure_codex_settings(self.dir / "codex/config.toml")
        if not launch:
            return
        for spec in wanted:
            if spec.kind == "agent":
                self.launch(spec.alias, os.environ.get(spec.variable, spec.default))

    def activate(self, pane):
        """Activates Namedrop from `pane`, which installs its Claude Code and Codex packages into
        the profile with hook wrappers baked to the profile's state directory."""
        if self.state.get("activated"):
            return
        helper = Path(self.state["plugin_root"]) / "scripts/setup.sh"
        code, text = self.in_pane(pane, "sh " + shlex.quote(str(helper)) + " activate", "activate")
        print(text, end="", flush=True)
        if code:
            fail("Namedrop activation failed; see " + str(self.dir / "logs/activate.log"))
        self.state["activated"] = True
        self.save()

    def type_line(self, pane, text):
        """Type `text` and Enter into `pane` in one request.

        `pane run` pastes, and a pane whose program died with bracketed paste on
        (a killed agent) hands bash 3.2 the paste markers as part of the line.
        `send-text` writes raw bytes, so a trailing carriage return submits it.
        """
        self.run(["herdr", "pane", "send-text", pane, text + "\r"])

    def launch(self, alias, command):
        pane = self.state["panes"][alias]
        live = self.herdr("pane", "list", "--workspace", self.state["workspace"])["panes"]
        if any(row["pane_id"] == pane and row.get("agent") for row in live):
            return
        if not command.strip():
            fail("the launch command for @{} must not be empty".format(alias))
        if alias == "claude":
            words = shlex.split(command)
            if words[0] != "claude":
                fail("the launch command for @claude must start with claude, the profile's command "
                     "that signs in with the testbed token: " + command)
            # The pane's PATH may not start with bin, so the profile's claude is named in full.
            command = shlex.join([str(self.dir / "bin/claude")] + words[1:])
        # No exec: when the CLI exits, the pane keeps its shell and the error stays
        # on screen, and the next `up` sees no agent in the pane and starts it again.
        self.type_line(pane, command)
        self.state["launched_" + alias] = True
        self.save()

    def reset_repo(self):
        self.seed()
        self.run(["git", "-c", "core.hooksPath=/dev/null", "reset", "--quiet", "--hard",
                  self.state["seed_commit"]], cwd=str(self.repo))
        self.run(["git", "clean", "-qfdx"], cwd=str(self.repo))

    def namedrop(self, *args):
        workspace = self.state.get("workspace")
        if not workspace:
            return None
        state = self.dir / "state/herdr/plugins/namedrop"
        return self.run([str(state / "runtime/namedrop"), "--plugin-state", str(state),
                         "--herdr-workspace", workspace] + list(args), check=False)

    def doctor(self):
        result = self.namedrop("doctor", "--human")
        if result is not None:
            print(result.stdout, end="", flush=True)
            if result.returncode:
                say("Doctor needs attention (exit {}). First-run prompts in the agent panes "
                    "often clear it.".format(result.returncode))

    def namedrop_version(self):
        root = self.state.get("plugin_root")
        manifest = Path(root) / "herdr-plugin.toml" if root else None
        if not manifest or not manifest.is_file():
            return "not installed"
        found = re.search(r'(?m)^version\s*=\s*"([^"]+)"', manifest.read_text())
        return found.group(1) if found else "unknown"

    def installed_commit(self):
        root = self.state.get("plugin_root")
        if root and (Path(root) / ".git").exists():
            result = self.run(["git", "-C", root, "rev-parse", "HEAD"], check=False)
            if result.returncode == 0:
                return result.stdout.strip()
        found = self.state.get("source", {})
        return found.get("sha") or found.get("commit")

    def tool_version(self, name):
        if name not in self.state.get("tools", {}):
            return "not installed"
        result = self.run([name, "--version"], check=False)
        return first_version(result.stdout) if result.returncode == 0 else "unknown"

    def up(self, request, agents, launch):
        self.prepare(agents)
        if "source" not in self.state:
            self.state["source"] = self.resolve(request)
            self.save()
        self.state["launch"] = launch
        self.save()
        self.start_server()
        self.install_plugin(self.state["source"])
        self.install_integrations(agents)
        self.seed()
        self.workspace()
        self.panes(agents, launch)
        self.doctor()

    def reset(self):
        if not self.running():
            fail("the server is stopped; run ./testbed up --profile {} first".format(self.name))
        workspace = self.state.get("workspace")
        if workspace and self.herdr("workspace", "get", workspace, check=False) is not None:
            self.herdr("workspace", "close", workspace)
        self.clear_panes()
        self.reset_repo()
        self.workspace()
        self.panes(self.state["agents"], self.state["launch"])
        self.doctor()

    def destroy(self):
        self.stop_server()
        self.owned()
        shutil.rmtree(str(self.dir))


def existing_profiles():
    directory = profiles_dir()
    if not directory.is_dir():
        return []
    return sorted(path.name for path in directory.iterdir() if path.is_dir())


def chosen_profile(name):
    if name:
        return Profile(name)
    names = existing_profiles()
    if len(names) == 1:
        return Profile(names[0])
    if not names:
        fail("no profiles yet; run ./testbed up first")
    fail("several profiles exist; pass --profile with one of: " + ", ".join(names))


def confirm_destroy(profile, yes):
    if yes:
        return
    try:
        typed = input("Type {} to destroy {}: ".format(profile.name, profile.dir))
    except EOFError:
        fail("destroy needs a typed confirmation; pass --yes to skip it")
    if typed.strip() != profile.name:
        fail("confirmation did not match; nothing was destroyed")


def destroy(profile, yes):
    if not profile.dir.exists():
        say("Profile already absent: " + profile.name)
        return
    profile.owned()
    confirm_destroy(profile, yes)
    with profile.locked():
        profile.destroy()
    say("Destroyed " + str(profile.dir))


def requested_source(args):
    if args.ref is not None:
        if not args.ref or args.ref.startswith("-") or re.search(r"\s", args.ref):
            fail("--ref needs a branch or tag name")
        return {"kind": "ref", "repo": args.repo, "ref": args.ref}
    if args.pr is not None:
        if args.pr < 1:
            fail("--pr needs a pull request number")
        return {"kind": "pr", "repo": args.repo, "number": args.pr}
    if args.link is not None:
        path = Path(args.link).expanduser().resolve()
        if not (path / "herdr-plugin.toml").is_file() or not (path / ".git").exists():
            fail("--link needs a Namedrop Git checkout: " + str(path))
        return {"kind": "link", "path": str(path)}
    return None


def chosen_agents(text, saved):
    if text is None:
        return tuple(saved or AGENTS)
    agents = tuple(dict.fromkeys(part.strip() for part in text.split(",") if part.strip()))
    if not agents or any(agent not in AGENTS for agent in agents):
        fail("--agents takes a comma-separated list of: " + ", ".join(AGENTS))
    return agents


def profile_for(args, prefix=""):
    """A non-empty prefix is required of the profile's name, so test never drives a profile
    a person works in."""
    if not sources.REPO_PATTERN.match(args.repo):
        fail("--repo needs OWNER/REPO")
    request = requested_source(args)
    name = args.profile
    if name and prefix and not name.startswith(prefix):
        fail("test runs only in profiles named {}*: {}".format(prefix, name))
    if not name and request:
        name = prefix + sources.default_profile_name(request)
    if not name and prefix:
        fail("pass --ref, --pr, --link or --profile")
    if not name:
        name = chosen_profile(None).name if existing_profiles() else "ref-main"
    profile = Profile(name)
    saved = profile.state.get("source")
    request = request or saved or {"kind": "ref", "repo": args.repo, "ref": "main"}
    if args.fresh:
        destroy(profile, args.yes)
        profile = Profile(name)
    elif saved and sources.identity(saved) != sources.identity(request):
        fail("profile {} was installed from {}; pass --fresh or another --profile".format(
            name, sources.describe(saved)))
    profile.login = claude_login(args.claude_default_login, args.claude_org)
    return profile, request


def add_source_options(parser):
    picked = parser.add_mutually_exclusive_group()
    picked.add_argument("--ref", help="install a branch or tag from GitHub")
    picked.add_argument("--pr", type=int, help="install a pull request")
    picked.add_argument("--link", help="build and link a local checkout")
    parser.add_argument("--repo", default=sources.DEFAULT_REPO,
                        help="GitHub OWNER/REPO for --ref and --pr")
    parser.add_argument("--fresh", action="store_true",
                        help="destroy this profile first and install the same source again")
    parser.add_argument("--yes", action="store_true", help="skip the --fresh confirmation")
    parser.add_argument("--claude-default-login", action="store_true",
                        help="when no Claude Code token file exists, use this machine's default "
                             "Claude Code login")
    parser.add_argument("--claude-org", metavar="NAME",
                        help="allow Claude Code to bill this team or enterprise organization")


def cmd_up(args):
    profile, request = profile_for(args)
    name = profile.name
    agents = chosen_agents(args.agents, profile.state.get("agents"))
    profile.create()
    with profile.locked():
        profile.up(request, agents, not args.no_launch)
    say("\nReady: profile {}, Namedrop {} from {}.".format(
        name, profile.namedrop_version(), sources.describe(profile.state["source"])))
    if "claude_account" in profile.state:
        say("Claude Code account: " + profile.state["claude_account"])
    say("Next:\n  1. From a plain terminal outside Herdr, run: ./testbed attach --profile " + name)
    if args.no_launch:
        say("  2. The agent CLIs are not running. To start them, run: ./testbed up --profile " + name)
    else:
        say("  2. Finish the first-run prompts in @claude and @codex yourself.")
    say("  3. Follow CHECKLIST.md.")


def cmd_test(args):
    import livetest
    lane = None
    if (args.daily_use or args.upgrade_check) and args.codex == "plain":
        fail("the daily-use and upgrade lanes run Codex with --no-daemon; the plain lane owns --codex plain")
    if args.upgrade_from and not args.upgrade_check:
        fail("--upgrade-from needs --upgrade-check")
    if args.upgrade_check:
        if not args.link:
            fail("--upgrade-check needs --link: the build under test is activated over the previous release")
        import daily_upgrade
        lane = daily_upgrade.lane(args.upgrade_from, args.repo, args.link)
    elif args.daily_use:
        import dailyuse
        lane = dailyuse.lane()
    profile, request = profile_for(args, prefix="test-upgrade-" if args.upgrade_check else "test-")
    if args.upgrade_check and profile.marker.exists():
        fail("an upgrade check needs a new profile, because its build replaces the release; pass --fresh")
    created = not profile.marker.exists()
    profile.create()
    if created:
        profile.state["purpose"] = "test"
    if profile.state.get("inside_rex", False) != args.inside_rex and profile.running():
        fail("profile {} runs its server {} Rex; pass --fresh to change lanes".format(
            profile.name, "inside" if profile.state.get("inside_rex") else "outside"))
    profile.state["inside_rex"] = args.inside_rex
    profile.save()
    passed = run_test(profile, lambda: livetest.run(profile, request, args.codex, args.keep, lane))
    sys.exit(0 if passed else 1)


def run_test(profile, run):
    """run() for cmd_test. On the --inside-rex lane, a run that fails or raises leaves no server
    behind: the profile is deleted, and its logs are kept under reports/."""
    passed = False
    try:
        passed = run()
    finally:
        if profile.state.get("inside_rex") and not passed:
            discard_failed(profile)
    return passed


def discard_failed(profile):
    if not profile.dir.exists():
        return
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    kept = testbed_home() / "reports" / "{}-{}-failed".format(profile.name, stamp)
    try:
        profile.stop_server()
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        say("Stopping the profile's Herdr server failed: {}".format(error))
        rex_lane.Lane(profile, fail, say).stop()
        profile.kill_leftovers()
    rex_lane.remove_stale(profile.socket)
    try:
        if (profile.dir / "logs").is_dir():
            shutil.copytree(str(profile.dir / "logs"), str(kept), symlinks=True)
    except OSError as error:
        say("Keeping the failed run's logs failed: {}".format(error))
    profile.owned()
    shutil.rmtree(str(profile.dir))
    say("Deleted profile {} after the failed run. Its logs are in {}".format(profile.name, kept))


def cmd_rex(args):
    import rex_live
    profile, request = profile_for(args, prefix="rex-")
    if args.action == "down":
        profile.owned()
        with profile.locked():
            rex_live.down(profile)
        return
    agents = chosen_agents(args.agents, profile.state.get("agents"))
    profile.create()
    if profile.state.get("inside_rex"):
        fail("profile {} belongs to the --inside-rex lane".format(profile.name))
    profile.state.update(purpose="rex", agents=list(agents))
    profile.save()
    with profile.locked():
        lane, session, blocks = rex_live.up(profile, request, agents)
    say("\nReady: profile {}, Namedrop {} from {}, Rex server {}.".format(
        profile.name, profile.namedrop_version(), sources.describe(profile.state["source"]),
        lane.server))
    if "claude_account" in profile.state:
        say("Claude Code account: " + profile.state["claude_account"])
    say("State directory: {}\nSession {}: {}".format(
        rex_live.state_directory(profile), session,
        ", ".join("@{} {}".format(alias, block) for alias, block in sorted(blocks.items()))))
    say("Start more agents in their own sessions with:")
    for agent in agents:
        say("  " + lane.command_line("new", "--shell", "none", "-c", str(profile.repo), "--",
                                     *rex_live.agent_command(profile, agent)))
    say("Stop it with: ./testbed rex down --profile " + profile.name)


def cmd_status(args):
    profile = chosen_profile(args.profile)
    profile.owned()
    say("Profile: {}\nDirectory: {}\nSource: {}\nSocket: {}".format(
        profile.name, profile.dir, sources.describe(profile.state["source"])
        if "source" in profile.state else "not resolved", profile.socket))
    if not profile.running():
        say("Server: stopped")
        return
    say("Server: running")
    for plugin in profile.plugins():
        say("Plugin: {} {}, {}, installed from {}\nPlugin root: {}".format(
            plugin.get("plugin_id"), plugin.get("version"),
            "enabled" if plugin.get("enabled", True) else "disabled",
            plugin_origin(plugin.get("source") or {}), plugin.get("plugin_root")))
    say("Panes:")
    for pane in profile.herdr("pane", "list")["panes"]:
        say("  {:<8} {:<10} agent {}".format(pane.get("pane_id"), pane.get("title") or "-",
                                            pane.get("agent_status")))
    result = profile.namedrop("address", "list", "--human")
    if result is not None:
        say("namedrop address list:")
        print(result.stdout, end="", flush=True)
        if result.returncode:
            say("Name listing needs attention (exit {}).".format(result.returncode))
    say("namedrop doctor:")
    profile.doctor()


def plugin_origin(origin):
    if origin.get("kind") == "github":
        return "GitHub {}/{} ref {} at {}".format(origin.get("owner"), origin.get("repo"),
                                                  origin.get("requested_ref"),
                                                  (origin.get("resolved_commit") or "?")[:7])
    return "a {} link".format(origin.get("kind", "local"))


def cmd_list(args):
    names = existing_profiles()
    if not names:
        say("No profiles under " + str(profiles_dir()))
        return
    rows = [("PROFILE", "SERVER", "NAMEDROP", "SOURCE")]
    for name in names:
        try:
            profile = Profile(name)
            profile.owned()
            source = profile.state.get("source")
            rows.append((name, "running" if profile.running() else "stopped",
                         profile.namedrop_version(),
                         sources.describe(source) if source else "not resolved"))
        except (RuntimeError, OSError, ValueError, KeyError) as error:
            rows.append((name, "invalid", "-", str(error)))
    widths = [max(len(row[column]) for row in rows) for column in range(3)]
    for row in rows:
        say("  ".join(cell.ljust(width) for cell, width in zip(row, widths)) + "  " + row[3])


def host_description():
    machine = platform.machine()
    if sys.platform == "darwin":
        return "macOS {} {}".format(platform.mac_ver()[0], machine)
    pretty = "unknown distribution"
    with contextlib.suppress(OSError):
        found = re.search(r'(?m)^PRETTY_NAME="?([^"\n]+)"?', Path("/etc/os-release").read_text())
        if found:
            pretty = found.group(1)
    return "Linux {} ({})".format(machine, pretty)


def compat_line(profile, models=None):
    """The line for Namedrop's docs/compatibility.md; models maps an agent to the models its
    transcripts show."""
    source = profile.state.get("source")
    commit = profile.installed_commit()

    def agent(name, label):
        found = (models or {}).get(name)
        return "{} {}{}".format(label, profile.tool_version(name), " with " + found if found else "")

    return "- **{}, {}**, Namedrop {} from {}{}: Herdr {}, {}, and {}.".format(
        host_description(), datetime.date.today().isoformat(), profile.namedrop_version(),
        sources.describe(source) if source else "an unknown source",
        ", commit {}".format(commit[:7]) if commit and source and source["kind"] == "ref" else "",
        profile.tool_version("herdr"), agent("codex", "Codex"), agent("claude", "Claude Code"))


def cmd_report(args):
    profile = chosen_profile(args.profile)
    profile.owned()
    say(compat_line(profile))


def cmd_stop(args):
    profile = chosen_profile(args.profile)
    profile.owned()
    with profile.locked():
        profile.stop_server()


def cmd_reset(args):
    profile = chosen_profile(args.profile)
    profile.owned()
    profile.require_claude_login()
    with profile.locked():
        profile.reset()
    say("Reset {}: the seed repo is restored and the panes are new.".format(profile.name))


def cmd_destroy(args):
    destroy(chosen_profile(args.profile), args.yes)


def cmd_attach(args):
    if os.environ.get("HERDR_PANE_ID"):
        fail("attach must run from a plain terminal window, outside an existing Herdr pane")
    profile = chosen_profile(args.profile)
    profile.owned()
    profile.require_claude_login()
    if not profile.running():
        fail("the server is stopped; run ./testbed up --profile {} first".format(profile.name))
    os.execvp("env", env_command(profile.env, ["herdr"]))


def cmd_shell(args):
    profile = chosen_profile(args.profile)
    profile.owned()
    profile.require_claude_login()
    say("Entering the clean environment of {}. Run exit to leave.".format(profile.name))
    os.execvp("env", profile.shell_command())


def cmd_check(args):
    ok = True
    if sys.version_info < (3, 9):
        say("FAIL python3 {} is older than 3.9".format(platform.python_version()))
        ok = False
    else:
        say("ok   python3 {}".format(platform.python_version()))
    tools = host_tools(os.environ.get("HERDR_BIN"))
    with tempfile.TemporaryDirectory(prefix="nd-tb-check-") as scratch:
        base = Path(scratch)
        for name in ("bin", "home", "codex"):
            (base / name).mkdir()
        link_tools(base / "bin", tools)
        env = clean_environment(base, base / "none.sock", host_toolchain())
        for name in REQUIRED_TOOLS + AGENTS + OPTIONAL_TOOLS:
            if name not in tools:
                required = name in REQUIRED_TOOLS + AGENTS
                ok = ok and not required
                say("{} {} missing from PATH{}".format("FAIL" if required else "warn", name,
                                                       "" if required else " (optional)"))
                continue
            result = subprocess.run(env_command(env, [name, "--version"]), cwd=scratch, text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
            say("ok   {} {} ({})".format(name, first_version(result.stdout), tools[name]))
    socket = socket_for(profiles_dir().absolute() / "example")
    length = len(os.fsencode(str(socket)))
    ok = ok and length < 100
    say("{} socket path is {} bytes: {}".format("ok  " if length < 100 else "FAIL", length, socket))
    import selftest
    ok = selftest.run(KIT) and ok
    say("check passed" if ok else "check FAILED")
    if not ok:
        sys.exit(1)


def main():
    parser = argparse.ArgumentParser(prog="testbed", description=__doc__)
    commands = parser.add_subparsers(dest="command", metavar="COMMAND")
    commands.required = True
    handlers = {}

    def command(name, handler, help_text, profile="profile name (default: the only profile)"):
        sub = commands.add_parser(name, help=help_text, description=help_text)
        if profile:
            sub.add_argument("--profile", help=profile)
        handlers[name] = handler
        return sub

    command("check", cmd_check, "check tools and run the self-test", profile=None)
    up = command("up", cmd_up, "create or resume a profile with Namedrop installed",
                 profile="profile name (default: derived from the source, such as pr-27)")
    add_source_options(up)
    up.add_argument("--agents", help="comma-separated agent panes (default: claude,codex)")
    up.add_argument("--no-launch", action="store_true", help="create and name panes, start no CLIs")
    test = command("test", cmd_test, "run CHECKLIST.md against real Claude Code and Codex",
                   profile="profile name starting with test- (default: test- and the source, "
                           "such as test-pr-27)")
    add_source_options(test)
    test.add_argument("--codex", choices=("plain", "no-daemon"),
                      help="how Codex starts (default: plain on Namedrop 1.2.0 and later)")
    test.add_argument("--keep", action="store_true", help="keep the profile after a pass")
    test.add_argument("--inside-rex", action="store_true",
                      help="run the profile's Herdr server in a block of a scratch Rex server")
    lanes = test.add_mutually_exclusive_group()
    lanes.add_argument("--daily-use", action="store_true",
                       help="run the scripted daily-use lane instead of CHECKLIST.md")
    lanes.add_argument("--upgrade-check", action="store_true",
                       help="activate --link over the previous release with deliveries open")
    test.add_argument("--upgrade-from", metavar="REF",
                      help="the release --upgrade-check starts from (default: the newest v* tag below --link)")
    rex = command("rex", cmd_rex, "run Claude Code and Codex in blocks of a scratch Rex server",
                  profile="profile name starting with rex- (default: rex- and the source)")
    rex.add_argument("action", choices=("up", "down"),
                     help="up builds, installs and starts the agents; down stops every session "
                          "and the server")
    add_source_options(rex)
    rex.add_argument("--agents", help="comma-separated agents (default: claude,codex)")
    command("attach", cmd_attach, "open the profile's Herdr from a plain terminal")
    command("status", cmd_status, "show the server, plugin, names and doctor")
    command("stop", cmd_stop, "stop the profile's server and keep its files")
    command("reset", cmd_reset, "restore the seed repo and start fresh panes and agent sessions")
    command("destroy", cmd_destroy, "stop the server and delete the profile").add_argument(
        "--yes", action="store_true", help="skip the typed confirmation")
    command("list", cmd_list, "list every profile", profile=None)
    command("report", cmd_report, "print a compatibility line to paste into Namedrop's docs")
    command("shell", cmd_shell, "open a shell in the profile's clean environment")
    args = parser.parse_args()
    handlers[args.command](args)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print("testbed: " + str(error), file=sys.stderr)
        sys.exit(1)
