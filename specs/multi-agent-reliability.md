# Make multi-agent orchestration cancellation-safe and deadlock-free

- Branch: `multi-agent-reliability` (proposed; spec authored on `master`)
- Status: Draft
- Owner(s): Eric Juta / implementing agent
- Created: 2026-07-22
- Last Updated: 2026-07-22
- Links: [Active Plan](../PLAN.md) | [Multi-agent comparison](../docs/CODEX_MULTI_AGENT_V2_COMPARISON.md)

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`,
`Decision Log`, and `Outcomes & Retrospective` current as research,
implementation, validation, and review proceed. When the next milestone is
clear, continue to it and update this spec instead of asking for generic next
steps.

## Purpose / Big Picture

Nanocodex's core `spawn()` and checkpoint `fork()` primitives work on their
covered happy paths, but the CLI's application-owned child-agent composition can
currently hang forever or leave work running after its parent is cancelled. Two
core conversation-boundary races can also give a fork stale context or make a
follow-up omit history after a failed model continuation.

After this change, the opt-in `spawn_agent`, `fork_agent`, and
`prompt_agent` tools remain a thin application adapter over the owned session
API, but they have explicit invocation ownership. Cancelling a parent stops its
active descendant work, self-waits and multi-child wait cycles fail promptly,
and CLI shutdown drains every child it accepted. A fork requested at a safe
model/tool boundary receives that boundary even while compaction is running.
After an exhausted or non-retryable model failure, the next turn performs one
complete client-owned replay instead of silently dropping the pending suffix.

This work does not add a core scheduler, provider abstraction, durable child
registry, app-server protocol, approval system, capability sandbox, or
MultiAgentsV2 compatibility layer. Depth, token, rollout, and residency budgets
remain application policy unless a separate product slice promotes them.

Success means:

- Cancelling or dropping a parent child-tool invocation causes its accepted
  child turn and recursively started descendants to reach cancellation before
  shutdown completes.
- A child attempting to wait on itself, or to close a wait cycle such as
  A -> B -> A, receives a deterministic tool failure without queueing the
  impossible follow-up.
- A fork taken while compaction is blocked includes the last completed
  response/tool-output pair and excludes partial or unmatched work.
- The first prompt after a failed continuation sends a full authoritative replay
  with no `previous_response_id`, including every completed response and paired
  tool output exactly once.
- Deterministic regression tests cover these behaviors; public examples compile,
  `just check` passes, and a live native subagent smoke is recorded when
  credentials are available.
- Child event streams remain independent and optional; no merged event protocol
  is introduced.

## Progress

- [x] (2026-07-22 11:23Z) Audit current core forks, CLI child orchestration,
  examples, shutdown paths, and existing tests.
- [x] (2026-07-22 11:23Z) Record the initial implementation and validation
  approach in this ExecPlan.
- [ ] Add deterministic failing tests for child cancellation, wait cycles,
  shutdown, compaction-time forks, and failed-turn recovery.
- [ ] Implement cancellation-safe child invocation ownership and wait-cycle
  rejection in the CLI adapter.
- [ ] Repair safe-boundary publication around compaction and failed-turn replay
  in the core model lifecycle.
- [ ] Correct the read-only wording and make the public subagent example capable
  of its advertised repository investigation.
- [ ] Run focused and workspace validation, then record exact evidence and
  residual risks.
- [ ] Update the PR and this spec with the final outcome.

## Surprises & Discoveries

- Observation: Existing automated coverage is green but does not exercise the
  risky CLI contracts.
  Evidence: `cargo test -p nanocodex --lib -- --test-threads=1` passed 60
  tests; `cargo test -p nanocodex-bin --tests` passed 118 CLI unit tests and
  one MCP integration test. `bin/nanocodex/src/subagents.rs` has no focused
  test module. The attached-subagent observability test is ignored and invokes
  only `spawn_agent`.

- Observation: Dropping a `Turn` intentionally does not cancel it, and a
  driver whose command channel closes waits for its active execution.
  Evidence: `crates/nanocodex/src/agent.rs` documents this at the `Turn`
  type and awaits the active execution when `commands_open` becomes false.
  Therefore ordinary Rust drop semantics cannot provide child cleanup.

- Observation: The CLI registers a child only after its first result.
  Evidence: `ChildAgent::execute` in
  `bin/nanocodex/src/subagents.rs` starts event draining, awaits
  `child.prompt(...).result()`, and calls `ChildAgents::insert` afterward.
  A cancelled parent can lose the only controllable child handle before the
  registry sees it.

- Observation: `prompt_agent` can target any registry ID and waits
  synchronously while every descendant receives the same recursive tools.
  Evidence: `PromptAgent::execute` performs `get(agent_id)`, then awaits a
  new turn without checking the caller session or active wait graph. A
  self-follow-up queues behind the turn that is waiting for it.

- Observation: Child contractual events are intentionally separate from root
  JSONL.
  Evidence: `PLAN.md` describes host-side event multiplexing as optional.
  This is not a defect and must not be changed implicitly by this branch.

- Observation: "Read-only" is currently instruction policy, not a capability
  boundary.
  Evidence: `bin/nanocodex/src/config.rs` preserves default tools for every
  child, and `nanocodex-tools` default workspace handlers include shell and
  patch operations.

- Observation: The public example disables every default tool while its default
  goal asks agents to investigate the repository.
  Evidence: `examples/subagents.rs` uses `.without_defaults()` and registers
  only the three child-agent tools.

## Decision Log

- Decision: Keep child orchestration in `bin/nanocodex`; do not promote a
  scheduler or child graph into the `nanocodex` crate.
  Rationale: The product contract explicitly keeps application-defined
  subagents as a thin consumer. The core already exposes the required owned
  `spawn()`, `fork()`, `prompt()`, and `TurnControl` primitives.
  Date/Author: 2026-07-22 / Codex

- Decision: Register child sessions before awaiting their first turn and model
  every in-flight child turn with an RAII invocation guard.
  Rationale: Accepted work must become reachable by shutdown before any
  cancellation point. The guard supplies cleanup when a tool future is dropped,
  while registry ownership allows normal reusable follow-ups after success.
  Date/Author: 2026-07-22 / Codex

- Decision: Track active wait edges by caller session ID and target child ID,
  and reject an edge that would create a directed cycle.
  Rationale: A self-only check misses A -> B -> A and longer generated
  topologies. `ToolContext::session_id` already identifies the invoking
  driver, so no public turn or transport ID needs to be exposed.
  Date/Author: 2026-07-22 / Codex

- Decision: On a terminal model-run error, preserve authoritative client history
  and force the next turn through one full replay.
  Rationale: Completed response/tool pairs may have already produced side
  effects and cannot be discarded. Clearing the suffix while retaining its
  server parent is inconsistent; full replay is slower once but unambiguous and
  uses the existing recovery path.
  Date/Author: 2026-07-22 / Codex

- Decision: Publish a safe snapshot immediately before any compaction await.
  Rationale: Compaction is a transport/context optimization. It must not delay
  visibility of an already complete response/tool-output boundary.
  Date/Author: 2026-07-22 / Codex

- Decision: Describe child read-only behavior as instruction-based and
  non-sandboxed in this slice.
  Rationale: A true capability boundary would need to constrain shell and every
  dynamic tool, not merely remove patch handlers. That is a separate security
  design; this branch should make no false guarantee.
  Date/Author: 2026-07-22 / Codex

## Outcomes & Retrospective

- Outcome: The audit and remediation design are complete; implementation has
  not started.
  Evidence: The current automated baseline is 60 passing library tests, 118
  passing CLI unit tests, one passing MCP CLI integration test, and successful
  compilation of all public example binaries.
  Remaining: All implementation milestones and acceptance gates below.

## Context and Orientation

Read the root `AGENTS.md` and `PLAN.md` before changing files. Nanocodex is a
headless Rust SDK. A private driver owns one agent's mutable conversation,
WebSocket, tool runtime, and sequential prompt queue. `Nanocodex` is the cheap
command handle. `Turn` is an independently awaitable result handle, and
`TurnControl` can steer or cancel that exact accepted turn.

A clean child comes from `AgentHandle::spawn()`; it receives fresh
conversation and cache lineage while reusing private configuration. A contextual
child comes from `AgentHandle::fork()`; it receives a new driver, WebSocket,
and tool runtime at the invoking agent's latest safe checkpoint.
`Nanocodex::fork_from(&TurnResult)` creates an exact historical branch.

The opt-in CLI composition lives in
`bin/nanocodex/src/subagents.rs`. `ChildAgents` maps model-visible numeric
IDs to retained `Nanocodex` handles and event-drain tasks. `ChildAgent`
implements both `spawn_agent` and `fork_agent`; `PromptAgent` implements
reusable follow-ups. `bin/nanocodex/src/config.rs` installs those tools through
a per-driver tool factory so recursive children receive handlers bound to their
own weak `AgentHandle`. The headless adapter calls registry shutdown in
`bin/nanocodex/src/run.rs`; the TUI currently retains the registry in
`bin/nanocodex/src/tui/mod.rs` but does not explicitly shut it down.

Core model state is in `crates/nanocodex/src/model/agent.rs`.
`ConversationState` stores immutable committed segments, a current delta, and
an optional server `previous_response_id`. `ModelRun::drive_session`
publishes fork snapshots before model calls, appends only completed response
items, executes completed tool calls, appends paired outputs, and may compact.
The driver in `crates/nanocodex/src/agent.rs` remains responsive to fork
commands while that model future awaits.

Current failure modes:

- Parent cancellation can drop `ChildAgent::execute` before registry insertion.
  The child driver then finishes independently and cannot be cancelled by
  `ChildAgents::shutdown`.
- A child can call `prompt_agent` with its own ID. Prompt acceptance succeeds,
  but its result is queued behind the current turn, which waits forever.
- Completed tool outputs are appended before compaction, but no new snapshot is
  visible until compaction returns.
- An exhausted/non-retryable continuation failure retains the session. The next
  prompt clears its delta, potentially omitting a tool output while retaining
  the response ID whose checkpoint contains the matching call.

Assumptions:

- The implementation runs inside the Tokio runtime already required by
  `Nanocodex::build()`; no alternate runtime mode is added.
- Child IDs remain process-local and model-visible. Transport response IDs,
  internal turn IDs, and raw checkpoints remain private.
- Deterministic Tower/mock Responses services can block and release child model
  attempts without network credentials, following existing agent-test patterns.
- If an RAII cleanup task cannot be made trackable and awaitable during runtime
  teardown, stop at D1 and revise the lifecycle design. Do not accept
  fire-and-forget cancellation as complete.

## Execution DAG

    D0 Freeze contracts and add deterministic failing tests
     |
     +--> D1 CLI child ownership, cancellation, and wait graph --+
     |                                                           |
     +--> D2 Core failed-turn replay and compaction snapshots ----+--> D4 Integration gate
     |                                                           |       |
     +--> D3 Honest capability wording and working example -------+   acceptance passes?
                                                                         /          \
                                                                       no            yes
                                                                       |              |
                                                           revise failing lane      D5 Live smoke
                                                                       |              |
                                                                       +-------> D4 <-+

D1, D2, and D3 may proceed in parallel after D0 because they own the CLI
adapter, core model lifecycle, and docs/example surface respectively. D4 does
not pass until every deterministic regression is green. D5 is required when
credentials are available; absence of credentials must be recorded, not hidden
as a pass.

## Plan of Work

### Milestone 1: Freeze contracts with failing regression tests

Scope: Add deterministic tests before changing behavior. Each test must fail for
the audited reason on the current implementation and pass only after its owning
fix. Do not weaken existing verifiers or rely on timing-only sleeps.

D0 also freezes the cancellation handoff before production edits: an invocation
guard's `Drop` synchronously sends one cleanup request through a
`tokio::sync::mpsc::UnboundedSender` to a single registry-owned cleanup worker.
That tracked worker owns the asynchronous `TurnControl::cancel()` call and is
awaited by shutdown. Registry state retains a control clone so shutdown can
cancel work even before a guard drops. This uses the existing public API and
adds no fire-and-forget task. If a failing test proves this handoff cannot close
the race, stop D0 and revise this spec before changing public interfaces.

Files and interfaces:

- `bin/nanocodex/src/subagents.rs`: add focused private unit tests and only the
  test support needed to instantiate the real adapter.
- `crates/nanocodex/src/model/agent_tests.rs`: add model-boundary and recovery
  regressions using deterministic mock Responses services.
- `bin/nanocodex/tests/observability_stress.rs`: extend only if a process-level
  shutdown assertion cannot be expressed in the unit module; keep the new
  correctness test non-ignored and independent of Jaeger.

Work:

Add these named behaviors:

- `cancelled_spawn_cancels_unreturned_child_and_shutdown_drains_it`: block a
  first child turn, drop or abort the parent tool invocation, and prove the
  child's active attempt is cancelled and registry shutdown completes within a
  bounded timeout.
- `cancelled_parent_cancels_child_and_grandchild`: block a child that has started
  a grandchild, cancel the parent invocation, and prove both descendant attempts
  terminate before shutdown returns.
- `shutdown_is_idempotent_rejects_late_insert_and_cancels_queued_turns`: call
  shutdown twice, race a new child insertion against the first call, and prove
  no late session or queued turn survives.
- `prompt_agent_rejects_self_wait_before_queueing`: create a retained child,
  invoke `prompt_agent` from that child's session ID, and expect a clear tool
  error without a queued second turn.
- `prompt_agent_rejects_multi_child_wait_cycle`: create A -> B, attempt B -> A,
  and expect rejection while acyclic fan-out still succeeds.
- `tui_restores_terminal_before_awaiting_child_shutdown`: exercise an extracted
  TUI lifecycle wrapper with a blocking shutdown future and prove terminal
  restoration occurs first on success and error.
- `fork_during_compaction_inherits_completed_tool_boundary`: stall the
  compaction response after a paired tool result, fork concurrently, and assert
  the child request contains the pair through checkpoint delta or exact replay.
- `fork_during_tool_free_compaction_inherits_completed_response`: stall
  compaction after an `end_turn == false` response without tools and prove a
  concurrent fork includes that completed response.
- `failed_continuation_replays_complete_safe_history_on_next_turn`: fail a
  continuation after a completed tool call/output, submit a new prompt, and
  assert `previous_response_id` is absent and the full local history includes
  the paired output exactly once.
- `subagent_example_exposes_workspace_and_agent_tools`: inspect the example's
  captured request/tool definitions and require workspace inspection plus
  `spawn_agent`, `fork_agent`, and `prompt_agent`.

Acceptance:

- Run `cargo test -p nanocodex-bin subagents -- --nocapture`; the new adapter
  tests should initially expose the lifecycle and cycle failures.
- Run `cargo test -p nanocodex fork_during_compaction -- --nocapture` and
  `cargo test -p nanocodex failed_continuation -- --nocapture`; both should
  initially expose the audited core behavior.
- Record the exact pre-fix failure modes in `Surprises & Discoveries`.

### Milestone 2: Own child invocation lifecycle and reject wait cycles

Scope: Make the application-owned CLI registry the sole owner of every accepted
child invocation from creation through completion or cancellation. Preserve
reusable successful children and recursive orchestration without adding a core
scheduler.

Files and interfaces:

- `bin/nanocodex/src/subagents.rs`: extend `ChildSession`,
  `ChildAgents`, `ChildAgent::execute`, and `PromptAgent::execute`.
- `bin/nanocodex/src/run.rs`: retain headless shutdown and make its ordering
  explicit.
- `bin/nanocodex/src/tui/mod.rs`: replace passive `_child_agents` retention
  with explicit shutdown after the UI loop on success and error.

Work:

Give each retained child session its model-visible ID, native session ID,
parent session ID, `Nanocodex` handle, event task, and the controls for every
active or queued turn. Capture the invoking session from
`ToolContext::session_id`. Insert a newly spawned/forked child into the
registry before awaiting its first turn.

Create a private invocation guard for each accepted child turn. On normal
completion it removes only its active control and wait edge. Its `Drop` performs
the synchronous D0 handoff to the registry-owned cleanup worker; the worker
cancels that exact `TurnControl`, removes the active state, and drains any
initial child that never returned an ID. The worker task and every request it
accepts remain registry-owned and are awaited by shutdown.

A failed initial turn that is already terminal is removed and drained without
calling `cancel()` again. A dropped or still-active initial turn is cancelled
before removal. Failed follow-ups leave the prior successful child reusable only
after their exact turn has reached a terminal state.

Make `ChildAgents::shutdown` idempotent and fail closed: stop new insertions,
cancel all active and queued controls, recursively drain cleanup tasks, drop
agent command handles, and await every event task. It must not hold a registry
mutex across an await. Rework the TUI event loop so all `Ok` and error exits
flow through this shutdown before returning.

Maintain a private directed wait graph with one edge per active invocation from
caller session ID to target child ID. Before inserting an edge, resolve child IDs
to native session IDs and reject it if the target can already reach the caller.
A dedicated guard removes the edge on success, error, cancellation, and future
drop. Support multiple concurrent acyclic edges from one caller so
`Promise.all` fan-out remains valid. Return a concise model-visible error such
as "prompt_agent would create a child wait cycle"; do not expose internal turn
or transport IDs.

Do not add a new dependency unless the standard library and Tokio primitives
already in the workspace cannot express tracked cleanup. Do not solve this with
an untracked `tokio::spawn`.

Acceptance:

- All cancellation, recursive-descendant, cycle, late-insert, queued-turn, and
  idempotent-shutdown regressions from Milestone 1 pass repeatedly.
- Existing parallel child tracing still shows overlapping sibling turns.
- Two independent follow-ups can run concurrently; two follow-ups to one child
  retain driver-owned queue order without a registry lock spanning either turn.
- Headless and TUI shutdown complete within the test timeout with no live child
  service attempts, cleanup workers, or event-drain tasks.
- The TUI lifecycle test proves terminal restoration precedes any potentially
  blocking child shutdown on both success and error.

### Milestone 3: Repair core safe-boundary and failed-turn recovery semantics

Scope: Ensure compaction never hides a completed safe boundary and ordinary
model failure cannot leave the next incremental request inconsistent.

Files and interfaces:

- `crates/nanocodex/src/model/agent.rs`: adjust snapshot publication and the
  error path around `drive_session`.
- `crates/nanocodex/src/model/agent_tests.rs`: retain the new deterministic
  regressions beside existing fork, compaction, retry, and cancellation tests.

Work:

After a completed response and all of its tool outputs are appended, publish a
fork snapshot before awaiting `maybe_compact`. Apply the same rule to the
tool-free `end_turn == false` continuation path. The pre-compaction snapshot
must contain only complete response/tool pairs. A successful compaction may
publish a newer replacement snapshot on the next safe loop boundary.

On `drive_session` error, retain only data already admitted by the normal
completed-response path, reset the conversation for a full request, and mark
that full delta to survive the next prompt's normal `clear_delta` step. Use a
purpose-named private flag or helper rather than extending
`preserve_inherited_delta` with undocumented double meaning. The first later
turn must send complete authoritative typed history with no
`previous_response_id`; healthy turns after that return to incremental deltas.

Do not execute or append partial streamed model output. Do not duplicate a tool
side effect in recovery. Do not change the typed retry owner in
`nanocodex-service`; retries within one attempt continue using the existing
full-replay policy.

Acceptance:

- All three Milestone 1 core regressions pass, including both compaction paths.
- Existing active-boundary, checkpoint-miss, reconnect, compaction, cancellation,
  and follow-on tests remain green.
- Captured requests prove exactly one recovery full replay followed by ordinary
  incremental requests.
- Run `cargo bench -p nanocodex-core --bench fork_history --
  active_boundary_snapshot_then_append` and record Criterion medians for 100,
  1,000, and 10,000 items. The 10,000-item `immutable_boundary` median must be
  no more than twice the 100-item median; otherwise stop and investigate
  retained-history cloning before acceptance.

### Milestone 4: Make capability language honest and repair the example

Scope: Remove the false implication of enforced read-only capability while
preserving the intended specialist policy, and give the public example the
tools needed for its default task.

Files and interfaces:

- `bin/nanocodex/src/subagents.rs`: update tool descriptions and delegated
  prompts.
- `examples/subagents.rs`: enable the normal inspection-capable tools while
  retaining the three agent-relative tools.
- `examples/README.md` and
  `docs/CODEX_MULTI_AGENT_V2_COMPARISON.md`: document instruction-based
  read-only policy, lifecycle guarantees delivered by this branch, and remaining
  application-defined budgets.

Work:

Say that children are instructed to operate read-only and that this is not a
sandbox or security boundary. Do not call a child simply "read-only" where a
caller could infer capability enforcement. Keep the no-destructive-command
prompt.

Remove `.without_defaults()` from the public subagent example, or construct the
equivalent default tool set, so its root and children can inspect the workspace.
The per-driver factory must still rebind `spawn_agent` and `fork_agent` to
the driver that invokes them. Do not add a special read-only shell in this
branch.

Acceptance:

- `cargo check -p nanocodex-examples --bins` succeeds.
- `cargo test -p nanocodex-examples --bin subagents
  subagent_example_exposes_workspace_and_agent_tools -- --nocapture` passes and
  proves the captured tool definitions include repository inspection plus all
  three child tools.
- Documentation explicitly distinguishes instruction policy from capability
  enforcement.

### Milestone 5: Integrate, validate, and record the gate

Scope: Run the full repository gate after focused tests are stable. Inspect
contractual events and traces for the child cancellation scenario; do not infer
success solely from exit status.

Files and interfaces:

- `specs/multi-agent-reliability.md`: update progress, discoveries, outcomes,
  exact commands, and residual risks.
- Changelogs for each affected published crate or binary, following existing
  repository release practice.

Work:

Run formatting, warnings-denied Clippy, workspace tests, public-example checks,
and the repository `just check` gate. Run the attached-subagent observability
stress when its Jaeger dependency is available and inspect the exported trace.
Perform a native live smoke with one clean child, one contextual fork, one
follow-up, and concurrent fan-out. Cancel one parent operation and verify no
child activity continues afterward.

Acceptance:

- Every command under `Validation and Acceptance` passes.
- Exactly one terminal result/event remains associated with every accepted
  prompt.
- Child cancellation and cycle rejection are visible as ordered tool/lifecycle
  evidence without stdout protocol corruption.
- The final spec names any unavailable external gate explicitly.

## Interfaces and Dependencies

Local interfaces:

- `nanocodex::AgentHandle::spawn` and `AgentHandle::fork`:
  - Inputs: a weak driver-relative capability materialized by the per-agent tool
    factory.
  - Outputs: a fresh `(Nanocodex, AgentEvents)` pair.
  - Failures: typed agent-stopped, no-safe-fork-boundary, tool-factory, or
    service-construction errors.

- `nanocodex::Nanocodex::prompt` and `TurnControl::cancel`:
  - Inputs: a non-empty prompt and an exact accepted-turn capability.
  - Outputs: command acceptance followed by an independent typed result;
    cancellation waits for active resources to stop.
  - Failures: stopped driver, non-cancellable terminal target, or normal turn
    failure.

- `nanocodex_tools::ToolContext::session_id`:
  - Inputs: supplied by the normal tool runtime for each call.
  - Outputs: stable native session identity for wait-graph caller attribution.
  - Failures: none; it is borrowed and must be copied into owned registry state
    before awaiting.

- Private `ChildAgents` lifecycle state:
  - Inputs: child creation, accepted turn controls, wait edges, completion,
    future drop, and process shutdown.
  - Outputs: reusable completed children plus a fully drainable set of active
    work.
  - Failures: unknown child, cycle rejection, shutting-down registry, or child
    turn failure. No poisoned/closed state may silently fall back.

- `ConversationState` recovery:
  - Inputs: complete typed history, current delta, and response checkpoint.
  - Outputs: either a healthy incremental request or one explicit full replay
    after failure.
  - Failures: malformed completed response state remains typed and terminal.

External dependencies:

- OpenAI Responses WebSocket API:
  - Version/source checked: existing typed wire contract and service stack in
    `nanocodex-core` and `nanocodex-service`; no web documentation lookup is
    required for this branch.
  - Expected behavior: a healthy request may use a stored response checkpoint;
    replay without that checkpoint sends complete client-owned history.
  - Failure handling: existing bounded service retries remain authoritative;
    exhausted failures arm one next-turn full replay.

- Tokio:
  - Version/source checked: workspace lockfile.
  - Expected behavior: bounded channels, task cancellation by future drop, and
    join handles are used within the already-required runtime.
  - Failure handling: all spawned cleanup must be registry-tracked and awaited;
    runtime teardown is not an acceptable cleanup mechanism.

No new external dependency is planned.

## Concrete Steps

From the repository root
(`/home/ericjuta/.openclaw/workspace/repos/nanocodex`):

    rg -n "ChildAgents|ChildAgent|PromptAgent|publish_fork_snapshot|maybe_compact|clear_delta" \
      bin/nanocodex/src crates/nanocodex/src examples
    cargo test -p nanocodex-bin subagents -- --nocapture
    cargo test -p nanocodex fork_during_compaction -- --nocapture
    cargo test -p nanocodex failed_continuation -- --nocapture
    cargo test -p nanocodex-examples --bin subagents \
      subagent_example_exposes_workspace_and_agent_tools -- --nocapture
    cargo bench -p nanocodex-core --bench fork_history -- \
      active_boundary_snapshot_then_append
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    cargo check -p nanocodex-examples --bins
    just bootstrap
    just check

When credentials are available, run:

    cargo run --quiet --manifest-path bin/nanocodex/Cargo.toml -- \
      run --thinking low --subagents true \
      "Spawn one clean child and one fork concurrently, follow up with the clean child, synthesize their reports, and do not modify files."

Expected evidence:

    test ...cancelled_spawn_cancels_unreturned_child_and_shutdown_drains_it ... ok
    test ...cancelled_parent_cancels_child_and_grandchild ... ok
    test ...shutdown_is_idempotent_rejects_late_insert_and_cancels_queued_turns ... ok
    test ...prompt_agent_rejects_self_wait_before_queueing ... ok
    test ...prompt_agent_rejects_multi_child_wait_cycle ... ok
    test ...tui_restores_terminal_before_awaiting_child_shutdown ... ok
    test ...fork_during_compaction_inherits_completed_tool_boundary ... ok
    test ...fork_during_tool_free_compaction_inherits_completed_response ... ok
    test ...failed_continuation_replays_complete_safe_history_on_next_turn ... ok
    test ...subagent_example_exposes_workspace_and_agent_tools ... ok

## Validation and Acceptance

Automated validation:

- `cargo test -p nanocodex-bin subagents -- --nocapture`: all focused adapter
  lifecycle, cycle, and shutdown regressions pass.
- `cargo test -p nanocodex --lib -- --test-threads=1`: all core tests pass,
  including compaction-time fork and failed-continuation recovery.
- `cargo test -p nanocodex-bin --tests`: CLI/TUI/MCP regressions pass; the
  expected ignored OTLP tests are reported separately.
- `cargo check -p nanocodex-examples --bins`: every public Rust example
  compiles.
- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`:
  no formatting or lint failures.
