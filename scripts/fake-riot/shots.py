#!/usr/bin/env python3
"""Capture every tracker tab through Slint's embedded MCP server.

The app serves MCP over Streamable HTTP when started with `--mcp-port PORT`;
`--features ui-debug` is not required for screenshots (only for the element
tree). This harness launches the app once per tab — Rust owns the active tab, so
`--tab` is the only honest way to prove the underline and the page agree — and
writes one PNG per tab.

    python3 scripts/fake-riot/shots.py --out /tmp/opencode/shots
    python3 scripts/fake-riot/shots.py --out /tmp/opencode/shots --fake-live

`--fake-live` sets `OUTPEEK_FAKE_LIVE=1`, the debug-only board (you, a party, an
incognito player, an unresolved name, a hidden rank, an unrated player) so the
Live Match page can be inspected on a host without VALORANT. Requires a debug
build; the flag is compiled out of release binaries.

Needs an X server (Xvfb is fine: `Xvfb :80 -screen 0 1600x1000x24`), and the
app's data directory pointed somewhere harmless, e.g.

    APPDATA=/tmp/opencode/fakeroot/Roaming \
    LOCALAPPDATA=/tmp/opencode/fakeroot/Local \
    DISPLAY=:80 python3 scripts/fake-riot/shots.py --out /tmp/opencode/shots
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load_shot_module():
    spec = importlib.util.spec_from_file_location("mcp_shot", HERE / "mcp-shot.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def capture(shot, binary: Path, tab: str, out: Path, env: dict, timeout: float) -> dict:
    port = free_port()
    log = tempfile.TemporaryFile(mode="w+")
    proc = subprocess.Popen(
        [str(binary), "--tab", tab, "--mcp-port", str(port)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=log,
    )
    started = time.monotonic()
    mcp = None
    try:
        while time.monotonic() - started < timeout:
            try:
                mcp = shot.Mcp(port)
                window = shot.first_window(mcp)
                break
            except Exception:
                time.sleep(0.25)
        if mcp is None:
            log.seek(0)
            tail = "".join(log.readlines()[-4:]).strip()
            raise RuntimeError(f"{tab}: the MCP server never answered — {tail}")
        # Let the first poll land (TRN cache, art) before the capture.
        time.sleep(4)
        props = json.loads(
            mcp.tool("get_window_properties", {"windowHandle": window})["content"][0][
                "text"
            ]
        )
        png = shot.screenshot(mcp, window)
        target = out / f"{tab}.png"
        target.write_bytes(png)
        return {"tab": tab, "path": str(target), "size": props.get("size")}
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bin",
        default="outpeek-app/target/debug/outpeek-app",
        help="app binary (default: the debug build)",
    )
    parser.add_argument("--out", required=True, help="directory for the PNGs")
    parser.add_argument(
        "--tabs",
        default="overview,matches,agents,maps,live",
        help="comma-separated tabs to capture",
    )
    parser.add_argument(
        "--fake-live",
        action="store_true",
        help="render the debug-only Live Match board (OUTPEEK_FAKE_LIVE=1)",
    )
    parser.add_argument("--timeout", type=float, default=25.0)
    args = parser.parse_args(argv)

    if not os.environ.get("DISPLAY"):
        print(
            "DISPLAY is not set: start an X server first, e.g.\n"
            "  Xvfb :80 -screen 0 1600x1000x24 &\n"
            "  DISPLAY=:80 python3 scripts/fake-riot/shots.py --out /tmp/shots",
            file=sys.stderr,
        )
        return 2

    shot = load_shot_module()
    binary = Path(args.bin)
    if not binary.exists():
        print(f"no such binary: {binary}", file=sys.stderr)
        return 2
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    if args.fake_live:
        env["OUTPEEK_FAKE_LIVE"] = "1"

    failed = 0
    for tab in [t.strip() for t in args.tabs.split(",") if t.strip()]:
        try:
            result = capture(shot, binary, tab, out, env, args.timeout)
            print(f"{result['tab']}: {result['size']} -> {result['path']}")
        except Exception as error:  # noqa: BLE001 - a tab that cannot answer is the report
            failed += 1
            print(f"{tab}: FAILED {error}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
