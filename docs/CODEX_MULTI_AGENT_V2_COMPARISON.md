# Nanocodex branching and Codex MultiAgentsV2

This note compares Nanocodex's checkpoint-based child-agent design with the
implementation in the local `openai/codex` checkout at
`3ac476bed22a7b7322a710a6ca79a0dbe917d604`. It is an architectural comparison,
not a claim that the two projects should expose compatible APIs.

## Summary

Nanocodex provides fast conversation primitives that an embedding application
can expose as Code Mode tools:

- `AgentHandle::spawn()` creates a clean child while privately reusing builder
  configuration.
- `AgentHandle::fork()` creates a child from the invoking agent's latest safe
  model/tool boundary.
- `Nanocodex::fork_from(&TurnResult)` creates a branch from an exact historical
  completed turn while the mainline may continue.

The model can generate the orchestration topology in Code Mode, including
loops, conditional delegation, and concurrent fan-out with `Promise.all`. The
SDK does not encode a workflow DAG or maintain a global agent graph.

Codex MultiAgentsV2 is a larger actor-like collaboration runtime. Its
`spawn_agent` creates a detached durable thread and returns a canonical task
path. The parent can subsequently use messaging, follow-up, wait, listing, and
interrupt tools. Child completions arrive through a mailbox. Codex therefore
owns task-path identity, a thread registry, persistence, status, concurrency
and rollout budgets, residency, unloading and resume behavior, event
projection, and TUI integration.

## Comparison

| Area | Nanocodex | Codex MultiAgentsV2 |
| --- | --- | --- |
| Orchestration | Generated Code Mode program using application-defined child tools | Direct collaboration tool calls across model turns |
| Child creation | Separate typed `spawn()` and `fork()` operations | `spawn_agent` with `fork_turns = none`, `all`, or a positive integer |
| Execution shape | Structured fan-out/fan-in; the tool can await the child result | Detached threads plus asynchronous mailbox completion |
| Fork source | Immutable completed `ModelCheckpoint` | Materialized, flushed, loaded, filtered parent rollout |
| Historical branch | Exact `fork_from(&TurnResult)` | Last-N truncation from the current snapshot; no exact historical turn handle |
| Inherited state | Exact completed Responses conversation state | Sanitized messages and durable context; tool calls, outputs, and reasoning are removed |
| API request | Child starts from the parent response ID and sends its delta | New thread initially sends reconstructed inherited input |
| Prompt cache | Contextual forks share explicit cache lineage | The V2 tree shares its root session ID, which is also the default cache key |
| Code Mode | Child tools can be nested tools inside generated JavaScript | Collaboration tools are intentionally unavailable inside `functions.exec` |
| Communication | Application-defined tool result flow | Mailboxes, `send_message`, `followup_task`, and `wait_agent` |
| Lifecycle | The application adapter owns accepted child invocations, propagates cancellation through descendants, rejects wait cycles, and drains shutdown; no durable registry | Status registry, interruption, residency, unload, reload, and durable resume |
| Limits | Depth, concurrent-child, token, deadline, rollout, and residency budgets remain application-defined | Built-in execution, residency, thread, and rollout budgets |
| Durability | In-memory typed history with API-checkpoint replay fallback | Durable thread, rollout, and agent-graph stores |

## Reliability and policy boundary

The bundled Nanocodex application adapter registers a child invocation before
awaiting its first turn and retains control of every accepted active or queued
turn. Cancelling a parent invocation cancels its active descendants, including
recursively started work. A follow-up that would wait on its own session or
close a multi-child wait cycle fails before the impossible prompt is queued.
Shutdown stops new insertions, cancels accepted work, and drains child turns,
cleanup work, command handles, and event tasks. These are in-process lifecycle
guarantees, not durable scheduling or restart recovery.

At the conversation boundary, a fork observes the latest complete
response/tool-output pair even when compaction is still running and never
inherits partial output or an unmatched call. After a terminal continuation
failure, the first later prompt performs one complete replay from authoritative
typed history without a previous response ID; healthy turns then return to
incremental deltas.

