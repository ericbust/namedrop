"""Prove U9's stub-host tests detect defects; restore each source in finally.

Run with no other test or builder using this checkout:
python3 -B -I tests/mutate_install_script.py OUTPUT
"""
import json
import os
from pathlib import Path
import subprocess
import sys

REPO = Path(__file__).resolve().parents[1]
MUTATIONS = [
    ('python-not-executed', 'scripts/install.sh',
     "if ! python3 -c 'import sys' >/dev/null 2>&1; then", 'if false; then',
     'test_python_stub_failure'),
    ('rex-not-checked', 'scripts/install.sh',
     'if ! command -v rex >/dev/null 2>&1; then', 'if false; then', 'test_missing_rex'),
    ('platform-not-checked', 'scripts/install.sh',
     "'Darwin arm64'|'Linux x86_64'|'Linux aarch64')", "*'unsupported'*|'Darwin arm64'|'Linux x86_64'|'Linux aarch64')",
     'test_platform_and_argument_refusal'),
    ('confirmation-skipped', 'scripts/install.sh', "if yes != 'yes':", 'if False:',
     'test_confirmation_is_once_and_read_only'),
    ('old-herdr-accepted', 'scripts/install.sh',
     'require(supported,', 'require(True,', 'test_old_herdr_refused_including_implicit_owner'),
    ('adoption-publishes', 'scripts/install.sh',
     "if decision == 'adopt':\n        command =", 'if False:\n        command =',
     'test_herdr_adoption_publishes_nothing'),
    ('handoff-accepted', 'scripts/install.sh',
     "require(reason != 'herdr_handoff',", 'require(True,',
     'test_retirement_precedes_owner_and_default_target_is_guarded'),
    ('stale-record-updated', 'scripts/install.sh',
     'recorded.is_absolute() and launcher.is_file() and os.access(launcher, os.X_OK)',
     'recorded.is_absolute()', 'test_archive_new_install_stale_record_and_rex_bin_dir'),
    ('failure-reported-as-success', 'scripts/install.sh',
     'sys.exit(result.returncode)', 'sys.exit(0)',
     'test_new_confirmation_default_home_and_publisher_failure'),
    ('archive-omits-installer', 'scripts/package-release.sh',
     'cp "$namedrop_root/scripts/install.sh" "$package/scripts/"', ':',
     'test_archive_new_install_stale_record_and_rex_bin_dir'),
    ('installer-not-checksummed', 'scripts/package-release.sh',
     'LICENSE scripts/setup.sh scripts/install.sh scripts/update.sh',
     'LICENSE scripts/setup.sh scripts/update.sh',
     'test_archive_new_install_stale_record_and_rex_bin_dir'),
]


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    environment = {k: v for k, v in os.environ.items()
                   if k not in {'REX_BLOCK', 'REX_SESSION', 'REX_SERVER', 'REX_CLIENT'}}
    results = []
    for name, source, old, replacement, test in MUTATIONS:
        target = REPO / source
        before = target.read_bytes()
        text = before.decode()
        assert text.count(old) == 1, (name, 'mutation anchor must be unique')
        command = [sys.executable, '-B', '-I', str(REPO / 'tests/install_script.py'), 'Installer.' + test]
        try:
            target.write_text(text.replace(old, replacement, 1))
            with (output / (name + '.log')).open('w') as log:
                process = subprocess.Popen(command, cwd=REPO, env=environment, stdout=log, stderr=subprocess.STDOUT)
                with (output / 'pids.jsonl').open('a') as record:
                    record.write(json.dumps(dict(mutation=name, pid=process.pid, command=command)) + '\n')
                code = process.wait()
            log = (output / (name + '.log')).read_text()
            killed = code != 0 and 'FAIL: ' + test in log and 'AssertionError' in log
            results.append(dict(mutation=name, source=source, test=test, exit=code, killed=killed))
            (output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
            print(name, 'killed' if killed else 'SURVIVED OR INVALID', flush=True)
            if not killed:
                raise SystemExit(1)
        finally:
            target.write_bytes(before)


if __name__ == '__main__':
    main()
