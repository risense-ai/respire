"""Loopback account service for the interruption and GUI recovery fixtures."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from legacy_vault_fixture import PREFIX, require


def account_fixture_handler(state, reached, release, v2=None):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, value, status=200):
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass  # Expected: the test terminates its own waiting CLI.

        def do_POST(self):
            if state.get("barrier_failed"):
                self.send_error(503)
                return
            data = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
            if self.path == "/register" and "receipt" in state:
                if state["registers"] != 0 or data.get("user") != "synthetic" or not data.get("pass_hash") or not data.get("salt"):
                    self.send_error(409)
                    return
                state["registers"] += 1
                state["auth_salt"] = data["salt"]
                Path(state["receipt"]).write_text(json.dumps({key: state[key] for key in ("posts", "registers", "logins")}), encoding="utf-8")
                self.reply({"user": "synthetic", "token": state["token"]})
            elif self.path == "/login":
                state["logins"] += 1
                if "receipt" in state:
                    Path(state["receipt"]).write_text(json.dumps({key: state[key] for key in ("posts", "registers", "logins")}), encoding="utf-8")
                    self.reply({"user": "synthetic", "token": state["token"], "session_id": "fixture"})
                    return
                self.reply({"user": "synthetic", "token": "synthetic-token", "session_id": "fixture"})
            elif self.path == "/api/self/vault":
                if self.headers.get("Authorization") != "Bearer " + state.get("token", "synthetic-token"):
                    self.send_error(401)
                    return
                state["vault"] = data
                state["posts"] += 1
                if "receipt" in state:
                    Path(state["receipt"]).write_text(json.dumps({key: state[key] for key in ("posts", "registers", "logins")}), encoding="utf-8")
                if state["barrier"] == "after_publish":
                    reached.set()
                    if not release.wait(120):
                        # Never let a timed-out barrier authorize further effects.
                        # Latch all fixture endpoints closed, including reconciliation.
                        state["barrier_failed"] = True
                        self.send_error(503)
                        return
                self.reply({"ok": True})
            elif self.path == "/api/self/password":
                require(self.headers.get("Authorization") == "Bearer synthetic-token", "password_update_not_authorized")
                salt_bytes = bytes.fromhex(data["salt"].removeprefix(PREFIX))
                require(data["pass_hash"] == hashlib.pbkdf2_hmac("sha256", b"synthetic-password", salt_bytes, 100000, 32).hex(),
                    "original_login_password_changed")
                state["auth_salt"] = data["salt"]
                state["password_posts"] += 1
                self.reply({"updated": True})
            elif self.path == "/push/batch":
                require(self.headers.get("Authorization") == "Bearer synthetic-token", "transfer_push_not_authorized")
                stage = state["transfer_stage"]
                rejected = stage == "initial_login"
                state["transfer_http"].append({"stage": stage, "method": "POST",
                    "route": "/push/batch", "status": 503 if rejected else 200})
                if rejected:
                    self.reply({"error": "synthetic transfer push outage"}, 503)
                    return
                for item in data["items"]:
                    state["sync_blobs"][item["id"]] = dict(item, user="synthetic")
                self.reply({"replaced": [True] * len(data["items"])})
            else:
                self.send_error(404)

        def do_GET(self):
            if state.get("barrier_failed"):
                self.send_error(503)
                return
            if self.path == "/pull" or self.path.startswith("/pull?"):
                state["sync_pulls"] += 1
                state["transfer_http"].append({"stage": state["transfer_stage"],
                    "method": "GET", "route": "/pull", "status": 200})
                blobs = list(state["sync_blobs"].values())
                self.reply({"blobs": blobs, "cursor": len(blobs), "total": len(blobs),
                            "alive": sum(not item["deleted"] for item in blobs)})
                return
            if self.path == "/auth/salt?user=synthetic":
                if state.get("auth_salt") is None:
                    self.send_error(404)
                    return
                self.reply({"salt": state["auth_salt"]})
                return
            if self.path != "/api/self/vault":
                self.send_error(404)
                return
            if self.headers.get("Authorization") != "Bearer " + state.get("token", "synthetic-token"):
                self.send_error(401)
                return
            state["gets"] += 1
            if state["vault"] is None:
                self.send_error(404)
                return
            # Runtime23 prepares migration twice around host credential custody;
            # only the third vault GET checks publication after candidate commit.
            if state["barrier"] == "before_publish" and state["gets"] == 3:
                reached.set()
                if not release.wait(120):
                    state["barrier_failed"] = True
                    self.send_error(503)
                    return
            self.reply(v2 if state.get("stale_vault_read") else state["vault"])

    return Handler


def gui_recovery_server():
    """Reuse the loopback account service for CLI fixture setup and real GUI reset.

    Only encrypted vault uploads are stored in memory. CLI/Runtime issue all
    recovery material; this server never supplies a super or operation receipt.
    """
    parser = argparse.ArgumentParser()
    parser.add_argument("--gui-recovery-server", action="store_true")
    parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise RuntimeError("hosted_account_fixture_required")
    state = {"vault": None, "posts": 0, "gets": 0, "registers": 0, "logins": 0,
             "barrier": None, "receipt": str(args.receipt), "token": secrets.token_urlsafe(32)}
    server = ThreadingHTTPServer(("127.0.0.1", 0), account_fixture_handler(state, None, None))
    args.receipt.write_text(json.dumps({key: state[key] for key in ("posts", "registers", "logins")}), encoding="utf-8")
    print(json.dumps({"addr": f"http://127.0.0.1:{server.server_port}"}), flush=True)
    server.serve_forever()
