#!/usr/bin/env python3
"""Drive stock Codex through the same 10-turn + three historical-fork workload."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import sys
import tempfile
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
    final_message: str | None


class AppServer:
    def __init__(self, command: list[str], environment: dict[str, str], effort: str) -> None:
        self.command = command
        self.environment = environment
        self.effort = effort
        self.process: asyncio.subprocess.Process | None = None
        self.next_id = 1
        self.pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self.turns: dict[str, asyncio.Future[dict[str, Any]]] = {}
        self.completed: dict[str, dict[str, Any]] = {}
        self.raw_usage: dict[str, dict[str, Any] | None] = {}
        self.token_usage: dict[str, dict[str, Any]] = {}
        self.final_messages: dict[str, str] = {}
        self.reader_task: asyncio.Task[None] | None = None
        self.stderr_task: asyncio.Task[None] | None = None

    async def start(self) -> None:
        self.process = await asyncio.create_subprocess_exec(
            *self.command,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=self.environment,
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
                "effort": self.effort,
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
            final_message=self.final_messages.pop(turn_id, None),
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
            elif method == "item/completed":
                item = params.get("item", {})
                if item.get("type") == "agentMessage":
                    self.final_messages[params["turnId"]] = item.get("text", "")
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


def prefix_prompt(workload: dict[str, Any]) -> str:
    rows = [
        workload["first_prompt_prefix"],
        *(
            f"FACT_{index:04}=VALUE_{index:04}_ABCDEFGHIJKLMNOPQRSTUVWXYZ"
            for index in range(workload["fact_count"])
        ),
        "Reply only ACK_01.",
    ]
    return "\n".join(rows)


def prompts(workload: dict[str, Any]) -> list[str]:
    values = [prefix_prompt(workload)]
    values.extend(
        f"Reply only ACK_{index:02}."
        for index in range(2, workload["chain_turns"] + 1)
    )
    values.append(workload["mainline_prompt"])
    values.extend(
        f"Forked from turn {turn}. Reply only FORK_{turn:02}."
        for turn in workload["fork_turns"]
    )
    return values


def digest_strings(values: list[str]) -> str:
    data = bytearray()
    for value in values:
        data.extend(value.encode())
        data.append(0)
    return fnv1a64(data)


def fnv1a64(data: bytes | bytearray) -> str:
    digest = 0xCBF29CE484222325
    for byte in data:
        digest ^= byte
        digest = (digest * 0x00000100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{digest:016x}"


def load_api_key(env_file: Path) -> str:
    key = os.environ.get("OPENAI_API_KEY")
    if key:
        return key
    for raw_line in env_file.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        name, value = line.split("=", 1)
        if name.strip() == "OPENAI_API_KEY":
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            if value:
                return value
    raise RuntimeError(f"OPENAI_API_KEY is missing from the environment and {env_file}")


def assert_message(turn: TurnMeasurement, expected: str) -> None:
    if turn.final_message is None:
        raise RuntimeError(f"turn {turn.turn_id} completed without an agent message")
    if turn.final_message.strip() != expected:
        raise RuntimeError(
            f"unexpected response: expected {expected!r}, got {turn.final_message!r}"
        )


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
    workload_bytes = args.workload.read_bytes()
    workload = json.loads(workload_bytes)
    if (
        workload["schema_version"] != 1
        or workload["model"] != args.model
        or workload["reasoning_effort"] != "low"
        or workload["text_verbosity"] != "low"
        or workload["chain_turns"] != 10
        or workload["fork_turns"] != [3, 6, 9]
    ):
        raise RuntimeError("workload is incompatible with this parity harness")
    all_prompts = prompts(workload)
    if digest_strings(all_prompts) != workload["prompt_fnv1a64"]:
        raise RuntimeError("generated prompts do not match the workload digest")
    api_key = load_api_key(args.env_file)
    clean_home = tempfile.TemporaryDirectory(prefix="codex-parity-home-")
    auth_file = Path(clean_home.name) / "auth.json"
    auth_file.write_text(json.dumps({"OPENAI_API_KEY": api_key}))
    auth_file.chmod(0o600)
    process_env = os.environ.copy()
    process_env.update(
        {
            "CODEX_HOME": clean_home.name,
            "CODEX_API_KEY": api_key,
            "OPENAI_API_KEY": api_key,
            "RUST_LOG": "warn",
        }
    )
    server = AppServer(args.app_server, process_env, workload["reasoning_effort"])
    created_threads: list[str] = []
    await server.start()
    try:
        thread_start_started = time.perf_counter()
        result = await server.request(
            "thread/start",
            {
                "model": args.model,
                "cwd": str(args.cwd),
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "baseInstructions": workload["base_instructions"],
                "config": {
                    "model_reasoning_effort": workload["reasoning_effort"],
                    "model_reasoning_summary": "none",
                    "model_verbosity": workload["text_verbosity"],
                },
                "experimentalRawEvents": True,
                "serviceName": "nanocodex_fork_benchmark",
            },
        )
        thread_start_wall_ms = (time.perf_counter() - thread_start_started) * 1000
        root = result["thread"]["id"]
        created_threads.append(root)

        chain: list[TurnMeasurement] = []
        for index in range(1, workload["chain_turns"] + 1):
            prompt = all_prompts[index - 1]
            turn_id, started = await server.start_turn(root, prompt)
            completed = await server.wait_turn(root, turn_id, started)
            assert_message(completed, f"ACK_{index:02}")
            chain.append(completed)

        main_id, main_started = await server.start_turn(
            root, all_prompts[workload["chain_turns"]]
        )
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
                for index in workload["fork_turns"]
            )
        )
        fork_wall_ms = (time.perf_counter() - fork_started) * 1000
        branches = [result["thread"]["id"] for result in fork_results]
        created_threads.extend(branches)

        branch_starts = await asyncio.gather(
            *(
                server.start_turn(branch, prompt)
                for branch, prompt in zip(
                    branches,
                    all_prompts[workload["chain_turns"] + 1 :],
                )
            )
        )
        main_task = server.wait_turn(root, main_id, main_started)
        branch_tasks = [
            server.wait_turn(branch, turn_id, started)
            for branch, (turn_id, started) in zip(branches, branch_starts)
        ]
        mainline, *branch_turns = await asyncio.gather(main_task, *branch_tasks)
        assert_message(mainline, "MAIN_11")
        for branch_turn, index in zip(branch_turns, workload["fork_turns"]):
            assert_message(branch_turn, f"FORK_{index:02}")

        return {
            "implementation": "stock_codex",
            "model": args.model,
            "reasoning_effort": workload["reasoning_effort"],
            "text_verbosity": workload["text_verbosity"],
            "source_commit": args.source_commit,
            "codex_cli_version": args.codex_cli_version,
            "workspace": str(args.cwd),
            "agents_md_fnv1a64": fnv1a64((args.cwd / "AGENTS.md").read_bytes()),
            "workload_fnv1a64": fnv1a64(workload_bytes),
            "prompt_fnv1a64": digest_strings(all_prompts),
            "thread_start_wall_ms": round(thread_start_wall_ms, 1),
            "chain_turn_latency_ms": [round(turn.latency_ms, 1) for turn in chain],
            "chain_usage": [turn.usage for turn in chain],
            "chain_median_latency_ms": round(
                statistics.median(turn.latency_ms for turn in chain), 1
            ),
            "fork_rpc_wall_ms": round(fork_wall_ms, 1),
            "mainline_latency_ms": round(mainline.latency_ms, 1),
            "mainline_usage": mainline.usage,
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
        clean_home.cleanup()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--app-server",
        nargs="+",
        default=["codex", "app-server"],
        help="command used to launch Codex app-server",
    )
    parser.add_argument("--model", default="gpt-5.6-sol")
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument(
        "--workload",
        type=Path,
        default=Path(__file__).with_name("codex_parity_workload.json"),
    )
    parser.add_argument("--env-file", type=Path, default=Path.cwd() / ".env")
    parser.add_argument("--source-commit", default="unknown")
    parser.add_argument("--codex-cli-version", default="unknown")
    return parser.parse_args()


if __name__ == "__main__":
    print(json.dumps(asyncio.run(benchmark(parse_args())), indent=2))
