# Nanocodex plan

## Goal

Build a small, high-performance, headless Rust agents SDK for the current best
supported OpenAI coding model. Nanocodex should be pleasant to embed in a CLI,
server, TUI, notebook, test harness, or future language binding without making
any of those application shapes part of the core SDK.

The library owns model execution, conversation history, prompt caching,
Responses WebSocket state, tools, retries, and cancellation. Applications own
their presentation, event selection, tracing subscriber, persistence, and
transport to their users.

## Product contract

- The primary entry point is `Nanocodex::new(api_key)` or
  `Nanocodex::builder(api_key)`.
- `build()` returns `(Nanocodex, AgentEvents)`: a cheap cloneable command handle
  and one optional ordered event receiver.
- `agent.prompt(...)` accepts a user turn and returns an independently awaitable
  `Turn`; `turn.result()` returns its typed `TurnResult`.
- Follow-on prompts automatically reuse the complete retained conversation.
  Callers never pass previous final messages, response IDs, reasoning items, or
  tool results back into the session.
- The default is one fixed model contract with medium thinking, the standard
  prompt, built-in tools, persistent Responses WebSocket, and bounded typed
  retry/reconnect policy.
- `Tools::builder().tool(...)` accepts both `#[tool]` functions and complete
  `Tool` implementations in the same heterogeneous registry.
- `Responses::builder()` lets callers layer or replace the concrete Tower
  service stack without boxing the client.
- The CLI and Harbor InstalledAgent are adapters over this API. JSONL, Python,
  Docker, and Harbor are not required to embed the library.

## Architecture

```text
application
  ├─ Nanocodex handle ── prompt() ──> private owned driver
  └─ AgentEvents <────────────────── typed ordered events
                                      │
                                      ├─ model/session history
                                      ├─ tool runtime + code mode
                                      └─ ResponsesClient<S>
                                           └─ caller Tower layers
                                                └─ retry policy
                                                     └─ persistent WebSocket
```

Crate ownership is fixed:

- `nanocodex-core`: dependency-light prompts, events, model configuration, and
  typed Responses request/event/item data.
- `nanocodex-service`: persistent WebSocket behavior, complete streamed
  attempts, Tower service/client, retry policy, typed errors, and transport
  telemetry.
- `nanocodex-tools`: code mode, local tools, custom-tool registry, process
  lifecycle, and bounded tool output.
- `nanocodex`: builders and the owned stateful agent lifecycle.
- `nanocodex-macros`: the `#[tool]` implementation.
- `bin/nanocodex`: the Ratatui daily-driver and headless JSONL adapter.

Lower crates must remain usable without importing higher orchestration crates.
Socket tasks and mutable driver details stay private.

## Foundation: complete

### 1. Repository and crate maintenance

- The workspace is a virtual manifest with the executable under `bin/` and
  focused library crates under `crates/`.
- Tools live in coherent modules under `nanocodex-tools/src/{shell,
  apply_patch,code_mode,...}` rather than one giant application crate.
- Obsolete root `src/`, duplicate CLI library helpers, and unused refactor paths
  are gone.
- Public crate boundaries follow ownership rather than historical file layout.

### 2. Responses WebSocket and Tower service

- One `Service<ResponsesAttempt>` call covers a complete streamed attempt, not
  merely a frame send.
- The standard retry policy classifies typed transient failures, honors server
  delay hints, reconnects, and safely replays committed history.
- `ResponsesClient<S>` stays generic over the caller's concrete service.
- Deferred `.layer(...)` composition and complete `.service(...)` replacement
  are public builder paths.
- Large replay history is shared, known API items are typed, unknown items are
  retained only at their genuinely dynamic boundary, and partial failures are
  never committed.

### 3. Owned library API and tools

- A private Tokio task drives sequential turns and owns all mutable state.
- Prompt acceptance and result waiting are separate; no join handle, explicit
  shutdown, result/event join, or caller-managed driver loop leaks into the
  common API.
- Follow-on turns reuse one response chain, WebSocket, cache key, history,
  code-mode runtime, and shell sessions.
- Custom tools use one registry whether defined as a full trait implementation
  or an inline `#[tool]` async function.
- The public examples cover minimal result-only use, event consumption,
  follow-on prompting, and custom tool registration.

## Active roadmap

### Phase 1: events and observability

This is the next production slice. Preserve the ownership and result API while
making the library straightforward to operate inside a long-lived application.

Outcomes:

1. Define stable tracing spans for agent session, turn, model call, Responses
   attempt, reconnect/backoff, and tool execution. Include IDs, durations,
   replay mode, error class, token usage, and cache usage; never include secrets
   or full prompt bodies.
2. Keep subscriber choice outside the library. The CLI may install a sensible
   stderr subscriber, while embedders can install OpenTelemetry, metrics, or
   their own tracing stack.
3. Keep contractual `AgentEvents` distinct from tracing. JSONL remains a lossless
   adapter encoding of typed events.
4. Evaluate event selection against concrete consumers: the CLI/Harbor adapter
   needs every contractual event, while a minimal embedder may need only final
   messages and lifecycle failures. Add public filtering or handlers only if a
   concrete consumer demonstrates that dropping the receiver is insufficient.
5. Add Tower-aware observability around `ResponsesAttempt` so logical model
   calls, attempts, retries, reconnects, stream duration, and backoff are not
   conflated.

Gate:

