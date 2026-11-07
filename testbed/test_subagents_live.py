import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("subagents_live", Path(__file__).with_name("subagents-live.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class TranscriptEvidence(unittest.TestCase):
    def native_child_records(self):
        fixture = json.loads((Path(__file__).parent / "fixtures/codex-0.164.0-child-context.json").read_text())
        return [{"type": "response_item", "payload": {
            "type": "message", "role": "developer", "content": [{"text": text}],
        }} for text in fixture["contexts"]]

    def test_codex_0164_native_child_contexts_are_allowed(self):
        records = self.native_child_records()
        self.assertEqual(probe.unexpected_contexts_in(records, set(), codex_child_version="0.164.0"), [])

    def test_native_child_allowance_still_rejects_arbitrary_hook_stdout(self):
        records = self.native_child_records()
        leaked = {"type": "response_item", "payload": {
            "type": "message", "role": "developer", "content": [{"text": "hook leaked text"}],
        }}
        self.assertEqual(probe.unexpected_contexts_in(records + [leaked], set(), codex_child_version="0.164.0"),
                         ["hook leaked text"])

    def test_native_child_contexts_are_rejected_in_unaccounted_parent_context(self):
        records = self.native_child_records()
        self.assertEqual(probe.unexpected_contexts_in(records, set()), probe.contexts_in(records))

    def test_native_child_contexts_are_rejected_for_other_codex_versions(self):
        records = self.native_child_records()
        for version in ("0.163.0", "0.164.1", "unknown"):
            with self.subTest(version=version):
                self.assertEqual(probe.unexpected_contexts_in(records, set(), codex_child_version=version),
                                 probe.contexts_in(records))

    def test_native_child_contexts_reject_changed_or_appended_hook_text(self):
        for record in self.native_child_records():
            original = probe.contexts_in([record])[0]
            for change, text in (("appended", original + "\nhook leaked text"), ("changed", "X" + original[1:])):
                with self.subTest(context=original[:60], change=change):
                    changed = {"type": "response_item", "payload": {
                        "type": "message", "role": "developer", "content": [{"text": text}],
                    }}
                    self.assertEqual(probe.unexpected_contexts_in([record, changed], set(), codex_child_version="0.164.0"),
                                     [text])

    def test_codex_arbitrary_hook_stdout_is_rejected_without_event_keywords(self):
        record = {
            "type": "response_item",
            "payload": {"type": "message", "role": "developer", "content": [{"text": "hook leaked text"}]},
        }
        self.assertEqual(probe.unexpected_contexts_in([record], set()), ["hook leaked text"])

    def test_claude_arbitrary_additional_context_is_rejected(self):
        record = {"type": "attachment", "attachment": {"type": "hook_additional_context", "content": ["arbitrary sentinel"]}}
        self.assertEqual(probe.unexpected_contexts_in([record], set()), ["arbitrary sentinel"])

    def test_existing_context_is_allowed_but_appended_output_is_not(self):
        expected = {probe.normalized_context("Namedrop roster. Agent available now")}
        changed = {"type": "attachment", "attachment": {
            "type": "hook_additional_context",
            "content": ["Namedrop roster. Agent busy; work queues until its session settles", "unattributed stdout"],
        }}
        self.assertEqual(probe.unexpected_contexts_in([changed], expected), ["unattributed stdout"])

    def test_child_contexts_use_the_same_strict_check(self):
        inherited = {"type": "response_item", "payload": {
            "type": "message", "role": "developer", "content": [{"text": "existing instructions"}],
        }}
        leaked = {"type": "response_item", "payload": {
            "type": "message", "role": "developer", "content": [{"text": "child leaked text"}],
        }}
        self.assertEqual(probe.unexpected_contexts_in([inherited, leaked], {"existing instructions"}), ["child leaked text"])


if __name__ == "__main__":
    unittest.main()
