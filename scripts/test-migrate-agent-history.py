#!/usr/bin/env python3
"""Tests for one-time history conversion; no running daemon or user data."""

import importlib.util
import json
from pathlib import Path
import tempfile
import sys
import unittest

spec = importlib.util.spec_from_file_location(
    "migration", Path(__file__).with_name("migrate-agent-history.py")
)
migration = importlib.util.module_from_spec(spec)
sys.dont_write_bytecode = True
spec.loader.exec_module(migration)


def record(sequence, event, session="session-a", version=1):
    return {"schema": migration.SCHEMA, "version": version,
            "event_id": f"event-{sequence}", "sequence": sequence,
            "session_id": session, "turn_id": "turn-a", "provider_id": "rig",
            "role_id": None, "session_context": None,
            "event_kind": next(iter(event)) if isinstance(event, dict) else event,
            "event": event, "provider_payload": {"original": True},
            "created_at_unix_ms": 1000 + sequence}


def request(call="call-a", occurrence=None):
    return {"ToolCallRequested": {"call_id": call, "occurrence_id": occurrence,
                                  "tool_id": "bash", "input": {"command": "pwd"}}}


def result(call="call-a", occurrence=None):
    return {"ToolCallFinished": {"call_id": call, "occurrence_id": occurrence,
                                 "output": {"ok": True}, "is_error": False,
                                 "denied": False}}


def convert(records):
    return migration.convert_bytes(migration.jsonl(records).encode())


