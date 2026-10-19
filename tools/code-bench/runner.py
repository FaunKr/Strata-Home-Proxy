#!/usr/bin/env python3
"""The code benchmark's runner, in its own container on the NAS.

The proxy sends a run: an API key of its own, the model and the tasks with
their hidden tests. For each task opencode works in a fresh folder against
the proxy's OpenAI endpoint; afterwards the hidden tests score what it left.
The proxy asks for the state until the run has ended.

  GET  /health               {"opencode": "<version>", "problem": <why runs can't start, or null>}
  POST /runs                 starts a run; 409 while one runs
  GET  /runs/current         the running or last run, 404 before the first
  POST /runs/current/cancel  ends the running task's opencode and the run

Only the standard library, so the image needs Python and opencode alone.
"""

import http.client
import ipaddress
import json
import os
import pwd
import shutil
import socket
import signal
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

LISTEN = os.environ.get("LISTEN_ADDR", "0.0.0.0:8096")
# Where opencode reaches the proxy, e.g. http://host.docker.internal:8080.
PROXY_URL = os.environ.get("PROXY_URL", "http://host.docker.internal:8080").rstrip("/")
OPENCODE = os.environ.get("OPENCODE_BIN", "opencode")
PYTHON = os.environ.get("PYTHON_BIN", sys.executable)
WORK_DIR = os.environ.get("WORK_DIR") or tempfile.gettempdir()
# The hidden tests' own limit, after opencode.
TEST_TIMEOUT = int(os.environ.get("TEST_TIMEOUT_SECS", "120"))
DEFAULT_TIMEOUT = 30 * 60
# Files the agent leaves that the page shows, at most this much each.
MAX_FILE_BYTES = 64 * 1024
MAX_OUTPUT_CHARS = 8 * 1024
SCORE_MARK = "CODEBENCH_SCORE "
# The user opencode and the hidden tests run as. Set in the image: the agent
# then reaches neither the runner's API nor the NAS or the home network
# (see firewall_rules). Unset, e.g. in the tests, they run as the runner.
AGENT_USER = os.environ.get("AGENT_USER") or None
FIREWALL_CHAIN = "CODEBENCH_AGENT"
# What the agent must not reach: loopback (the runner's API), private,
# link-local and CGNAT ranges (the NAS and the home network).
BLOCKED_NETS = [
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "100.64.0.0/10",
    "0.0.0.0/8",
]
# Where opencode reaches the proxy: a relay in the runner that passes on the
# OpenAI endpoint (/v1) only, so the agent cannot reach the proxy's web UI.
RELAY_ADDR = os.environ.get("RELAY_ADDR", "127.0.0.1:8097")
# Docker's own DNS in a user-defined network.
DOCKER_DNS = "127.0.0.11"
# Why runs are refused, e.g. the firewall could not be set.
PROBLEM = None

# Runs the hidden tests and prints how many passed on a line of its own, so
# whatever the tested code prints does not count. Started isolated (`-I`),
# with the task folder after the standard library: a file the agent names
# like a standard module does not replace it.
SCORE_SCRIPT = """
import json, os, sys, unittest
sys.path.append(os.getcwd())
suite = unittest.defaultTestLoader.loadTestsFromName("hidden_test")
result = unittest.TextTestRunner(stream=sys.stdout, verbosity=2).run(suite)
# A skipped test did not pass.
failed = (len(result.failures) + len(result.errors) + len(result.skipped)
          + len(result.expectedFailures) + len(result.unexpectedSuccesses))
print("%s" + json.dumps({"passed": result.testsRun - failed, "total": result.testsRun}))
""" % SCORE_MARK

PROMPT = (
    "Solve the task in TASK.md in the current folder. "
    "Write the files it asks for into this folder and check them with the example tests. "
    "Work on your own until the task is done."
)


_version = None
_version_lock = threading.Lock()


def opencode_version():
    """opencode's version, asked once: /health is asked every few seconds."""
    global _version
    with _version_lock:
        if _version is None:
            _version = _ask_version()
        return _version


