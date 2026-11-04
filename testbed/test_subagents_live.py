import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("subagents_live", Path(__file__).with_name("subagents-live.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class TranscriptEvidence(unittest.TestCase):
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