- Existing result-only and follow-on examples remain unchanged.
- JSONL remains contiguous with one terminal event per accepted prompt.
- Tracing writes no stdout and no secrets.
- Warnings-denied Clippy, workspace tests, public examples, a native CLI smoke,
  and representative retained-trace benchmarks pass.

### Phase 2: lifecycle control, steering, and branching

Do not expose this surface until Phase 1 is stable and a real multi-turn
consumer exists.

Desired semantics:

- Plain `prompt(...)` queues after the active turn and remains the default API.
- Steering targets the latest active turn by default. Advanced targeting may
  expose an opaque turn handle/ID without forcing IDs into ordinary prompting.
- A prompt may branch from a prior committed turn. Branch history should share
  an immutable prefix and allocate only its new tail.
- One WebSocket is sequential. Truly concurrent branches require independent
  response chains/connections; do not serialize nominally parallel agents
  through one socket or reconnect from scratch for every branch.
- Queue capacity and scheduling policy stay internal unless measured consumer
  pressure proves a public knob is necessary.
- Cancellation has an explicit terminal result and cleans up descendant tool
  processes.

Gate:

- Deterministic multi-turn tests cover queue order, steer-latest behavior,
  targeted rejection, cancellation, and branch isolation.
- A retained trace demonstrates that a follow-on turn sends only its delta and
  that a reconnect/branch replay preserves the stable cache prefix.
- The default one-prompt program does not gain lifecycle ceremony.

### Phase 3: bindings and richer consumers

The Ratatui client, PyO3 extension, and Node/browser WASM packages are promoted
embedded consumers of the same handle/turn/event contract:

- PyO3 owns one native Tokio runtime per constructed agent and releases the GIL
  while waiting for turn results or events.
- Node and web use one shared Rust/WASM model, history, cache, protocol, and
  Tower implementation. JavaScript owns only WebSocket/code-mode host
  capabilities and application-defined tools.
- Browser credentials/endpoints remain application policy. The SDK does not
  introduce an app server, relay, daemon, or JSON-RPC boundary.

The deterministic binding gate covers construction/error translation, one
persistent Node WebSocket across follow-on turns, incremental response IDs,
stable cache/session headers, custom JavaScript tools, unified events, and the
browser host contract. Full cancellation remains part of Phase 2 rather than a
binding-specific alternate lifecycle.

## Performance policy

- Optimize representative retained API/JSONL traces and real turns, not type
  aesthetics or isolated parser throughput.
- Preserve the stable prompt prefix, session cache key, `store: false`, and
  incremental `previous_response_id` path. Prompt caching is a primary runtime
  invariant.
- Known history remains typed. `RawValue` is appropriate for intentionally
  opaque retained payloads; `Value` belongs only at dynamic JSON/tool
  boundaries.
- Share immutable history and preallocate only where measured cardinality makes
  it useful. Do not add `SmallVec`, buffer pools, SIMD JSON, or custom allocators
  without a before/after retained-trace benchmark.
- Generic Tower dispatch is already negligible beside JSON, network, and model
  latency. Middleware should be chosen for correctness and operability first.
- Keep subprocess output bounded during production and preserve explicit
  process-group cancellation.

The detailed implemented transport invariants and current microbenchmarks live
in [`docs/RESPONSES_TOWER.md`](docs/RESPONSES_TOWER.md).

## Validation

For ordinary library changes:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo check --workspace --all-targets
```

Run `just run` when the public agent path changes. Use two or three focused,
fast Harbor tasks for model/tool behavior changes. Run the complete configured
`just eval` for a milestone, release, or cross-cutting lifecycle/transport
rewrite.

Latest full gate: `master@408eb96`, 41 Terminal-Bench tasks, 40/41 reward in 24
minutes 16 seconds. All 41 JSONL streams were contiguous and ended in one
`run.completed`; there were zero errored/retried Harbor trials, zero Responses
retries, and zero WebSocket reconnects. The run used 13,676,067 input tokens,
13,013,613 cached input tokens (95.16%), 128,800 output tokens, 633 model calls,
and 1,106 tool calls. The sole miss left a local verification binary beside the
required source file; it was not a runtime or transport failure. The retained
record is under
`.nanocodex/harbor/jobs/2026-07-18__20-37-28-eval-52286`.

Harbor results and ATIF are the eval record. Do not copy another append-only
experiment diary into this plan; use Git history and retained job paths for
past investigations.

## Codex parity checkpoint

The local upstream review is complete through
`openai/codex@35eaf3ffb0bf2001486c68c47a3d946b34d16634`. Nanocodex adopted the
272,000-token Sol context window and 244,800-token automatic compaction
threshold. Audio forwarding remains deferred until the supported model
advertises audio input. Review and classify every later upstream commit before
advancing this checkpoint.

## Deferred and out of scope

- Provider/model abstraction and backwards compatibility.
- A Nanocodex-owned app server, JSON-RPC protocol, or daemon.
- Additional language bindings without a concrete embedded consumer.
- Browser/computer-use runtimes until a deterministic eval and consumer justify
  the capability.
- Skills/plugins, approval machinery, alternate runtime modes, or duplicate
  shell implementations.
- JJ provenance, graders, human-review state, durable replay journals, and local
  multi-agent scheduling until promoted by a concrete product slice.
- Broad event buses, collector traits, shared mutable run state, and generic
  provider/client layers without a current consumer.
