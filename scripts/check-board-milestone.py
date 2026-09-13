#!/usr/bin/env python3
"""Exercise the real board/agent daemons with a deterministic local provider.

Build with cargo build --workspace first. All data, sockets, configuration and
Git operations belong to a temporary repository. No external API is called.
"""

import argparse
import http.server
import json
import os
import re
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


def write_command(key):
    prefix = "test -s alpha.txt && " if key == "extension" else ""
    return prefix + f"printf '{key}\\n' > {key}.txt"


def commit_command(key):
    return f"git add -- {key}.txt && git commit -m 'Implement {key}' && git rev-parse HEAD"


def verification_command(key):
    keys = [key] if key else ["alpha", "beta", "extension"]
    return " && ".join(f'test "$(cat {name}.txt)" = {name}' for name in keys)


class Provider(http.server.BaseHTTPRequestHandler):
    requests_seen = 0
    errors = []
    starts = set()
    verifications = set()
    implementation_barrier = threading.Barrier(2, timeout=20)
    verification_barrier = threading.Barrier(2, timeout=20)
    lock = threading.Lock()

    def log_message(self, *_args):
        pass

    def judge_reply(self, body):
        text = "\n".join(message_text(m) for m in body["messages"])
        match = re.search(r"<<<UNTRUSTED_ARGS_[^>]+>>>\n(.*?)\n<<<END_UNTRUSTED_ARGS_", text, re.S)
        if not match:
            raise AssertionError("Unrecognized fixture approval request")
        command = json.loads(match.group(1)).get("command")
        allowed = {commit_command(k) for k in ["alpha", "beta", "extension"]}
        if command not in allowed:
            raise AssertionError(f"Unexpected approval request: {command}")
        first_stage = "single character" in message_text(body["messages"][0])
        content = "N" if first_stage else '{"verdict":"AutoApprove","reasoning":"Commit the assigned task in its isolated fixture worktree."}'
        result = {"id":"fixture-judge","object":"chat.completion","created":1,"model":"milestone-fixture",
                  "choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop",
                              "logprobs":{"content":[{"token":"N","logprob":-0.001,"bytes":[78],"top_logprobs":[]}]}}],
                  "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(result).encode())

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with self.lock:
                type(self).requests_seen += 1
                if self.requests_seen > 300:
                    raise AssertionError("Unexpected provider retry loop")
            if not body.get("stream"):
                return self.judge_reply(body)
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

            def result(call):
                value = json.loads(next(message_text(m) for m in tail if m.get("tool_call_id") == call["id"]))
                if isinstance(value, dict) and (value.get("is_error") or "error" in value):
                    raise AssertionError(value)
                return value

            tool = None
            if assignment:
                item_id = assignment["item"]
                attempt = assignment["attempt"]
                if "board.read" not in names:
                    tool = ("board.read", {"id": item_id})
                elif "board.report" not in names:
                    item = result(next(c for c in calls if c["function"]["name"] == "board.read"))
                    flow = item["workflow"]
                    work = assignment["work"]
                    report = None
                    if work == "Plan":
                        tasks = [{"key":key,"title":key,"instructions":f"Implement {key} and verify its content.",
                                  "acceptance":[f"{key}.txt contains {key}"],"depends_on":deps,
                                  "scope":{"paths":[f"{key}.txt"],"functions":[key]}}
                                 for key, deps in [("extension",["alpha"]),("beta",[]),("alpha",[])]]
                        report = {"kind":"plan","plan":{
                            "summary":"Build three fixture outputs.","reason":"Reconcile completed work and remaining dependencies.",
                            "acceptance":["All three files contain their keys."],"tasks":tasks,
                            "decisions":[{"key":"scope","question":"Include the extension?","context":"The extension scope is unresolved.",
                                          "recommendation":"Include it.","consequence":"Enables the extension task; independent work can continue.",
                                          "affected_tasks":["extension"]}]}}
                    elif isinstance(work, dict) and "Discuss" in work:
                        decision = next(d for d in flow["plan"]["decisions"] if d["key"] == work["Discuss"]["key"])
                        settled = decision["messages"][-1]["text"] == "Include the extension."
                        report = {"kind":"discussion","reply":"The extension requires one additional task.",
                                  "resolution":"Include the extension." if settled else None,"acceptance":None}
                    else:
                        key = flow["task"]["key"] if flow["task"] else None
                        bash_calls = [c for c in calls if c["function"]["name"] == "bash"]
                        if isinstance(work, dict) and "Task" in work:
                            if not bash_calls:
                                with self.lock:
                                    first = key not in self.starts
                                    self.starts.add(key)
                                if first and key in ["alpha","beta"]:
                                    self.implementation_barrier.wait()
                                tool = ("bash", {"command":write_command(key)})
                            elif len(bash_calls) == 1:
                                assert result(bash_calls[0])["exit_code"] == 0
                                tool = ("bash", {"command":commit_command(key)})
                            else:
                                output = result(bash_calls[-1])
                                assert output["exit_code"] == 0, output
                                commit = re.findall(r"(?m)^[0-9a-f]{40}$",output["output"])[-1]
                                report = {"kind":"task","summary":f"Implemented {key}.","checks":["Created and committed the fixture file."],"commit":commit}
                        elif work == "Verify":
                            command = verification_command(key)
                            if not bash_calls:
                                with self.lock:
                                    first = key not in self.verifications
                                    self.verifications.add(key)
                                if first and key in ["alpha","beta"]:
                                    self.verification_barrier.wait()
                                tool = ("bash", {"command":command})
                            else:
                                output = result(bash_calls[-1])
                                assert output["exit_code"] == 0, output
                                criterion = flow["task"]["acceptance"][0] if key else flow["plan"]["acceptance"][0]
                                report = {"kind":"verification","verification":{
                                    "summary":"Verified the prepared commit.","commit":flow["integration"]["head"],"checks":[command],"decisions":[],
                                    "evidence":[{"criterion":criterion,"detail":"The real shell check passed.","satisfied":True,"decision":None,"check":command}]}}
                        else:
                            raise AssertionError(f"Unexpected assignment: {assignment}")
                    if report:
                        tool = ("board.report", {"id":item_id,"attempt":attempt,"report":report})
                else:
                    result(next(c for c in calls if c["function"]["name"] == "board.report"))
            else:
                available = [t["function"]["name"] for t in body.get("tools", [])]
                if "memory.update" in available and "memory.update" not in names:
                    tool = ("memory.update", {"no_update":{"reason":"No fixture context is missing."}})
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            stream_id = "chatcmpl-" + uuid.uuid4().hex
            delta = {"role":"assistant"}
            if tool:
                name, arguments = tool
                delta["tool_calls"] = [{"index":0,"id":"call_" + uuid.uuid4().hex,"type":"function",
                                        "function":{"name":name,"arguments":json.dumps(arguments)}}]
            else:
                delta["content"] = "Fixture step complete."
            for value, finish in [(delta,None),({},"tool_calls" if tool else "stop")]:
                event = {"id":stream_id,"object":"chat.completion.chunk","created":1,"model":"milestone-fixture",
                         "choices":[{"index":0,"delta":value,"finish_reason":finish}]}
                self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except Exception as error:
            type(self).errors.append(str(error))
            self.send_error(500,str(error))


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
        for command in [["git", "init", "-q", "-b", "main"], ["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@invalid", "commit", "--allow-empty", "-qm", "Fixture"]]:
            subprocess.run(command, cwd=repository, env=environment, check=True, capture_output=True)
        for key, value in [("user.name","Fixture"),("user.email","fixture@invalid")]:
            subprocess.run(["git","config","--local",key,value],cwd=repository,env=environment,check=True,capture_output=True)
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
            deadline = time.monotonic() + 90
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

        cli("add","Implement fixture","--body","Build independent alpha and beta outputs, then an optional extension.","--json")
        cli("milestone","1","--json")
        wait_for(lambda f: f and f["plan"] and f["plan"]["decisions"])
        cli("answer","1","scope","Would that be expensive?","--json")
        waiting = wait_for(lambda f: f["plan"] and f["plan"]["decisions"][0]["messages"]
                           and not f["plan"]["decisions"][0]["messages"][-1]["owner"] and not f["active"])
        assert waiting["plan"]["decisions"][0]["resolution"] is None

        def tasks():
            milestone = cli("flow","1","--json")
            return {item["workflow"]["task"]["key"]:item for item in
                    [cli("show",str(task_id),"--json") for task_id in milestone["plan"]["tasks"]]}

        wait_for(lambda f: all(tasks()[key]["workflow"]["integrated"] for key in ["alpha","beta"]) and not f["active"])
        initial = tasks()
        assert {"alpha","beta"}.issubset(Provider.starts)
        assert "extension" not in Provider.starts
        assert initial["alpha"]["workflow"]["worker"]["worktree"] != initial["beta"]["workflow"]["worker"]["worktree"]
        assert any("Main advanced" in reason for item in initial.values() for reason in item["workflow"]["history"])
        # Restart with the decision durable; a read-only replan may still be in flight.
        processes[-1].terminate()
        processes[-1].wait(timeout=10)
        with (root / "horizon-agentd.log").open("a") as log:
            processes.append(subprocess.Popen([binaries / "horizon-agentd","--socket",root / "agentd.sock"],
                                              cwd=repository,env=environment,stdout=log,stderr=log))
        cli("answer","1","scope","Include the extension.","--json")
        complete = wait_for(lambda f: f["achieved"] and not f["active"])
        assert complete["plan"]["decisions"][0]["resolution"] == "Include the extension."
        final_tasks = tasks()
        assert all(item["workflow"]["integrated"] for item in final_tasks.values())
        assert len({item["workflow"]["worker"]["worktree"] for item in final_tasks.values()}) == 3
        for key in ["alpha","beta","extension"]:
            assert (repository / f"{key}.txt").read_text() == key + "\n"
        assert cli("show","1","--json")["status"] == "done"
        assert len(complete["history"]) >= 3, "Results must cause automatic replanning"
        passed = True
        print("PASS: scoped discussion → independent parallel worktrees → verified main merges → restart → decision → dependent work → milestone achievement")
        print(f"PASS: main advancement forced fresh verification; {Provider.requests_seen} local provider requests")

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