- `just check`: full configured repository gate passes without modifying eval
  tasks or verifiers.

Manual or runtime validation:

- Start the headless happy-path command under `Concrete Steps` with normal
  credentials and observe concurrent clean/fork reports plus a reusable
  follow-up.
- Separately start the interactive TUI from the repository root:

      cargo run --quiet --manifest-path bin/nanocodex/Cargo.toml -- \
        --thinking low --subagents true \
        --prompt "Start a child that performs a long read-only inspection and report when it finishes."

- While the child is active, use the TUI's cancel action for the root turn, then
  exit normally.
- Observe terminal restoration, terminal cancellation evidence, and no later
  child model/tool activity. Stdout remains flushed root JSONL only in headless
  mode; diagnostics and optional child lifecycle JSONL remain on stderr.
- If live credentials are unavailable, record that D5 was not run. The
  deterministic process-level recursive cancellation test remains mandatory and
  is the acceptance authority for cleanup.

Regression checks:

- Healthy follow-ons reuse their WebSocket, response chain, typed history, shell
  sessions, and cache identity.
- Forks still receive fresh drivers, service stacks, WebSockets, and per-driver
  tool handlers.
- Active snapshots continue excluding partial streamed output and unmatched tool
  calls.
- `Promise.all` fan-out to independent children remains concurrent.
- Unknown child IDs remain clear tool failures.
- Child events remain optional independent streams, not a new merged bus.

