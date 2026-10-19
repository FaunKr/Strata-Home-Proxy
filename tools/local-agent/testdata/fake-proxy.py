#!/usr/bin/env python3
"""Fake Proxy for the local-agent tests.

Usage: fake-proxy.py <scenario.json> <port-file>

Answers GET /health from the scenario file, which it re-reads on every
request, so a test (or the fake claude) can change it at any time:

    {"health": [{"status": 504, "tag": "wake_timeout"}, {"status": 200, "max_context": 262144}]}

Each request takes the first entry and drops it from the file while more than
one is left, so the last entry answers every later request. Every request is
appended to <scenario.json>.requests as "<path> <x-api-key>".
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

scenario_path, port_file = sys.argv[1], sys.argv[2]


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        with open(scenario_path + ".requests", "a") as f:
            f.write(f"{self.path} {self.headers.get('x-api-key', '')}\n")
        with open(scenario_path) as f:
            scenario = json.load(f)
        answers = scenario.get("health") or [{"status": 200, "max_context": 262144}]
        answer = answers[0]
        if len(answers) > 1:
            scenario["health"] = answers[1:]
            with open(scenario_path, "w") as f:
                json.dump(scenario, f)
        status = answer.get("status", 200)
        if status == 200:
            body = {"status": "ok", "max_context": answer.get("max_context", 262144), "service": "strata"}
        else:
            body = {"type": "error", "error": {"type": "api_error", "message": answer.get("tag", "")}}
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        if answer.get("tag"):
            self.send_header("x-strata-proxy-status", answer["tag"])
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
