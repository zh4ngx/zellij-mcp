#!/usr/bin/env python3
"""Smoke-test driver for zellij-mcp.

Spawns the binary, walks through MCP handshake and each tool call. Prints a
human-readable PASS/FAIL line per step and a JSON dump of every response so
failures are debuggable.

Usage:
    python3 scripts/smoke.py [--binary PATH] [--session NAME]

If --session is provided, all tool calls scope to that zellij session. If
omitted, the script discovers the current session from `zellij list-sessions`.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_BIN = REPO / "target" / "debug" / "zellij-mcp"


class McpClient:
    def __init__(self, binary: str):
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
            env={**os.environ},
        )
        self._next_id = 1

    def _send(self, payload: dict) -> None:
        line = json.dumps(payload) + "\n"
        self.proc.stdin.write(line.encode())
        self.proc.stdin.flush()

    def _recv(self) -> dict:
        line = self.proc.stdout.readline()
        if not line:
            err = self.proc.stderr.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"server closed stdout. stderr: {err}")
        return json.loads(line.decode())

    def request(self, method: str, params: dict | None = None) -> dict:
        rid = self._next_id
        self._next_id += 1
        payload = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            payload["params"] = params
        self._send(payload)
        resp = self._recv()
        if resp.get("id") != rid:
            raise RuntimeError(f"id mismatch: sent {rid}, got {resp.get('id')}: {resp}")
        return resp

    def notify(self, method: str, params: dict | None = None) -> None:
        payload = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        self._send(payload)

    def close(self) -> str:
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=2)
        except Exception:
            self.proc.kill()
        stderr = self.proc.stderr.read().decode("utf-8", errors="replace")
        return stderr


def step(label: str, fn):
    print(f"\n=== {label} ===")
    try:
        result = fn()
        print(f"PASS: {label}")
        return result
    except Exception as e:
        print(f"FAIL: {label}: {e}")
        raise


def expect_no_error(resp: dict, label: str):
    if "error" in resp:
        raise RuntimeError(f"{label}: rpc error {resp['error']}")
    return resp.get("result", {})


def find_text_payload(call_result: dict) -> str:
    """Pull text content out of a tools/call response."""
    content = call_result.get("content") or []
    for c in content:
        if c.get("type") == "text":
            return c.get("text", "")
    # Could also be a structured-only response
    sc = call_result.get("structuredContent")
    if sc is not None:
        return json.dumps(sc)
    return ""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", default=str(DEFAULT_BIN))
    ap.add_argument(
        "--session",
        default=None,
        help="zellij session name to scope tool calls. Defaults to current/discovered.",
    )
    ap.add_argument(
        "--keep-focus-on",
        default=os.environ.get("ZELLIJ_PANE_ID"),
        help="Pane ID to restore focus to after spawn-pane (default: $ZELLIJ_PANE_ID)",
    )
    ap.add_argument(
        "--no-mutating-tests",
        action="store_true",
        help="Skip spawn-pane / send-text / kill-pane (read-only smoke only)",
    )
    args = ap.parse_args()

    if not Path(args.binary).is_file():
        print(f"binary not found: {args.binary}", file=sys.stderr)
        return 2

    if args.session is None:
        try:
            out = subprocess.check_output(["zellij", "list-sessions", "-s"], text=True)
            sessions = [s for s in out.strip().splitlines() if s]
            if sessions:
                args.session = sessions[0]
                print(f"(auto) using session: {args.session}")
        except Exception as e:
            print(f"could not discover session: {e}", file=sys.stderr)

    cli = McpClient(args.binary)
    overall_ok = True
    spawned_pane: str | None = None
    try:
        # ----------------------------------------------------------------
        # 1. initialize
        # ----------------------------------------------------------------
        def _init():
            r = cli.request(
                "initialize",
                {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "smoke", "version": "0.0.0"},
                },
            )
            res = expect_no_error(r, "initialize")
            print(json.dumps(res, indent=2)[:600])
            return res

        step("initialize", _init)
        cli.notify("notifications/initialized")

        # ----------------------------------------------------------------
        # 2. tools/list — confirm 6 tools with output schemas
        # ----------------------------------------------------------------
        def _list_tools():
            r = cli.request("tools/list")
            res = expect_no_error(r, "tools/list")
            tools = res.get("tools", [])
            names = sorted(t["name"] for t in tools)
            print(f"tools ({len(tools)}): {names}")
            expected = sorted(
                ["list-panes", "spawn-pane", "send-text", "read-pane", "focus-pane", "kill-pane"]
            )
            assert names == expected, f"expected {expected}, got {names}"
            for t in tools:
                if "outputSchema" not in t:
                    raise AssertionError(f"tool {t['name']} missing outputSchema")
            return tools

        step("tools/list", _list_tools)

        # ----------------------------------------------------------------
        # 3. list-panes
        # ----------------------------------------------------------------
        def _list_panes():
            params = {"name": "list-panes", "arguments": {}}
            if args.session:
                params["arguments"]["session"] = args.session
            r = cli.request("tools/call", params)
            res = expect_no_error(r, "list-panes")
            if res.get("isError"):
                raise RuntimeError(f"tool reported error: {res}")
            payload = res.get("structuredContent")
            if payload is None:
                # fall back to text
                payload = json.loads(find_text_payload(res))
            panes = payload.get("panes", [])
            print(f"panes: {len(panes)}")
            for p in panes[:6]:
                print(f"  {p['id']:14s} focused={p['is_focused']} title={p['title']!r}")
            assert len(panes) > 0, "expected at least one pane"
            return panes

        panes = step("list-panes (initial)", _list_panes)

        if args.no_mutating_tests:
            print("\n(skipping mutating tests per --no-mutating-tests)")
        else:
            # ------------------------------------------------------------
            # 4. spawn-pane (with keep_focus_on)
            # ------------------------------------------------------------
            def _spawn():
                arguments = {
                    "cwd": "/tmp",
                    "command": ["sh", "-c", "echo hello-from-smoke; sleep 600"],
                    "name": "zellij-mcp-smoke",
                    "floating": True,
                }
                if args.session:
                    arguments["session"] = args.session
                if args.keep_focus_on:
                    arguments["keep_focus_on"] = args.keep_focus_on
                r = cli.request(
                    "tools/call", {"name": "spawn-pane", "arguments": arguments}
                )
                res = expect_no_error(r, "spawn-pane")
                if res.get("isError"):
                    raise RuntimeError(f"tool reported error: {res}")
                sc = res.get("structuredContent") or json.loads(find_text_payload(res))
                pid = sc["pane_id"]
                print(f"spawned: {pid}")
                return pid

            spawned_pane = step("spawn-pane", _spawn)
            time.sleep(0.6)  # let the pane render

            # ------------------------------------------------------------
            # 5. read-pane (verify the spawned pane shows our echo)
            # ------------------------------------------------------------
            def _read():
                arguments = {"pane_id": spawned_pane, "full": True}
                if args.session:
                    arguments["session"] = args.session
                r = cli.request(
                    "tools/call", {"name": "read-pane", "arguments": arguments}
                )
                res = expect_no_error(r, "read-pane")
                if res.get("isError"):
                    raise RuntimeError(f"tool reported error: {res}")
                sc = res.get("structuredContent") or json.loads(find_text_payload(res))
                text = sc["text"]
                snippet = text[:300].replace("\n", " ↵ ")
                print(f"viewport (first 300): {snippet!r}")
                if "hello-from-smoke" not in text:
                    raise AssertionError(
                        f"expected 'hello-from-smoke' in viewport, got: {text!r}"
                    )
                return text

            step("read-pane (after spawn)", _read)

            # ------------------------------------------------------------
            # 6. send-text (without submit)
            # ------------------------------------------------------------
            def _send_no_submit():
                arguments = {
                    "pane_id": spawned_pane,
                    "text": "# smoke test typed (no submit)",
                    "submit": False,
                }
                if args.session:
                    arguments["session"] = args.session
                r = cli.request(
                    "tools/call", {"name": "send-text", "arguments": arguments}
                )
                res = expect_no_error(r, "send-text")
                if res.get("isError"):
                    raise RuntimeError(f"tool reported error: {res}")
                sc = res.get("structuredContent") or json.loads(find_text_payload(res))
                assert sc.get("ok") is True
                return sc

            step("send-text (submit=false)", _send_no_submit)

            # ------------------------------------------------------------
            # 7. focus-pane (focus the spawned pane, then back)
            # ------------------------------------------------------------
            def _focus_back_and_forth():
                if not args.keep_focus_on:
                    print("(skipping — no --keep-focus-on / ZELLIJ_PANE_ID)")
                    return
                # focus spawned pane
                r = cli.request(
                    "tools/call",
                    {
                        "name": "focus-pane",
                        "arguments": {
                            "pane_id": spawned_pane,
                            **({"session": args.session} if args.session else {}),
                        },
                    },
                )
                expect_no_error(r, "focus-pane(spawned)")
                # focus back
                r = cli.request(
                    "tools/call",
                    {
                        "name": "focus-pane",
                        "arguments": {
                            "pane_id": args.keep_focus_on,
                            **({"session": args.session} if args.session else {}),
                        },
                    },
                )
                expect_no_error(r, "focus-pane(original)")

            step("focus-pane (round-trip)", _focus_back_and_forth)

            # ------------------------------------------------------------
            # 8. kill-pane (cleanup)
            # ------------------------------------------------------------
            def _kill():
                arguments = {"pane_id": spawned_pane}
                if args.session:
                    arguments["session"] = args.session
                r = cli.request(
                    "tools/call", {"name": "kill-pane", "arguments": arguments}
                )
                res = expect_no_error(r, "kill-pane")
                if res.get("isError"):
                    raise RuntimeError(f"tool reported error: {res}")

            step("kill-pane", _kill)
            spawned_pane = None  # mark cleaned

            # ------------------------------------------------------------
            # 9. error path: focus-pane to bogus pane id (zellij errors here;
            # note that write-chars/close-pane/dump-screen silently succeed
            # for nonexistent ids — a zellij CLI quirk we don't paper over).
            # ------------------------------------------------------------
            def _focus_bad_id():
                arguments = {"pane_id": "terminal_999999"}
                if args.session:
                    arguments["session"] = args.session
                r = cli.request(
                    "tools/call", {"name": "focus-pane", "arguments": arguments}
                )
                res = expect_no_error(r, "focus-pane(bad)")
                if not res.get("isError"):
                    raise AssertionError(
                        f"expected isError=true on bad pane id, got: {res}"
                    )
                content = find_text_payload(res)
                print(f"bad pane error: {content[:200]!r}")
                if "not found" not in content:
                    raise AssertionError(
                        f"expected zellij 'not found' message in error, got: {content!r}"
                    )

            step("focus-pane (bogus pane id → tool error)", _focus_bad_id)

    except Exception:
        overall_ok = False
        # best-effort cleanup
        if spawned_pane is not None:
            try:
                arguments = {"pane_id": spawned_pane}
                if args.session:
                    arguments["session"] = args.session
                cli.request("tools/call", {"name": "kill-pane", "arguments": arguments})
            except Exception as e:
                print(f"(cleanup failed: {e})")
    finally:
        stderr = cli.close()
        print("\n--- server stderr ---")
        print(stderr.strip() or "(empty)")
        print("--- end stderr ---")

    print()
    print("OVERALL:", "PASS" if overall_ok else "FAIL")
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main())
