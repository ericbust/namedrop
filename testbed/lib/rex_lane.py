import sys

sys.dont_write_bytecode = True

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

SESSION = "testbed-herdr"


def exec_environment(base, inherited):
    environment = dict(base)
    environment.update((key, value) for key, value in inherited.items() if key.startswith("REX_"))
    return environment


def socket_for(directory):
    digest = hashlib.sha256(str(directory).encode()).hexdigest()[:16]
    return Path("/tmp/nd-tb-rex-{}-{}.sock".format(os.getuid(), digest))


def process_environment(pid):
    environ = Path("/proc/{}/environ".format(pid))
    if environ.exists():
        return environ.read_bytes().decode(errors="replace").split("\0")
    shown = subprocess.run(["ps", "-Eww", "-o", "command=", "-p", str(pid)], capture_output=True,
                           text=True, check=False).stdout
    return shown.split()


class Lane:
    def __init__(self, profile, fail, say):
        self.profile, self.fail, self.say = profile, fail, say
        self.dir = profile.dir / "rex"
        self.socket = socket_for(profile.dir)
        self.server = "unix://" + str(self.socket)

    def binary(self):
        found = self.profile.state.get("rex_bin") or shutil.which("rex")
        if not found:
            self.fail("--inside-rex needs the rex CLI on PATH")
        found = str(Path(found).resolve())
        self.profile.state["rex_bin"] = found
        return found

    def environment(self):
        env = self.profile.env
        return {"HOME": env["HOME"], "USER": env["USER"], "PATH": "/usr/bin:/bin",
                "LANG": env["LANG"], "REX_SERVER": self.server}

    def rex(self, *args, check=True):
        command = ["env", "-i"] + ["{}={}".format(*item) for item in sorted(self.environment().items())]
        command += [self.binary(), "-S", self.server, "--autostart=false"] + list(args)
        result = subprocess.run(command, text=True, capture_output=True, check=False)
        if check and result.returncode != 0:
            self.fail("rex {} failed: {}".format(" ".join(args), result.stderr or result.stdout))
        return result

    def running(self):
        return self.socket.exists() and self.rex("server", "status", check=False).returncode == 0

    def start_server(self):
        if self.running():
            return
        if self.socket.exists():
            self.fail("the scratch Rex socket exists but cannot be reached; inspect it: " +
                      str(self.socket))
        (self.dir / "config").mkdir(parents=True, exist_ok=True, mode=0o700)
        config = self.dir / "config/init.lua"
        config.write_text("")
        log = self.profile.dir / "logs/rex-server.log"
        command = ["env", "-i"] + ["{}={}".format(*item) for item in sorted(self.environment().items())]
        command += [self.binary(), "server", "run", "--listen", self.server,
                    "--state-dir", str(self.dir / "state"), "--config", str(config)]
        self.say("Starting a scratch Rex server on " + str(self.socket))
        # `rex server stop` waits for the server's pid to exit, which a zombie child of this
        # process never does, so the server starts from a shell that exits at once.
        detach = ["/bin/sh", "-c", '"$@" >> "$0" 2>&1 < /dev/null &', str(log)] + command
        subprocess.run(detach, cwd=str(self.profile.dir), check=True, start_new_session=True)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.running():
                return
            time.sleep(0.1)
        self.fail("the scratch Rex server did not become ready\n" + log.read_text())

    def sessions(self):
        listed = json.loads(self.rex("list", "--json").stdout)
        return [session["session_id"] for session in listed.get("sessions") or []
                if session.get("label") == SESSION]

    def start_herdr(self, python, log):
        self.start_server()
        for stale in self.sessions():
            self.rex("kill", stale)
        environment = self.dir / "herdr-environment.json"
        environment.write_text(json.dumps(self.profile.env, sort_keys=True) + "\n")
        environment.chmod(0o600)
        created = json.loads(self.rex(
            "new", SESSION, "--shell", "none", "-c", str(self.profile.dir), "--json", "--",
            python, "-I", str(Path(__file__).resolve()), "exec", str(environment), str(log),
            "--", "herdr", "server").stdout)
        session = created["session_id"]
        block = created["initial_windows"][0]["block_ids"][0]
        self.profile.state.update(rex_session=session, rex_block=block)
        self.profile.state.pop("rex_proof", None)
        self.profile.save()
        return session, block

    def prove_herdr_in_block(self):
        session, block = self.profile.state["rex_session"], self.profile.state["rex_block"]
        process = json.loads(self.rex("block", "call", "-s", session, "-b", block, "process").stdout)
        child = process.get("child") or {}
        if child.get("name") != "herdr":
            self.fail("the Rex block runs {!r}, not the Herdr server".format(child))
        if "REX_BLOCK=" + block not in process_environment(child["pid"]):
            self.fail("the Herdr server {} does not carry REX_BLOCK={}".format(child["pid"], block))
        detail = "Herdr server {} is the program of Rex block {} and carries its REX_BLOCK".format(
            child["pid"], block)
        self.profile.state["rex_proof"] = detail
        self.profile.save()
        return detail

    def stop(self):
        if not self.running():
            return
        for session in self.sessions():
            self.rex("kill", session)
        self.rex("server", "stop")
        deadline = time.monotonic() + 10
        while self.socket.exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        if self.socket.exists():
            self.fail("the scratch Rex socket remains; inspect it: " + str(self.socket))
        self.say("Stopped the scratch Rex server of " + self.profile.name)


def main(arguments):
    if len(arguments) < 5 or arguments[0] != "exec" or arguments[3] != "--":
        sys.exit("usage: rex_lane.py exec ENVIRONMENT LOG -- COMMAND...")
    base = json.loads(Path(arguments[1]).read_text())
    log = os.open(arguments[2], os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    for descriptor in (1, 2):
        os.dup2(log, descriptor)
    command = arguments[4:]
    os.execvpe(command[0], command, exec_environment(base, os.environ))


if __name__ == "__main__":
    main(sys.argv[1:])
