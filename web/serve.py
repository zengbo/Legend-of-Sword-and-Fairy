#!/usr/bin/env python3
"""Dev / self-host server for the web build.

SharedArrayBuffer (which the engine worker needs for input + sleeping)
requires cross-origin isolation, so this wraps http.server with the COOP/COEP
headers. Serves the repository root so both /web/ and /pal/ are reachable.

Also exposes a small REST API for accounts + cloud save slots (DOS .rpg):

  Auth
    POST   /api/auth/register     {username, password} → session
    POST   /api/auth/login        {username, password} → session
    POST   /api/auth/logout       Bearer token
    GET    /api/auth/me           Bearer token → {username}

  Saves (requires Authorization: Bearer <token>)
    GET    /api/saves             list slots for the logged-in user
    GET    /api/saves/<slot>      download one slot (raw bytes)
    PUT    /api/saves/<slot>      upload one slot (raw bytes)
    DELETE /api/saves/<slot>      delete one slot

On-disk layout under web/saves/:

  _auth/users/<username>.json     password hash + salt
  _auth/sessions.json             token → {username, expires}
  <username>/<slot>.rpg           DOS save blobs

Usage: python3 web/serve.py [port]   (default 8080; open /web/)
"""
from __future__ import annotations

import functools
import hashlib
import http.server
import json
import os
import re
import secrets
import sys
import threading
import time
import urllib.parse
from pathlib import Path


# Slots match the classic PAL menu (1–5).
SLOT_RE = re.compile(r"^[1-5]$")
USER_RE = re.compile(r"^[A-Za-z0-9_]{3,32}$")
TOKEN_RE = re.compile(r"^[A-Za-z0-9_-]{20,128}$")
MAX_SAVE_BYTES = 256 * 1024
MAX_PASSWORD_BYTES = 256
MIN_PASSWORD_LEN = 6
SESSION_DAYS = 30
PBKDF2_ITERS = 200_000


