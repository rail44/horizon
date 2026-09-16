#!/usr/bin/env python3
"""Run the ordinary board flow against real daemons and a local fake provider.

Requires matching `cargo build --workspace` binaries and host socket/process
permissions. All Git operations, stores, sockets and provider requests are
isolated below a temporary directory. No external API or live board is used.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

MAIN = "FIXTURE_MAIN"
DEPENDENT = "FIXTURE_DEPENDENT"
OWNER = "FIXTURE_IMPLEMENT: proceed with the agreed output and review policy."
CONSULTATION = "FIXTURE_CONSULTATION: Shall I implement alpha with a separate review?"
REVIEW_FIX = "FIXTURE_REVIEW_FIX: alpha.txt must contain final, not draft."
REVIEW_OK = "FIXTURE_REVIEW_OK: the complete multi-commit change satisfies the checks."
WAITING_REVIEW = "FIXTURE_WAITING_REVIEW: requested independent review."
WAITING_REREVIEW = "FIXTURE_WAITING_REREVIEW: correction awaits independent review."
COMPLETE = "FIXTURE_COMPLETE: reviewed output is available on main."
DEPENDENCY_SEEN = "FIXTURE_DEPENDENCY_SEEN: prerequisite output is now available."


def message_text(message):
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    return "\n".join(part.get("text", "") for part in content or [] if isinstance(part, dict))


def records(path):
    if not path.exists():
        return []
    raw = path.read_bytes()
    lines = raw.splitlines()
    if raw and not raw.endswith(b"\n"):
        lines.pop()
    return [json.loads(line) for line in lines if line.strip()]


class Provider(http.server.BaseHTTPRequestHandler):
    requests_seen = 0
    errors = []
    lock = threading.Lock()
    root = None
    repository = None
    base = None
    review_checks = {}
    task_session = None

    def wait_for_settled_task_answer(self, text):
        # A fast reviewer can otherwise return before the owner-triggered
        # turn ends, hiding a missing reply address on the next turn.
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            history = [r["event"] for r in records(self.root / "agent.jsonl")
                       if r["session_id"] == self.task_session and isinstance(r["event"], dict)]
            answered = any(e.get("InputOutcome", {}).get("outcome") == {"Success": {"text": text}}
                           for e in history)
            states = [e["StateChanged"] for e in history if "StateChanged" in e]
            if answered and states and states[-1] == "WaitingForUser":
                return
            time.sleep(0.05)
        raise AssertionError(f"Task did not settle before review delivery: {text}")

    def log_message(self, *_args):
        pass

    @classmethod
    def commands(cls):
        root = shlex.quote(str(cls.repository))
        guard = f'test "$(git rev-parse --show-toplevel)" != {root} && '
        return {
            "first": guard + "printf 'draft\\n' > alpha.txt && git add -- alpha.txt && git commit -m 'Add draft output' && git rev-parse HEAD",
            "second": guard + "printf 'fixture checks\\n' > evidence.txt && git add -- evidence.txt && git commit -m 'Add validation evidence' && printf 'uncommitted-poison\\n' > alpha.txt && git rev-parse HEAD",
            "correct": guard + "printf 'final\\n' > alpha.txt && git add -- alpha.txt && git commit -m 'Apply review correction' && printf 'uncommitted-poison\\n' > alpha.txt && git rev-parse HEAD",
        }

    def judge_reply(self, body):
        text = "\n".join(message_text(m) for m in body["messages"])
        match = re.search(r"<<<UNTRUSTED_ARGS_[^>]+>>>\n(.*?)\n<<<END_UNTRUSTED_ARGS_", text, re.S)
        if not match:
            raise AssertionError("Unrecognized fixture approval request")
        command = json.loads(match.group(1)).get("command")
        allowed = set(self.commands().values())
        allowed.add('test "$(cat alpha.txt)" = final')
        # Integration uses the task tip explicitly, never main's own HEAD.
        if command and re.fullmatch(r"git -C " + re.escape(shlex.quote(str(self.repository))) + r" merge --ff-only [0-9a-f]{40}", command):
            allowed.add(command)
        if command not in allowed:
            raise AssertionError(f"Unexpected approval request: {command}")
        first_stage = "single character" in message_text(body["messages"][0])
        content = "N" if first_stage else json.dumps({"verdict": "AutoApprove", "reasoning": "The fixture authorizes this exact commit or integration command in its isolated repository."})
        result = {"id": "fixture-judge", "object": "chat.completion", "created": 1, "model": "board-flow-fixture",
                  "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop",
                               "logprobs": {"content": [{"token": "N", "logprob": -0.001, "bytes": [78], "top_logprobs": []}]}}],
                  "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(result).encode())

    def choose(self, body):
        messages = body.get("messages", [])
        text = "\n".join(message_text(m) for m in messages)
        system = "\n".join(message_text(m) for m in messages if m.get("role") == "system")
        available = {t["function"]["name"] for t in body.get("tools", [])}
        calls = [c for m in messages for c in m.get("tool_calls", [])]
        outputs = {m["tool_call_id"]: message_text(m) for m in messages if "tool_call_id" in m}
        for call in calls:
            if call["id"] in outputs:
                try:
                    value = json.loads(outputs[call["id"]])
                except json.JSONDecodeError:
                    continue
                if isinstance(value, dict) and (value.get("is_error") or "error" in value or value.get("exit_code", 0) != 0):
                    raise AssertionError(f"Tool failed: {call['function']}: {value}")

        def called(name, arguments):
            return next((c for c in reversed(calls) if c["function"]["name"] == name
                         and json.loads(c["function"]["arguments"]) == arguments), None)

        def once(name, arguments):
            return None if called(name, arguments) else (name, arguments)

        def tip(command):
            call = called("bash", {"command": command})
            value = json.loads(outputs[call["id"]])
            return re.findall(r"(?m)^[0-9a-f]{40}$", value["output"])[-1]

        if "board.read" not in available:
            if "memory.update" in available:
                tool = once("memory.update", {"no_update": {"reason": "The fixture keeps its state in task records."}})
                if tool:
                    return tool, None
            return None, "Fixture checkpoint complete."
        for role in ["board-organizer", "board-task", "board-reviewer"]:
            if f"Read the {role} skill" in system and "skill.read" in available:
                tool = once("skill.read", {"id": role})
                if tool:
                    return tool, None
        if "Read the board-organizer skill" in system:
            actions = [
                ("board.read", {}),
                ("board.update", {"action": "add", "title": DEPENDENT, "body": "Wait for task 1 to complete, then verify its result is available."}),
                ("board.update", {"action": "dependencies", "id": 2, "depends_on": [1]}),
                ("board.update", {"action": "move", "id": 1, "position": "first"}),
                ("board.session", {"action": "consult", "id": 2, "text": "Wait for prerequisite 1 before completing this dependent task."}),
                ("board.session", {"action": "consult", "id": 1, "text": "Investigate the desired output and ask the owner before implementation."}),
            ]
            for name, arguments in actions:
                tool = once(name, arguments)
                if tool:
                    return tool, None
            return None, "Fixture priorities and prerequisites organized."
        if "Read the board-reviewer skill" in system:
            reviews = re.findall(r"Review task #(\d+) from ([0-9a-f]{40}) to ([0-9a-f]{40})", text)
            if not reviews:
                raise AssertionError("Reviewer received no exact base/tip request")
            task, base, target = reviews[-1]
            assert task == "1" and base == self.base
            tool = once("board.read", {"id": 1})
            if tool:
                return tool, None
            # Each review is a fresh conversation. Decide from the requested
            # commit's files, never a previous review's messages or call count.
            command = (f'test "$(git rev-parse HEAD)" = {target} && '
                       'test -z "$(git status --porcelain)" && '
                       f'test "$(cat alpha.txt)" = "$(git show {target}:alpha.txt)" && '
                       'test "$(cat alpha.txt)" != uncommitted-poison && '
                       f'git rev-list --count {base}..{target} && '
                       'git rev-parse --show-toplevel && git rev-parse HEAD && '
                       'cat alpha.txt && cat evidence.txt')
            tool = once("bash", {"command": command})
            if tool:
                return tool, None
            output = json.loads(outputs[called("bash", {"command": command})["id"]])["output"]
            lines = output.splitlines()
            assert len(lines) == 5 and int(lines[0]) >= 2, output
            assert lines[2] == target, output
            assert lines[3] in ["draft", "final"] and lines[4] == "fixture checks", output
            assert Path(lines[1]).resolve() != self.repository.resolve(), output
            with self.lock:
                self.review_checks[target] = {"root": lines[1], "head": lines[2], "content": lines[3]}
            self.wait_for_settled_task_answer(WAITING_REREVIEW if lines[3] == "final" else WAITING_REVIEW)
            return None, REVIEW_OK if lines[3] == "final" else REVIEW_FIX
        if "Read the board-task skill" in system:
            assert "board-integration" in system, "Project policy must be advertised to task sessions"
            tool = once("skill.read", {"id": "board-integration"})
            if tool:
                return tool, None
            assignments = re.findall(r"Task #(\d+)\. Board organizer session:", text)
            if not assignments:
                raise AssertionError("Task session has no explicit task identity")
            task = int(assignments[0])
            tool = once("board.read", {"id": task})
            if tool:
                return tool, None
            if task == 2:
                if "Prerequisite #1 completed." not in text:
                    return None, "FIXTURE_WAITING: prerequisite 1 is incomplete."
                command = "test \"$(cat alpha.txt)\" = final"
                tool = once("bash", {"command": command}) or once("board.update", {"action": "complete", "id": 2, "completed": True})
                return tool, None if tool else DEPENDENCY_SEEN
            assert task == 1
            if OWNER not in text:
                return None, CONSULTATION
            actions = [("board.session", {"action": "implement", "id": 1, "base": self.base}),
                       ("bash", {"command": self.commands()["first"]}),
                       ("bash", {"command": self.commands()["second"]})]
            for name, arguments in actions:
                tool = once(name, arguments)
                if tool:
                    return tool, None
            initial_tip = tip(self.commands()["second"])
            tool = once("board.session", {"action": "review", "id": 1, "base": self.base, "tip": initial_tip, "checks": "Two commits; alpha.txt and evidence.txt exist. Verify final content."})
            if tool:
                return tool, None
            if REVIEW_FIX not in text:
                return None, WAITING_REVIEW
            tool = once("bash", {"command": self.commands()["correct"]})
            if tool:
                return tool, None
            corrected_tip = tip(self.commands()["correct"])
            tool = once("board.session", {"action": "review", "id": 1, "base": self.base, "tip": corrected_tip, "checks": "Three commits including review correction; alpha.txt must contain final."})
            if tool:
                return tool, None
            if REVIEW_OK not in text:
                return None, WAITING_REREVIEW
            command = f"git -C {shlex.quote(str(self.repository))} merge --ff-only {corrected_tip}"
            tool = once("bash", {"command": command}) or once("board.update", {"action": "complete", "id": 1, "completed": True})
            return tool, None if tool else COMPLETE
        raise AssertionError("Unknown role or unexpected ordinary session")

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with self.lock:
                type(self).requests_seen += 1
                request_number = self.requests_seen
            if request_number > 250:
                raise AssertionError("Unexpected provider retry loop")
            (self.root / f"provider-{request_number:03}.json").write_text(json.dumps(body, indent=2))
            if not body.get("stream"):
                return self.judge_reply(body)
            tool, answer = self.choose(body)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            stream_id = "chatcmpl-" + uuid.uuid4().hex
            delta = {"role": "assistant"}
            if tool:
                name, arguments = tool
                delta["tool_calls"] = [{"index": 0, "id": "call_" + uuid.uuid4().hex, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]
            else:
                delta["content"] = answer
            for value, finish in [(delta, None), ({}, "tool_calls" if tool else "stop")]:
                event = {"id": stream_id, "object": "chat.completion.chunk", "created": 1, "model": "board-flow-fixture", "choices": [{"index": 0, "delta": value, "finish_reason": finish}]}
                self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except Exception as error:
            with self.lock:
                self.errors.append(repr(error))
            self.send_error(500, str(error))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--keep", action="store_true", help="Retain isolated artifacts even after success")
    args = parser.parse_args()
    binaries = args.bin_dir.resolve()
    for name in ["horizon", "horizon-agentd", "horizon-logd"]:
        if not (binaries / name).is_file():
            parser.error(f"Missing {binaries / name}; run cargo build --workspace")
    root = Path(tempfile.mkdtemp(prefix="horizon-board-flow-"))
    repository = root / "repo"
    repository.mkdir()
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("GIT_", "HORIZON_", "OPENAI_", "EXA_"))}
    environment.update({
        "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null",
        "XDG_DATA_HOME": str(root / "data"), "XDG_CONFIG_HOME": str(root / "config-home"),
        "XDG_RUNTIME_DIR": str(root / "runtime"), "HORIZON_CONFIG": str(root / "config.toml"),
        "HORIZON_AGENT_EVENT_LOG": str(root / "agent.jsonl"), "HORIZON_AGENT_STATE_DB": str(root / "agent.duckdb"),
        "HORIZON_LOGD_SOCKET": str(root / "logd.sock"), "HORIZON_LOGD_BINARY": str(binaries / "horizon-logd"),
        "HORIZON_AGENTD_SOCKET": str(root / "agentd.sock"), "HORIZON_AGENTD_BINARY": str(binaries / "horizon-agentd"),
        "OPENAI_API_KEY": "local-fixture-only", "HORIZON_RIG_MODEL": "board-flow-fixture", "HORIZON_SKIP_TARGET_SEED": "1",
    })
    processes = []
    server = None
    passed = False
    try:
        def git(*arguments):
            return subprocess.run(["git", *arguments], cwd=repository, env=environment, check=True, capture_output=True, text=True).stdout.strip()
        git("init", "-q", "-b", "main")
        git("config", "--local", "user.name", "Fixture")
        git("config", "--local", "user.email", "fixture@invalid")
        (repository / "AGENTS.md").write_text("This is an isolated automated fixture. Create commits in the task worktree. Read the board-integration skill for this project's integration policy. Never alter another repository.\n")
        policy = repository / ".horizon/skills/board-integration/SKILL.md"
        policy.parent.mkdir(parents=True)
        policy.write_text("---\nname: board-integration\ndescription: Integration policy for this isolated fixture.\n---\nAfter independent agent review and passing checks, integrate with a fast-forward merge into this fixture repository's main branch without another approval.\n")
        git("add", "AGENTS.md", ".horizon/skills/board-integration/SKILL.md")
        git("commit", "-qm", "Fixture policy")
        Provider.root, Provider.repository, Provider.base = root, repository, git("rev-parse", "HEAD")
        (root / "config.toml").write_text(f"trusted_projects = [{json.dumps(str(repository))}]\n")
        (root / "runtime").mkdir(mode=0o700)
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        environment["OPENAI_BASE_URL"] = f"http://127.0.0.1:{server.server_port}/v1"

        def start(name, socket):
            with (root / f"{name}.log").open("a") as log:
                process = subprocess.Popen([binaries / name, "--socket", root / socket], cwd=repository, env=environment, stdout=log, stderr=log)
            processes.append(process)
            return process
        start("horizon-logd", "logd.sock")
        agentd = start("horizon-agentd", "agentd.sock")

        def cli(*arguments, json_output=True):
            result = subprocess.run([binaries / "horizon", "board", *arguments], cwd=repository, env=environment, capture_output=True, text=True, timeout=15)
            if result.returncode:
                raise AssertionError(result.stderr)
            return json.loads(result.stdout) if json_output else result.stdout

        def wait_for(description, predicate):
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline:
                if Provider.errors:
                    raise AssertionError(Provider.errors)
                if agentd.poll() is not None:
                    raise AssertionError(f"agentd exited ({agentd.returncode})")
                value = predicate()
                if value:
                    return value
                time.sleep(0.1)
            raise AssertionError(f"Timed out: {description}")

        def item(id):
            return cli("show", str(id), "--json")

        def has_message(id, marker):
            return any(marker in message["text"] for message in item(id)["comments"])

        def activated(session):
            return [r for r in records(root / "agent.jsonl") if r["session_id"] == session and "EnvironmentActivated" in r["event"]]

        cli("add", MAIN, "--body", "Create alpha.txt containing final and evidence.txt. Consult before implementation; independent review and main integration are required for completion.", "--json")
        wait_for("automatic organizer consultation", lambda: has_message(1, CONSULTATION))
        initial = item(1)
        task_session = initial["session_id"]
        Provider.task_session = task_session
        assert task_session and not activated(task_session), "Consultation must not allocate a worktree"
        wait_for("dependent consultation", lambda: has_message(2, "FIXTURE_WAITING"))
        assert item(2)["depends_on"] == [1]
        assert item(1)["rank"] < item(2)["rank"]

        # Restart after a settled consultation. Replayed records must not
        # generate another organizer registration or duplicate final answer.
        before_ids = [m["id"] for m in initial["comments"]]
        agentd.terminate()
        agentd.wait(timeout=15)
        agentd = start("horizon-agentd", "agentd.sock")
        cli("comment", "1", "--author", "owner", OWNER, json_output=False)
        wait_for("reviewed implementation and dependency notification", lambda: item(1)["completed"] and item(2)["completed"])
        wait_for("task's post-review final answer on the board", lambda: has_message(1, COMPLETE))
        messages = [message["text"] for message in item(1)["comments"]]
        assert messages.count(WAITING_REREVIEW) == 1, "Correction report must return to the board"
        assert messages.count(COMPLETE) == 1, "Post-review completion report must return once"
        assert REVIEW_FIX not in messages and REVIEW_OK not in messages, "Review details go to the task session"
        assert item(1)["session_id"] == task_session
        assert all(message in [m["id"] for m in item(1)["comments"]] for message in before_ids)
        assert sum(CONSULTATION in m["text"] for m in item(1)["comments"]) == 1
        environments = activated(task_session)
        assert environments, "Implementation must publish environment activation"
        environment_record = environments[-1]["event"]["EnvironmentActivated"]
        assert environment_record["base"] == Provider.base
        assert Path(environment_record["path"]).resolve() != repository.resolve()
        latest_reviewer = item(1)["review_session_id"]
        assert latest_reviewer and latest_reviewer != task_session
        assert (repository / "alpha.txt").read_text() == "final\n"
        assert int(git("rev-list", "--count", f"{Provider.base}..main")) == 3
        events = records(root / "agent.jsonl")
        requests = {}
        for entry in events:
            event = entry["event"]
            sent = event.get("SessionInputSent", {}) if isinstance(event, dict) else {}
            match = re.match(r"Review task #1 from ([0-9a-f]{40}) to ([0-9a-f]{40})", sent.get("input", {}).get("text", ""))
            if entry["session_id"] == task_session and match:
                requests[sent["input"]["id"]] = (sent["session_id"], match.group(2))
        assert len(requests) == 2, "Correction must request a second pinned review"
        reviewers = {session for session, _ in requests.values()}
        assert len(reviewers) == 2 and task_session not in reviewers
        assert latest_reviewer in reviewers
        review_roots = set()
        outcomes = set()
        for reviewer, target in requests.values():
            checks = Provider.review_checks[target]
            assert checks["head"] == target
            snapshot = activated(reviewer)[-1]["event"]["EnvironmentActivated"]
            assert Path(checks["root"]).resolve() == Path(snapshot["path"]).resolve()
            assert Path(checks["root"]).resolve() != Path(environment_record["path"]).resolve()
            review_roots.add(checks["root"])
            outcomes.add(checks["content"])
            reviewer_text = json.dumps([r["event"] for r in events if r["session_id"] == reviewer])
            assert (REVIEW_OK if checks["content"] == "final" else REVIEW_FIX) in reviewer_text
        assert len(review_roots) == 2 and outcomes == {"draft", "final"}
        # Both review requests were made while the task tree contained poison.
        # Its uncommitted bytes must never have entered either pinned snapshot.
        assert (Path(environment_record["path"]) / "alpha.txt").read_text() == "uncommitted-poison\n"
        dependency_inputs = [r for r in events if r["session_id"] == item(2)["session_id"] and "InputAccepted" in r["event"]
                             and "Prerequisite #1 completed." in json.dumps(r["event"])]
        assert len(dependency_inputs) == 1, "Completion must deliver one durable notification"
        ids = [m["id"] for m in item(1)["comments"]]
        assert len(ids) == len(set(ids)), "Stable message delivery must not duplicate IDs"
        assert len(cli("list", "--all", "--json")["items"]) == 2, "Restart must not duplicate registration"
        passed = True
        print("PASS: registration → priorities/dependencies → consultation → restart → same-session worktree → multi-commit review/correction → main integration → dependent notification")
        print(f"PASS: fresh pinned reviewers exclude uncommitted task changes; stable messages and explicit base; {Provider.requests_seen} local provider requests")
        print("PASS: reviews resume settled task turns; task reports return to the board, review details stay in session history")
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        if server:
            server.shutdown()
            server.server_close()
        if passed and not args.keep:
            shutil.rmtree(root)
        else:
            print(f"Fixture and diagnostics retained at {root}")


if __name__ == "__main__":
    main()
