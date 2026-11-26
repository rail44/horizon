"""A small MCP server over stdio for the agent's tests: `echo` only reads
(it says so), `note` appends its text to the file NOTE_FILE names."""

import json
import os
import sys

TOOLS = [
    {
        "name": "echo",
        "description": "Answers with the text it is given.",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "note",
        "description": "Appends the text to the note file.",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
    },
]


def answer(request_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if "id" not in message:
        continue
    params = message.get("params") or {}
    if method == "initialize":
        answer(message["id"], {
            "protocolVersion": params.get("protocolVersion"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "test", "version": "0"},
        })
    elif method == "tools/list":
        answer(message["id"], {"tools": TOOLS})
    elif method == "tools/call":
        arguments = params.get("arguments") or {}
        text = arguments.get("text", "")
        if params.get("name") == "note":
            with open(os.environ["NOTE_FILE"], "a") as note:
                note.write(text + "\n")
            text = "noted"
        answer(message["id"], {"content": [{"type": "text", "text": text}]})
    else:
        answer(message["id"], {})

# Its connection closed: say so, for a test of when the server stops.
with open(os.environ["NOTE_FILE"], "a") as note:
    note.write("ended\n")
