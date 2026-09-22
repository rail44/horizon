"""Pinned tool installation and captured process execution."""

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


class AuditError(Exception):
    pass


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False, allow_nan=False) + "\n")


def git(root, *args):
    result = subprocess.run(["git", *args], cwd=root, capture_output=True)
    if result.returncode:
        raise AuditError(result.stderr.decode(errors="replace").strip())
    return result.stdout


class Toolchain:
    def __init__(self, root):
        self.cache = root / "target/refactor-audit/tools"
        self.specs = json.loads((HERE / "tool-versions.json").read_text())
        self.used = {}

    def install(self, name):
        spec = self.specs[name]
        key = platform.system().lower() + "-" + platform.machine().lower()
        asset = spec.get("archive", spec.get(key))
        if not asset:
            return self.resolve(name)
        destination = self.cache / spec["command"]
        stamp = destination.with_suffix(".install.json")
        if destination.exists() and stamp.exists():
            installed = json.loads(stamp.read_text())
            if installed.get("archive_sha256") == asset["sha256"] and installed.get(
                "binary_sha256"
            ) == sha256(destination.read_bytes()):
                return self.resolve(name)
        self.cache.mkdir(parents=True, exist_ok=True)
        print(f"Downloading {name} {spec['version']}", flush=True)
        request = urllib.request.Request(
            asset["url"], headers={"User-Agent": "horizon-refactor-audit"}
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            archive = response.read()
        if sha256(archive) != asset["sha256"]:
            raise AuditError(f"Checksum mismatch for {name}; cache unchanged")
        if "member" in asset:
            import io

            with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as bundle:
                member = bundle.getmember(asset["member"])
                if not member.isfile():
                    raise AuditError(f"Unexpected archive member for {name}")
                binary = bundle.extractfile(member).read()
        else:
            binary = archive
        with tempfile.NamedTemporaryFile(dir=self.cache, delete=False) as temporary:
            temporary.write(binary)
            temp_path = Path(temporary.name)
        temp_path.chmod(0o755)
        os.replace(temp_path, destination)
        write_json(stamp, {"archive_sha256": asset["sha256"], "binary_sha256": sha256(binary)})
        return self.resolve(name)

    def resolve(self, name):
        if name in self.used:
            return Path(self.used[name]["path"])
        spec = self.specs[name]
        local = self.cache / spec["command"]
        path = local if local.exists() else Path(shutil.which(spec["command"]) or local)
        if not path.is_file():
            hint = spec.get("install_hint", "run setup --history")
            raise AuditError(f"Missing {name} {spec['version']}; run setup or: {hint}")
        digest = sha256(path.read_bytes())
        stamp = local.with_suffix(".install.json")
        if path == local and name != "code-maat" and stamp.exists():
            if json.loads(stamp.read_text()).get("binary_sha256") != digest:
                raise AuditError(f"Cached {name} changed; rerun setup")
        if name == "code-maat":
            if digest != spec["archive"]["sha256"]:
                raise AuditError("Code Maat JAR checksum mismatch")
            output = "release " + spec["version"]
        else:
            try:
                check = subprocess.run(
                    [str(path), "--version"], capture_output=True, text=True, timeout=30
                )
            except (OSError, subprocess.TimeoutExpired) as error:
                raise AuditError(f"Could not check {name} version: {error}") from error
            output = (check.stdout + check.stderr).strip()
            if check.returncode or not re.search(
                r"(?<![\d.])" + re.escape(spec["version"]) + r"(?![\d.])", output
            ):
                raise AuditError(
                    f"Expected {name} {spec['version']}, got {output!r}; {spec['install_hint']}"
                )
        self.used[name] = {
            "version": spec["version"],
            "reported_version": output,
            "path": str(path),
            "sha256": digest,
        }
        return path


class Runner:
    def __init__(self, output):
        self.output = output
        self.records = []
        (output / "raw").mkdir()

    def run(self, name, args, cwd, timeout=300):
        args = list(map(str, args))
        started = time.monotonic()
        try:
            result = subprocess.run(args, cwd=cwd, capture_output=True, timeout=timeout)
            stdout, stderr, code = result.stdout, result.stderr, result.returncode
        except subprocess.TimeoutExpired as error:
            stdout, stderr, code = error.stdout or b"", error.stderr or b"", None
        except OSError as error:
            stdout, stderr, code = b"", str(error).encode(), None
        (self.output / "raw" / (name + ".stdout")).write_bytes(stdout)
        (self.output / "raw" / (name + ".stderr")).write_bytes(stderr)
        self.records.append(
            {
                "name": name,
                "command": args,
                "cwd": str(cwd),
                "exit_code": code,
                "seconds": round(time.monotonic() - started, 4),
            }
        )
        if code != 0:
            raise AuditError(f"{name} failed (exit {code}); see raw/{name}.stderr")
        return stdout
