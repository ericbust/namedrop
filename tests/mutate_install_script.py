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
     "if ! python3 -B -I -c 'import sys' >/dev/null 2>&1; then", 'if false; then',
     'test_python_stub_failure'),
    ('rex-not-checked', 'scripts/install.sh',
     'if ! command -v rex >/dev/null 2>&1; then', 'if false; then', 'test_missing_rex'),
    ('platform-not-checked', 'scripts/install.sh',
     "'Darwin arm64'|'Linux x86_64'|'Linux aarch64')", "*'unsupported'*|'Darwin arm64'|'Linux x86_64'|'Linux aarch64')",
     'test_platform_and_argument_refusal'),
    ('confirmation-skipped', 'scripts/install.sh', "if yes != 'yes':", 'if False:',
     'test_confirmation_is_once_and_read_only'),
    ('old-herdr-accepted', 'scripts/install.sh',
     'require(supports_rex(launcher),', 'require(True,', 'test_old_herdr_refused_including_implicit_owner'),
    ('adoption-publishes', 'scripts/install.sh',
     "if decision == 'adopt':\n        command =", 'if False:\n        command =',
     'test_herdr_adoption_publishes_nothing'),
    ('handoff-accepted', 'scripts/install.sh',
     "require(reason != 'herdr_handoff',", 'require(True,',
     'test_retirement_precedes_owner_and_default_target_is_guarded'),
    ('stale-record-updated', 'scripts/install.sh',
     "    try:\n        launcher = os.lstat(state / 'runtime/namedrop')\n    except OSError:\n        return None\n",
     "    return state\n",
     'test_archive_new_install_stale_record_and_rex_bin_dir'),
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
    # The fix round's: the nine the review's mutants left alive (m1-m6,
    # m9, m10, m12), then one for each fix.
    ('m1-bytecode-written', 'scripts/install.sh', 'export PYTHONDONTWRITEBYTECODE=1\n', '',
     'test_archive_new_install_stale_record_and_rex_bin_dir'),
    ('m2-rex-bin-dir-not-on-path', 'scripts/install.sh', '    PATH="$REX_BIN_DIR:$PATH"\n    export PATH\n', '    :\n',
     'test_archive_new_install_stale_record_and_rex_bin_dir'),
    ('m3-herdr-default-left-to-runtime-py', 'scripts/install.sh',
     "require(recorded or selected_owner != 'herdr',", 'require(True,',
     'test_retirement_precedes_owner_and_default_target_is_guarded'),
    ('m4-launcher-mode-ignored', 'scripts/install.sh',
     'stat.S_ISREG(launcher.st_mode) and launcher.st_mode & 0o111', 'stat.S_ISREG(launcher.st_mode)',
     'test_record_is_read_as_the_binary_reads_it'),
    ('m5-any-retirement-reason', 'scripts/install.sh', "require(reason == 'removed',", 'require(True,',
     'test_retirement_precedes_owner_and_default_target_is_guarded'),
    ('m6-report-not-validated', 'scripts/install.sh',
     "require(report.get('status') == 'ok' and report.get('operation') == 'install',", 'require(True,',
     'test_report_is_validated_and_manual_config_is_printed'),
    ('m9-relative-home', 'scripts/install.sh',
     "require(home.is_absolute(), 'Namedrop install needs an absolute HOME.')", 'pass',
     'test_home_must_be_set_and_absolute'),
    ('m10-no-cancel-message', 'scripts/install.sh',
     "            print('Installation cancelled; no files changed.')", '            pass',
     'test_confirmation_is_once_and_read_only'),
    ('m12-manual-config-dropped', 'scripts/install.sh', "if data.get('manual_config'):", 'if False:',
     'test_report_is_validated_and_manual_config_is_printed'),
    ('u8-manual-reason-dropped', 'scripts/install.sh', "        if data.get('reason'):", '        if False:',
     'test_report_is_validated_and_manual_config_is_printed'),
    ('f2-confirmation-omits-targets', 'scripts/install.sh',
     "    show_targets(plan['targets'])\n", '',
     'test_confirmation_is_once_and_read_only'),
    ('f2-report-omits-targets', 'scripts/install.sh',
     "show_targets(data.get('targets') or plan['targets'])", 'show_targets([])',
     'test_herdr_adoption_publishes_nothing'),
    ('f3-no-server-ignored', 'scripts/install.sh', 'if preview.returncode:', 'if False:',
     'test_no_rex_server_stops_before_any_write'),
    ('f4-bytecode-under-isolation', 'scripts/install.sh',
     'python3 -B -I - "$namedrop_root"', 'python3 -I - "$namedrop_root"',
     'test_confirmation_is_once_and_read_only'),
    ('f6-keys-as-json', 'scripts/install.sh',
     """print(f"  {key.get('key')}: {key.get('action')} ({effect})")""", 'print(json.dumps(key))',
     'test_herdr_adoption_publishes_nothing'),
    ('f6-path-hint-dropped', 'scripts/install.sh', "if shell.get('next_step'):", 'if False:',
     'test_herdr_adoption_publishes_nothing'),
    ('f6-progress-captured', 'scripts/install.sh',
     'stdout=subprocess.PIPE, text=True, stdin=subprocess.DEVNULL', 'capture_output=True, text=True',
     'test_archive_new_install_stale_record_and_rex_bin_dir'),
    ('f7-closed-stdin-duplicated', 'scripts/install.sh',
     'if (exec 9<&0) 2>/dev/null; then exec 3<&0; else exec 3</dev/null; fi', 'exec 3<&0',
     'test_closed_stdin'),
    ('f8-interrupt-uncaught', 'scripts/install.sh', 'except KeyboardInterrupt:', 'except ZeroDivisionError:',
     'test_interrupt_at_the_prompt'),
    ('herdr-without-record-ignored', 'scripts/install.sh',
     "if published and herdr_lists_namedrop(home, herdr)[0] and owner(herdr, \"Herdr's Namedrop state\") == 'herdr':",
     'if False:', 'test_herdr_without_a_record_is_refused'),
    # The pre-release round's, for the U9 review's N1.
    ('n1-registry-ignored', 'scripts/install.sh', 'if published and herdr_lists_namedrop(home, herdr)[0] and',
     'if published and', 'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('n1-any-plugin-counts', 'scripts/install.sh', "plugin.get('plugin_id') == 'namedrop'", 'True',
     'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('n1-registry-xdg-ignored', 'scripts/install.sh', "(home / '.config' if config is None else Path(config))",
     "(home / '.config')", 'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('n1-unreadable-registry-as-empty', 'scripts/install.sh',
     "    except (OSError, ValueError, RecursionError):\n        # Python 3.9, macOS's own, parses deep nesting by recursion.\n        plugins = None\n",
     "    except (OSError, ValueError, RecursionError):\n        # Python 3.9, macOS's own, parses deep nesting by recursion.\n        plugins = []\n",
     'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('n1-missing-registry-refused', 'scripts/install.sh', 'plugins = None if relative else []',
     'plugins = None', 'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('n1-registry-read-without-a-state', 'scripts/install.sh', 'if published and herdr_lists_namedrop',
     'if herdr_lists_namedrop', 'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('n1-lost-record-told-update', 'scripts/install.sh', "if supports_rex(herdr / 'runtime/namedrop'):", 'if False:',
     'test_a_current_herdr_whose_record_was_lost_is_told_to_activate'),
    ('n1-old-herdr-told-activate', 'scripts/install.sh', "if supports_rex(herdr / 'runtime/namedrop'):", 'if True:',
     'test_a_current_herdr_whose_record_was_lost_is_told_to_activate'),
    ('n1-owner-raw-errno', 'scripts/install.sh',
     "    except (OSError, UnicodeDecodeError):\n        raise ValueError(f'Namedrop cannot read the owner",
     "    except ZeroDivisionError:\n        raise ValueError(f'Namedrop cannot read the owner",
     'test_an_owner_that_cannot_be_read_is_named'),
    ('n1-owner-not-utf8-raw', 'scripts/install.sh',
     "    except (OSError, UnicodeDecodeError):\n        raise ValueError(f'Namedrop cannot read the owner",
     "    except OSError:\n        raise ValueError(f'Namedrop cannot read the owner",
     'test_an_owner_that_cannot_be_read_is_named'),
    ('nit-current-directory-imported', 'scripts/install.sh',
     'python3 -B -I - "$namedrop_root"', 'python3 -B - "$namedrop_root"',
     'test_current_directory_is_not_imported'),
    ('nit-record-stripped', 'scripts/install.sh',
     "text[:-1] if text.endswith('\\n') else text", 'text.strip()',
     'test_record_is_read_as_the_binary_reads_it'),
    ('nit-launcher-link-followed', 'scripts/install.sh',
     "os.lstat(state / 'runtime/namedrop')", "os.stat(state / 'runtime/namedrop')",
     'test_record_is_read_as_the_binary_reads_it'),
    ('nit-record-not-utf8-stops', 'scripts/install.sh',
     'except (OSError, UnicodeDecodeError):\n        return None', 'except OSError:\n        return None',
     'test_record_is_read_as_the_binary_reads_it'),
    ('nit-home-unset-unexplained', 'scripts/install.sh',
     "require(os.environ.get('HOME'), 'Namedrop install needs HOME set to your home directory.')", 'pass',
     'test_home_must_be_set_and_absolute'),
    # Pre-release round C, for round A's review.
    ('c1-recorded-herdr-list-ignored', 'scripts/install.sh', 'require(listed,',
     'require(True,', 'test_a_recorded_herdr_state_is_adopted_only_while_herdr_lists_namedrop'),
    ('c1-recorded-herdr-list-of-the-default-state', 'scripts/install.sh',
     'listed, unlisted = herdr_lists_namedrop(home, state)',
     "listed, unlisted = herdr_lists_namedrop(home, home / '.local/state/herdr/plugins/namedrop')",
     'test_a_recorded_herdr_state_is_adopted_only_while_herdr_lists_namedrop'),
    ('c2-xdg-config-absolute-only', 'scripts/install.sh',
     "(home / '.config' if config is None else Path(config))",
     "(Path(config) if config and Path(config).is_absolute() else home / '.config')",
     'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('c2-relative-missing-list-as-no-plugins', 'scripts/install.sh',
     'plugins = None if relative else []', 'plugins = []',
     'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    ('c7-deep-list-traceback', 'scripts/install.sh', 'except (OSError, ValueError, RecursionError):',
     'except (OSError, ValueError):', 'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
    # Its fix round, for round C's review: name the list read, and the way
    # out for a Herdr that reads another one first.
    ('cf2-unlisted-names-the-default-list', 'scripts/install.sh',
     'return listed, f"Herdr\'s plugin list in {registry} does not name',
     'return listed, f"Herdr\'s plugin list in {home}/.config/herdr/plugins.json does not name',
     'test_a_recorded_herdr_state_is_adopted_only_while_herdr_lists_namedrop'),
    ('cf2-unlisted-other-config-way-dropped', 'scripts/install.sh',
     'does not name Namedrop.{elsewhere}"', 'does not name Namedrop."',
     'test_a_recorded_herdr_state_is_adopted_only_while_herdr_lists_namedrop'),
    ('cf3-relative-note-dropped', 'scripts/install.sh',
     '"directory, which may not be Herdr\'s." if relative else \'\')',
     '"directory, which may not be Herdr\'s." if False else \'\')',
     'test_a_recorded_herdr_state_is_adopted_only_while_herdr_lists_namedrop'),
    ('cf3-unreadable-other-config-way-dropped', 'scripts/install.sh',
     "({herdr}).{elsewhere} Otherwise", '({herdr}). Otherwise',
     'test_herdr_state_counts_only_while_herdr_lists_namedrop'),
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
