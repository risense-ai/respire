"""Loopback sync proxy with response barriers for the outbox smoke test."""
import json
import socket
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


UPSTREAM = "https://api-dev.rsrs.rs"


def require(condition, code):
    if not condition:
        raise RuntimeError(code)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class Gate:
    def __init__(self):
        self.hold = False
        self.fail = False
        self.entered = threading.Event()
        self.release = threading.Event()
        self.acknowledged = set()
        self.fault_requests = 0
        self.capability_requests = 0
        self.sync_read_requests = 0
        self.proxy_response_parse_failures = 0
        self.request_counts = {"capability": 0, "pull": 0, "snapshot": 0, "push": 0}
        self.held_calls = {}
        self.lock = threading.Lock()
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        gate = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                self.forward()

            def do_POST(self):
                self.forward()

            def forward(self):
                try:
                    require(self.path.startswith("/") and not self.path.startswith("//"), "invalid_proxy_path")
                    path = self.path.split("?")[0]
                    kind = next((name for suffix, name in (("/sync/capabilities", "capability"),
                        ("/v2/pull", "pull"), ("/v2/snapshot", "snapshot"), ("/v2/push/batch", "push"))
                        if path.endswith(suffix)), None)
                    is_sync_read = self.command == "GET" and kind in ("capability", "pull", "snapshot")
                    hold_call = None
                    with gate.lock:
                        if kind:
                            gate.request_counts[kind] += 1
                        if kind == "capability":
                            gate.capability_requests += 1
                        if is_sync_read:
                            gate.sync_read_requests += 1
                            hold_call = gate.held_calls.get(gate.sync_read_requests)
                    if is_sync_read and gate.fail:
                        with gate.lock:
                            gate.fault_requests += 1
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 <= size <= 8 * 1024 * 1024, "proxy_request_too_large")
                    body = self.rfile.read(size) if size else None
                    headers = {name: self.headers[name] for name in ("Authorization", "Content-Type") if name in self.headers}
                    request = urllib.request.Request(UPSTREAM + self.path, body, headers, method=self.command)
                    try:
                        response = gate.opener.open(request, timeout=45)
                    except urllib.error.HTTPError as error:
                        response = error
                    with response:
                        status = response.code
                        raw = response.read(16 * 1024 * 1024 + 1)
                        require(len(raw) <= 16 * 1024 * 1024 and not 300 <= status < 400, "invalid_upstream_response")
                        content_type = response.headers.get("Content-Type", "application/json")
                    if self.command == "POST" and kind == "push" and 200 <= status < 300:
                        try:
                            sent = json.loads(body)
                            received = json.loads(raw)
                            ids = {item["op_id"]: item["blob"]["id"] for item in sent["items"]}
                            accepted = set()
                            for result in received["results"]:
                                if result["status"] in ("applied", "rebased", "duplicate") and (result.get("stored_rev") or result.get("head_rev") or 0) > 0:
                                    accepted.add(ids[result["op_id"]])
                            with gate.lock:
                                gate.acknowledged.update(accepted)
                        except Exception:
                            # Observation must not turn an already committed real response into a disconnect.
                            with gate.lock:
                                gate.proxy_response_parse_failures += 1
                    if is_sync_read and gate.hold:
                        gate.entered.set()
                        require(gate.release.wait(90), "sync_read_gate_timeout")
                    if hold_call is not None:
                        hold_call[0].set()
                        require(hold_call[1].wait(90), "numbered_sync_read_gate_timeout")
                    self.send_response(status)
                    self.send_header("Content-Type", content_type)
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                except (Exception, BrokenPipeError):
                    # Never print provider bodies, authorization headers or exception payloads.
                    self.close_connection = True

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def address(self):
        return "http://127.0.0.1:" + str(self.server.server_port)

    def close(self):
        self.release.set()
        for _, release in self.held_calls.values():
            release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(3)

    def hold_call(self, ordinal):
        events = (threading.Event(), threading.Event())
        with self.lock:
            require(ordinal > self.sync_read_requests, "sync_read_gate_armed_too_late")
            self.held_calls[ordinal] = events
        return events
