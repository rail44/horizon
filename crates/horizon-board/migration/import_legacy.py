#!/usr/bin/env python3
"""One-time selected-record converter. Never discovers or writes a live board."""
import argparse
import copy
import hashlib
import json
from pathlib import Path

SCHEMA = "horizon.board.event_log"


def fold_legacy(source):
    raw = source.read_bytes()
    fingerprint = hashlib.sha256(raw).hexdigest()
    items, high_water = {}, 0
    if raw and not raw.endswith(b"\n"):
        raise ValueError("Source has a torn trailing record; repair a copy first")
    for seq, line in enumerate(raw.splitlines(), 1):
        if not line.strip():
            continue
        event = json.loads(line)
        if event.get("schema") != SCHEMA or event.get("version") != 1:
            raise ValueError(f"Unexpected source schema at line {seq}")
        high_water = max(high_water, event.get("id", 0))
        kind = event.get("type")
        if kind == "workflow-batch":
            for item in event["items"]:
                high_water = max(high_water, item["id"])
                items[item["id"]] = copy.deepcopy(item)
        elif kind == "item-created":
            items[event["id"]] = {key: event[key] for key in ("id", "title", "body", "rank")}
        elif kind == "item-updated" and event["id"] in items:
            items[event["id"]].update({key: value for key, value in event.items()
                                     if key in ("status", "rank", "parent", "depends_on", "links", "title", "body")})
        elif kind == "workflow-changed" and event["id"] in items:
            item = items[event["id"]]
            item["workflow"] = copy.deepcopy(event["workflow"])
            for key in ("title", "body"):
                if event.get(key) is not None:
                    item[key] = event[key]
        elif kind == "comment-added" and event["id"] in items:
            items[event["id"]].setdefault("comments", []).append(
                {"author": event["author"], "text": event["text"], "at": event.get("at")})
    return items, high_water, fingerprint


def extract(items, selected, fingerprint):
    missing = selected - items.keys()
    if missing:
        raise ValueError(f"Selected tasks do not exist: {sorted(missing)}")
    result = []
    for id in sorted(selected):
        old = items[id]
        parent = old.get("parent")
        dependencies = old.get("depends_on", [])
        references = set(dependencies) | ({parent} if parent is not None else set())
        if references - selected:
            raise ValueError(f"Task {id} references unselected tasks {sorted(references - selected)}; select them or edit the isolated source")
        messages = []
        def message(coordinate, author, text, at):
            source = f"legacy:{fingerprint}:task:{id}:{coordinate}"
            messages.append({"id": source, "source": source, "author": author, "text": text, "at": at})
        for index, comment in enumerate(old.get("comments", [])):
            message(f"comment:{index}", comment["author"], comment["text"], comment.get("at"))
        decisions = (old.get("workflow") or {}).get("plan") or {}
        for index, decision in enumerate(decisions.get("decisions", [])):
            for offset, entry in enumerate(decision.get("messages", [])):
                # Old discussion records have no timestamp or agent identity.
                author = "owner" if entry["owner"] else "legacy-agent (identity unavailable)"
                message(f"decision:{index}:message:{offset}", author, entry["text"], None)
        body = old.get("body", "")
        if old.get("links"):
            body += "\n\nReferences retained from the legacy task:\n" + "\n".join(f"- {link}" for link in old["links"])
        result.append({"id": id, "title": old.get("title", ""), "body": body,
                       "status": old.get("status", ""), "completed": old.get("status") == "done",
                       "rank": old["rank"], "parent": parent, "depends_on": dependencies,
                       "comments": messages, "session_id": None, "review_session_id": None})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="Explicit copied legacy events.jsonl")
    parser.add_argument("--select", help="Explicit comma-separated task IDs; never selects all implicitly")
    parser.add_argument("--output", type=Path, help="New isolated output file; must not exist")
    args = parser.parse_args()
    items, high_water, fingerprint = fold_legacy(args.source)
    inventory = {"source_sha256": fingerprint, "high_water": high_water,
                 "tasks": [{"id": id, "title": i.get("title"), "parent": i.get("parent"),
                            "depends_on": i.get("depends_on", []), "comments": len(i.get("comments", [])),
                            "legacy_session": (i.get("workflow") or {}).get("worker")}
                           for id, i in sorted(items.items())]}
    print(json.dumps(inventory, ensure_ascii=False, indent=2))
    if args.output:
        if not args.select:
            parser.error("--output requires explicit --select")
        tasks = extract(items, {int(id) for id in args.select.split(",")}, fingerprint)
        events = [{"type": "import-high-water", "id": high_water}]
        events += [{"type": "task-imported", "id": task["id"], "item": task} for task in tasks]
        # Import records are deliberately distinct from fresh registration.
        with args.output.open("x", encoding="utf-8") as output:
            for event in events:
                output.write(json.dumps({"schema": SCHEMA, "version": 2, "at": 0, **event}, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
