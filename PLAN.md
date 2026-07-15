# Harbor-first OpenAI harness

## Goal

Build a thin Rust coding harness for the current best OpenAI model and API,
without a TUI, provider abstraction, approval system, or backwards compatibility.
The public process surface is JSONL on stdin/stdout. Harbor owns benchmark task
isolation, verification, result storage, and ATIF.

## Architecture

```text
development                       evaluation

task.start                        Harbor job YAML
    |                                  |
cargo run                         task container
    |                                  |
JSONL stdout                      upload Rust binary
                                       |
                                  run in /app
                                   |       |
                                OpenAI   tools
                                       |
                                    verifier
```

The Python `BaseInstalledAgent` integration is only a lifecycle adapter:
upload one static executable, run it headlessly, retain JSONL, and derive ATIF.
Rust performs API calls and tools directly inside the task container.

The governing runtime constraint is hosted-first. OpenAI owns model reasoning,
the Programmatic Tool Calling JavaScript runtime, root/subagent orchestration,
stored response state, prompt caching, and compaction. Rust owns the narrow
capabilities that must touch the Harbor task container: JSONL, local shell
execution, bounded process cleanup, and API-visible measurements. Do not grow a
local agent scheduler, transcript manager, compactor, or second eval record.

Local artifacts are built by a native-architecture Linux BuildKit container.
Cargo `dev` is the default; `HARNESS_BUILD_PROFILE=profiling` selects an
optimized profile with full symbols. Hosted jobs will eventually fetch a
versioned, digest-verified artifact instead of requiring the source tree.

## JSONL contract

Every event has this envelope:

```json
{"protocol_version":1,"request_id":"...","seq":1,"type":"...","payload":{}}
```

Initial input is `task.start`. Output includes `run.started`, model events,
`tool.call`, `tool.result`, assistant messages, metrics, and exactly one
`run.completed` or `run.failed`. Stdout is flushed JSONL; diagnostics are stderr.
Raw streams are authoritative and ATIF is derived from them.

## Milestone 0: installed-agent eval baseline

Status: complete.

- Clap `run` command and native `just run`.
- Cached native Linux artifact build without rebuilding task images.
- Harbor-native YAML selecting Terminal-Bench `fix-git`.
- Thin InstalledAgent upload/run adapter with no tool bridge.
- Rust shell call and canonical assertions producing reward `1`.
- Raw input/events/stderr plus valid ATIF retained per trial.
- Content-addressed native eval image with verifier dependencies baked once.

Measured on the development machine:

- native `just run`: about 0.27 seconds warm;
- real source-edit artifact rebuilds: about 2 seconds steady state;
- Harbor environment startup: about 1.2 seconds warm;
- Harbor agent upload/setup: about 0.4 seconds;
- Rust positive-control execution: about 0.1 seconds;
- unchanged canonical assertions: about 0.9 seconds;
- full Harbor trial: about 3.6 seconds warm;
- full `just eval`, including the cached agent build: about 6.7 seconds warm.

The first run after task, platform, or eval-image changes also builds the
content-addressed native image. Keep that cold setup cost separate from warm
source-edit measurements.

## Milestone 1: OpenAI execution

Status: first real model/tool vertical slice complete.

### OpenAI runtime contract

1. Target `gpt-5.6-sol` directly through the Responses API. Do not add a
   provider interface, alternate model path, HTTP/SSE fallback, or legacy wire
   compatibility.
2. Keep one persistent Responses WebSocket connection. Streaming is implicit;
   do not send the HTTP `stream` field. Warm stable request state with
   `generate: false`, continue incrementally with `previous_response_id`, and
   preserve every raw inbound and outbound API event.
3. Expose the Responses native local `shell` tool exclusively through hosted
   Programmatic Tool Calling with `allowed_callers: ["programmatic"]`. Rust
   executes `shell_call` actions and returns typed `shell_call_output` items.
   Do not expose a direct-call fallback, imitate Codex's internal
   `exec_command` function schema, or run generated JavaScript locally.
4. Treat one generated JavaScript program as a bounded mechanical phase. Use
   `Promise.all` for independent reads, sequence dependent work and mutations,
   reduce intermediate results in hosted JavaScript, retry transient work at
   most once, and return to the model only for semantic judgment. Preserve
   every `program`, `program_output`, `call_id`, and `caller` relationship.
5. A completed response is not a completed task until the root emits a final
   assistant message. A response containing only program or tool work
   continues from its response ID.
6. Default to hosted state with `store: true` and `previous_response_id` so a
   reconnect can rehydrate the response chain. Do not maintain or replay a
   parallel local transcript. A later explicit ZDR mode may use `store: false`
   and complete encrypted-item replay, but it must not complicate the default.