class HistoryConversionTests(unittest.TestCase):
    def test_unambiguous_identity_and_envelope_survive_conversion_and_replay(self):
        source = [record(10, request()), record(11, {"ToolCallStarted": "call-a"}),
                  record(12, result())]
        converted, archived, manifest = convert(source)
        self.assertFalse(archived)
        self.assertFalse(manifest["archived_sessions"])
        identities = [next(iter(r["event"].values()))["occurrence_id"] for r in converted]
        self.assertEqual(len(set(identities)), 1)
        for old, new in zip(source, converted):
            for key in old.keys() - {"version", "event"}:
                self.assertEqual(old[key], new[key])
        self.assertEqual(convert(converted)[0], converted)
        self.assertEqual(convert(source)[0], converted)

    def test_reused_call_ids_are_distinct_after_each_attempt_finishes(self):
        converted, archived, _ = convert([
            record(1, request()), record(2, result()),
            record(3, request()), record(4, result()),
        ])
        self.assertFalse(archived)
        first, first_result, second, second_result = [
            next(iter(r["event"].values()))["occurrence_id"] for r in converted]
        self.assertEqual(first, first_result)
        self.assertEqual(second, second_result)
        self.assertNotEqual(first, second)

    def test_ambiguous_result_archives_whole_session_without_affecting_others(self):
        source = [record(1, request()), record(2, request()), record(3, result()),
                  record(4, {"TurnEnded": "Completed"}, session="session-b")]
        converted, archived, manifest = convert(source)
        self.assertEqual([r["sequence"] for r in converted], [4])
        self.assertEqual(archived, source[:3])
        self.assertIn("ambiguous", manifest["archived_sessions"]["session-a"])

    def test_explicit_retry_identity_does_not_get_attributed_to_latest_request(self):
        source = [record(1, request(occurrence="first")),
                  record(2, request(occurrence="second")),
                  record(3, result(occurrence="first")),
                  record(4, result(occurrence="second"))]
        converted, archived, _ = convert(source)
        self.assertFalse(archived)
        self.assertEqual(converted[2]["event"]["ToolCallFinished"]["occurrence_id"], "first")

    def test_nested_prior_result_proves_which_retry_starts(self):
        approval = {"ApprovalRequested": {
            "call_id": "call-a", "occurrence_id": "second", "reason": "retry",
            "kind": {"DomainDenialRetry": {
                "domains": ["example.test"],
                "prior_result": result(occurrence="first")["ToolCallFinished"]}}}}
        converted, archived, _ = convert([
            record(1, request(occurrence="first")),
            record(2, request(occurrence="second")), record(3, approval),
            record(4, {"ToolCallStarted": "call-a"})])
        self.assertFalse(archived)
        self.assertEqual(converted[-1]["event"]["ToolCallStarted"]["occurrence_id"], "second")
        prior = converted[2]["event"]["ApprovalRequested"]["kind"]["DomainDenialRetry"]["prior_result"]
        self.assertEqual(prior["outcome"], "Succeeded")

    def test_retired_variants_are_archived_without_inventing_reason_or_authority(self):
        for event in ({"TurnEnded": "Halted"},
                      {"ContinueTurnRequested": {"resumed_from": "Halted"}},
                      {"ApprovalRequested": {"call_id": "call-a", "kind": "SandboxDenialRetry"}}):
            with self.subTest(event=event):
                converted, archived, manifest = convert([record(1, request()), record(2, event)])
                self.assertFalse(converted)
                self.assertEqual(len(archived), 2)
                self.assertTrue(manifest["archived_sessions"])

    def test_orphan_result_and_duplicate_occurrence_are_archived(self):
        for events in ([result()], [request(occurrence="same"), request(occurrence="same")],
                       [request(), result(occurrence="unknown")]):
            converted, archived, _ = convert([record(i, event) for i, event in enumerate(events)])
            self.assertFalse(converted)
            self.assertEqual(len(archived), len(events))

    def test_corruption_and_duplicates_abort_instead_of_silently_losing_data(self):
        for source in (b"{incomplete", b"not json\n",
                       migration.jsonl([record(1, request()), record(1, result())]).encode(),
                       migration.jsonl([record(1, request(), version=99)]).encode()):
            with self.subTest(source=source), self.assertRaises(migration.ConversionError):
                migration.convert_bytes(source)

    def test_v2_outcomes_and_nested_retry_are_explicit_and_idempotent(self):
        source = []
        expected = ["Succeeded", "Failed", "Denied", "Cancelled",
                    {"Superseded": {"retry_occurrence_id": "retry"}}]
        outputs = [{"ok": True}, {"message": "denied by user"}, {"is_error": False},
                   {"cancelled": True}, {"superseded_by_retry": True,
                                        "retry_occurrence_id": "retry", "message": "replaced"}]
        for i, output in enumerate(outputs):
            call = f"call-{i}"
            occurrence = f"attempt-{i}"
            source.append(record(len(source), request(call, occurrence), version=2))
            if i == 4:
                source.append(record(len(source), request(call, "retry"), version=2))
            event = result(call, occurrence)
            event["ToolCallFinished"].update(output=output, is_error=i in (1, 2), denied=i == 2)
            source.append(record(len(source), event, version=2))
        converted, archived, _ = convert(source)
        self.assertFalse(archived)
        results = [r["event"]["ToolCallFinished"] for r in converted if "ToolCallFinished" in r["event"]]
        self.assertEqual([r["outcome"] for r in results], expected)
        self.assertEqual(results[-1]["output"], {"message": "replaced"})
        self.assertTrue(all("is_error" not in r and "denied" not in r for r in results))
        self.assertEqual(convert(converted)[0], converted)

    def test_domain_grant_close_can_reference_a_later_retry_request(self):
        replaced = result(occurrence="first")
        replaced["ToolCallFinished"]["output"] = {"superseded_by_retry": True, "retry_occurrence_id": "retry"}
        cancelled = result(occurrence="retry")
        cancelled["ToolCallFinished"]["output"] = {"cancelled": True, "message": "retry was not executed"}
        converted, archived, _ = convert([
            record(1, request(occurrence="first"), version=2), record(2, replaced, version=2),
            record(3, request(occurrence="retry"), version=2), record(4, cancelled, version=2)])
        self.assertFalse(archived)
        self.assertEqual(converted[1]["event"]["ToolCallFinished"]["outcome"],
                         {"Superseded": {"retry_occurrence_id": "retry"}})
        self.assertEqual(converted[3]["event"]["ToolCallFinished"]["outcome"], "Cancelled")

    def test_uncertain_or_contradictory_outcomes_archive_whole_session(self):
        bad_results = [
            {"output": {"superseded_by_retry": True}},
            {"output": {"superseded_by_retry": True, "retry_occurrence_id": "first"}},
            {"output": {"cancelled": True}, "is_error": True},
            {"denied": True, "is_error": False},
            {"denied": None},
        ]
        for patch in bad_results:
            event = result(occurrence="first")
            event["ToolCallFinished"].update(patch)
            source = [record(1, request(occurrence="first"), version=2), record(2, event, version=2)]
            with self.subTest(patch=patch):
                converted, archived, _ = convert(source)
                self.assertFalse(converted)
                self.assertEqual(archived, source)

    def test_output_bundle_preserves_original_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.jsonl"
            original = migration.jsonl([record(1, request()), record(2, result())]).encode()
            source.write_bytes(original)
            output = root / "converted"
            manifest = migration.write_bundle(source, output)
            self.assertEqual(source.read_bytes(), original)
            self.assertEqual((output / "original.jsonl").read_bytes(), original)
            self.assertEqual(json.loads((output / "manifest.json").read_text()), manifest)
            with self.assertRaises(FileExistsError):
                migration.write_bundle(source, output)


if __name__ == "__main__":
    unittest.main()
