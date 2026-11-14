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

    def test_package_lock_pin_is_explicit_and_exact(self):
        prefix='## files (set)\n'
        def row(path,text):
            return json.dumps({"path":path,"text":text},sort_keys=True)+'\n'
        pin=row("<ROOT>/home/.config/namedrop/packages.lock", "")
        self.assertNotEqual(self.compare(prefix,prefix+pin),0)
        self.assertEqual(self.compare(prefix,prefix+pin,["--allow-package-lock"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<ROOT>/home/.config/namedrop/other", ""),["--allow-package-lock"]),0)
        self.assertNotEqual(self.compare(prefix,prefix+row("<ROOT>/home/.config/namedrop/packages.lock", "unexpected"),["--allow-package-lock"]),0)

if __name__ == '__main__':
    unittest.main()