## Idempotence and Recovery

All tests and build commands are repeatable and use deterministic mock services
or existing repository fixtures. No schema migration, external data mutation,
or generated source is required.

- Re-running focused tests is safe because they must create isolated temporary
  workspaces and close mock services.
- If lifecycle implementation fails halfway, keep the failing regression tests,
  revert only the incomplete production edits, and resume from D1. Do not remove
  timeouts or weaken assertions to make shutdown appear successful.
- If full-replay recovery duplicates a tool output, stop at D2 and inspect the
  exact captured request JSON before changing history segmentation.
- If TUI cleanup risks skipping terminal restoration, preserve
  `TerminalSession` RAII and move child shutdown outside the UI loop rather
  than adding early-return cleanup calls. The
  `tui_restores_terminal_before_awaiting_child_shutdown` regression must fail
  until restoration ordering is explicit.
- Backout plan: revert the production changes and associated changelog entries
  as one branch while retaining this spec and audit evidence. There is no
  persistent data rollback.

## Rollout and Operations

- Feature flags/config/env vars: child tools remain opt-in through
  `--subagents` / `NANOCODEX_SUBAGENTS`; defaults do not change.
- Migration/backfill steps: none.
- Monitoring/alerts/logs: inspect child run start/terminal events, cancellation
  latency, active child/control counts at shutdown, tool failure text for cycle
  rejection, and trace parentage. Do not add a global metrics collector solely
  for this branch.
