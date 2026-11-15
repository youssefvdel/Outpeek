#!/usr/bin/env python3
"""Fake Riot Client loopback API, for running the native app off-Windows.

The app talks to the Riot Client over HTTPS on 127.0.0.1, using credentials it
reads from `%LOCALAPPDATA%\\Riot Games\\Riot Client\\Config\\lockfile`. This
process supplies exactly that: it listens on loopback, writes the lockfile
itself, and answers the four endpoints the app actually reads.

Mirrored from (read these before changing a field):

* `src-tauri/src/tracker.rs::lockfile_auth` + `outpeek-app/src/identity.rs` —
  lockfile shape `name:pid:port:password:protocol`, `Authorization: Basic
  riot:<password>`, certificate validation deliberately off.
* `outpeek-app/src/identity.rs` — `/entitlements/v1/token`,
  `/player-account/aliases/v1/active`, `/chat/v4/presences`.
* `src-tauri/src/live_facts.rs::derive_facts` — the decoded `private` blob:
  `matchPresenceData.sessionLoopState` (`MENU|PREGAME|INGAME`), `matchMap`,
  `queueId`, and the flat `partyOwnerMatchScoreAllyTeam` /
  `partyOwnerMatchScoreEnemyTeam`.

What is deliberately missing: every remote Riot host (`pd.<shard>.a.pvp.net`),
so `/mmr` still fails from this harness — see README.md.

Python 3 stdlib only. Binds 127.0.0.1 and nothing else. Logs request paths and
status codes only, never bodies and never the lockfile password.

Usage:
    python3 scripts/fake-riot/server.py --state pregame \
        --lockfile-dir /tmp/x/Local/"Riot Games"/"Riot Client"/Config

    # switch a running server
    curl -sk "https://127.0.0.1:<port>/__fake/state?name=coregame"
"""

from __future__ import annotations

import argparse
import base64
import hmac
import json
import os
import shutil
import signal
import ssl
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

# --- fixture facts -----------------------------------------------------------

#: Account the fake client is "signed in" as. Same values the app's own tests
#: and docs use, so a screenshot is recognisable.
GAME_NAME = "lil ga7ed"
TAG_LINE = "zngr"
PUUID = "6f0a1d2e-3b4c-4d5e-8f90-a1b2c3d4e5f6"
PARTY_FRIEND_PUUID = "0c9b8a76-5432-4def-8abc-0123456789ab"

#: Lockfile name field: the real client writes "Riot Client".
LOCKFILE_NAME = "Riot Client"
LOCKFILE_FILE = "lockfile"

#: Protocol string the app never uses beyond the shape; real clients write https.
LOCKFILE_PROTOCOL = "https"

DEFAULT_STATE = "idle"
STATES_DIR = Path(__file__).resolve().parent / "states"

#: Opaque stand-in for the entitlement token. Not a credential: it authorises
#: nothing, because no remote host in this harness accepts it.
ENTITLEMENT_TOKEN = "fake-riot-entitlements-token-not-a-credential"


def access_token() -> str:
    """A JWT-shaped token whose payload carries the shard claim.

    `outpeek-app/src/rank.rs::region_from_access_token` base64-decodes the
    payload and reads `/pp/c`; `eu` keeps the (unreachable) shard host stable.
    The signature is fake and never verified by anything.
    """
    header = {"alg": "none", "typ": "JWT"}
    payload = {"pp": {"c": "eu"}, "sub": PUUID, "exp": 4102444800}
    return ".".join(
        base64.urlsafe_b64encode(json.dumps(part, separators=(",", ":")).encode())
        .decode()
        .rstrip("=")
        for part in (header, payload)
    ) + ".fake-signature"


# --- state files -------------------------------------------------------------


class States:
    """The `states/*.json` fixtures, read on demand (editing one is picked up)."""

    def __init__(self, directory: Path, initial: str) -> None:
        self.directory = directory
        self._lock = threading.Lock()
        self.name = ""
        self.set(initial)

    def available(self) -> list[str]:
        return sorted(p.stem for p in self.directory.glob("*.json"))

    def set(self, name: str) -> None:
        if name not in self.available():
            raise SystemExit(
                f"fake-riot: unknown state {name!r}; have {', '.join(self.available())}"
            )
        with self._lock:
            self.name = name

    def get(self) -> str:
        with self._lock:
            return self.name

    def document(self) -> dict:
        """The `/chat/v4/presences` body with every `private` object encoded.

        Files keep `private` as readable JSON so a human can edit a fixture;
        the wire needs base64 of the UTF-8 JSON, which is what the app decodes.
        An already-string `private` is passed through untouched.
        """
        path = self.directory / f"{self.get()}.json"
        doc = json.loads(path.read_text(encoding="utf-8"))
        for presence in doc.get("presences", []):
            private = presence.get("private")
            if isinstance(private, dict):
                raw = json.dumps(private, separators=(",", ":")).encode("utf-8")
                presence["private"] = base64.b64encode(raw).decode("ascii")
        return doc