The public example enables the normal repository tools for useful inspection.
Those tools include mutation-capable handlers. Children are instructed to
operate read-only by not modifying files or running destructive commands, but
that instruction-based policy is not a sandbox, security boundary, or
capability boundary. Applications needing enforcement must restrict both
registered tools and their execution environment.

Lifecycle correctness does not choose resource policy. Maximum depth,
concurrent children, token spend, deadlines, rollout count, and child residency
remain explicit embedding-application budgets. Child event streams also remain
independent and optional rather than becoming a merged scheduler event bus.

## Fork mechanics

Nanocodex checkpoints only successful terminal turns. A contextual child gets
a new session, WebSocket, driver, service stack, and tool runtime while sharing
the immutable committed transcript and prompt-cache lineage. Its first healthy
request references the parent's stored response and contains only the child
delta. If that API checkpoint is unavailable, Nanocodex drops the response ID
and replays its complete client-owned typed history.

Codex MultiAgentsV2 first materializes and flushes the parent rollout, reloads
the model context from its thread store, optionally truncates it to the last N
turns, and filters it before creating a child thread. Its full-history filter
retains system, developer, and user messages plus final assistant answers, but
removes reasoning, function and custom-tool calls, tool outputs, searches, and
inter-agent communication records. This makes the fork durable and sanitized,
but it is not an exact copy of the parent's Responses state.

Codex descendants share the root `AgentControl` session ID. Because the
Responses client uses that session ID as its default prompt-cache key, V2 can
receive cross-child cache hits even though a new child still uploads its
reconstructed inherited input. This differs from generic app-server
`thread/fork`; benchmark results for that operation must not be presented as
measurements of current MultiAgentsV2.

There is also a semantic difference during an active parent turn. Codex flushes
the current rollout and sanitizes incomplete tool records, so the current user
request can be inherited. Nanocodex deliberately samples the latest fully
completed checkpoint and excludes all partial current-turn work. A Nanocodex
child tool must therefore include its delegated task explicitly.

## Product tradeoff

The Nanocodex approach is preferable for fast, exact, programmatically composed
branches. It avoids rollout persistence and reconstruction on the hot path,
minimizes child request bytes, supports exact historical branching, and lets
Code Mode generate rather than merely select the orchestration structure.

Codex's approach is preferable when children must remain independently alive,
receive later messages, perform multiple turns, survive process restarts, be
navigable as first-class threads, or operate under centrally enforced limits.
That functionality necessarily brings substantially more control-plane and
event-lifecycle machinery.

## Direction for Nanocodex

Keep `AgentHandle` and checkpoint forks as the core primitive. Do not introduce
a central workflow graph solely to imitate Codex. The lifecycle and recovery
rules above are the reliability baseline for the application adapter and owned
session. The remaining production policies to evaluate independently are:

1. Concrete depth, concurrent-child, token, deadline, rollout, and residency
   budgets for each embedding application.
2. A small typed child-task handle if a real consumer needs detached execution.
3. Separating conversation lineage from cache affinity so clean children may
   optionally reuse a byte-stable shared prefix without inheriting history.
4. Caller-owned checkpoint persistence if process durability becomes a concrete
   library requirement.

Messaging, durable task paths, status registries, and unload/resume machinery
should remain outside the core until a consumer demonstrates a need for
long-lived communicating agents rather than structured child tasks.

## Codex implementation references

The comparison above was verified against these files in the referenced Codex
checkpoint:

- `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/message_tool.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/wait.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/interrupt_agent.rs`
- `codex-rs/core/src/agent/control.rs`
- `codex-rs/core/src/agent/control/spawn.rs`
- `codex-rs/core/src/agent/control/execution.rs`
- `codex-rs/core/src/agent/control/residency.rs`
- `codex-rs/core/src/session/session.rs`
- `codex-rs/core/src/session/mod.rs`
- `codex-rs/core/src/client.rs`
- `codex-rs/core/src/config/mod.rs`