- PR/branch workflow: implement on `multi-agent-reliability`, keep this spec
  updated at each milestone, include focused test evidence in review, and run
  the full configured eval only if maintainers classify this as a milestone or
  release gate.

## Risks and Open Questions

- Risk: Drop-triggered cleanup could become fire-and-forget and race process
  teardown.
  Mitigation: register children before awaiting, track every cleanup task in
  `ChildAgents`, and make shutdown await them. The cancellation regression
  must assert no live mock attempt remains.

- Risk: A wait graph could reject valid concurrent fan-out or retain stale edges.
  Mitigation: use one guarded edge per invocation, support multiple outgoing
  edges, remove edges on every terminal/drop path, and test acyclic diamonds as
  well as cycles.

- Risk: Cancelling a failed follow-up could accidentally destroy a reusable
  child's previously committed session.
  Mitigation: cancel the exact `TurnControl`; remove the child only when its
  initial turn never returned an ID or shutdown owns the entire registry.

- Risk: Publishing before compaction could expose a checkpoint with a large
  pre-compaction history.
  Mitigation: this is the authoritative safe state already required for replay;
  compaction may replace it later. Retain segmented-history memory assertions.

- Risk: Full replay after failure may increase one request's bytes and latency.
  Mitigation: limit it to the first next turn, assert subsequent incremental
  behavior, and prefer correctness over a broken checkpoint chain.