class AuthStore:
    """File-backed users + sessions. One process, locked for ThreadingHTTPServer."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.users_dir = root / "_auth" / "users"
        self.sessions_path = root / "_auth" / "sessions.json"
        self.users_dir.mkdir(parents=True, exist_ok=True)
        self.sessions_path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()
        if not self.sessions_path.is_file():
            self._write_json(self.sessions_path, {})

    # --- password helpers -------------------------------------------------

    @staticmethod
    def _hash_password(password: str, salt: bytes | None = None) -> tuple[str, str]:
        if salt is None:
            salt = secrets.token_bytes(16)
        dk = hashlib.pbkdf2_hmac(
            "sha256", password.encode("utf-8"), salt, PBKDF2_ITERS
        )
        return salt.hex(), dk.hex()

    @staticmethod
    def _verify_password(password: str, salt_hex: str, hash_hex: str) -> bool:
        try:
            salt = bytes.fromhex(salt_hex)
            expected = bytes.fromhex(hash_hex)
        except ValueError:
            return False
        dk = hashlib.pbkdf2_hmac(
            "sha256", password.encode("utf-8"), salt, PBKDF2_ITERS
        )
        return secrets.compare_digest(dk, expected)

    # --- json io ----------------------------------------------------------

    @staticmethod
    def _read_json(path: Path, default):
        try:
            with path.open("r", encoding="utf-8") as f:
                return json.load(f)
        except (OSError, json.JSONDecodeError):
            return default

    @staticmethod
    def _write_json(path: Path, obj) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(path.suffix + ".tmp")
        with tmp.open("w", encoding="utf-8") as f:
            json.dump(obj, f, ensure_ascii=False, indent=2)
            f.write("\n")
        os.replace(tmp, path)

    def _user_path(self, username: str) -> Path:
        return self.users_dir / f"{username}.json"

    # --- public API -------------------------------------------------------

    def register(self, username: str, password: str) -> tuple[dict | None, str | None]:
        """Returns (session_payload, error)."""
        if not USER_RE.match(username):
            return None, "username must be 3–32 chars [A-Za-z0-9_]"
        if len(password) < MIN_PASSWORD_LEN:
            return None, f"password must be at least {MIN_PASSWORD_LEN} characters"
        if len(password.encode("utf-8")) > MAX_PASSWORD_BYTES:
            return None, "password too long"

        with self._lock:
            path = self._user_path(username)
            if path.is_file():
                return None, "username already taken"
            salt_hex, hash_hex = self._hash_password(password)
            self._write_json(
                path,
                {
                    "username": username,
                    "salt": salt_hex,
                    "hash": hash_hex,
                    "created": int(time.time()),
                },
            )
            token = self._create_session_unlocked(username)
            return {"token": token, "username": username}, None

    def login(self, username: str, password: str) -> tuple[dict | None, str | None]:
        if not USER_RE.match(username or ""):
            return None, "invalid credentials"
        with self._lock:
            path = self._user_path(username)
            data = self._read_json(path, None)
            if not data:
                # Constant-ish work to reduce user-enumeration timing signal.
                self._hash_password(password or "x")
                return None, "invalid credentials"
            if not self._verify_password(password, data["salt"], data["hash"]):
                return None, "invalid credentials"
            token = self._create_session_unlocked(username)
            return {"token": token, "username": username}, None

    def logout(self, token: str | None) -> None:
        if not token or not TOKEN_RE.match(token):
            return
        with self._lock:
            sessions = self._read_json(self.sessions_path, {})
            if token in sessions:
                del sessions[token]
                self._write_json(self.sessions_path, sessions)

    def resolve(self, token: str | None) -> str | None:
        """Return username for a valid token, or None."""
        if not token or not TOKEN_RE.match(token):
            return None
        now = int(time.time())
        with self._lock:
            sessions = self._read_json(self.sessions_path, {})
            entry = sessions.get(token)
            if not entry:
                return None
            if int(entry.get("expires", 0)) < now:
                del sessions[token]
                self._write_json(self.sessions_path, sessions)
                return None
            return entry.get("username")

    def _create_session_unlocked(self, username: str) -> str:
        sessions = self._read_json(self.sessions_path, {})
        # Drop expired entries opportunistically.
        now = int(time.time())
        expired = [t for t, e in sessions.items() if int(e.get("expires", 0)) < now]
        for t in expired:
            del sessions[t]
        token = secrets.token_urlsafe(32)
        sessions[token] = {
            "username": username,
            "expires": now + SESSION_DAYS * 86400,
            "created": now,
        }
        self._write_json(self.sessions_path, sessions)
        return token


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".js": "text/javascript",
    }

    # Set by main() after computing the repo root.
    saves_root: Path = Path("web/saves")
    auth: AuthStore | None = None

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_GET(self):
        if self.path.startswith("/api/"):
            self._handle_api("GET")
            return
        super().do_GET()

    def do_POST(self):
        if self.path.startswith("/api/"):
            self._handle_api("POST")
            return
        self.send_error(405, "Method Not Allowed")

    def do_PUT(self):
        if self.path.startswith("/api/"):
            self._handle_api("PUT")
            return
        self.send_error(405, "Method Not Allowed")

    def do_DELETE(self):
        if self.path.startswith("/api/"):
            self._handle_api("DELETE")
            return
        self.send_error(405, "Method Not Allowed")

    def do_OPTIONS(self):
        if self.path.startswith("/api/"):
            self.send_response(204)
            self.send_header("Allow", "GET, POST, PUT, DELETE, OPTIONS")
            self.end_headers()
            return
        self.send_error(405, "Method Not Allowed")

    # --- routing ----------------------------------------------------------

    def _handle_api(self, method: str) -> None:
        parsed = urllib.parse.urlparse(self.path)
        parts = [p for p in parsed.path.split("/") if p]
        if len(parts) < 2 or parts[0] != "api":
            self._json(404, {"error": "not found"})
            return

        resource = parts[1]
        if resource == "auth":
            self._handle_auth(method, parts[2:])
            return
        if resource == "saves":
            self._handle_saves(method, parts[2:])
            return
        self._json(404, {"error": "not found"})

    # --- auth endpoints ---------------------------------------------------

    def _handle_auth(self, method: str, rest: list[str]) -> None:
        if not rest:
            self._json(404, {"error": "not found"})
            return
        action = rest[0]

        if action == "register" and method == "POST":
            body = self._read_json_body()
            if body is None:
                return
            payload, err = self.auth.register(
                str(body.get("username", "")).strip(),
                str(body.get("password", "")),
            )
            if err:
                code = 409 if "taken" in err else 400
                self._json(code, {"error": err})
                return
            self._json(201, payload)
            return

        if action == "login" and method == "POST":
            body = self._read_json_body()
            if body is None:
                return
            payload, err = self.auth.login(
                str(body.get("username", "")).strip(),
                str(body.get("password", "")),
            )
            if err:
                self._json(401, {"error": err})
                return
            self._json(200, payload)
            return

        if action == "logout" and method == "POST":
            self.auth.logout(self._bearer_token())
            self._json(200, {"ok": True})
            return

        if action == "me" and method == "GET":
            user = self.auth.resolve(self._bearer_token())
            if not user:
                self._json(401, {"error": "not logged in"})
                return
            self._json(200, {"username": user})
            return

        self._json(404, {"error": "not found"})

    # --- save endpoints (auth required) -----------------------------------

    def _handle_saves(self, method: str, rest: list[str]) -> None:
        user = self.auth.resolve(self._bearer_token())
        if not user:
            self._json(401, {"error": "login required"})
            return

        if not rest:
            if method != "GET":
                self._json(405, {"error": "list is GET only"})
                return
            self._list_saves(user)
            return

        if len(rest) == 1:
            slot = rest[0]
            if not SLOT_RE.match(slot):
                self._json(400, {"error": "slot must be 1-5"})
                return
            if method == "GET":
                self._get_save(user, slot)
            elif method == "PUT":
                self._put_save(user, slot)
            elif method == "DELETE":
                self._delete_save(user, slot)
            else:
                self._json(405, {"error": "method not allowed"})
            return

        self._json(404, {"error": "not found"})

    def _user_dir(self, username: str) -> Path:
        # username already validated by USER_RE at registration.
        d = self.saves_root / username
        d.mkdir(parents=True, exist_ok=True)
        return d

    def _slot_path(self, username: str, slot: str) -> Path:
        return self._user_dir(username) / f"{slot}.rpg"

    def _list_saves(self, username: str) -> None:
        d = self._user_dir(username)
        slots = {}
        for slot in ("1", "2", "3", "4", "5"):
            p = d / f"{slot}.rpg"
            if p.is_file():
                st = p.stat()
                slots[slot] = {
                    "size": st.st_size,
                    "mtime": int(st.st_mtime),
                }
        self._json(200, {"username": username, "slots": slots})

    def _get_save(self, username: str, slot: str) -> None:
        p = self._slot_path(username, slot)
        if not p.is_file():
            self._json(404, {"error": "slot empty"})
            return
        data = p.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Pal-Save-Slot", slot)
        self.end_headers()
        self.wfile.write(data)

    def _put_save(self, username: str, slot: str) -> None:
        length = self.headers.get("Content-Length")
        if length is None:
            self._json(411, {"error": "Content-Length required"})
            return
        try:
            n = int(length)
        except ValueError:
            self._json(400, {"error": "bad Content-Length"})
            return
        if n < 0 or n > MAX_SAVE_BYTES:
            self._json(413, {"error": f"save too large (max {MAX_SAVE_BYTES})"})
            return
        if n > 0 and n < 128:
            self._json(400, {"error": "save too short to be a valid .rpg"})
            return
        body = self.rfile.read(n)
        if len(body) != n:
            self._json(400, {"error": "truncated body"})
            return
        path = self._slot_path(username, slot)
        tmp = path.with_suffix(".rpg.tmp")
        tmp.write_bytes(body)
        os.replace(tmp, path)
        self._json(200, {"ok": True, "slot": int(slot), "size": n})

    def _delete_save(self, username: str, slot: str) -> None:
        p = self._slot_path(username, slot)
        if p.is_file():
            p.unlink()
        self._json(200, {"ok": True, "slot": int(slot)})

    # --- request helpers --------------------------------------------------

    def _bearer_token(self) -> str | None:
        auth = self.headers.get("Authorization") or ""
        if auth.lower().startswith("bearer "):
            return auth[7:].strip()
        # Also accept X-Pal-Token for simple clients.
        return self.headers.get("X-Pal-Token")

    def _read_json_body(self) -> dict | None:
        length = self.headers.get("Content-Length")
        if length is None:
            self._json(411, {"error": "Content-Length required"})
            return None
        try:
            n = int(length)
        except ValueError:
            self._json(400, {"error": "bad Content-Length"})
            return None
        if n < 0 or n > 16 * 1024:
            self._json(413, {"error": "body too large"})
            return None
        raw = self.rfile.read(n)
        try:
            obj = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._json(400, {"error": "invalid JSON"})
            return None
        if not isinstance(obj, dict):
            self._json(400, {"error": "JSON object required"})
            return None
        return obj

    def _json(self, code: int, obj: dict) -> None:
        raw = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def log_message(self, fmt: str, *args) -> None:
        if self.path.startswith("/api/"):
            sys.stderr.write("[api] ")
        super().log_message(fmt, *args)


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8080
    root = Path(__file__).resolve().parent.parent
    saves = root / "web" / "saves"
    saves.mkdir(parents=True, exist_ok=True)
    Handler.saves_root = saves
    Handler.auth = AuthStore(saves)

    handler = functools.partial(Handler, directory=str(root))
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as httpd:
        print(f"serving {root} at http://127.0.0.1:{port}/web/")
        print(f"auth+saves →  http://127.0.0.1:{port}/api/  (store: {saves})")
        print("  POST /api/auth/register|login  ·  Bearer token on /api/saves")
        httpd.serve_forever()


if __name__ == "__main__":
    main()