def _ask_version():
    try:
        out = subprocess.run(
            [OPENCODE, "--version"], capture_output=True, text=True, timeout=30
        )
        return out.stdout.strip() or None
    except (OSError, subprocess.TimeoutExpired):
        return None


# What the proxy's own page carries; a page without it is someone else's.
PROXY_MARK = b"<title>Strata Proxy</title>"


def proxy_problem():
    """Why PROXY_URL is not the Strata proxy, or None. Asks its web UI's page,
    which never wakes the PC: another service on that address would answer
    the agent's model requests with its own errors."""
    url = urlsplit(PROXY_URL)
    conn_class = http.client.HTTPSConnection if url.scheme == "https" else http.client.HTTPConnection
    conn = conn_class(url.hostname, url.port, timeout=10)
    try:
        conn.request("GET", url.path.rstrip("/") + "/admin")
        resp = conn.getresponse()
        body = resp.read(256 * 1024)
    except OSError as e:
        return f"The proxy at PROXY_URL ({PROXY_URL}) does not answer: {e}."
    finally:
        conn.close()
    if resp.status == 200 and PROXY_MARK in body:
        return None
    sample = body[:200].decode(errors="replace").strip()
    return (
        f"PROXY_URL ({PROXY_URL}) answers, but not as the Strata proxy (HTTP {resp.status}: {sample}). "
        "Set PROXY_URL in the code-bench service to the proxy's address and port (its LISTEN_ADDR)."
    )


def problem():
    """Why runs can't start now, or None."""
    return PROBLEM or proxy_problem()


def config(model, api_key):
    """opencode's settings for one task: the proxy as its only provider."""
    return {
        "$schema": "https://opencode.ai/config.json",
        "provider": {
            "strata": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "Strata",
                "options": {"baseURL": f"http://{RELAY_ADDR}/v1", "apiKey": api_key},
                "models": {
                    model: {"name": model, "temperature": True, "options": {"temperature": 0}}
                },
            }
        },
        "agent": {"build": {"temperature": 0}},
        "permission": {"edit": "allow", "bash": "allow", "webfetch": "deny"},
        "share": "disabled",
        "autoupdate": False,
    }


class Stats:
    """What opencode's JSON events tell about the model's work."""

    def __init__(self):
        self.steps = 0
        self.input_tokens = 0
        self.output_tokens = 0
        # Milliseconds the model spent on the steps.
        self.step_ms = 0
        self._step_started = None
        # What opencode's error event said, e.g. that the model is unknown.
        self.error = None

    def read(self, line):
        try:
            event = json.loads(line)
        except ValueError:
            return
        if not isinstance(event, dict):
            return
        kind = event.get("type")
        at = event.get("timestamp")
        if kind == "error":
            data = (event.get("error") or {}).get("data") or {}
            self.error = str(data.get("message") or event.get("error"))
        elif kind == "step_start":
            self._step_started = at
        elif kind == "step_finish":
            self.steps += 1
            tokens = (event.get("part") or {}).get("tokens") or {}
            self.input_tokens += int(tokens.get("input") or 0)
            self.output_tokens += int(tokens.get("output") or 0) + int(
                tokens.get("reasoning") or 0
            )
            if isinstance(at, (int, float)) and isinstance(self._step_started, (int, float)):
                self.step_ms += max(0, at - self._step_started)
            self._step_started = None

    def output_tps(self):
        if not self.step_ms or not self.output_tokens:
            return None
        return round(self.output_tokens / (self.step_ms / 1000), 1)


