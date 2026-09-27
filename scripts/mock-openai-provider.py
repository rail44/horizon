#!/usr/bin/env python3
"""Minimal OpenAI-compatible chat-completions mock for the ACP agent-flow check.

Usage: mock-openai-provider.py <port> [<log path>]

Replies are keyed on the request's messages:
  * the last message has role "tool"            -> text echoing the tool output
  * the last user message mentions "bash tool"  -> one tool call: bash {"command": "echo acp-approval-check"}
  * otherwise                                   -> the text "ACP-OK"

Streams SSE chunks when the request sets "stream": true, plain JSON otherwise.
GET /v1/models lists the single model "mock-model".
"""
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18081
MODEL = "mock-model"
REPLY_DELAY = float(os.environ.get("HORIZON_ACP_CHECK_REPLY_DELAY", "1.5"))
LOG = open(sys.argv[2], "a") if len(sys.argv) > 2 else sys.stderr


def log(msg):
    LOG.write(msg + "\n")
    LOG.flush()


def text_of(content):
    if isinstance(content, list):
        return " ".join(part.get("text", "") for part in content if isinstance(part, dict))
    return str(content or "")


def decide(messages):
    last = messages[-1] if messages else {}
    if last.get("role") == "tool":
        text = text_of(last.get("content")).strip()
        try:
            parsed = json.loads(text)
            if isinstance(parsed, dict):
                text = str(parsed.get("output") or parsed.get("stdout") or text)
        except Exception:
            pass
        return ("text", f"tool output: {text.strip()}")
    for message in reversed(messages):
        if message.get("role") == "user":
            if "bash tool" in text_of(message.get("content")):
                return ("tool", None)
            break
    return ("text", "ACP-OK")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        log("http " + fmt % args)

    def _json(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            return self._json(200, {"object": "list", "data": [{"id": MODEL, "object": "model", "owned_by": "mock"}]})
        return self._json(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            req = json.loads(raw)
        except Exception:
            return self._json(400, {"error": "bad json"})
        if not self.path.rstrip("/").endswith("/chat/completions"):
            return self._json(404, {"error": "not found"})
        kind, text = decide(req.get("messages", []))
        log(f"request kind={kind} n_messages={len(req.get('messages', []))} stream={req.get('stream')}")
        created = int(time.time())
        base = {"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": created, "model": MODEL}
        usage = {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}

        if kind == "tool":
            function = {"name": "bash", "arguments": json.dumps({"command": "echo acp-approval-check"})}
            tool_call = {"index": 0, "id": "call_mock_1", "type": "function", "function": function}
            chunks = [
                {"choices": [{"index": 0, "delta": {"role": "assistant", "content": None, "tool_calls": [tool_call]}, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
            message = {"role": "assistant", "content": None,
                       "tool_calls": [{"id": "call_mock_1", "type": "function", "function": function}]}
            finish = "tool_calls"
        else:
            chunks = [
                {"choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
            message = {"role": "assistant", "content": text}
            finish = "stop"

        if not req.get("stream"):
            return self._json(200, {
                "id": "chatcmpl-mock", "object": "chat.completion", "created": created, "model": MODEL,
                "choices": [{"index": 0, "message": message, "finish_reason": finish}], "usage": usage,
            })

        # Hold the reply briefly so a client polling the shell can observe the
        # turn in flight before it ends.
        time.sleep(REPLY_DELAY)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        for chunk in chunks:
            payload = dict(base)
            payload.update(chunk)
            self.wfile.write(f"data: {json.dumps(payload)}\n\n".encode())
            self.wfile.flush()
            time.sleep(0.05)
        tail = dict(base)
        tail.update({"choices": [], "usage": usage})
        self.wfile.write(f"data: {json.dumps(tail)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    log(f"mock openai listening on 127.0.0.1:{PORT}")
    server.serve_forever()
