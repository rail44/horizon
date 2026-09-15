import json
import tempfile
import unittest
from pathlib import Path
from import_legacy import SCHEMA, fold_legacy, extract


class ImportTests(unittest.TestCase):
    def test_snapshots_later_edits_duplicate_messages_and_high_water(self):
        comment = {"author": "owner", "text": "same", "at": 42}
        child = {"id": 8, "title": "batch child", "body": "body", "rank": "an", "parent": 1,
                 "status": "consulting", "comments": [comment, comment],
                 "workflow": {"plan": {"decisions": [{"messages": [
                     {"owner": True, "text": "actual answer"},
                     {"owner": False, "text": "agent judgment"}], "resolution": "machine resolution"}]}}}
        events = [
            {"type": "item-created", "id": 1, "title": "parent", "body": "parent body", "rank": "n"},
            {"type": "workflow-batch", "id": 8, "items": [child]},
            {"type": "item-updated", "id": 8, "body": "later body"},
            {"type": "comment-added", "id": 8, "author": "owner", "text": "same", "at": 43},
            {"type": "future-type", "id": 900},
        ]
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "legacy.jsonl"
            source.write_text("".join(json.dumps({"schema": SCHEMA, "version": 1, "at": 1, **e}) + "\n" for e in events))
            items, high_water, fingerprint = fold_legacy(source)
            tasks = extract(items, {1, 8}, fingerprint)
            self.assertEqual(high_water, 900)
            child = tasks[1]
            self.assertEqual(child["body"], "later body")
            self.assertEqual(child["rank"], "an")
            self.assertEqual(child["parent"], 1)
            self.assertEqual([m["text"] for m in child["comments"]], ["same"] * 3 + ["actual answer", "agent judgment"])
            self.assertEqual(len({m["id"] for m in child["comments"]}), 5)
            self.assertIsNone(child["comments"][-1]["at"])
            self.assertNotIn("workflow", child)
            self.assertIsNone(child["session_id"])
            self.assertEqual(tasks, extract(items, {1, 8}, fingerprint))
            with self.assertRaisesRegex(ValueError, "unselected"):
                extract(items, {8}, fingerprint)

    def test_selection_excludes_generated_records(self):
        items = {1: {"id": 1, "rank": "n", "title": "retain"},
                 2: {"id": 2, "rank": "o", "title": "generated", "workflow": {"active": {}}}}
        self.assertEqual([i["id"] for i in extract(items, {1}, "source")], [1])


if __name__ == "__main__":
    unittest.main()