class Run:
    def __init__(self, run_id, model, api_key, tasks, timeout):
        self.lock = threading.Lock()
        self.id = run_id
        self.model = model
        self.api_key = api_key
        self.timeout = timeout
        self.hidden = {t["name"]: t["hidden_test"] for t in tasks}
        self.files = {t["name"]: t["files"] for t in tasks}
        self.state = "running"
        self.error = None
        self.started = int(time.time())
        self.ended = None
        self.cancelled = False
        self.process = None
        self.tasks = [
            {
                "name": t["name"],
                "title": t.get("title", t["name"]),
                "state": "pending",
                "total_secs": None,
                "steps": 0,
                "input_tokens": 0,
                "output_tokens": 0,
                "output_tps": None,
                "passed": 0,
                "total": 0,
                "timed_out": False,
                "files": {},
                "test_output": "",
                "error": None,
            }
            for t in tasks
        ]

    def view(self):
        with self.lock:
            return {
                "id": self.id,
                "model": self.model,
                "state": self.state,
                "error": self.error,
                "started": self.started,
                "ended": self.ended,
                "tasks": [dict(t) for t in self.tasks],
            }

    def cancel(self):
        with self.lock:
            if self.state != "running":
                return False
            self.cancelled = True
            process = self.process
        if process is not None:
            stop(process)
        return True

    def work(self):
        try:
            for i, task in enumerate(self.tasks):
                if self.cancelled:
                    break
                self.task(i, task["name"])
            with self.lock:
                self.state = "cancelled" if self.cancelled else "done"
        except Exception as e:  # noqa: BLE001 - the page shows why
            with self.lock:
                self.state = "failed"
                self.error = str(e)
        finally:
            with self.lock:
                self.ended = int(time.time())
                self.api_key = None

    def update(self, i, **fields):
        with self.lock:
            self.tasks[i].update(fields)

    def task(self, i, name):
        self.update(i, state="running")
        folder = tempfile.mkdtemp(prefix=f"code-bench-{name}-", dir=WORK_DIR)
        try:
            for path, text in self.files[name].items():
                write(folder, path, text)
            write(folder, "opencode.json", json.dumps(config(self.model, self.api_key)))
            give_to_agent(folder)
            stats, secs, timed_out, error = self.opencode(i, folder)
            if self.cancelled:
                self.update(i, state="cancelled")
                return
            files = solution_files(folder, set(self.files[name]))
            passed, total, output = score(folder, self.hidden[name])
            self.update(
                i,
                state="done",
                total_secs=round(secs, 1),
                steps=stats.steps,
                input_tokens=stats.input_tokens,
                output_tokens=stats.output_tokens,
                output_tps=stats.output_tps(),
                passed=passed,
                total=total,
                timed_out=timed_out,
                files=files,
                test_output=output,
                error=error,
            )
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def opencode(self, i, folder):
        """Lets opencode work on the task; its stats, the time, whether it
        ran out of time, and why it ended otherwise."""
        env = agent_env(
            dict(os.environ, OPENCODE_DISABLE_MODELS_FETCH="1", OPENCODE_DISABLE_AUTOUPDATE="1")
        )
        env.pop("OPENCODE_CONFIG", None)
        # opencode takes its project folder, and so its opencode.json, from PWD.
        env["PWD"] = folder
        args = [
            OPENCODE, "run", "--pure", "--format", "json",
            "--model", f"strata/{self.model}",
            "--title", f"code bench {self.tasks[i]['name']}",
            PROMPT,
        ]
        stats = Stats()
        started = time.monotonic()
        process = subprocess.Popen(
            args,
            cwd=folder,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
            **as_agent(),
        )
        with self.lock:
            self.process = process
            cancelled = self.cancelled
        if cancelled:
            stop(process)
        timed_out = threading.Event()
        finished = threading.Event()

        def deadline():
            # opencode may have ended while something it started still holds
            # its output open; that counts as running out of time too.
            if not finished.is_set():
                timed_out.set()
                stop(process)

        def reap():
            # What the agent started in the background ends with opencode, so
            # it neither outlives the task nor keeps its output open.
            process.wait()
            kill_group(process.pid)

        threading.Thread(target=reap, daemon=True).start()

        timer = threading.Timer(self.timeout, deadline)
        timer.start()
        errors = []
        reader = threading.Thread(target=lambda: errors.extend(process.stderr), daemon=True)
        reader.start()
        try:
            for line in process.stdout:
                stats.read(line)
                self.update(
                    i,
                    steps=stats.steps,
                    input_tokens=stats.input_tokens,
                    output_tokens=stats.output_tokens,
                    total_secs=round(time.monotonic() - started, 1),
                )
            code = process.wait()
            finished.set()
        finally:
            timer.cancel()
            kill_group(process.pid)
            reader.join(timeout=5)
            with self.lock:
                self.process = None
        error = stats.error
        if timed_out.is_set():
            error = f"stopped after {self.timeout // 60} min"
        elif code != 0 and not self.cancelled and not error:
            tail = "".join(errors)[-500:].strip()
            error = f"opencode ended with code {code}" + (f": {tail}" if tail else "")
        return stats, time.monotonic() - started, timed_out.is_set(), error