# --- certificate -------------------------------------------------------------


def ensure_certificate(cert: str | None, key: str | None) -> tuple[Path, Path, Path | None]:
    """Return (cert, key, temp_dir_to_remove).

    Without `--cert/--key`, generate a throwaway self-signed pair with the
    system `openssl` into a private temp directory. The app accepts invalid
    certificates by design (the real Riot Client's cert is not issued for
    127.0.0.1 either), so no trust store is touched.
    """
    if cert and key:
        cert_path, key_path = Path(cert).resolve(), Path(key).resolve()
        for path in (cert_path, key_path):
            if not path.is_file():
                raise SystemExit(f"fake-riot: no such file: {path}")
        return cert_path, key_path, None

    if (cert is None) != (key is None):
        raise SystemExit("fake-riot: pass both --cert and --key, or neither")

    openssl = shutil.which("openssl")
    if openssl is None:
        raise SystemExit(
            "fake-riot: openssl not found; install it or pass --cert/--key"
        )
    directory = Path(tempfile.mkdtemp(prefix="fake-riot-cert-"))
    cert_path = directory / "cert.pem"
    key_path = directory / "key.pem"
    command = [
        openssl,
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-sha256",
        "-nodes",
        "-days",
        "2",
        "-keyout",
        str(key_path),
        "-out",
        str(cert_path),
        "-subj",
        "/CN=127.0.0.1",
        "-addext",
        "subjectAltName=IP:127.0.0.1",
    ]
    try:
        subprocess.run(command, check=True, capture_output=True, timeout=60)
    except subprocess.CalledProcessError as error:
        shutil.rmtree(directory, ignore_errors=True)
        detail = error.stderr.decode("utf-8", "replace").strip().splitlines()
        raise SystemExit(
            "fake-riot: openssl could not make a certificate"
            + (f": {detail[-1]}" if detail else "")
        ) from error
    return cert_path, key_path, directory


# --- HTTP handler ------------------------------------------------------------


class RiotHandler(BaseHTTPRequestHandler):
    """Loopback GETs the app makes, plus two `/__fake/` control routes."""

    protocol_version = "HTTP/1.1"
    server_version = "fake-riot/1"
    sys_version = ""

    # Injected by `serve()`.
    states: States
    password: str

    # -- routing --

    def do_GET(self) -> None:  # noqa: N802 (http.server API)
        parts = urlsplit(self.path)
        path = parts.path
        query = parse_qs(parts.query)

        # Control routes are unauthenticated: they are this harness's own
        # surface, on loopback only, and never answer with client facts.
        if path == "/__fake/health":
            self._send(200, {"ok": True, "state": self.states.get()})
            return
        if path == "/__fake/states":
            self._send(
                200, {"state": self.states.get(), "states": self.states.available()}
            )
            return
        if path == "/__fake/state":
            name = (query.get("name") or [""])[0].strip()
            try:
                self.states.set(name)
            except SystemExit as error:
                self._send(
                    400,
                    {
                        "httpStatus": 400,
                        "message": str(error),
                        "states": self.states.available(),
                    },
                )
                return
            log(f"state -> {name}")
            self._send(200, {"state": name})
            return

        if not self._authorized():
            self._send(
                401,
                {"httpStatus": 401, "errorCode": "BAD_AUTH", "message": "Unauthorized"},
            )
            return

        if path == "/entitlements/v1/token":
            self._send(
                200,
                {
                    "accessToken": access_token(),
                    "token": ENTITLEMENT_TOKEN,
                    "subject": PUUID,
                    "issuer": "https://fake-riot.invalid",
                },
            )
            return
        if path == "/player-account/aliases/v1/active":
            # `puuid` is optional for the app (it falls back to the entitlements
            # subject), but the real endpoint carries it, so it is here too.
            self._send(
                200,
                {
                    "game_name": GAME_NAME,
                    "tag_line": TAG_LINE,
                    "puuid": PUUID,
                    "active": True,
                },
            )
            return
        if path == "/chat/v4/presences":
            self._send(200, self.states.document())
            return

        # Everything else: Riot answers for a signed-in client, but these are
        # endpoints this app does not need. 404 proves the app degrades.
        self._send(
            404,
            {
                "httpStatus": 404,
                "errorCode": "RESOURCE_NOT_FOUND",
                "message": f"{path} is not faked by this harness",
            },
        )

    def do_POST(self) -> None:  # noqa: N802
        self._send(404, {"httpStatus": 404, "errorCode": "RESOURCE_NOT_FOUND"})

    do_PUT = do_POST
    do_DELETE = do_POST
    do_PATCH = do_POST

    # -- plumbing --

    def _authorized(self) -> bool:
        """`Authorization: Basic riot:<lockfile password>` — exactly what the
        app sends (`identity.rs::local_get`). Compared in constant time."""
        header = self.headers.get("Authorization", "")
        scheme, _, encoded = header.partition(" ")
        if scheme.lower() != "basic" or not encoded:
            return False
        try:
            decoded = base64.b64decode(encoded.strip(), validate=True).decode("utf-8")
        except (ValueError, UnicodeDecodeError):
            return False
        user, sep, password = decoded.partition(":")
        if sep != ":" or user != "riot":
            return False
        return hmac.compare_digest(password, self.password)

    def _send(self, status: int, payload: dict) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True
        # Path and status only: the app's bodies are its account data.
        log(f"{self.command} {urlsplit(self.path).path} -> {status}")

    def log_message(self, format: str, *args) -> None:  # noqa: A002
        """Silence http.server's own logging; `_send` is the one log line."""

    def log_error(self, format: str, *args) -> None:  # noqa: A002
        log(f"error: {format % args}")


