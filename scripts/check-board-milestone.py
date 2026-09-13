#!/usr/bin/env python3
"""Exercise the real board/agent daemons with a deterministic local provider.

Build with cargo build --workspace first. All data, sockets, configuration and
Git operations belong to a temporary repository. No external API is called.
"""

import argparse
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import uuid


def message_text(message):
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    return "\n".join(part.get("text", "") for part in content or [] if isinstance(part, dict))


class Provider(http.server.BaseHTTPRequestHandler):
    requests_seen = 0
    errors = []

    def log_message(self, *_args):
        pass

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            type(self).requests_seen += 1
            if type(self).requests_seen > 100:
                raise AssertionError("Unexpected provider retry loop")
            messages = body.get("messages", [])
            assignment = None
            start = 0
            for index, message in enumerate(messages):
                text = message_text(message)
                if message.get("role") == "user" and text.startswith("Milestone assignment: "):
                    assignment = json.JSONDecoder().raw_decode(text[len("Milestone assignment: "):])[0]
                    start = index
            tail = messages[start:]
            calls = [call for message in tail for call in message.get("tool_calls", [])]
            names = [call["function"]["name"] for call in calls]
            tool = None
            if assignment:
                item_id = assignment["milestone"]
                attempt = assignment["attempt"]
                if "board.read" not in names:
                    tool = ("board.read", {"id": item_id})
                elif "board.report" not in names:
                    read_id = next(call["id"] for call in calls if call["function"]["name"] == "board.read")
                    read_result = next(message_text(m) for m in tail if m.get("tool_call_id") == read_id)
                    item = json.loads(read_result)
                    flow = item["workflow"]
                    if assignment["work"] == "Plan":
                        decisions = [] if flow["answers"] else [{
                            "key": "scope", "question": "Should the fixture include the extension?",
                            "context": "This adds a second task that depends on the first.",
                            "recommendation": "Include it to exercise dependency handoff.",
                            "consequence": "Both lines will be written in the same worktree.",
                        }]
                        tasks = [{
                            "key": key, "title": key, "instructions": "Implement the fixture and check it.",
                            "acceptance": ["The fixture contains the expected line."], "depends_on": deps,
                        } for key, deps in [("extend", ["create"]), ("create", [])]]
                        report = {"kind": "plan", "plan": {
                            "summary": "Build the two-line fixture.", "acceptance": ["Both lines exist."],
                            "tasks": tasks, "decisions": decisions,
                        }}
                        tool = ("board.report", {"id": item_id, "attempt": attempt, "report": report})
                    elif "bash" not in names:
                        key = assignment["work"]["Task"]["key"]
                        command = (
                            "printf 'implemented\\n' > result.txt && test -s result.txt && printf CHECK_OK"
                            if key == "create" else
                            "test -s result.txt && printf 'extended\\n' >> result.txt && printf CHECK_OK"
                        )
                        tool = ("bash", {"command": command})
                    else:
                        bash_id = next(call["id"] for call in calls if call["function"]["name"] == "bash")
                        output = next(message_text(m) for m in tail if m.get("tool_call_id") == bash_id)
                        if "CHECK_OK" not in output:
                            raise AssertionError(f"Implementation check did not pass: {output}")
                        tool = ("board.report", {"id": item_id, "attempt": attempt, "report": {
                            "kind": "task", "summary": "Implemented the assigned fixture step.",
                            "checks": ["Fixture shell check emitted CHECK_OK."],
                        }})
                else:
                    report_id = next(call["id"] for call in calls if call["function"]["name"] == "board.report")
                    result = next(message_text(m) for m in tail if m.get("tool_call_id") == report_id)
                    if "error" in json.loads(result):
                        raise AssertionError(f"Report rejected: {result}")
            else:
                # Keeper wakes are unrelated to this test's assignments.
                available = [t["function"]["name"] for t in body.get("tools", [])]
                if "memory.update" in available and "memory.update" not in names:
                    tool = ("memory.update", {"no_update": {"reason": "No fixture context is missing."}})

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            stream_id = "chatcmpl-" + uuid.uuid4().hex
            delta = {"role": "assistant"}
            if tool:
                name, arguments = tool
                delta["tool_calls"] = [{"index": 0, "id": "call_" + uuid.uuid4().hex,
                                        "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]
            else:
                delta["content"] = "Fixture step complete."
            for value, finish in [(delta, None), ({}, "tool_calls" if tool else "stop")]:
                event = {"id": stream_id, "object": "chat.completion.chunk", "created": 1,
                         "model": "milestone-fixture", "choices": [{"index": 0, "delta": value, "finish_reason": finish}]}
                self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except Exception as error:
            type(self).errors.append(str(error))
            self.send_error(500, str(error))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    args = parser.parse_args()
    binaries = args.bin_dir.resolve()
    for name in ["horizon", "horizon-agentd", "horizon-logd"]:
        if not (binaries / name).is_file():
            parser.error(f"Missing {binaries / name}; run cargo build --workspace")
    root = Path(tempfile.mkdtemp(prefix="horizon-milestone-check-"))
    repository = root / "repo"
    repository.mkdir()
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    environment.update({
        "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null",
        "XDG_DATA_HOME": str(root / "data"), "XDG_CONFIG_HOME": str(root / "config-home"),
        "XDG_RUNTIME_DIR": str(root / "runtime"), "HORIZON_CONFIG": str(root / "config.toml"),
        "HORIZON_AGENT_EVENT_LOG": str(root / "agent.jsonl"), "HORIZON_AGENT_STATE_DB": str(root / "agent.duckdb"),
        "HORIZON_LOGD_SOCKET": str(root / "logd.sock"), "HORIZON_LOGD_BINARY": str(binaries / "horizon-logd"),
        "OPENAI_API_KEY": "local-fixture-only", "HORIZON_RIG_MODEL": "milestone-fixture",
        "HORIZON_SKIP_TARGET_SEED": "1",
    })
    processes = []
    server = None
    passed = False
    try:
        for command in [["git", "init", "-q"], ["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@invalid", "commit", "--allow-empty", "-qm", "Fixture"]]:
            subprocess.run(command, cwd=repository, env=environment, check=True, capture_output=True)
        (root / "config.toml").write_text(f"trusted_projects = [{json.dumps(str(repository))}]\n")
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        environment["OPENAI_BASE_URL"] = f"http://127.0.0.1:{server.server_port}/v1"
        for name, socket in [("horizon-logd", "logd.sock"), ("horizon-agentd", "agentd.sock")]:
            with (root / f"{name}.log").open("w") as log:
                processes.append(subprocess.Popen([binaries / name, "--socket", root / socket], cwd=repository,
                                                  env=environment, stdout=log, stderr=log))

        def cli(*arguments):
            result = subprocess.run([binaries / "horizon", "board", *arguments], cwd=repository,
                                    env=environment, capture_output=True, text=True, timeout=15)
            if result.returncode:
                raise AssertionError(result.stderr)
            return json.loads(result.stdout)

        def wait_for(predicate):
            deadline = time.monotonic() + 45
            last = None
            while time.monotonic() < deadline:
                if Provider.errors:
                    raise AssertionError(Provider.errors)
                last = cli("flow", "1", "--json")
                if predicate(last):
                    return last
                if last and last.get("problem"):
                    raise AssertionError(last["problem"])
                time.sleep(0.1)
            raise AssertionError(f"Timed out waiting for workflow: {last}")

        cli("add", "Implement fixture", "--body", "Create and extend the fixture in order.", "--json")
        cli("milestone", "1", "--json")
        waiting = wait_for(lambda f: f and not f["active"] and f["plan"] and f["plan"]["decisions"])
        assert waiting["worker"] is None, "A decision must prevent premature implementation"
        # A saved decision must survive daemon replacement before the answer
        # arrives. Startup's resume barrier must finish before dispatch resumes.
        processes[-1].terminate()
        processes[-1].wait(timeout=10)
        with (root / "horizon-agentd.log").open("a") as log:
            processes.append(subprocess.Popen([binaries / "horizon-agentd", "--socket", root / "agentd.sock"],
                                              cwd=repository, env=environment, stdout=log, stderr=log))
        cli("answer", "1", "scope", "Include the extension.", "--json")
        complete = wait_for(lambda f: len(f["results"]) == 2 and not f["active"])
        assert [result["key"] for result in complete["results"]] == ["create", "extend"]
        assert len({result["session"] for result in complete["results"]}) == 1
        worktree = Path(complete["worker"]["worktree"])
        assert worktree != repository
        assert (worktree / "result.txt").read_text() == "implemented\nextended\n"
        assert not (repository / "result.txt").exists(), "The source checkout must stay untouched"
        assert cli("show", "1", "--json")["status"] == "review"
        passed = True
        print("PASS: goal → decision → daemon restart → answer → revised plan → dependency-ordered implementation → review")
        print(f"PASS: two real shell tasks shared an isolated worktree; {Provider.requests_seen} local provider requests")
    finally:
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        if server:
            server.shutdown()
            server.server_close()
        if passed:
            shutil.rmtree(root)
        else:
            print(f"Fixture and diagnostic logs preserved at {root}")


if __name__ == "__main__":
    main()