7. Enable server-side compaction through `context_management` on every
   generated response. Preserve opaque compaction items in API order; never
   interpret, reorder, or replace them with a local natural-language summary.
   Seed the quality-first profile near 350K tokens and evaluate a cost-sensitive
   profile just below GPT-5.6 Sol's 272K long-context pricing boundary.
8. Use explicit GPT-5.6 prompt caching. Put exact stable developer instructions
   and tools before dynamic task/environment content, place an explicit cache
   breakpoint at that boundary, and derive `prompt_cache_key` from the selected
   model, profile version, stable-prefix bytes, and tool-catalog bytes. Record
   `cached_tokens` and `cache_write_tokens`; do not churn the stable prefix.
9. Use `reasoning.context: "all_turns"` while task goals remain stable. Keep
   `reasoning.mode: "standard"` for the interactive tool loop and make effort a
   CLI/eval setting: low for the fast smoke loop, max only when the measured
   quality gain warrants its latency and cost.
10. Enable hosted Responses Multi-agent rather than implementing local
    subagents. Start with the recommended three concurrent subagents and use
    them only for concrete independent workstreams. OpenAI owns spawning,
    messaging, waiting, interruption, contexts, scheduling, and result
    delivery; the Rust client executes only developer-defined local tool calls.
11. Keep PTC and Multi-agent as separate orchestration planes. PTC handles
    predictable mechanical control flow; Multi-agent handles semantic
    delegation. Hosted collaboration actions are never executed locally or
    made callable from PTC.
12. In Multi-agent WebSocket turns, execute each local call and send its output
    with `response.inject` as soon as it is ready so the waiting agent can
    resume. Preserve agent attribution and injection acknowledgement in JSONL.
13. Multi-agent does not support `reasoning.summary`. Preserve exposed root and
    agent messages, encrypted content, and raw events honestly; never claim to
    have captured hidden chain of thought. If a later single-agent mutation
    phase requests an API-visible summary, label it as a summary.
14. Record model, mode, effort, response and agent IDs, cache activity, tokens,
    latency, tool execution, injections, retries, and compactions in JSONL and
    the Harbor-derived ATIF. Harbor remains the eval record.

Gate: at least one Terminal-Bench task completes with a real OpenAI-driven tool
loop, canonical reward, raw API events, and trustworthy usage/timing metadata.

Gate achieved twice on Terminal-Bench `fix-git`. The final regression run earned
reward `1.0` with 9 model calls, 8 PTC shell calls, 29.5 seconds inside Rust,
and 35 seconds of Harbor runtime. It used 24,186 input tokens (17,712 cached),
4,546 cache-write tokens, and 1,086 output tokens. The benchmark task and
verifier were not modified.

## Milestone 1.1: runtime cleanup

Status: planned. Complete this before eval-driven tuning. Reduce production
surface area while preserving the working OpenAI/Harbor vertical slice; avoid
new framework layers whose main effect is moving code around.

1. Delete the `phase0` and `fix_git` modes, their CLI/config dispatch, and the
   synchronous shell path used only by the positive control. Model execution
   becomes the only runtime path.
2. Construct a fully configured model client in `main`. Required API and model
   configuration is validated at construction and stored directly on the
   client, rather than represented as `Option` and checked again during the
   run. Keep `eyre` for top-level error reporting.
3. Give the model run an owning struct and `impl` block. It owns the client
   session, event writer, task context, timing, and run statistics; helpers use
   that state instead of threading long argument lists and mutable statistics
   references through free functions.
4. Keep contractual JSONL events separate from diagnostic tracing. Use typed
   protocol events at the repeated wire boundary, but use compact `json!`
   values for one-off static tool schemas where dedicated serde types only add
   lines. Do not add event buses, channels, collector traits, or shared mutable
   statistics.
5. Replace the custom Codex-shaped `exec_command` function with the Responses
   native local `shell` contract. Implement the native action/output shape
   exactly and delete the custom function schema, including its ignored
   `yield_time_ms` and unsupported `tty` fields.
6. Collapse redundant model-stream state and processing: consume completed
   output once, remove unread response state and unused error variants, move
   owned function calls into concurrent execution instead of cloning them, and
   remove `Result` layers from operations whose expected failures are already
   represented as tool outcomes.
7. Replace post-hoc command-output truncation with bounded collection while the
   subprocess runs. Preserve useful truncation metadata without retaining
   unbounded stdout or stderr, and adopt Codex's process-group/parent-death
   cleanup pattern so timeout or cancellation also cleans up descendants.
8. Consolidate repeated defensive bookkeeping where the runtime already has a
   hard invariant: sample terminal duration once, avoid silent saturating
   counters, and eliminate validation repeated by constructed types.

