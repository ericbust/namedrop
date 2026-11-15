"""The remote-host lab: a scratch Rex server A on this machine reaches a Linux Rex server B in a
Docker container over `ssh://`, each with its own Namedrop. `up` builds and starts both, `check`
runs the lab checks of PLAN #11, and `down` stops both and removes the container.

Every ssh key, known_hosts file and Rex home stays in the lab's directory. A's `rex` runs under
`env -i` with the lab's HOME, the lab's `ssh` first on PATH, `REX_HOME` naming A's socket,
`--autostart=false` and an explicit `-S`, so no call reaches the person's own Rex server or
hosts list. B runs Superlogical's signed apt package, pinned in testbed/rex-remote/Dockerfile."""

import sys

sys.dont_write_bytecode = True

import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import sqlite3
import subprocess
import time

KIT = Path(__file__).resolve().parent.parent
FILES = KIT / "rex-remote"
IMAGE = "ndrp-rexlab"
LABEL = "labb"
HTTP_LABEL = "labb-http"
B_USER = "nd"
B_HOME = "/home/nd"
B_STATE = B_HOME + "/ndstate"
B_SOCKET = B_HOME + "/.local/state/rex/run/server.sock"
B_NAMEDROP = B_HOME + "/.local/bin/namedrop"
LINUX_RUST = "rust:1.97.0@sha256:b92b8c8574f8f3b207fcb0912fb3e2de4041580b5934d90312d53938c9a038a9"
MARKER = "ndrp-rexlab.json"


def fail(message):
    raise RuntimeError(message)


def say(message):
    print(message, flush=True)


def run(command, check=True, **options):
    options.setdefault("text", True)
    options.setdefault("capture_output", True)
    result = subprocess.run(command, check=False, **options)
    if check and result.returncode != 0:
        fail("{} failed ({}): {}".format(shlex.join(command[:6]), result.returncode,
                                         (result.stderr or result.stdout).strip()[-2000:]))
    return result


def free_port():
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    return port


def rex_version(binary, home):
    """`0.1.0-preview.18` from `rex version`, run with a scratch HOME and no server."""
    shown = run(["env", "-i", "HOME=" + str(home), "PATH=/usr/bin:/bin", binary,
                 "--autostart=false", "-S", "unix:///nonexistent/ndrp-lab.sock", "version"],
                check=False).stdout
    found = re.search(r"^rex (\S+)", shown, re.M)
    if not found:
        fail("cannot read the version of {}: {}".format(binary, shown))
    return found.group(1)


def processes():
    """This user's processes: pid to (parent pid, start time, command line)."""
    listed = run(["ps", "-U", str(os.getuid()), "-ww", "-o", "pid=,ppid=,lstart=,command="]).stdout
    table = {}
    for line in listed.splitlines():
        fields = line.split(None, 7)
        if len(fields) == 8 and fields[0].isdigit() and fields[1].isdigit():
            table[int(fields[0])] = (int(fields[1]), " ".join(fields[2:7]), fields[7])
    return table


def lineage(table):
    """This process and its ancestors, which `down` never signals, even when their command lines
    name the lab."""
    spared = {os.getpid()}
    pid = os.getppid()
    while pid > 1 and pid not in spared:
        spared.add(pid)
        pid = table.get(pid, (0,))[0]
    return spared


def signal_all(found, number):
    """Sends `number` to each process of `found` that still has the same pid, start time and
    command line, and returns those."""
    now = processes()
    alive = {pid: identity for pid, identity in found.items() if now.get(pid, (None,))[1:] == identity}
    for pid in alive:
        try:
            os.kill(pid, number)
        except (ProcessLookupError, PermissionError):
            pass
    return alive


def hosts_by_label(listed):
    """`rex hosts list --json` as {label: endpoints}. Each entry nests its host, as
    `{"hosts": [{"host": {"label": …, "endpoints": […]}, …}]}`."""
    return {entry["host"]["label"]: list(entry["host"].get("endpoints") or [])
            for entry in listed.get("hosts") or [] if isinstance(entry.get("host"), dict)}


def apt_version(version):
    """Rex's apt package spells `0.1.0-preview.18` as `0.1.0~preview.18`."""
    return version.replace("-", "~", 1)


