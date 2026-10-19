"""The runner against a fake opencode: tasks, scores, stats, limits."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request
from http.server import ThreadingHTTPServer
from pathlib import Path
from threading import Thread

HERE = Path(__file__).parent
os.environ["WORK_DIR"] = tempfile.mkdtemp()
# Like /tmp in the image: the agent reaches its task folders.
os.chmod(os.environ["WORK_DIR"], 0o755)
# Like opencode in the image, where the agent can run it; the checkout may
# sit under a home folder the agent can't enter (CI's runner).
_bin = tempfile.mkdtemp()
os.chmod(_bin, 0o755)
os.environ["OPENCODE_BIN"] = shutil.copy(HERE / "testdata" / "fake-opencode", _bin)
sys.path.insert(0, str(HERE))
import runner  # noqa: E402

TASK = {
    "name": "double",
    "title": "Double",
    "files": {"TASK.md": "Write double.py with double(n).\n"},
    "hidden_test": (
        "import unittest\nfrom double import double\n\n"
        "class T(unittest.TestCase):\n"
        "    def test_two(self):\n        self.assertEqual(double(2), 4)\n\n"
        "    def test_zero(self):\n        self.assertEqual(double(0), 0)\n"
    ),
}


def fake_server(status, body):
    """A server that answers every GET with status and body; its URL."""

    class Answer(runner.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(status)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Answer)
    Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_port}"


class RunnerTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), runner.Handler)
        cls.url = f"http://127.0.0.1:{cls.server.server_port}"
        Thread(target=cls.server.serve_forever, daemon=True).start()
        # The proxy's sign-in page, as the runner checks PROXY_URL with it.
        cls.proxy, runner.PROXY_URL = fake_server(200, b"<html><title>Strata Proxy</title></html>")

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.proxy.shutdown()

    def setUp(self):
        os.environ.pop("FAKE_MODE", None)
        os.environ.pop("FAKE_SOLUTION", None)

    def call(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.url + path, data=data, method=method)
        try:
            with urllib.request.urlopen(req) as resp:
                return resp.status, json.load(resp)
        except urllib.error.HTTPError as e:
            return e.code, json.load(e)

    def run_tasks(self, tasks, timeout=60):
        code, run = self.call(
            "POST", "/runs", {"model": "IQ3_XXS", "api_key": "shp_k", "tasks": tasks, "timeout_secs": timeout}
        )
        self.assertEqual(code, 202, run)
        return self.wait()

    def wait(self):
        for _ in range(400):
            code, run = self.call("GET", "/runs/current")
            if run["state"] != "running":
                return run
            time.sleep(0.025)
        self.fail(f"run did not end: {run}")

    def test_health_names_the_opencode_version(self):
        self.assertEqual(self.call("GET", "/health"), (200, {"opencode": "1.19.3", "problem": None}))

    def test_without_its_firewall_the_runner_refuses_runs(self):
        runner.PROBLEM = "no firewall"
        try:
            self.assertEqual(self.call("GET", "/health")[1]["problem"], "no firewall")
            code, answer = self.call(
                "POST", "/runs", {"model": "IQ3_XXS", "api_key": "shp_k", "tasks": [TASK]}
            )
            self.assertEqual((code, answer["reason"]), (503, "no firewall"))
        finally:
            runner.PROBLEM = None

    def test_another_service_at_proxy_url_is_a_problem(self):
        other, url = fake_server(404, b'{"detail":"Not Found"}')
        proxy_url, runner.PROXY_URL = runner.PROXY_URL, url
        try:
            problem = self.call("GET", "/health")[1]["problem"]
            code, answer = self.call(
                "POST", "/runs", {"model": "IQ3_XXS", "api_key": "shp_k", "tasks": [TASK]}
            )
        finally:
            runner.PROXY_URL = proxy_url
            other.shutdown()
        self.assertIn("not as the Strata proxy", problem)
        self.assertIn('{"detail":"Not Found"}', problem)
        self.assertEqual((code, answer["reason"]), (503, problem))

    def test_no_answer_at_proxy_url_is_a_problem(self):
        proxy_url, runner.PROXY_URL = runner.PROXY_URL, "http://127.0.0.1:9"
        try:
            problem = self.call("GET", "/health")[1]["problem"]
        finally:
            runner.PROXY_URL = proxy_url
        self.assertIn("does not answer", problem)

    def test_the_firewall_lets_the_agent_reach_the_proxy_and_the_internet_only(self):
        rules = [" ".join(r) for r in runner.firewall_rules(1001, "127.0.0.1", 8097)]
        chain = runner.FIREWALL_CHAIN
        allow_relay = f"iptables -w -A {chain} -d 127.0.0.1 -p tcp --dport 8097 -j ACCEPT"
        self.assertIn(allow_relay, rules)
        # The relay before the loopback range that includes it.
        block_loopback = f"iptables -w -A {chain} -d 127.0.0.0/8 -j REJECT"
        self.assertLess(rules.index(allow_relay), rules.index(block_loopback))
        for net in ["172.16.0.0/12", "10.0.0.0/8", "192.168.0.0/16"]:
            self.assertIn(f"iptables -w -A {chain} -d {net} -j REJECT", rules)
        self.assertEqual(rules[-1], f"iptables -w -A OUTPUT -m owner --uid-owner 1001 -j {chain}")
        # Docker's DNS on its port only.
        self.assertEqual([r for r in rules if runner.DOCKER_DNS in r], [
            f"iptables -w -A {chain} -d {runner.DOCKER_DNS} -p {p} --dport 53 -j ACCEPT" for p in ("udp", "tcp")
        ])
        # Nothing rejects all: the internet stays open.
        self.assertFalse([r for r in rules if r.endswith("-j REJECT") and " -d " not in r])

    @unittest.skipUnless(
        os.geteuid() == 0 and shutil.which("unshare") and shutil.which("iptables"),
        "needs root, unshare and iptables for a network namespace",
    )
    def test_the_firewall_keeps_the_agent_to_the_relay_on_loopback(self):
        out = subprocess.run(
            ["unshare", "--net", sys.executable, "-I", str(HERE / "testdata" / "firewall-probe.py")],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(
            json.loads(out.stdout),
            {"problem": None, "agent_relay": True, "agent_other": False, "root_other": True},
        )

    def test_links_the_agent_leaves_are_not_followed(self):
        target = Path(os.environ["WORK_DIR"], "outside.py")
        target.write_text("SECRET = 1\n")
        os.environ["FAKE_MODE"], os.environ["FAKE_TARGET"] = "links", str(target)
        try:
            task = self.run_tasks([TASK])["tasks"][0]
        finally:
            os.environ.pop("FAKE_TARGET")
        self.assertEqual(target.read_text(), "SECRET = 1\n")
        self.assertNotIn("leak.py", task["files"])
        self.assertEqual(task["total"], 2, task)

    def test_a_folder_in_the_hidden_tests_place_scores_the_task_not_the_run(self):
        os.environ["FAKE_MODE"] = "dir"
        os.environ["FAKE_SOLUTION"] = "double.py=def double(n):\n    return n * 2\n"
        run = self.run_tasks([TASK, dict(TASK, name="double2")])
        self.assertEqual(run["state"], "done", run)
        self.assertEqual([t["passed"] for t in run["tasks"]], [2, 2], run)

    def test_the_relay_passes_on_v1_only_and_streams(self):
        seen = []

        class Upstream(runner.BaseHTTPRequestHandler):
            def do_POST(self):
                seen.append((self.path, self.headers.get("authorization"), self.rfile.read(int(self.headers["content-length"]))))
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.end_headers()
                self.wfile.write(b"data: one\n\ndata: two\n\n")

            def log_message(self, *args):
                pass

        upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        Thread(target=upstream.serve_forever, daemon=True).start()
        relay_server = ThreadingHTTPServer(("127.0.0.1", 0), runner.Relay)
        Thread(target=relay_server.serve_forever, daemon=True).start()
        proxy_url = runner.PROXY_URL
        runner.PROXY_URL = f"http://127.0.0.1:{upstream.server_port}"
        base = f"http://127.0.0.1:{relay_server.server_port}"
        try:
            req = urllib.request.Request(
                base + "/v1/chat/completions", data=b'{"x": 1}', method="POST",
                headers={"authorization": "Bearer shp_k", "content-type": "application/json"},
            )
            with urllib.request.urlopen(req) as resp:
                self.assertEqual(resp.read(), b"data: one\n\ndata: two\n\n")
            self.assertEqual(seen, [("/v1/chat/completions", "Bearer shp_k", b'{"x": 1}')])
            for path in ["/admin/api/login", "/admin", "/v1"]:
                with self.assertRaises(urllib.error.HTTPError) as e:
                    urllib.request.urlopen(urllib.request.Request(base + path, data=b"", method="POST"))
                self.assertEqual(e.exception.code, 403, path)
            self.assertEqual(len(seen), 1)
        finally:
            runner.PROXY_URL = proxy_url
            upstream.shutdown()
            relay_server.shutdown()

    @unittest.skipUnless(os.geteuid() == 0, "needs root to switch users")
    def test_opencode_and_the_tests_run_as_the_agent(self):
        import pwd
        nobody = pwd.getpwnam("nobody")
        seen = {}
        original = runner.score

        def keep(folder, hidden):
            seen.update(json.loads(Path(folder, "fake-args.json").read_text()))
            return original(folder, hidden)

        os.environ["FAKE_SOLUTION"] = "double.py=import os\ndef double(n):\n    assert os.getuid() != 0\n    return n * 2\n"
        runner.AGENT_USER, runner.score = "nobody", keep
        try:
            run = self.run_tasks([TASK])
        finally:
            runner.AGENT_USER, runner.score = None, original
        self.assertEqual(seen["uid"], nobody.pw_uid)
        self.assertEqual(run["tasks"][0]["passed"], 2, run)

    def test_a_task_is_scored_by_its_hidden_tests(self):
        os.environ["FAKE_SOLUTION"] = "double.py=def double(n):\n    return n * 2\n"
        run = self.run_tasks([TASK])
        self.assertEqual(run["state"], "done", run)
        task = run["tasks"][0]
        self.assertEqual((task["passed"], task["total"]), (2, 2), task)
        self.assertEqual(task["steps"], 2)
        self.assertEqual(task["input_tokens"], 1200)
        self.assertEqual(task["output_tokens"], 200)
        # 200 tokens over 2 s + 1 s of steps.
        self.assertEqual(task["output_tps"], 66.7)
        self.assertEqual(task["files"], {"double.py": "def double(n):\n    return n * 2\n"})
        self.assertIn("test_two", task["test_output"])
        self.assertNotIn(runner.SCORE_MARK, task["test_output"])
        self.assertIsNone(task["error"])
        self.assertIsNone(self.call("GET", "/runs/current")[1].get("api_key"))

    def test_opencode_gets_the_proxy_the_model_and_no_webfetch(self):
        seen = {}
        original = runner.score

        def keep(folder, hidden):
            seen.update(json.loads(Path(folder, "fake-args.json").read_text()))
            return original(folder, hidden)

        runner.score = keep
        try:
            self.run_tasks([TASK])
        finally:
            runner.score = original
        self.assertIn("--pure", seen["args"])
        # A title of its own: otherwise opencode asks the model for one.
        self.assertIn("--title", seen["args"])
        self.assertEqual(seen["args"][seen["args"].index("--model") + 1], "strata/IQ3_XXS")
        provider = seen["config"]["provider"]["strata"]["options"]
        # Through the runner's relay, which passes on /v1 only.
        self.assertEqual(provider["baseURL"], f"http://{runner.RELAY_ADDR}/v1")
        self.assertEqual(provider["apiKey"], "shp_k")
        self.assertEqual(seen["config"]["permission"]["webfetch"], "deny")

    def test_code_that_does_not_import_scores_nothing(self):
        run = self.run_tasks([TASK])
        task = run["tasks"][0]
        self.assertEqual((task["passed"], task["total"]), (0, 2), task)
        self.assertIn("double", task["test_output"])

    def test_a_planted_unittest_module_changes_no_score(self):
        os.environ["FAKE_SOLUTION"] = "unittest.py=raise SystemExit(0)\n"
        run = self.run_tasks([TASK])
        self.assertEqual(run["tasks"][0]["passed"], 0, run)

    def test_skipped_tests_do_not_pass(self):
        os.environ["FAKE_SOLUTION"] = (
            "double.py=import unittest\ndef double(n):\n    raise unittest.SkipTest('no')\n"
        )
        run = self.run_tasks([TASK])
        self.assertEqual(run["tasks"][0]["passed"], 0, run)

    def test_a_score_printed_by_the_tested_code_does_not_count(self):
        os.environ["FAKE_SOLUTION"] = (
            "double.py=import sys\n"
            f"print({runner.SCORE_MARK!r} + '{{\"passed\": 99, \"total\": 99}}', file=sys.stderr)\n"
        )
        task = self.run_tasks([TASK])["tasks"][0]
        self.assertEqual((task["passed"], task["total"]), (0, 2), task)

    def test_what_the_agent_left_running_ends_with_opencode(self):
        os.environ["FAKE_MODE"] = "background"
        os.environ["FAKE_SOLUTION"] = "double.py=def double(n):\n    return n * 2\n"
        started = time.monotonic()
        run = self.run_tasks([TASK])
        self.assertLess(time.monotonic() - started, 5)
        task = run["tasks"][0]
        self.assertEqual((task["passed"], task["timed_out"]), (2, False), task)

    def test_the_time_limit_stops_opencode_and_still_scores(self):
        os.environ["FAKE_MODE"] = "hang"
        run = self.run_tasks([TASK], timeout=1)
        task = run["tasks"][0]
        self.assertTrue(task["timed_out"], task)
        self.assertEqual(task["state"], "done")
        self.assertEqual(task["total"], 2)
        self.assertIn("stopped after", task["error"])

    def test_a_failing_opencode_keeps_its_message(self):
        os.environ["FAKE_MODE"] = "fail"
        run = self.run_tasks([TASK])
        self.assertIn("code 3: boom", run["tasks"][0]["error"])

    def test_an_error_event_names_what_went_wrong(self):
        os.environ["FAKE_MODE"] = "error"
        run = self.run_tasks([TASK])
        self.assertEqual(run["tasks"][0]["error"], "Model not found")

    def test_cancel_ends_the_run(self):
        os.environ["FAKE_MODE"] = "hang"
        code, _ = self.call(
            "POST", "/runs", {"model": "m", "api_key": "k", "tasks": [TASK, dict(TASK, name="two")]}
        )
        self.assertEqual(code, 202)
        self.assertEqual(self.call("POST", "/runs", {"model": "m", "api_key": "k", "tasks": [TASK]})[0], 409)
        time.sleep(0.3)
        self.assertEqual(self.call("POST", "/runs/current/cancel", {})[0], 202)
        run = self.wait()
        self.assertEqual(run["state"], "cancelled")
        self.assertEqual([t["state"] for t in run["tasks"]], ["cancelled", "pending"])

    def test_a_bad_run_is_refused(self):
        self.assertEqual(self.call("POST", "/runs", {"model": "m"})[0], 400)
        self.assertEqual(
            self.call("POST", "/runs", {"model": "a/b", "api_key": "k", "tasks": [TASK]})[0], 400
        )

    def test_the_real_tasks_pass_with_reference_solutions(self):
        """The hidden tests of the shipped tasks agree with their TASK.md."""
        for name, solution in REFERENCE.items():
            folder = tempfile.mkdtemp()
            target = {"roman": "roman.py", "csv-summary": "summary.py", "lru-cache": "lru.py"}[name]
            runner.write(folder, target, solution)
            for test in ("example_test.py",):
                runner.write(folder, test, (HERE / "tasks" / name / test).read_text())
            hidden = (HERE / "tasks" / name / "hidden_test.py").read_text()
            passed, total, output = runner.score(folder, hidden)
            self.assertEqual(passed, total, f"{name}: {output}")
            self.assertEqual(total, runner.total_tests(hidden), name)


REFERENCE = {
    name: (HERE / "testdata" / f"{name}.py").read_text()
    for name in ("roman", "csv-summary", "lru-cache")
}

if __name__ == "__main__":
    unittest.main()