Validation for this cleanup is `cargo fmt`, Clippy with warnings denied, a real
native `just run`, and a real `just eval`. Inspect the JSONL stream, Harbor
result, trajectory, verifier output, long-output truncation behavior, and
timeout cleanup. Do not add unit tests in this milestone.

Gate: the model-only path retains the canonical reward and exactly one terminal
event per accepted request, long command output remains memory-bounded,
timed-out commands leave no descendant processes, and the cleanup produces a
material net reduction in Rust LOC.

## Milestone 1.2: combined hosted API compatibility gate

Before treating the hosted surface as the eval baseline, prove its combined
event matrix with one real vertical smoke rather than separate mocks:

1. Open the WebSocket with both the WebSocket and Responses Multi-agent beta
   headers and warm the stable prompt/tool state with `generate: false`.
2. Enable hosted PTC, native local shell, server compaction, explicit prompt
   caching, stored response state, and hosted Multi-agent together.
3. Give the root a bounded task that requires one subagent to use PTC and issue
   a local shell call.
4. Execute the shell action in Rust, inject its typed result into the active
   response, and verify agent attribution plus PTC caller linkage survives.
5. Use a deliberately low smoke-only threshold to force an encrypted compaction
   item, then continue to one final root assistant message.
6. Inspect raw JSONL, cache/usage metrics, injection acknowledgement, compaction
   event ordering, and the final task result. Retain the Harbor trajectory; do
   not create a fixture or local journal until a demonstrated regression needs
   one.

Gate: the combined live request completes without a compatibility fallback and
without Python tool plumbing. Rust emits exactly one task terminal and all
non-API wall time is measured.

## Milestone 2: eval-driven tuning

Use Harbor as the runner and result store. First rerun `fix-git` and
`openssl-selfsigned-cert` independently against the hosted-runtime baseline.
Then add one Terminal-Bench task at a time in `evals/*.yaml`, ordered from small
repository investigation/editing through compilation, debugging, and long tool
output. Never modify a benchmark task or verifier to make the harness pass.

For every new task:

1. Run one attempt and inspect the JSONL, ATIF trajectory, verifier output, and
   task-container diff before changing the harness.
2. Separate cold artifact/image work from the warm source-edit loop. Break wall
   time into local artifact build, Harbor setup/upload, task-container startup,
   WebSocket connection/warmup, model generation, local tool execution,
   injection/continuation, verifier, and teardown.
3. Attribute model time further with time-to-first-event/output, per-response
   duration, per-agent activity, tool wait, token/cache usage, and compaction.
4. Optimize only an observed bottleneck. The intended steady state is dominated
   by OpenAI API time; local compilation, upload, execution, and verifier
   overhead should remain small and measured.
5. Prefer deletion, prompt/tool-contract correction, or one narrow typed path
   over a framework. Do not add speculative abstractions, mock-heavy suites, or
   benchmark-specific Rust cheats.
6. Commit each proven vertical improvement with its eval evidence before moving
   to the next task. Re-run earlier tasks after changes to model/tool behavior.

Report reward alongside wall time, model time, tool time, Harbor overhead,
tokens, cache utilization, compactions, and cost when the API reports it. Once
one attempt works, use repeated attempts to estimate variance and p50/p95 rather
than drawing tuning conclusions from one lucky trajectory. Add private taste or
regression tasks only after the public baseline is stable.

## Milestone 3: review provenance

Only after useful model-produced diffs exist, add a narrow `jj-lib` timeline:
baseline the workspace, checkpoint coherent mutation batches, and link each JJ
change to the prompt and exact JSONL sequence interval that caused it. Do not
add a second event journal, WAL, artifact graph, or hunk index first.

## Milestone 4: graders and review loops

After checkpoint links work, add verifier/grader subagents, bounded autoresearch
loops, user-defined taste constraints, and hunk-oriented human review. Reuse an
existing UI only if it cleanly exposes trace links; keep the CLI as the control.

## Deferred

- TUI work.
- Provider/model abstraction and backwards compatibility.
- Local multi-agent scheduling where hosted orchestration suffices.
- Approval and policy machinery.
- Durable replay, a parallel journal, or content-addressed artifact storage.
- Large mock-heavy unit-test suites ahead of working end-to-end behavior.
- Unit tests for the current runtime cleanup; rely on the real run/eval gates
  until a demonstrated regression justifies a focused deterministic test.
- Improving the environment-secret-name heuristic.
- Preserving byte-exact inbound WebSocket frames instead of parsed and
  reserialized API events.
- Removing duplicate derived assistant/reasoning delta events or otherwise
  reducing event volume; first establish which representation the ATIF adapter
  should consume.