def agent():
    """The agent's user entry, or None when it runs as the runner."""
    return pwd.getpwnam(AGENT_USER) if AGENT_USER else None


def as_agent():
    """Popen arguments that start a process as the agent."""
    user = agent()
    if user is None:
        return {}
    return {"user": user.pw_uid, "group": user.pw_gid, "extra_groups": []}


def agent_env(env):
    user = agent()
    if user is not None:
        env["HOME"] = user.pw_dir
        env["USER"] = env["LOGNAME"] = user.pw_name
    return env


def give_to_agent(folder):
    """Hands the task folder to the agent, so it can work in it."""
    user = agent()
    if user is None:
        return
    for root, dirs, names in os.walk(folder):
        os.chown(root, user.pw_uid, user.pw_gid)
        for name in dirs + names:
            os.chown(os.path.join(root, name), user.pw_uid, user.pw_gid, follow_symlinks=False)


def firewall_rules(uid, relay_ip, relay_port):
    """iptables commands that keep the agent's connections off the runner's
    API, the proxy, the NAS and the home network, apart from the relay to
    the proxy's /v1; the internet stays open."""
    chain = ["iptables", "-w"]
    rules = [
        chain + ["-N", FIREWALL_CHAIN],
        chain + ["-F", FIREWALL_CHAIN],
        chain + ["-A", FIREWALL_CHAIN, "-d", DOCKER_DNS, "-p", "udp", "--dport", "53", "-j", "ACCEPT"],
        chain + ["-A", FIREWALL_CHAIN, "-d", DOCKER_DNS, "-p", "tcp", "--dport", "53", "-j", "ACCEPT"],
        chain + ["-A", FIREWALL_CHAIN, "-d", relay_ip, "-p", "tcp", "--dport", str(relay_port), "-j", "ACCEPT"],
    ]
    for net in BLOCKED_NETS:
        rules.append(chain + ["-A", FIREWALL_CHAIN, "-d", net, "-j", "REJECT"])
    rules.append(chain + ["-A", "OUTPUT", "-m", "owner", "--uid-owner", str(uid), "-j", FIREWALL_CHAIN])
    return rules


def relay_address():
    """The relay's IP and port, from RELAY_ADDR."""
    host, _, port = RELAY_ADDR.rpartition(":")
    return socket.gethostbyname(host), int(port)


def set_firewall():
    """Sets the agent's firewall; why it failed, or None."""
    user = agent()
    if user is None:
        return None
    try:
        relay_ip, relay_port = relay_address()
        ipaddress.ip_address(relay_ip)
    except (OSError, ValueError) as e:
        return f"The relay address in RELAY_ADDR ({RELAY_ADDR}) does not resolve: {e}"
    for rule in firewall_rules(user.pw_uid, relay_ip, relay_port):
        # A restarted container may have the chain already.
        if rule[2] == "-N" and run_quietly(rule[:2] + ["-L", FIREWALL_CHAIN]):
            continue
        if rule[2] == "-A" and rule[3] == "OUTPUT" and run_quietly(rule[:2] + ["-C"] + rule[3:]):
            continue
        if not run_quietly(rule):
            return (
                "The code-bench container cannot set its firewall for the agent. "
                "It needs the NET_ADMIN capability (cap_add in docker-compose.yml)."
            )
    # No IPv6 for the agent: the ranges above are IPv4 only.
    no_ipv6 = ["OUTPUT", "-m", "owner", "--uid-owner", str(user.pw_uid), "-j", "REJECT"]
    if not run_quietly(["ip6tables", "-w", "-C"] + no_ipv6):
        run_quietly(["ip6tables", "-w", "-A"] + no_ipv6)
    return None


