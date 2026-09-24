#!/usr/bin/env python3
"""Convert a stopped agent event log into a separate, reviewable v2 bundle.

Never modifies the source or activates the output. Sessions with ambiguous tool
identity or retired events stay in the archive, together with their full history.
"""

import argparse
import copy
import hashlib
import json
from pathlib import Path
import uuid

SCHEMA = "horizon.agent.event_log"
VERSION = 2
NAMESPACE = uuid.UUID("bc822f0b-9ecd-4409-a508-f00be6df6d0a")


class ConversionError(ValueError):
    pass


def identifier(value, label):
    if not isinstance(value, str) or not value:
        raise ConversionError(f"missing or invalid {label}")
    return value


def convert_session(records):
    requests = {}
    finished = set()
    retired = set()
    output = []

    def resolve(payload, *, execution=False):
        call = identifier(payload.get("call_id"), "call_id")
        occurrence = payload.get("occurrence_id")
        if occurrence is not None:
            identifier(occurrence, "occurrence_id")
            if requests.get(occurrence) != call:
                raise ConversionError("identity does not name a preceding request")
        else:
            candidates = [key for key, value in requests.items()
                          if value == call and key not in finished
                          and (not execution or key not in retired)]
            if len(candidates) != 1:
                raise ConversionError(f"ambiguous execution for call {call!r}")
            occurrence = candidates[0]
        payload["occurrence_id"] = occurrence
        return occurrence

    for original in records:
        record = copy.deepcopy(original)
        event = record["event"]
        if isinstance(event, dict) and len(event) == 1:
            kind, payload = next(iter(event.items()))
        elif isinstance(event, str):
            kind, payload = event, None
        else:
            raise ConversionError("invalid event envelope")
        if (kind == "TurnEnded" and payload == "Halted") or (
            kind == "ContinueTurnRequested" and isinstance(payload, dict)
            and payload.get("resumed_from") == "Halted"
        ):
            raise ConversionError("legacy halt has no recorded guard reason")
        if kind == "ToolCallRequested":
            call = identifier(payload.get("call_id"), "call_id")
            occurrence = payload.get("occurrence_id")
            if occurrence is None:
                occurrence = str(uuid.uuid5(NAMESPACE, record["event_id"]))
            identifier(occurrence, "occurrence_id")
            if occurrence in requests:
                raise ConversionError("duplicate request occurrence")
            payload["occurrence_id"] = occurrence
            requests[occurrence] = call
        elif kind == "ToolCallStarted":
            if isinstance(payload, str):
                payload = {"call_id": payload}
                event[kind] = payload
            resolve(payload, execution=True)
        elif kind in ("ApprovalRequested", "ApprovalResolved"):
            approval_kind = payload.get("kind", "Standard")
            if approval_kind == "SandboxDenialRetry":
                raise ConversionError("legacy retry has no narrow authority grant")
            if isinstance(approval_kind, dict):
                for details in approval_kind.values():
                    if isinstance(details, dict) and "prior_result" in details:
                        # Nested prior results are authoritative identity evidence,
                        # but do not count as a published ToolCallFinished.
                        retired.add(resolve(details["prior_result"]))
            resolve(payload, execution=True)
        elif kind == "ToolCallFinished":
            finished.add(resolve(payload))
        record["version"] = VERSION
        output.append(record)
    return output


def convert_bytes(source):
    if source and not source.endswith(b"\n"):
        raise ConversionError("incomplete final line; preserve and inspect the source first")
    records = []
    event_ids = set()
    sequences = set()
    sessions = {}
    for number, line in enumerate(source.splitlines(), 1):
        if not line.strip():
            continue
        try:
            record = json.loads(line)
            if record["schema"] != SCHEMA or record["version"] not in (1, VERSION):
                raise ConversionError("unsupported schema or version")
            event_id = identifier(record["event_id"], "event_id")
            session = identifier(record["session_id"], "session_id")
            sequence = record["sequence"]
            if not isinstance(sequence, int) or isinstance(sequence, bool) or sequence < 0:
                raise ConversionError("invalid sequence")
            if event_id in event_ids or sequence in sequences:
                raise ConversionError("duplicate event ID or sequence")
            if "event" not in record:
                raise ConversionError("missing event")
            event_ids.add(event_id)
            sequences.add(sequence)
            records.append(record)
            sessions.setdefault(session, []).append(record)
        except (ValueError, KeyError, TypeError) as error:
            raise ConversionError(f"line {number}: {error}") from error
    converted = []
    archived = []
    reasons = {}
    for session, history in sessions.items():
        history.sort(key=lambda record: record["sequence"])
        try:
            converted.extend(convert_session(history))
        except (ConversionError, AttributeError, TypeError) as error:
            archived.extend(history)
            reasons[session] = str(error)
    converted.sort(key=lambda record: record["sequence"])
    archived.sort(key=lambda record: record["sequence"])
    manifest = {
        "source_sha256": hashlib.sha256(source).hexdigest(),
        "source_records": len(records),
        "converted_records": len(converted),
        "archived_records": len(archived),
        "archived_sessions": reasons,
        "source_max_sequence": max(sequences, default=None),
        "converted_max_sequence": max((r["sequence"] for r in converted), default=None),
    }
    return converted, archived, manifest


def jsonl(records):
    return "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in records)


def write_bundle(source_path, output_dir):
    source = source_path.read_bytes()
    converted, archived, manifest = convert_bytes(source)
    # An exclusive directory prevents replacing an earlier conversion or backup.
    output_dir.mkdir(mode=0o700, parents=False, exist_ok=False)
    (output_dir / "original.jsonl").write_bytes(source)
    (output_dir / "events.jsonl").write_text(jsonl(converted), encoding="utf-8")
    (output_dir / "archive.jsonl").write_text(jsonl(archived), encoding="utf-8")
    if source_path.read_bytes() != source:
        raise ConversionError("source changed during conversion; discard this bundle and stop agentd")
    (output_dir / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output_dir", type=Path)
    args = parser.parse_args()
    try:
        manifest = write_bundle(args.source, args.output_dir)
    except (OSError, ConversionError) as error:
        parser.exit(1, f"conversion failed: {error}\n")
    print(json.dumps(manifest, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