def log(message: str) -> None:
    print(f"[fake-riot] {message}", flush=True)


# --- lifecycle ---------------------------------------------------------------


def write_lockfile(directory: Path, port: int, password: str) -> Path:
    """`name:pid:port:password:protocol`, the exact shape the app parses."""
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / LOCKFILE_FILE
    text = f"{LOCKFILE_NAME}:{os.getpid()}:{port}:{password}:{LOCKFILE_PROTOCOL}"
    path.write_text(text, encoding="utf-8")
    try:
        path.chmod(0o600)
    except OSError:
        # Not fatal on a filesystem without POSIX modes (a Windows checkout).
        pass
    return path


def make_password() -> str:
    """Random, colon-free (the lockfile is colon-delimited), never logged."""
    return base64.urlsafe_b64encode(os.urandom(18)).decode("ascii").rstrip("=")


def serve(args: argparse.Namespace) -> int:
    states = States(Path(args.states_dir).resolve(), args.state)
    cert_path, key_path, generated_dir = ensure_certificate(args.cert, args.key)
    password = make_password()

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certfile=str(cert_path), keyfile=str(key_path))

    handler = type("BoundRiotHandler", (RiotHandler,), {
        "states": states,
        "password": password,
    })
    # 127.0.0.1 literal: never 0.0.0.0, never a LAN address.
    httpd = ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    httpd.daemon_threads = True
    port = httpd.server_address[1]
    httpd.socket = context.wrap_socket(httpd.socket, server_side=True)

    lockfile = write_lockfile(Path(args.lockfile_dir).resolve(), port, password)

    stopping = threading.Event()

    def on_signal(signum, _frame) -> None:
        if stopping.is_set():
            return
        stopping.set()
        log(f"{signal.Signals(signum).name}: shutting down")
        # shutdown() waits for serve_forever's loop, so it cannot run here.
        threading.Thread(target=httpd.shutdown, daemon=True).start()

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, on_signal)

    log(f"state={states.get()} cert={cert_path}")
    log(f"listening https://127.0.0.1:{port}")
    log(f"lockfile {lockfile}")
    log(f'switch state: curl -sk "https://127.0.0.1:{port}/__fake/state?name=pregame"')
    try:
        httpd.serve_forever(poll_interval=0.2)
    finally:
        httpd.server_close()
        try:
            lockfile.unlink()
            log(f"removed {lockfile}")
        except FileNotFoundError:
            pass
        if generated_dir is not None:
            shutil.rmtree(generated_dir, ignore_errors=True)
        log("stopped")
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Fake Riot Client loopback API for the Outpeek app.",
        epilog="Binds 127.0.0.1 only; writes the lockfile the app reads.",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=0,
        help="loopback port (0 = pick a free one; default 0)",
    )
    parser.add_argument(
        "--state",
        default=DEFAULT_STATE,
        help=f"presence fixture in states/ (default {DEFAULT_STATE})",
    )
    parser.add_argument(
        "--states-dir",
        default=str(STATES_DIR),
        help="directory of presence fixtures (default scripts/fake-riot/states)",
    )
    parser.add_argument(
        "--lockfile-dir",
        required=True,
        help="directory that receives the `lockfile` (…\\Riot Client\\Config)",
    )
    parser.add_argument("--cert", help="PEM certificate (default: generate one)")
    parser.add_argument("--key", help="PEM private key (default: generate one)")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if not 0 <= args.port <= 65535:
        raise SystemExit("fake-riot: --port must be 0–65535")
    return serve(args)


if __name__ == "__main__":
    sys.exit(main())
