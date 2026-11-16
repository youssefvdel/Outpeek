#!/usr/bin/env python3
"""Capture the running app's window through Slint's embedded MCP server.

The app serves MCP over Streamable HTTP when launched with `--mcp-port PORT`
(and `--features ui-debug` for the element tree; the screenshot works either
way). This is the proof step of the fake-Riot harness: start the app through
`run.sh`, then capture what it renders.

Usage:
    python3 scripts/fake-riot/mcp-shot.py --port 8099 --out /tmp/opencode/fake-riot.png

Exit status 1 means no image came back; the server's own error text is printed.
"""

from __future__ import annotations

import argparse
import base64
import json
import sys
import time
import urllib.request

URL_TEMPLATE = "http://127.0.0.1:{port}/mcp"
HEADERS = {
    "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream",
}


class Mcp:
    def __init__(self, port: int) -> None:
        self.url = URL_TEMPLATE.format(port=port)
        self._id = 0
        self._call(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "fake-riot-shot", "version": "1"},
            },
        )

    def _call(self, method: str, params: dict) -> dict:
        self._id += 1
        body = json.dumps(
            {"jsonrpc": "2.0", "id": self._id, "method": method, "params": params}
        ).encode("utf-8")
        request = urllib.request.Request(self.url, data=body, headers=HEADERS)
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = json.load(response)
        if "error" in payload:
            raise RuntimeError(f"{method}: {payload['error']}")
        return payload["result"]

    def tool(self, name: str, arguments: dict | None = None) -> dict:
        return self._call("tools/call", {"name": name, "arguments": arguments or {}})


def first_window(mcp: Mcp) -> dict:
    handles = json.loads(mcp.tool("list_windows")["content"][0]["text"])["windowHandles"]
    if not handles:
        raise RuntimeError("the app reports no windows")
    return handles[0]


def screenshot(mcp: Mcp, window: dict) -> bytes:
    result = mcp.tool("take_screenshot", {"windowHandle": window})
    for item in result.get("content", []):
        if item.get("type") == "image" and item.get("data"):
            return base64.b64decode(item["data"])
        # Some builds nest the payload under `source`.
        source = item.get("source") or {}
        if source.get("type") == "base64" and source.get("data"):
            return base64.b64decode(source["data"])
    raise RuntimeError(f"no image in take_screenshot result: {json.dumps(result)[:400]}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Screenshot the app via Slint MCP.")
    parser.add_argument("--port", type=int, required=True, help="the app's --mcp-port")
    parser.add_argument("--out", required=True, help="PNG path to write")
    parser.add_argument(
        "--wait",
        type=float,
        default=0.0,
        help="seconds to wait for a window to exist (default 0)",
    )
    args = parser.parse_args(argv)

    deadline = time.monotonic() + max(args.wait, 0.0)
    while True:
        try:
            mcp = Mcp(args.port)
            window = first_window(mcp)
            break
        except Exception:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.5)

    png = screenshot(mcp, window)
    with open(args.out, "wb") as handle:
        handle.write(png)
    print(f"wrote {args.out} ({len(png)} bytes, window {window})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
