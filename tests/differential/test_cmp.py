import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

class Comparison(unittest.TestCase):
    def compare(self, left, right, flags=()):
        with tempfile.TemporaryDirectory() as directory:
            a, b = Path(directory) / "a", Path(directory) / "b"
            a.write_text(left)
            b.write_text(right)
            return subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("cmp.py")), str(a), str(b), *flags], capture_output=True).returncode

    def test_scrubbed_environment_is_ignored_but_both_timeouts_are_compared(self):
        def report(env):
            return '## spawns (set)\n' + json.dumps({"args": ["flush"], "env": env}) + '\n'
        self.assertEqual(self.compare(report([]), report([["REX_BLOCK", "noise"]])), 0)
        for name in ["NAMEDROP_REX_TIMEOUT_MS", "NAMEDROP_HERDR_TIMEOUT_MS"]:
            self.assertNotEqual(self.compare(report([[name, "1"]]), report([[name, "2"]])), 0)

    def test_only_the_documented_transient_badge_is_ignored(self):
        prefix = '## herdr calls (set)\n'
        def call(attention):
            return json.dumps({"args": ["pane", "report-metadata", "p1", "--source", "namedrop", "--token", "namedrop-awaiting=4", "--token", "namedrop-attention=" + attention], "env": []}) + '\n'
        self.assertEqual(self.compare(prefix, prefix + call("2")), 0)
        self.assertNotEqual(self.compare(prefix, prefix + call("1")), 0)

    def test_file_contents_and_stderr_are_compared(self):
        for prefix in ['## files (set)\n', '## activate\n--stderr\n']:
            self.assertEqual(self.compare(prefix + 'same\n', prefix + 'same\n'), 0)
            self.assertNotEqual(self.compare(prefix + 'before\n', prefix + 'after\n'), 0)

    def test_a_file_s_loopback_port_is_normalized(self):
        prefix='## files (set)\n'
        def row(port,pid):
            text=json.dumps({"pid":pid,"port":port},separators=(",",":"))
            return json.dumps({"path":"<STATE>/dashboard/server.json","text":text},sort_keys=True)+'\n'
        self.assertEqual(self.compare(prefix+row(52285,"<PID>"),prefix+row(53111,"<PID>")),0)
        self.assertNotEqual(self.compare(prefix+row(52285,"<PID>"),prefix+row(52285,"other")),0)

    def test_package_lock_pin_is_explicit_and_exact(self):
        prefix='## files (set)\n'
        def row(path,text,mode="0o600"):
            return json.dumps({"mode":mode,"path":path,"text":text},sort_keys=True)+'\n'
        pin=row("<ROOT>/home/.config/namedrop/packages.lock", "")
        self.assertNotEqual(self.compare(prefix,prefix+pin),0)
        self.assertEqual(self.compare(prefix,prefix+pin,["--allow-package-lock"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<ROOT>/home/.config/namedrop/other", ""),["--allow-package-lock"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<ROOT>/home/.config/namedrop/packages.lock", "unexpected"),["--allow-package-lock"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<ROOT>/home/.config/namedrop/packages.lock", "", "0o644"),["--allow-package-lock"]),0)

    def test_a_step_s_lines_compare_in_order_under_their_stream(self):
        def step(stdout, stderr):
            return '## activate\nexit=0\n--stdout\n' + ''.join(line + '\n' for line in stdout) + '--stderr\n' + ''.join(line + '\n' for line in stderr)
        self.assertEqual(self.compare(step(['a', 'b'], []), step(['a', 'b'], [])), 0)
        self.assertNotEqual(self.compare(step(['a', 'b'], []), step(['a'], ['b'])), 0, "a line moved to stderr")
        self.assertNotEqual(self.compare(step(['a', 'b'], []), step(['a', 'b', 'b'], [])), 0, "a line printed twice")
        self.assertNotEqual(self.compare(step(['a', 'b'], []), step(['b', 'a'], [])), 0, "lines reordered")

    def test_a_file_s_mode_is_compared(self):
        prefix = '## files (set)\n'
        def row(mode):
            return json.dumps({"mode": mode, "path": "<HOME>/.local/bin/namedrop", "text": "x"}, sort_keys=True) + '\n'
        self.assertEqual(self.compare(prefix + row("0o755"), prefix + row("0o755")), 0)
        self.assertNotEqual(self.compare(prefix + row("0o755"), prefix + row("0o644")), 0)

    def test_runtime_owner_pin_is_explicit_and_exact(self):
        prefix='## files (set)\n'
        def row(path,text,mode="0o600"):
            return json.dumps({"mode":mode,"path":path,"text":text},sort_keys=True)+'\n'
        pin=row("<STATE>-runtime/runtime/owner", "herdr\n")
        self.assertNotEqual(self.compare(prefix,prefix+pin),0)
        self.assertEqual(self.compare(prefix,prefix+pin,["--allow-runtime-owner"]),0)
        self.assertNotEqual(self.compare(prefix,prefix,["--allow-runtime-owner"]),0)
        self.assertNotEqual(self.compare(prefix+pin,prefix+pin,["--allow-runtime-owner"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<STATE>-runtime/runtime/owner", "standalone\n"),["--allow-runtime-owner"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<STATE>/runtime/owner", "herdr\n"),["--allow-runtime-owner"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<STATE>-runtime/runtime/owner", "herdr\n", "0o644"),["--allow-runtime-owner"]),0)

    def test_rex_ui_pin_is_explicit_and_exact(self):
        base = {"runtime_protocol": 1, "helper_gate": 1}
        def info(value):
            return '## runtime-info\n--stdout\n' + json.dumps(value, separators=(",", ":")) + '\n'
        def copied(value, path="<RELEASE>/runtime.json"):
            return '## files (set)\n' + json.dumps({"mode": "0o644", "path": path, "text": json.dumps(value)}, sort_keys=True) + '\n'
        gained = dict(base, rex_ui=1)
        for report in (info, copied):
            self.assertNotEqual(self.compare(report(base), report(gained)), 0)
            self.assertEqual(self.compare(report(base), report(gained), ["--allow-rex-ui"]), 0)
            for other in (dict(base, rex_ui=2), dict(base, rex_ui=True), dict(base, rex_ui="1"), dict(gained, extra=1)):
                self.assertNotEqual(self.compare(report(base), report(other), ["--allow-rex-ui"]), 0, other)
        self.assertNotEqual(self.compare(copied(base, "<STATE>/other.json"), copied(gained, "<STATE>/other.json"), ["--allow-rex-ui"]), 0)

if __name__ == '__main__':
    unittest.main()
