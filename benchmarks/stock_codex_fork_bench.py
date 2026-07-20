#!/usr/bin/env python3
"""Drive stock Codex through the same 10-turn + three historical-fork workload."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class TurnMeasurement:
    thread_id: str
    turn_id: str
    latency_ms: float
    usage: dict[str, Any] | None


class AppServer:
    def __init__(self, command: list[str]) -> None:
        self.command = command
        self.process: asyncio.subprocess.Process | None = None
        self.next_id = 1
        self.pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self.turns: dict[str, asyncio.Future[dict[str, Any]]] = {}
        self.completed: dict[str, dict[str, Any]] = {}
        self.raw_usage: dict[str, dict[str, Any] | None] = {}
        self.token_usage: dict[str, dict[str, Any]] = {}
        self.reader_task: asyncio.Task[None] | None = None
        self.stderr_task: asyncio.Task[None] | None = None

    async def start(self) -> None:
        self.process = await asyncio.create_subprocess_exec(
            *self.command,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        self.reader_task = asyncio.create_task(self._read_stdout())
        self.stderr_task = asyncio.create_task(self._drain_stderr())
        await self.request(
            "initialize",
            {
                "clientInfo": {
                    "name": "nanocodex_fork_benchmark",
                    "title": "Nanocodex fork benchmark",
                    "version": "1.0.0",
                },
                "capabilities": {"experimentalApi": True},
            },
        )
        await self.notify("initialized", {})

    async def close(self) -> None:
        if self.process is None:
            return
        if self.process.stdin is not None:
            self.process.stdin.close()
        try:
            await asyncio.wait_for(self.process.wait(), timeout=5)
        except asyncio.TimeoutError:
            self.process.terminate()
            await self.process.wait()
        for task in (self.reader_task, self.stderr_task):
            if task is not None:
                task.cancel()

    async def request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        future: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        self.pending[request_id] = future
        await self._write({"method": method, "id": request_id, "params": params})
        response = await future
        if "error" in response:
            raise RuntimeError(f"{method} failed: {json.dumps(response['error'])}")
        return response["result"]

    async def notify(self, method: str, params: dict[str, Any]) -> None:
        await self._write({"method": method, "params": params})

    async def start_turn(self, thread_id: str, text: str) -> tuple[str, float]:
        started = time.perf_counter()
        result = await self.request(
            "turn/start",
            {
                "threadId": thread_id,
                "input": [{"type": "text", "text": text}],
            },
        )
        return result["turn"]["id"], started

    async def wait_turn(self, thread_id: str, turn_id: str, started: float) -> TurnMeasurement:
        completed = self.completed.pop(turn_id, None)
        if completed is None:
            future = self.turns.get(turn_id)
            if future is None:
                future = asyncio.get_running_loop().create_future()
                self.turns[turn_id] = future
            completed = await future
        status = completed["turn"]["status"]
        if status != "completed":
            raise RuntimeError(f"turn {turn_id} ended with {status}: {json.dumps(completed)}")
        return TurnMeasurement(
            thread_id=thread_id,
            turn_id=turn_id,
            latency_ms=(time.perf_counter() - started) * 1000,
            usage=self.raw_usage.pop(turn_id, None)
            or self.token_usage.pop(turn_id, {}).get("last"),
        )

    async def _write(self, value: dict[str, Any]) -> None:
        if self.process is None or self.process.stdin is None:
            raise RuntimeError("app-server is not running")
        self.process.stdin.write((json.dumps(value, separators=(",", ":")) + "\n").encode())
        await self.process.stdin.drain()

    async def _read_stdout(self) -> None:
        assert self.process is not None and self.process.stdout is not None
        while line := await self.process.stdout.readline():
            message = json.loads(line)
            if "id" in message and ("result" in message or "error" in message):
                future = self.pending.pop(message["id"], None)
                if future is not None and not future.done():
                    future.set_result(message)
                continue
            method = message.get("method")
            params = message.get("params", {})
            if method == "turn/completed":
                turn_id = params["turn"]["id"]
                future = self.turns.pop(turn_id, None)
                if future is None:
                    self.completed[turn_id] = params
                elif not future.done():
                    future.set_result(params)
            elif method == "rawResponse/completed":
                self.raw_usage[params["turnId"]] = params.get("usage")
            elif method == "thread/tokenUsage/updated":
                self.token_usage[params["turnId"]] = params["tokenUsage"]
            elif "id" in message:
                # This workload should not trigger approvals or dynamic tools.
                await self._write(
                    {
                        "id": message["id"],
                        "error": {"code": -32601, "message": "benchmark rejects server requests"},
                    }
                )

    async def _drain_stderr(self) -> None:
        assert self.process is not None and self.process.stderr is not None
        while line := await self.process.stderr.readline():
            if os.environ.get("CODEX_BENCH_STDERR") == "1":
                sys.stderr.buffer.write(line)
                sys.stderr.flush()


def prefix_prompt(facts: int) -> str:
    rows = [
        "Memorize these deterministic facts for later turns. Do not use tools.",
        *(f"FACT_{index:04}=VALUE_{index:04}_ABCDEFGHIJKLMNOPQRSTUVWXYZ" for index in range(facts)),
        "Reply only ACK_01.",
    ]
    return "\n".join(rows)


def cached_tokens(usage: dict[str, Any] | None) -> int | None:
    if usage is None:
        return None
    for path in (
        ("cachedInputTokens",),
        ("cached_input_tokens",),
        ("inputTokens", "cachedTokens"),
        ("input_tokens_details", "cached_tokens"),
    ):
        value: Any = usage
        for key in path:
            if not isinstance(value, dict):
                value = None
                break
            value = value.get(key)
        if isinstance(value, int):
            return value
    return None


async def benchmark(args: argparse.Namespace) -> dict[str, Any]:
    server = AppServer(args.app_server)
    created_threads: list[str] = []
    await server.start()
    try:
        result = await server.request(
            "thread/start",
            {
                "model": args.model,
                "cwd": str(args.cwd),
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "experimentalRawEvents": True,
                "serviceName": "nanocodex_fork_benchmark",
            },
        )
        root = result["thread"]["id"]
        created_threads.append(root)

        chain: list[TurnMeasurement] = []
        for index in range(1, 11):
            prompt = prefix_prompt(args.facts) if index == 1 else f"Reply only ACK_{index:02}."
            turn_id, started = await server.start_turn(root, prompt)
            chain.append(await server.wait_turn(root, turn_id, started))

        main_id, main_started = await server.start_turn(root, "Reply only MAIN_11.")
        fork_started = time.perf_counter()
        fork_results = await asyncio.gather(
            *(
                server.request(
                    "thread/fork",
                    {
                        "threadId": root,
                        "lastTurnId": chain[index - 1].turn_id,
                        "ephemeral": True,
                        "excludeTurns": True,
                    },
                )
                for index in (3, 6, 9)
            )
        )
        fork_wall_ms = (time.perf_counter() - fork_started) * 1000
        branches = [result["thread"]["id"] for result in fork_results]
        created_threads.extend(branches)

        branch_starts = await asyncio.gather(
            *(
                server.start_turn(branch, f"Forked from turn {index}. Reply only FORK_{index:02}.")
                for branch, index in zip(branches, (3, 6, 9), strict=True)
            )
        )
        main_task = server.wait_turn(root, main_id, main_started)
        branch_tasks = [
            server.wait_turn(branch, turn_id, started)
            for branch, (turn_id, started) in zip(branches, branch_starts, strict=True)
        ]
        mainline, *branch_turns = await asyncio.gather(main_task, *branch_tasks)

        return {
            "implementation": "stock_codex",
            "model": args.model,
            "source_commit": args.source_commit,
            "chain_turn_latency_ms": [round(turn.latency_ms, 1) for turn in chain],
            "chain_median_latency_ms": round(
                statistics.median(turn.latency_ms for turn in chain), 1
            ),
            "fork_rpc_wall_ms": round(fork_wall_ms, 1),
            "mainline_latency_ms": round(mainline.latency_ms, 1),
            "branch_latency_ms": [round(turn.latency_ms, 1) for turn in branch_turns],
            "branch_cached_tokens": [cached_tokens(turn.usage) for turn in branch_turns],
            "branch_usage": [turn.usage for turn in branch_turns],
        }
    finally:
        for thread_id in created_threads:
            try:
                await server.request("thread/delete", {"threadId": thread_id})
            except Exception:
                pass
        await server.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--app-server",
        nargs="+",
        default=["codex", "app-server"],
        help="command used to launch Codex app-server",
    )
    parser.add_argument("--model", default="gpt-5.6-sol")
    parser.add_argument("--facts", type=int, default=600)
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--source-commit", default="unknown")
    return parser.parse_args()


if __name__ == "__main__":
    print(json.dumps(asyncio.run(benchmark(parse_args())), indent=2))