- Risk: Enabling default tools in the example also enables mutation-capable
  tools.
  Mitigation: explicitly document instruction-only read-only policy. A true
  sandbox remains out of scope.

- Risk: The frozen Drop-to-cleanup handoff may reveal that an application cannot
  guarantee cancellation with the existing async-only `TurnControl`.
  Mitigation: D0 fixes a synchronous channel handoff to one tracked cleanup
  worker and tests it before production edits. If that design fails, stop and
  revise this ExecPlan; do not improvise a public non-blocking cancellation API
  during D1.

- Open question: Should depth, concurrent-child, or residency limits be added to
  the bundled CLI after lifecycle correctness?
  Owner/next step: product owner. Keep separate from this fix unless a concrete
  consumer and acceptance budget are approved.

## Artifacts and Notes

Baseline audit evidence from 2026-07-22:

    cargo test -p nanocodex --lib -- --test-threads=1
    test result: ok. 60 passed; 0 failed

    cargo test -p nanocodex-bin --tests
    main unit tests: 118 passed
    mcp_cli: 1 passed
    observability_stress: 2 ignored manual tests

    cargo check -p nanocodex-examples --bins
    Finished dev profile successfully

The workspace was clean after the audit. The vendored source template is
`specs/_template.md`, copied byte-for-byte from
`../perps-iii/specs/_template.md` with SHA-256
`cd521472930ac4eee3f7f5449e761ea8e6391fb6c336e4191c0119a89cc7bb17`.

## Revision Notes

- 2026-07-22: Created from the vendored critical-change template using the
  multi-agent audit findings. Proposed staged, deterministic fixes without
  promoting application orchestration into the core SDK.
- 2026-07-22: Tightened the plan after independent review by freezing the
  synchronous Drop-to-cleanup handoff, adding recursive/queued/shutdown and
  tool-free-compaction regressions, specifying the history benchmark gate,
  making example validation executable, and separating TUI cancellation from
  the headless happy-path smoke.
