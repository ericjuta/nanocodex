"""Install and run the Rust harness inside a Harbor task environment."""

from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path
from typing import Any
from uuid import uuid4

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.trajectories import Agent, Step, Trajectory
from harbor.utils.trajectory_utils import format_trajectory_json


PROTOCOL_VERSION = 1
TERMINAL_EVENTS = {"run.completed", "run.failed"}


class HarnessAgent(BaseInstalledAgent):
    """Upload one Rust binary, run it once, and retain its JSONL."""

    SUPPORTS_ATIF = True
    _BINARY = "/installed-agent/harness"
    _INPUT = "/logs/agent/input.jsonl"
    _EVENTS = "/logs/agent/events.jsonl"
    _STDERR = "/logs/agent/stderr.log"

    def __init__(
        self,
        logs_dir: Path,
        binary_path: str | Path = ".harness/installed/harness",
        mode: str = "phase0",
        **kwargs: Any,
    ) -> None:
        super().__init__(logs_dir=logs_dir, **kwargs)
        self._binary_path = Path(binary_path).resolve()
        self._mode = mode

    @staticmethod
    def name() -> str:
        return "harness"

    def get_version_command(self) -> str:
        return f"{self._BINARY} --version"

    async def install(self, environment: BaseEnvironment) -> None:
        if not self._binary_path.is_file():
            raise RuntimeError(
                f"missing harness binary at {self._binary_path}; "
                "run `just build-agent`"
            )
        await environment.upload_file(self._binary_path, self._BINARY)
        await self.exec_as_root(environment, f"chmod 0755 {self._BINARY}")

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        del context
        request = {
            "protocol_version": PROTOCOL_VERSION,
            "request_id": str(self.context_id or self.session_id or uuid4()),
            "seq": 1,
            "type": "task.start",
            "payload": {"instruction": instruction, "workspace": "/app"},
        }
        input_path = self.logs_dir / "input.jsonl"
        input_path.write_text(
            json.dumps(request, separators=(",", ":")) + "\n", encoding="utf-8"
        )
        if not environment.capabilities.mounted:
            await environment.upload_file(input_path, self._INPUT)

        command = (
            f"{self._BINARY} run --mode {shlex.quote(self._mode)} "
            f"< {self._INPUT} 2> {self._STDERR} | tee {self._EVENTS}"
        )
        result = await self.exec_as_agent(environment, command, cwd="/app")
        if result.stdout:
            print(result.stdout, end="", flush=True)
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr, flush=True)

    def populate_context_post_run(self, context: AgentContext) -> None:
        requests = self._read_jsonl(self.logs_dir / "input.jsonl")
        if len(requests) != 1 or requests[0].get("type") != "task.start":
            raise RuntimeError("input.jsonl must contain one task.start event")
        request = requests[0]
        request_id = request["request_id"]

        events = self._read_jsonl(self.logs_dir / "events.jsonl")
        for seq, event in enumerate(events, start=1):
            if (
                event.get("protocol_version") != PROTOCOL_VERSION
                or event.get("request_id") != request_id
                or event.get("seq") != seq
                or not isinstance(event.get("type"), str)
                or not isinstance(event.get("payload"), dict)
            ):
                raise RuntimeError(f"invalid harness event at sequence {seq}")

        terminal = next(
            (event for event in events if event["type"] in TERMINAL_EVENTS), None
        )
        terminal_payload = terminal["payload"] if terminal else {}
        model_calls = terminal_payload.get("model_calls", 0)
        tool_calls = sum(event["type"] == "tool.call" for event in events)
        message = next(
            (
                event["payload"].get("text", "")
                for event in reversed(events)
                if event["type"] == "assistant.message"
            ),
            "Harness emitted no assistant message.",
        )

        trajectory = Trajectory(
            session_id=request_id,
            agent=Agent(name=self.name(), version=self.version()),
            steps=[
                Step(
                    step_id=1,
                    source="user",
                    message=request["payload"]["instruction"],
                ),
                Step(
                    step_id=2,
                    source="agent",
                    message=message,
                    llm_call_count=model_calls,
                    extra={
                        "terminal_event_type": terminal["type"] if terminal else None,
                        "terminal_payload": terminal_payload,
                    },
                ),
            ],
            notes=None if terminal else "The process emitted no terminal event.",
        )
        (self.logs_dir / "trajectory.json").write_text(
            format_trajectory_json(trajectory.to_json_dict()), encoding="utf-8"
        )

        context.n_input_tokens = 0
        context.n_cache_tokens = 0
        context.n_output_tokens = 0
        context.cost_usd = 0.0
        context.metadata = {
            "protocol_version": PROTOCOL_VERSION,
            "terminal_event_type": terminal["type"] if terminal else None,
            "model_calls": model_calls,
            "tool_calls": tool_calls,
        }

    @staticmethod
    def _read_jsonl(path: Path) -> list[dict[str, Any]]:
        try:
            values = [
                json.loads(line)
                for line in path.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
        except (OSError, json.JSONDecodeError) as error:
            raise RuntimeError(f"failed to read JSONL from {path}: {error}") from error
        if not all(isinstance(value, dict) for value in values):
            raise RuntimeError(f"all JSONL values in {path} must be objects")
        return values