class Lab:
    def __init__(self, directory):
        self.dir = Path(directory).expanduser().absolute()
        digest = hashlib.sha256(str(self.dir).encode()).hexdigest()[:12]
        self.container = "ndrp-rexlab-b-" + digest
        self.a_socket = Path("/tmp/nd-tb-rexremote-{}-{}.sock".format(os.getuid(), digest))
        self.state_file = self.dir / "lab.json"
        self.state = json.loads(self.state_file.read_text()) if self.state_file.exists() else {}

    def claim(self):
        """Makes the directory a lab, or proves it is one: `up` writes only into a directory that
        is new, empty, or holds the lab's marker, and `--fresh` and `down` act only on one with it."""
        marker = self.path(MARKER)
        if marker.is_symlink() or (marker.exists() and not marker.is_file()):
            fail("{} is not a regular file".format(marker))
        if marker.exists():
            self.owned()
            return
        if self.dir.exists() and any(self.dir.iterdir()):
            fail("{} is not a remote-host lab: it holds files and no {}. Pass a new or empty --dir"
                 .format(self.dir, MARKER))
        self.dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        marker.write_text(json.dumps({"kind": "ndrp-rexlab", "dir": str(self.dir)}) + "\n")

    def owned(self):
        marker = self.path(MARKER)
        try:
            shown = json.loads(marker.read_text()) if marker.is_file() and not marker.is_symlink() else None
        except ValueError:
            shown = None
        if shown != {"kind": "ndrp-rexlab", "dir": str(self.dir)}:
            fail("{} is not a remote-host lab: {} is missing or names another directory".format(self.dir, MARKER))

    def save(self):
        self.dir.mkdir(parents=True, exist_ok=True)
        temporary = self.state_file.with_suffix(".new")
        temporary.write_text(json.dumps(self.state, indent=2, sort_keys=True) + "\n")
        temporary.replace(self.state_file)

    def path(self, *parts):
        return self.dir.joinpath(*parts)

    # A: the scratch server on this machine.

    def rex_bin(self):
        return self.state["rex_bin"]

    def a_env(self):
        return {"HOME": str(self.path("home-a")), "USER": os.environ.get("USER", "nd"),
                "LANG": "C.UTF-8", "REX_HOME": "unix://" + str(self.a_socket),
                "PATH": os.pathsep.join([str(self.path("bin")), str(Path(self.rex_bin()).parent),
                                         "/usr/bin", "/bin"])}

    def ar(self, *args, server=None, timeout=None, check=False, stdin=None):
        """`rex` as A runs it. `server` is a label of A's hosts or None for A itself."""
        command = ["env", "-i"] + ["{}={}".format(*item) for item in sorted(self.a_env().items())]
        command += [self.rex_bin(), "--autostart=false", "-S", server or "unix://" + str(self.a_socket)]
        if timeout:
            command += ["--timeout", timeout]
        return run(command + list(args), check=check, input=stdin)

    def ar_json(self, *args, **options):
        result = self.ar(*args, check=True, **options)
        return json.loads(result.stdout)

    def a_namedrop(self, *args, check=True, stdin=None):
        env = dict(self.a_env(), NAMEDROP_STATE_DIR=str(self.path("a-namedrop")))
        command = ["env", "-i"] + ["{}={}".format(*item) for item in sorted(env.items())]
        command += [str(self.path("target-mac/release/namedrop"))] + list(args)
        command += ["--endpoint", "rex:" + str(self.a_socket)]
        return run(command, check=check, input=stdin)

    def a_running(self):
        return "rex_bin" in self.state and self.a_socket.exists() and self.ar("server", "status").returncode == 0

    def start_a(self):
        if self.a_running():
            return
        if self.a_socket.exists():
            fail("A's socket exists but answers nothing; inspect and remove it: " + str(self.a_socket))
        config = self.path("a-rex/init.lua")
        config.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        config.write_text("")
        command = ["env", "-i"] + ["{}={}".format(*item) for item in sorted(self.a_env().items())]
        command += [self.rex_bin(), "--autostart=false", "server", "run", "--listen",
                    "unix://" + str(self.a_socket), "--state-dir", str(self.path("a-rex/state")),
                    "--config", str(config)]
        log = self.path("logs/a-server.log")
        with log.open("a") as output:
            server = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                                      stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.a_running():
                shown = processes().get(server.pid)
                self.state.pop("a_server_pid", None)
                self.state["a_server"] = {"pid": server.pid, "start": shown[1], "command": shown[2]} if shown else None
                self.save()
                return
            time.sleep(0.2)
        fail("A's scratch Rex server did not start:\n" + log.read_text()[-2000:])

    # B: the container.

    def docker(self, *args, check=True, stdin=None):
        return run(["docker"] + list(args), check=check, input=stdin)

    def bx(self, *args, check=True, stdin=None, user=B_USER):
        command = ["exec"] + (["-i"] if stdin is not None else [])
        command += ["-u", user, "-e", "HOME=" + B_HOME, "-e", "PATH={}/.local/bin:/usr/bin:/bin".format(B_HOME),
                    "-e", "NAMEDROP_STATE_DIR=" + B_STATE, "-e", "LANG=C.UTF-8", self.container]
        return self.docker(*(command + list(args)), check=check, stdin=stdin)

    def b_namedrop(self, *args, check=True, stdin=None):
        return self.bx(B_NAMEDROP, *args, "--endpoint", "rex:" + B_SOCKET, check=check, stdin=stdin)

    def b_rex(self, *args, check=True):
        return self.bx("rex", "--autostart=false", "-S", "unix://" + B_SOCKET, *args, check=check)

    def container_state(self):
        shown = self.docker("inspect", "-f", "{{.State.Status}} {{.State.Paused}}", self.container,
                            check=False)
        return shown.stdout.strip() if shown.returncode == 0 else "absent"

    def b_running(self):
        return self.b_rex("server", "status", check=False).returncode == 0

    def start_b(self, restore=True, http=False):
        if self.b_running():
            return
        args = ["rex", "server", "start"]
        if not restore:
            args.append("--restore=false")
        if http:
            args += ["--extra-listen", "http://0.0.0.0:7297", "--allow-anonymous"]
        self.bx(*args)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.b_running():
                return
            time.sleep(0.2)
        fail("B's Rex server did not start")

    def bring_b_back(self):
        """B's container running and unpaused, with sshd, B's Rex server and B's bridge: what a
        check that stops or pauses B leaves, even when it raises."""
        shown = self.container_state()
        if shown.startswith("paused"):
            self.docker("unpause", self.container)
        elif shown != "absent" and not shown.startswith("running"):
            self.docker("start", self.container)
        self.wait_sshd()
        self.start_b()
        self.start_b_bridge()

    def stop_b(self):
        if self.b_running():
            self.bx("rex", "--autostart=false", "-S", "unix://" + B_SOCKET, "server", "stop")

    def b_namedrop_pids(self, args=""):
        """B's Namedrop processes: those whose command line starts with B's Namedrop binary."""
        prefix = (B_NAMEDROP + " " + args).rstrip()
        listed = self.bx("ps", "-eo", "pid=,args=", check=False).stdout
        pids = []
        for line in listed.splitlines():
            pid, _, command = line.strip().partition(" ")
            command = command.strip()
            if pid.isdigit() and (command == prefix or command.startswith(prefix + " ")):
                pids.append(pid)
        return pids

    def install_b_namedrop(self):
        """Copies the Linux build to B unless B has it already. A running Namedrop on B is stopped
        first, because a bridge whose binary was replaced answers every relay call `retry` with
        `No such file or directory`; `start_b_bridge` starts it again."""
        local = self.path("target-linux/release/namedrop")
        want = hashlib.sha256(local.read_bytes()).hexdigest()
        have = self.bx("sha256sum", B_NAMEDROP, check=False).stdout.split()[:1]
        if have == [want]:
            return
        pids = self.b_namedrop_pids()
        if pids:
            say("Stopping B's Namedrop {} to replace its binary".format(" ".join(pids)))
            self.bx("kill", *pids, check=False)
            deadline = time.monotonic() + 10
            while self.b_namedrop_pids() and time.monotonic() < deadline:
                time.sleep(0.2)
            left = self.b_namedrop_pids()
            if left:
                self.bx("kill", "-9", *left, check=False)
        self.bx("mkdir", "-p", B_HOME + "/.local/bin", B_STATE + "/relay")
        self.docker("cp", str(local), self.container + ":" + B_NAMEDROP)
        self.docker("exec", self.container, "chown", "{0}:{0}".format(B_USER), B_NAMEDROP)

    def start_b_bridge(self):
        if self.b_namedrop_pids("rex bridge"):
            return
        self.bx("sh", "-c", "nohup {} rex bridge --endpoint rex:{} >> {}/bridge.log 2>&1 &".format(
            B_NAMEDROP, B_SOCKET, B_HOME))

    # Building.

    def build(self, checkout):
        commit = run(["git", "-C", str(checkout), "rev-parse", "HEAD"]).stdout.strip()
        if run(["git", "-C", str(checkout), "status", "--porcelain", "--untracked-files=no"]).stdout.strip():
            say("warning: {} has uncommitted changes; the lab builds its last commit".format(checkout))
        source = self.path("src")
        if not source.exists():
            run(["git", "clone", "--quiet", "--no-checkout", str(checkout), str(source)])
        run(["git", "-C", str(source), "fetch", "--quiet", str(checkout), commit])
        run(["git", "-C", str(source), "checkout", "--quiet", "--detach", commit])
        if self.state.get("built") == commit and self.path("target-mac/release/namedrop").exists() \
                and self.path("target-linux/release/namedrop").exists():
            return commit
        say("Building Namedrop {} for macOS (A) and Linux (B)".format(commit[:12]))
        mac_log = self.path("logs/build-mac.log")
        with mac_log.open("w") as log:
            subprocess.run(["cargo", "build", "--release", "--locked"], cwd=str(source),
                           env=dict(os.environ, CARGO_TARGET_DIR=str(self.path("target-mac"))),
                           stdout=log, stderr=subprocess.STDOUT, check=True)
        linux_log = self.path("logs/build-linux.log")
        with linux_log.open("w") as log:
            # The Linux build runs in Rust's official image; the lib's test binary runs on B
            # in check 9.
            subprocess.run(["docker", "run", "--rm", "--name", self.container + "-build",
                            "-v", "{}:/src:ro".format(source), "-v", "{}:/target".format(self.path("target-linux")),
                            "-v", "{}:/cargo".format(self.path("cargo-linux")),
                            "-e", "CARGO_HOME=/cargo", "-e", "CARGO_TARGET_DIR=/target", "-w", "/src",
                            LINUX_RUST, "sh", "-c",
                            "cargo build --release --locked && cargo test --release --locked --lib --no-run"],
                           stdout=log, stderr=subprocess.STDOUT, check=True)
        self.state["built"] = commit
        self.save()
        return commit

    def linux_test_binary(self):
        shown = self.path("logs/build-linux.log").read_text()
        found = re.findall(r"Executable unittests src/lib\.rs \(/target/(release/deps/namedrop-[0-9a-f]+)\)", shown)
        return self.path("target-linux", found[-1]) if found else None

    def image(self, version):
        tag = "{}:rex-{}".format(IMAGE, version.replace("~", "-"))
        say("Building B's image {} (Rex {} from Superlogical's signed apt repository)".format(tag, apt_version(version)))
        with self.path("logs/image.log").open("a") as log:
            subprocess.run(["docker", "build", "--build-arg", "REX_VERSION=" + apt_version(version),
                            "-t", tag, str(FILES)], stdout=log, stderr=subprocess.STDOUT, check=True)
        return tag

    # Lifecycle.

    def ssh_files(self):
        ssh = self.path("ssh")
        ssh.mkdir(parents=True, exist_ok=True, mode=0o700)
        key = ssh / "id_lab"
        if not key.exists():
            run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "ndrp-rexlab", "-f", str(key)])
        template = [line for line in (FILES / "ssh_config").read_text().splitlines() if not line.startswith("#")]
        (ssh / "config").write_text("\n".join(template).replace("@LAB@", str(self.dir)) + "\n")
        (ssh / "known_hosts").touch()
        self.path("bin").mkdir(exist_ok=True)
        wrapper = self.path("bin/ssh")
        shutil.copyfile(str(FILES / "ssh"), str(wrapper))
        wrapper.chmod(0o755)

    def forget(self):
        """Deletes A's and B's run state and keeps the builds, the cargo cache and the reports."""
        self.owned()
        if self.a_running() or self.container_state() != "absent":
            fail("run `rex-remote down` before `up --fresh`")
        for name in ("home-a", "a-namedrop", "a-rex", "ssh", "bin", "logs/ssh-calls.log",
                     "logs/a-server.log", "logs/a-bridge.log"):
            path = self.path(name)
            if path.is_dir():
                shutil.rmtree(str(path))
            elif path.exists():
                path.unlink()
        self.state = {key: value for key, value in self.state.items() if key == "built"}
        self.save()

    def up(self, checkout, rex_bin, b_version, fresh=False):
        self.claim()
        if fresh:
            self.forget()
        for name in ("logs", "home-a", "a-namedrop", "target-linux", "cargo-linux"):
            self.path(name).mkdir(parents=True, exist_ok=True)
        self.state["rex_bin"] = str(Path(rex_bin).resolve())
        a_version = rex_version(self.rex_bin(), self.path("home-a"))
        b_version = b_version or a_version
        self.state.update(a_version=a_version)
        self.save()
        self.ssh_files()
        commit = self.build(checkout)
        tag = self.image(b_version)
        if self.state.get("image") != tag and self.container_state() != "absent":
            say("B's Rex version changed; removing the old container")
            self.down_b()
        shown = self.container_state()
        if shown == "absent":
            ssh_port, http_port = free_port(), free_port()
            self.docker("run", "-d", "--name", self.container, "--hostname", LABEL,
                        "--label", "ndrp-rexlab=" + str(self.dir),
                        "-p", "127.0.0.1:{}:22".format(ssh_port), "-p", "127.0.0.1:{}:7297".format(http_port),
                        "-v", "{}:/lab/authorized_keys:ro".format(self.path("ssh/id_lab.pub")), tag)
            self.path("ssh/known_hosts").write_text("")
            self.state.update(image=tag, b_version=b_version, ssh_port=ssh_port, http_port=http_port)
            self.state.pop("b", None)
            self.save()
        elif shown.startswith("paused"):
            self.docker("unpause", self.container)
        elif not shown.startswith("running"):
            self.docker("start", self.container)
        self.wait_sshd()
        self.install_b_namedrop()
        self.start_b()
        self.start_a()
        self.list_b()
        self.name_b()
        self.name_a()
        self.pin_b()
        say("\nReady: lab {}\n  A: Rex {} on {} (Namedrop {})\n  B: Rex {} in container {}, "
            "ssh://{}@127.0.0.1:{} as label {}\n  @lead {} on A, @bterm {} on B".format(
                self.dir, a_version, self.a_socket, commit[:12], self.state["b_version"], self.container,
                B_USER, self.state["ssh_port"], LABEL, self.state["a"]["lead"], self.state["b"]["bterm"]))
        say("Run the checks with: ./testbed rex-remote check --dir " + shlex.quote(str(self.dir)))

    def b_endpoint(self):
        return "ssh://{}@127.0.0.1:{}".format(B_USER, self.state["ssh_port"])

    def list_b(self):
        """Lists B as `labb` in A's hosts at B's current port: added the first time, and edited
        when a new container has another port."""
        listed = hosts_by_label(self.ar_json("hosts", "list", "--json"))
        if LABEL not in listed:
            self.ar("hosts", "add", LABEL, self.b_endpoint(), check=True)
        elif listed[LABEL] != [self.b_endpoint()]:
            say("B moved to {}; editing {} in A's hosts".format(self.b_endpoint(), LABEL))
            self.ar("hosts", "edit", LABEL, "--endpoint", self.b_endpoint(), check=True)

    def peer_pin(self, label=LABEL):
        """The `host_id` A's relay last recorded for `label` in `peers`, or None."""
        path = self.path("a-namedrop/relay.sqlite3")
        if not path.exists():
            return None
        connection = sqlite3.connect("file:{}?mode=ro".format(path), uri=True, timeout=10)
        try:
            row = connection.execute("select host_id from peers where label = ?", (label,)).fetchone()
        finally:
            connection.close()
        return row[0] if row else None

    def pin_b(self, missing=False):
        """When A's relay still pins `labb` to the `host_id` of an earlier B, as after `down` or a
        new Rex version on B, or with `missing` when it pins none yet, sends one item to
        @bterm@labb, so A's relay worker takes B's `host_id` from a live `server.status`, the only
        place it takes one (ADAPTER.md, "Who B is"). Items still pinned to the old server then
        fail as `labb now answers as another Rex server`."""
        current = json.loads(self.ar("api", "call", "server.status", server=LABEL, check=True).stdout)["host_id"]
        pinned = self.peer_pin()
        if pinned == current or (pinned is None and not missing):
            return
        say("A's relay pins {} to host_id {}; sending one item so it takes B's {}".format(LABEL, pinned, current))
        a = self.state["a"]
        self.a_namedrop("send", "--json", "--space", a["session"], "--pane", a["lead"], "--input-json",
                        json.dumps({"request_id": "lab-pin-" + str(int(time.time())),
                                    "source": "@bterm@{} echo lab-pin".format(LABEL)}))
        deadline = time.monotonic() + 60
        while self.peer_pin() != current:
            if time.monotonic() >= deadline:
                fail("A's relay did not take B's host_id {} within 60 s".format(current))
            time.sleep(0.5)

    def wait_sshd(self):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            probe = run([str(self.path("bin/ssh")), "-p", str(self.state["ssh_port"]), "-o", "BatchMode=yes",
                         "-o", "ConnectTimeout=3", B_USER + "@127.0.0.1", "true"], check=False)
            if probe.returncode == 0:
                return
            time.sleep(0.5)
        fail("B's sshd does not admit the lab's key: " + probe.stderr)

    def new_b_session(self, label, program):
        created = json.loads(self.b_rex("new", label, "--shell", "none", "-c", B_HOME, "--json", "--", *program).stdout)
        return created["session_id"], created["initial_windows"][0]["block_ids"][0]

    def name_b(self):
        if self.state.get("b"):
            self.start_b_bridge()
            return
        session, block = self.new_b_session("nd-bterm", ["/bin/bash"])
        time.sleep(1)
        self.b_namedrop("name", "@bterm", "--type", "terminal", "--space", session, "--pane", block, "--human")
        self.bx("touch", B_STATE + "/relay/allow-terminals")
        self.state["b"] = {"session": session, "bterm": block}
        self.save()
        self.start_b_bridge()

    def a_has_block(self, block):
        resolved = json.loads(self.ar("api", "call", "target.resolve",
                                      json.dumps({"queries": [block], "kinds": ["block"]}), check=True).stdout)
        return any(result.get("status") == "resolved" for result in resolved.get("results") or [])

    def name_a(self):
        """Names @lead once. A's server restores its sessions after `down`, so a later `up` keeps
        the same @lead in the same block."""
        if self.state.get("a") and self.a_has_block(self.state["a"]["lead"]):
            self.start_a_bridge()
            return
        if self.state.get("a"):
            fail("A's server did not restore @lead's block {}; run `up --fresh`".format(self.state["a"]["lead"]))
        created = self.ar_json("new", "nd-lead", "--shell", "none", "-c", str(self.path("home-a")), "--json", "--", "/bin/bash")
        session, block = created["session_id"], created["initial_windows"][0]["block_ids"][0]
        time.sleep(1)
        self.a_namedrop("name", "@lead", "--type", "terminal", "--space", session, "--pane", block, "--human")
        self.state["a"] = {"session": session, "lead": block}
        self.save()
        self.start_a_bridge()

    def start_a_bridge(self):
        if self.a_pids("rex bridge"):
            return
        env = dict(self.a_env(), NAMEDROP_STATE_DIR=str(self.path("a-namedrop")))
        command = ["env", "-i"] + ["{}={}".format(*item) for item in sorted(env.items())]
        command += [str(self.path("target-mac/release/namedrop")), "rex", "bridge", "--endpoint", "rex:" + str(self.a_socket)]
        subprocess.run(["/bin/sh", "-c", '"$@" >> "$0" 2>&1 < /dev/null &', str(self.path("logs/a-bridge.log"))] + command,
                       check=True, start_new_session=True)

    def lab_processes(self):
        """The lab's processes on this machine, by pid, start time and command line: those whose
        command starts with A's Namedrop binary, the ssh runs that start with the lab's ssh config,
        and A's server as `start_a` recorded it. A process that only names the lab in its
        arguments, as a shell, an editor or `tail` may, is not one, and neither is this process or
        an ancestor of it."""
        table = processes()
        spared = lineage(table)
        namedrop = str(self.path("target-mac/release/namedrop"))
        ssh = "/usr/bin/ssh -F " + str(self.path("ssh/config")) + " "
        server = self.state.get("a_server") or {}
        found = {}
        for pid, (_, start, command) in table.items():
            ours = command == namedrop or command.startswith((namedrop + " ", ssh)) \
                or (pid == server.get("pid") and start == server.get("start") and command == server.get("command"))
            if ours and pid not in spared:
                found[pid] = (start, command)
        return found

    def a_pids(self, args=""):
        """A's Namedrop processes whose arguments start with `args`."""
        prefix = (str(self.path("target-mac/release/namedrop")) + " " + args).rstrip()
        return [pid for pid, (_, command) in self.lab_processes().items() if command.startswith(prefix)]

    def stop_lab_processes(self):
        """Sends SIGTERM to the lab's processes, then SIGKILL to those still running with the
        same pid, start time and command line after 10 s, and returns the pids it found."""
        found = self.lab_processes()
        pids = sorted(found)
        for number in (15, 9):
            found = signal_all(found, number)
            deadline = time.monotonic() + 10
            while found and time.monotonic() < deadline:
                time.sleep(0.2)
                now = processes()
                found = {pid: identity for pid, identity in found.items() if now.get(pid, (None,))[1:] == identity}
            if not found:
                return pids
        say("Still running after SIGKILL: {}".format(sorted(found)))
        return pids

    def remove_tunnel_dirs(self):
        """Rex keeps the local end of each ssh tunnel under /tmp/rex-UID/<hash>/ssh/. The lab's
        ssh log names the directories A's tunnels used, and each one that holds nothing but
        sockets, locks and directories is removed, then /tmp/rex-UID when it is empty."""
        calls = self.path("logs/ssh-calls.log")
        used = set(re.findall(r"-L (/tmp/rex-{}/[0-9a-f]+)/ssh/".format(os.getuid()),
                              calls.read_text())) if calls.exists() else set()
        for directory in sorted(Path(path) for path in used):
            entries = list(directory.rglob("*")) if directory.is_dir() else []
            if not directory.is_dir():
                continue
            if all(entry.is_dir() or entry.is_socket() or entry.suffix == ".lock" for entry in entries):
                shutil.rmtree(str(directory))
                say("Removed A's ssh tunnel directory " + str(directory))
                try:
                    directory.parent.rmdir()
                except OSError:
                    pass
            else:
                say("Kept {}: it holds more than tunnel sockets and locks".format(directory))

    def down_b(self):
        if self.container_state() != "absent":
            self.docker("rm", "-f", "-v", self.container)
            say("Removed container " + self.container)
        self.state.pop("b", None)
        self.save()

    def down(self):
        self.owned()
        stopped = self.stop_lab_processes()
        if stopped:
            say("Stopped the lab's processes {}".format(" ".join(map(str, stopped))))
        if self.a_socket.exists() and not self.a_running():
            self.a_socket.unlink()
        self.down_b()
        self.remove_tunnel_dirs()
        for key in ("a_server", "a_server_pid"):
            self.state.pop(key, None)
        self.save()
        say("The lab's files stay in {}; delete them with rm -rf {}".format(self.dir, shlex.quote(str(self.dir))))


def lab_dir(args):
    if args.dir:
        return Path(args.dir)
    home = Path(os.environ.get("TESTBED_HOME") or str(Path.home() / ".namedrop-testbed")).expanduser()
    return home / "rex-remote"


def default_rex():
    found = os.environ.get("REX_BIN") or shutil.which("rex")
    if not found:
        fail("the lab needs a macOS `rex`; pass --rex-bin PATH")
    return found


def main(args):
    lab = Lab(lab_dir(args))
    if args.action == "up":
        lab.up(Path(args.link or KIT.parent), args.rex_bin or lab.state.get("rex_bin") or default_rex(), args.b_rex,
               fresh=args.fresh)
    elif args.action == "check":
        lab.owned()
        import rex_remote_checks
        sys.exit(0 if rex_remote_checks.run(lab, args.only) else 1)
    else:
        lab.down()