def run_quietly(args):
    try:
        return subprocess.run(args, capture_output=True, timeout=30).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def stop(process):
    """Ends opencode and whatever it started."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    kill_group(process.pid)


def kill_group(pid):
    """Kills what is left of opencode's process group."""
    try:
        os.killpg(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def write(folder, path, text):
    """Writes a file the runner puts in a task folder. What the agent left
    under that name goes first, and links are never followed: the runner may
    be root, the folder is the agent's."""
    full = os.path.join(folder, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    if os.path.isdir(full) and not os.path.islink(full):
        shutil.rmtree(full)
    elif os.path.lexists(full):
        os.unlink(full)
    fd = os.open(full, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(text)


def solution_files(folder, given):
    """The Python files the agent left, apart from those it was given."""
    files = {}
    for root, dirs, names in os.walk(folder):
        dirs[:] = [d for d in dirs if not d.startswith(".") and d != "__pycache__"]
        for name in sorted(names):
            path = os.path.relpath(os.path.join(root, name), folder)
            full = os.path.join(root, name)
            # A link could point at any file in the container.
            if not name.endswith(".py") or path in given or os.path.islink(full):
                continue
            try:
                fd = os.open(full, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            except OSError:
                continue
            with os.fdopen(fd, "rb") as f:
                if not os.path.isfile(full):
                    continue
                data = f.read(MAX_FILE_BYTES + 1)
            text = data[:MAX_FILE_BYTES].decode("utf-8", "replace")
            if len(data) > MAX_FILE_BYTES:
                text += "\n… (cut)"
            files[path] = text
    return files


def score(folder, hidden_test):
    """How many hidden tests pass, out of how many, and their output."""
    try:
        write(folder, "hidden_test.py", hidden_test)
        write(folder, "_codebench_score.py", SCORE_SCRIPT)
        give_to_agent(folder)
    except OSError as e:
        return 0, total_tests(hidden_test), f"The hidden tests could not be put in place: {e}"
    # Its own process group: what the tested code starts ends with it.
    process = subprocess.Popen(
        [PYTHON, "-I", "_codebench_score.py"],
        cwd=folder,
        env=agent_env(dict(os.environ)),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
        **as_agent(),
    )
    try:
        output, errors = process.communicate(timeout=TEST_TIMEOUT)
    except subprocess.TimeoutExpired:
        kill_group(process.pid)
        output, _ = process.communicate()
        return 0, total_tests(hidden_test), (output or "") + f"\nThe tests took longer than {TEST_TIMEOUT} s."
    finally:
        kill_group(process.pid)
    passed, total = 0, total_tests(hidden_test)
    # Only stdout carries the mark; the score script prints it last.
    for line in output.splitlines():
        if line.startswith(SCORE_MARK):
            try:
                result = json.loads(line[len(SCORE_MARK):])
                # Code that does not import counts as one failed test there.
                passed = int(result["passed"])
                total = max(int(result["total"]), total)
            except (ValueError, KeyError, TypeError):
                pass
    shown = "\n".join(l for l in (output + errors).splitlines() if not l.startswith(SCORE_MARK))
    return passed, total, shown[-MAX_OUTPUT_CHARS:]


def total_tests(hidden_test):
    """The number of tests, for code that does not even import."""
    return sum(1 for l in hidden_test.splitlines() if l.strip().startswith("def test_"))


class Runner:
    def __init__(self):
        self.lock = threading.Lock()
        self.run = None
        self.next_id = 1

    def start(self, body):
        try:
            model = str(body["model"])
            api_key = str(body["api_key"])
            tasks = [
                {
                    "name": str(t["name"]),
                    "title": str(t.get("title") or t["name"]),
                    "files": {str(k): str(v) for k, v in t["files"].items()},
                    "hidden_test": str(t["hidden_test"]),
                }
                for t in body["tasks"]
            ]
            timeout = int(body.get("timeout_secs") or DEFAULT_TIMEOUT)
        except (KeyError, TypeError, ValueError, AttributeError) as e:
            return 400, {"reason": f"bad run: {e}"}
        if not tasks or not model or "/" in model:
            return 400, {"reason": "bad run: no tasks or no model"}
        reason = problem()
        if reason:
            return 503, {"reason": reason}
        with self.lock:
            if self.run is not None and self.run.view()["state"] == "running":
                return 409, {"reason": "A code benchmark is running already."}
            run = Run(self.next_id, model, api_key, tasks, timeout)
            self.next_id += 1
            self.run = run
        threading.Thread(target=run.work, daemon=True).start()
        return 202, run.view()


RUNNER = Runner()


class Relay(BaseHTTPRequestHandler):
    """Passes opencode's requests to the proxy's OpenAI endpoint (/v1) and
    nothing else, streaming the answer."""

    def relay(self):
        if not self.path.startswith("/v1/"):
            self.send_error(403, "only /v1 is relayed")
            return
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length) if length else None
        url = urlsplit(PROXY_URL)
        conn_class = http.client.HTTPSConnection if url.scheme == "https" else http.client.HTTPConnection
        conn = conn_class(url.hostname, url.port, timeout=DEFAULT_TIMEOUT)
        headers = {
            k: v for k, v in self.headers.items() if k.lower() not in ("host", "connection", "content-length")
        }
        try:
            conn.request(self.command, url.path.rstrip("/") + self.path, body=body, headers=headers)
            resp = conn.getresponse()
        except OSError as e:
            self.send_error(502, f"proxy unreachable: {e}")
            return
        # HTTP/1.0: the answer ends when the connection closes, so a stream
        # passes through as it comes.
        self.send_response(resp.status, resp.reason)
        for k, v in resp.getheaders():
            if k.lower() not in ("transfer-encoding", "connection", "content-length"):
                self.send_header(k, v)
        self.end_headers()
        try:
            while chunk := resp.read1(64 * 1024):
                self.wfile.write(chunk)
                self.wfile.flush()
        except OSError:
            pass
        finally:
            conn.close()

    do_GET = do_POST = relay

    def log_message(self, fmt, *args):
        pass


def serve_relay():
    host, _, port = RELAY_ADDR.rpartition(":")
    server = ThreadingHTTPServer((host, int(port)), Relay)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


class Handler(BaseHTTPRequestHandler):
    def answer(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/health":
            self.answer(200, {"opencode": opencode_version(), "problem": problem()})
        elif self.path == "/runs/current":
            run = RUNNER.run
            if run is None:
                self.answer(404, {"reason": "No run yet."})
            else:
                self.answer(200, run.view())
        else:
            self.answer(404, {"reason": "not found"})

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b""
        if self.path == "/runs":
            try:
                body = json.loads(raw or b"{}")
            except ValueError:
                self.answer(400, {"reason": "bad JSON"})
                return
            code, answer = RUNNER.start(body)
            self.answer(code, answer)
        elif self.path == "/runs/current/cancel":
            run = RUNNER.run
            if run is not None and run.cancel():
                self.answer(202, run.view())
            else:
                self.answer(409, {"reason": "No code benchmark is running."})
        else:
            self.answer(404, {"reason": "not found"})

    def log_message(self, fmt, *args):
        sys.stderr.write("code-bench: " + (fmt % args) + "\n")


def main():
    global PROBLEM
    PROBLEM = set_firewall()
    if PROBLEM:
        sys.stderr.write(f"code-bench: {PROBLEM}\n")
    serve_relay()
    host, _, port = LISTEN.rpartition(":")
    server = ThreadingHTTPServer((host or "0.0.0.0", int(port)), Handler)
    sys.stderr.write(f"code-bench: listening on {LISTEN}, proxy at {PROXY_URL}\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
