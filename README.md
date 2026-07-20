# Nanocodex

Nanocodex is a small, library-first Rust agents SDK built around the OpenAI
Responses WebSocket API. It keeps the useful coding-agent loop—persistent
conversations, shell and patch tools, Code Mode, MCP, steering, cancellation,
queueing, and conversation forks—without requiring an app server or making a durable agent
control plane part of every application.

It is best understood as a deliberate alternative to embedding Codex, not as a
drop-in reimplementation of the Codex application.

## Nanocodex versus Codex

Codex is a complete agent product and a large durable runtime. Nanocodex is an
embeddable SDK: the caller owns the process, chooses the tools, and receives a
cheap prompt handle, independently awaitable typed results, and an optional
ordered event stream.

| | Nanocodex | Codex |
| --- | --- | --- |
| Primary boundary | Rust library in the caller's process | Application, app server, and durable agent runtime |
| Conversation state | One owned in-memory driver with client-owned typed history | Persisted threads and rollouts |
| Follow-on turns | Persistent WebSocket plus a new delta and `previous_response_id` | Full Codex session lifecycle |
| Historical forks | Exact completed response checkpoint while the mainline continues | Reconstructed or sanitized durable thread history |
| Tools | Small Code Mode surface over caller-defined Rust tools and MCP | Broad built-in tool and integration surface |
| Middleware | Caller-composable concrete Tower service | Codex-owned runtime policy |
| Events | Optional typed stream independent from typed turn results | Product-wide rollout and UI event lifecycle |
| Orchestration | Application tools; the model can generate the topology in Code Mode | First-class agents, mailboxes, task identities, budgets, and lifecycle controls |

The smaller boundary is the point. A normal consumer builds an agent, receives
`(Nanocodex, AgentEvents)`, submits prompts through a cloneable handle, and
awaits `TurnResult`s. The CLI, Harbor adapter, Python binding, and Rust/WASM
binding are all consumers of that same API rather than alternate runtimes.

### Performance

The checkpoint benchmark uses `gpt-5.6-sol`, a deterministic 600-fact prefix,
a ten-turn conversation, and concurrent historical forks. A three-run live
rerun on 2026-07-20 using Nanocodex `210ac85` and stock Codex CLI
`0.145.0-alpha.18` measured:

| Measurement | Nanocodex checkpoint path | Stock Codex app server | Difference |
| --- | ---: | ---: | ---: |
| Ten short sequential turns, median total | 14.78 s | 24.99 s | **1.69x faster** |
| Warm turn p50, turns 3–10 | 1.304 s | 1.532 s | **1.18x faster** |
| Historical fork to first answer, p50 | 1.570 s | 6.530 s | **4.16x faster** |
| Historical fork model time, p50 | 1.291 s | 5.862 s | **4.54x faster** |

The ten-turn totals sum request-to-completion model-turn latency. Nanocodex's
separately measured WebSocket handshake had a 361 ms median; Codex app-server
process and thread initialization were also outside the reported total.

Nanocodex's fork sends about 725 bytes of new request data from an exact stored
checkpoint. Replaying the same Nanocodex history would send 27–29 KB, a 97.4%
reduction. Each child gets its own WebSocket, session, driver, service stack,
and tool runtime while the parent continues independently.

These are checkpoint-path measurements, **not a normalized full-agent quality
or model-runtime comparison**. The Nanocodex arm deliberately uses a minimal
benchmark developer message and no production tool definitions; the Codex arm
runs the complete stock app-server agent with its system instructions, tools,
and repository context. That makes the workload useful for measuring the cost
of continuation and historical branching, but it does not establish that a
fully configured Nanocodex agent is always 4x faster or uses 68% fewer tokens.
The methodology, earlier trials, cache observations, and reproduction commands
are in [`benchmarks/fork_results.md`](benchmarks/fork_results.md).

On a real 41-task coding gate, Nanocodex completed 38/41 tasks with 92.23% of
input tokens cached, zero Responses retries, and zero WebSocket reconnects.
That demonstrates a useful coding agent, but it is not yet an apples-to-apples
Codex quality result or a completed Terminal-Bench 2.1 leaderboard submission.

### Fewer top-level tools, not necessarily fewer capabilities

The model normally sees two Nanocodex tool definitions: Code Mode and its wait
operation. Code Mode can call the nested Rust registry, which includes shell
execution, persistent shell input, patching, planning, image inspection,
optional web search and image generation, MCP providers, and application tools.
The model can compose those operations in generated JavaScript with loops,
conditionals, and `Promise.all` rather than paying for every tool as a separate
top-level schema.

For repository work, the important capabilities are still present: inspect
files, run commands, edit code, execute tests, and repeat. Applications can add
domain-specific tools with `#[tool]` or MCP. The smaller default surface can
reduce prompt material and tool-selection noise, but it is not inherently a
quality advantage; tasks that depend on a missing integration must supply it.

### Tradeoffs

Nanocodex intentionally gives up product breadth and durability in exchange
for a smaller embeddable boundary:

- It currently supports one model family, `gpt-5.6-sol`, through the OpenAI
  Responses WebSocket API. It is not a provider abstraction.
- Sessions, branches, child registries, and typed history are owned by the
  running process. Codex is the better fit for durable threads, restart/resume,
  detached agents, and long-lived mailbox-driven collaboration.
- Multi-agent tools are application-defined. Nanocodex does not yet provide
  Codex's central task registry, execution budgets, residency controls,
  or recursive cancellation propagation. A caller can cancel one exact live
  turn, but application-defined parent/child trees must propagate that policy
  themselves. Unbounded recursive orchestration can spend tokens quickly.
- The caller owns sandboxing, permissions, and tool policy. There is no built-in
  approval product or compatibility app server.
- Code Mode requires Node.js 12.22 or newer on `PATH`. Browser and computer-use
  integrations are not built in.
- The Ratatui client is useful but intentionally thinner than Codex's mature
  TUI and IDE ecosystem.

Choose Nanocodex when the agent belongs inside your Rust service, CLI, notebook,
or language binding and you want direct ownership of tools, middleware,
results, events, and fast structured branches. Choose Codex when durable
sessions, built-in integrations, approval UX, managed subagent lifecycles, and
a complete daily-driver product matter more than a small library boundary.

## Use the daily-driver CLI

Install the repository binary and launch it from the workspace you want the
agent to edit:

```sh
curl -fsSL https://raw.githubusercontent.com/gakonst/nanocodex/master/install | bash
export OPENAI_API_KEY=...
nanocodex
```

The installer selects the macOS or Linux release for the host architecture,
verifies it against the release's `SHA256SUMS`, and installs it under
`~/.nanocodex/bin`. Windows binaries are available on the
[latest GitHub Release](https://github.com/gakonst/nanocodex/releases/latest).
An existing release installation updates itself with:

```sh
nanocodex update
```

Source builds remain available with `cargo install --path bin/nanocodex` from
a checkout. See the [release changelog](CHANGELOG.md) and
[release process](docs/RELEASING.md) for the versioned artifacts and crate
publishing contract.

The Ratatui interface keeps one agent and WebSocket alive across follow-on
prompts, streams assistant output, shows tool activity, accepts prompts while a
turn is running, and retains prompt history and scrollback for the session.
Press Enter to submit, Ctrl+J or Shift+Enter for a newline, Up/Down for prompt
history, PageUp/PageDown or the mouse wheel to scroll, Esc to clear the
composer, and Ctrl+C to exit. Use `--cwd`, `--thinking`, `--instructions`,
`--web-search`, and `--image-generation` to configure the session; `--prompt`
submits an initial turn immediately.

After at least one completed turn, enter `/btw <question>` to fork the latest
checkpoint into a right-hand side conversation. The main thread keeps running
on its original WebSocket. Press BackTab to switch panes and `/close` to dismiss
an idle BTW branch. While a turn is running, Enter steers that turn at its next
safe model/tool boundary and Tab explicitly queues a follow-up turn. Pending
steers and queued turns remain visibly separate. `/cancel` stops the active turn
in the focused pane; queued follow-ups then continue from the last completed
checkpoint. Cancellation also terminates live Code Mode and shell process trees.

The headless adapter remains available for scripts and evals. Its stdout is
flushed JSONL only:

```sh
nanocodex run "Inspect this repository and summarize it."
```

The CLI accepts the same MCP providers as the library. For example, a local
stdio server can be exercised across repeated turns on one retained session:

```sh
nanocodex \
  --mcp-stdio workspace=node \
  --mcp-arg workspace=./server.mjs \
  run --repeat 3 "Search the workspace tools and summarize the result."
```

Lifecycle tracing is written to stderr for headless runs and to
`.nanocodex/logs/tui.log` for the TUI. `--log-format json` selects structured
local logs, `RUST_LOG` or `--log-filter` controls filtering, and
`--otel-endpoint http://localhost:4318` exports spans over OTLP/HTTP.
`OTEL_LEVEL` or `--otel-filter` controls export independently from local logs.
Run `just otel-up` followed by `just otel-demo` for a local Jaeger waterfall;
use `just otel-stress` for the deterministic hostile-tool pressure gate. The
complete walkthrough is in [`docs/OBSERVABILITY.md`](docs/OBSERVABILITY.md).

## Use it as a library

The public crates share one version and are published to crates.io. Most
applications only need the top-level crate:

```toml
[dependencies]
nanocodex = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The smallest useful program submits one prompt and awaits its typed result. If
you do not need live events, destructure them as `_`; the receiver is dropped
immediately and event production becomes a no-op:

```rust
use nanocodex::Nanocodex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let (agent, _) = Nanocodex::new(api_key)?;

    let turn = agent.prompt("Inspect this repository and summarize it.").await?;
    let result = turn.result().await?;
    println!("{}", result.final_message);
    Ok(())
}
```

`Nanocodex::new` uses the standard instructions, medium thinking, built-in tools,
persistent WebSocket, and retry/reconnect policy. Node.js 12.22 or newer must be
available on `PATH` for model-generated code mode.

### API at a glance

The API separates configuration, command submission, control, and completion.
Callers never manage response IDs, turn IDs, socket tasks, history replay, or
tool results:

| Intent | API | Resulting behavior |
| --- | --- | --- |
| Use all defaults | `Nanocodex::new(api_key)` | Starts one owned agent and returns its handle plus optional events |
| Configure policy | `Nanocodex::builder(api_key)` | Sets instructions, thinking, workspace, tools, session identity, and Responses policy before startup |
| Submit ordinary input | `agent.prompt(input).await` | Appends one FIFO turn and returns an independently awaitable `Turn` |
| Add input to the active turn | `turn.steer(input).await` | Joins that exact turn at its next safe model boundary |
| Split result and control ownership | `turn.control()` | Returns a cloneable `TurnControl` for another task |
| Stop exact work | `turn.cancel().await` | Removes a queued turn or stops an active turn and its subprocesses |
| Continue normally | `agent.prompt(input).await` again | Reuses retained history, WebSocket, tools, and response chain automatically |
| Branch from the latest commit | `agent.fork().await` | Starts an independent agent at the latest completed checkpoint |
| Branch from an older commit | `agent.fork_from(&result).await` | Starts an independent agent at that exact historical `TurnResult` |
| Build a clean child inside a tool | `handle.spawn().await` | Reuses private builder policy without inheriting conversation context |
| Build a contextual child inside a tool | `handle.fork().await` | Forks the invoking agent rather than accidentally targeting the root |

`Turn` is the capability for unfinished work: it can be steered, cancelled, or
consumed by `result()`. `TurnResult` represents committed work: it contains the
final message and an opaque immutable checkpoint, but has no steering or
cancellation methods. `AgentHandle` is a weak, per-driver capability supplied
only to tools factories; it does not keep its parent alive or expose the API key.

### Turns, queueing, steering, and cancellation

`build()` spawns the stateful agent driver and returns `(Nanocodex,
AgentEvents)`. `Nanocodex` is a cheap, cloneable command handle. Calling
`prompt(...)` accepts and queues a turn, then immediately returns a `Turn`; the
agent continues independently until `turn.result()` is awaited.
`turn.steer(...)` instead targets that exact turn. It acknowledges only
after the instruction enters that turn's bounded FIFO and returns a typed error
when the turn is queued, completed, or its steering queue is full. Steering is
sampled only between complete model responses and tool outputs; it does not
create another `Turn` or another terminal event.

`turn.cancel()` targets the same opaque unfinished turn. A queued turn is
removed without reaching the model; cancellation of an active turn waits until
model work, Code Mode cells, and shell process groups have stopped. In either
case the result resolves to the typed `NanocodexError::TurnCancelled`. Partial
model and tool work is discarded, and the next surviving prompt resumes from
the last completed checkpoint. Queued cancellation is acknowledged immediately,
while its result and terminal event retain FIFO event order behind earlier
turns. If one task must await the result while another controls it, clone
`turn.control()` before moving the `Turn` into the result task. Normal code can
call `steer` and `cancel` directly on `Turn`; the separate control value is only
needed when ownership splits. Completed `TurnResult`s are deliberately inert;
historical branching remains `agent.fork_from(&result)` because a fork creates
another owned agent lifecycle.

```rust
use nanocodex::{Nanocodex, NanocodexError};

# async fn cancellation(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let (agent, _) = Nanocodex::new(api_key)?;
let turn = agent.prompt("Run a long repository investigation.").await?;
let control = turn.control();
let result_task = tokio::spawn(async move { turn.result().await });

control.steer("Prioritize failing tests.").await?;
control.cancel().await?;
assert!(matches!(
    result_task.await?,
    Err(NanocodexError::TurnCancelled)
));
# Ok(())
# }
```

The session retains the complete typed conversation history. A follow-on prompt
does **not** need the previous `final_message`, transcript, response ID, or tool
results passed back into it. On a healthy socket Nanocodex continues with
`previous_response_id`; after a reconnect it transparently replays its retained
history.

```rust
use nanocodex::{AgentEventKind, Nanocodex};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let (agent, mut events) = Nanocodex::new(api_key)?;

    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if event.kind == AgentEventKind::AssistantMessage {
                eprintln!("assistant message emitted");
            }
        }
    });

    let first = agent.prompt("Choose one word for this project.").await?;
    // The caller can do unrelated work while the turn runs.
    let first = first.result().await?;
    println!("first: {}", first.final_message);

    // No first.final_message is passed here. The agent has the first turn.
    let second = agent
        .prompt("Return the word you chose, but in uppercase.")
        .await?;
    println!("second: {}", second.result().await?.final_message);
    Ok(())
}
```

`AgentEvents` is the single ordered event stream for the session and is
independent from turn results. A server, TUI, notebook, or language binding can
translate all events, select a subset, or ignore them without changing prompt
and result handling.

### Fork conversations without stopping the mainline

Forks contain only committed work. If the root has an active turn,
`agent.fork()` uses its latest completed checkpoint and excludes partial model
output and tool work. `agent.fork_from(&result)` can branch from any retained
historical result while the root continues advancing. Each branch gets a fresh
driver, WebSocket, event stream, response chain, service stack, and tool runtime;
immutable typed history and the cache lineage remain shared.

```rust
use nanocodex::Nanocodex;

# async fn branching(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let (root, _) = Nanocodex::new(api_key)?;
let historical_checkpoint = root
    .prompt("Record early decision A.")
    .await?
    .result()
    .await?;
root.prompt("Record later decision B.")
    .await?
    .result()
    .await?;

// The root can keep running while old and latest checkpoints are forked.
let root_turn = root.prompt("Add in-flight mainline decision C.").await?;
let ((historical, _), (latest, _)) = tokio::try_join!(
    root.fork_from(&historical_checkpoint),
    root.fork(),
)?;

let historical_turn = historical.prompt("Explore an alternative to A.").await?;
let latest_turn = latest.prompt("Review the latest committed context.").await?;
let (root_result, historical_result, latest_result) = tokio::try_join!(
    root_turn.result(),
    historical_turn.result(),
    latest_turn.result(),
)?;

println!("root: {}", root_result.final_message);
println!("historical: {}", historical_result.final_message);
println!("latest: {}", latest_result.final_message);
# Ok(())
# }
```

The checkpoint and provider-side `previous_response_id` are intentionally
opaque. A healthy fork sends only its new input from the stored checkpoint. If
that checkpoint is missing, Nanocodex drops the ID once and replays its
client-owned committed history before returning to incremental requests.

### Handle typed errors

Lifecycle failures are direct `NanocodexError` variants, so common control flow
does not require a nested agent-error match. Transport and API failures preserve
their typed source even through caller-provided Tower middleware:

```rust
use nanocodex::{NanocodexError, ResponsesError};

# fn handle(error: NanocodexError) -> Result<(), NanocodexError> {
match &error {
    NanocodexError::TurnCancelled => return Ok(()),
    NanocodexError::TurnNotSteerable => eprintln!("submit a new prompt instead"),
    _ => {}
}

if matches!(
    error.responses_error(),
    Some(ResponsesError::InvalidImageRequest { .. })
) {
    eprintln!("the Responses API rejected the image payload");
}

Err(error)
# }
```

The error surface distinguishes invalid configuration, workspace and project
instruction I/O, stopped agents, turn lifecycle conflicts, checkpoint lineage,
tool construction, Responses transport/service failures, and caller middleware
failures. Source details remain available through the standard `Error::source`
chain as well as `responses_error()`.

### Define custom tools

The `#[tool]` macro turns a normal async Rust function into a typed tool. It
derives the JSON Schema from the function arguments, decodes calls, awaits the
function, and returns the serialized result through the heterogeneous tool
registry:

```rust
use nanocodex::{Nanocodex, Tools, tool};

#[tool(description = "Multiplies two signed integers.")]
async fn multiply(left: i64, right: i64) -> Result<i64, &'static str> {
    left.checked_mul(right).ok_or("integer overflow")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let tools = Tools::builder().tool(multiply).build()?;
    let (agent, _) = Nanocodex::builder(api_key).tools(tools).build()?;

    let result = agent
        .prompt("Use the multiply tool to calculate 6 × 7, then return it.")
        .await?
        .result()
        .await?;
    println!("{}", result.final_message);
    Ok(())
}
```

`Tools::builder()` starts with the standard optional web-search and
image-generation integrations enabled. Use `.without_defaults()` to disable
those optional integrations before adding application tools. The core local
coding tools remain available through code mode.

Manual `Tool` implementations return `ToolResult`, so input decoding and SDK
operations compose with `?`. The runtime converts `Err` into a failed
model-visible tool result, allowing the model to recover without terminating
the agent turn. An explicit `Ok(ToolExecution { success: false, .. })` is
reserved for preserving structured failure payloads from remote tool protocols.

For dynamic state, freeform inputs, multimodal outputs, metadata, or custom
decoding, implement the public `Tool` trait directly and register the value with
the same `.tool(...)` method. Internal and external tools use the same
heterogeneous registry. See
[`custom_tool.rs`](examples/custom_tool.rs) for a runnable
example.

Runnable examples live in the top-level [`examples`](examples) package:

```sh
cargo run -p nanocodex-examples --bin minimal
cargo run -p nanocodex-examples --bin follow-on
cargo run -p nanocodex-examples --bin custom-tool
cargo run -p nanocodex-examples --bin subagents
cargo run -p nanocodex-examples --bin fork-conversations
cargo run -p nanocodex-examples --bin mcp
```

### Add deferred MCP tools

`nanocodex-mcp` implements Streamable HTTP and stdio MCP clients as a dynamic
Code Mode tool provider. Each configured server initializes and runs
`tools/list` concurrently when the owned agent starts. Only the compact
`tool_search` definition is in the initial model prompt; matching tools are
activated on demand and can be called immediately from the same code cell.

```rust
use nanocodex::{Mcp, McpServer, Nanocodex, Tools};

# async fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let mcp = Mcp::builder()
    .server(
        "workspace",
        McpServer::http("https://mcp.example.com/mcp")
            .bearer_token_env("WORKSPACE_MCP_TOKEN"),
    )
    .server(
        "local",
        McpServer::stdio("node").args(["./server.mjs"]),
    )
    .build()?;
let tools = Tools::builder().provider(mcp).build()?;
let (agent, _) = Nanocodex::builder(api_key).tools(tools).build()?;

let result = agent
    .prompt("Search the configured MCP tools, use the relevant read-only tool, and summarize.")
    .await?
    .result()
    .await?;
println!("{}", result.final_message);
# Ok(())
# }
```

HTTP authentication can come from a bearer token or arbitrary fixed/environment
headers; secret values are resolved only by the background connection task.
Server/tool filters and startup/tool timeouts are configured per `McpServer`.
See [`mcp.rs`](examples/mcp.rs) for a runnable example.

### Add tracing and OpenTelemetry

Nanocodex libraries emit stable `tracing` spans for sessions, turns, model
calls, Responses attempts and connections, retries, tools, and MCP activity.
They never install a global subscriber, so an embedding application can use
its existing formatting, metrics, or OpenTelemetry stack. Contractual
`AgentEvents` remain separate from diagnostic tracing.

The optional `nanocodex-observability` crate provides the same compact stderr,
JSON/file, and OTLP/HTTP setup used by the CLI:

```toml
[dependencies]
nanocodex-observability = { git = "https://github.com/gakonst/nanocodex" }
```

```rust
use nanocodex_observability::{LogFormat, ObservabilityBuilder};

# fn install() -> Result<(), Box<dyn std::error::Error>> {
let _guard = ObservabilityBuilder::new("my-agent", env!("CARGO_PKG_VERSION"))
    .filter("warn,nanocodex=info,nanocodex_service=info,nanocodex_mcp=info")
    .otel_filter("warn,nanocodex=info,nanocodex_service=info,nanocodex_mcp=info")
    .format(LogFormat::Json)
    .otlp_endpoint("http://localhost:4318")
    .install()?;
# Ok(())
# }
```

Keep the returned guard alive for the application lifetime so non-blocking
formatting and batched trace export are flushed during shutdown. Spans include
IDs, attempt/replay state, durations, status, token/cache usage, structural
prompt/tool metadata, process outcomes, and API-visible reasoning summaries.
Full prompts, Code Mode source, tool argument values, hidden reasoning, and API
keys are never attached.

### Embed from Python, Node.js, or a browser Worker

The language bindings preserve the same owned session rather than wrapping the
CLI or starting an app server:

```python
from nanocodex import Nanocodex

agent, events = Nanocodex(api_key, thinking="low")
first = agent.prompt("Choose one word for this project.")
print(first.result())
second = agent.prompt("Return that word in uppercase.")
print(second.result())  # no previous result or transcript is passed back
```

The PyO3 extension owns a native Tokio runtime and exposes `Nanocodex`, `Turn`,
and the ordered event receiver directly. See
[`bindings/python`](bindings/python) for build instructions and the top-level
[`examples/python`](examples/python) programs.

Node.js and web consumers use one shared Rust/WASM artifact. Node supplies a
header-capable WebSocket and can define async JavaScript tools; a browser Worker
supplies its own authenticated WebSocket boundary and browser-native tools:

```js
const turn = agent.prompt("Use multiply to calculate 6 × 7.");
console.log(await turn.result());
const followOn = agent.prompt("Add one to that result.");
console.log(await followOn.result());
```

See the top-level [`examples/node`](examples/node) and
[`examples/react-vite`](examples/react-vite) consumers. The React example runs
the persistent Rust/WASM agent in a real module Worker, displays the ordered
event stream, and registers a browser-native custom tool. Browser WebSockets
cannot set the Responses authorization upgrade header, so Nanocodex does not
pretend direct browser authentication works and does not ship a relay; the
embedding application supplies an already-authorized endpoint or custom
`createWebSocket` implementation.

### Let Code Mode orchestrate application-defined agents

[`subagents.rs`](examples/subagents.rs) shows that delegation does not require a
multi-agent subsystem in the library. Its application-defined Code Mode tools
contrast `spawn_agent`, which builds an independent conversation, with
`fork_agent`, which forks the invoking agent's latest completed checkpoint
while that agent's turn is running. Both return an `agent_id`; `prompt_agent`
uses it for follow-on turns through the same child's retained conversation,
response chain, cache lineage, WebSocket, and tools. A per-driver
`tools_factory` creates a new handler around a weak `AgentHandle` for every
root, child, and grandchild.
`AgentHandle::spawn()` starts a clean conversation while privately reusing the
builder's credentials, model, workspace policy, service factory, and tools
factory; `AgentHandle::fork()` inherits the invoking agent's latest commit.
Recursive children therefore have the correct parent without a self-reference
cycle or API-key plumbing. A weak application-owned child registry retains only
cheap `Nanocodex` handles, and its mutex is released before each follow-up model
turn. The host does not encode a DAG: given a high-level goal, Code Mode chooses
worker count, context strategy, concurrency, follow-ups, sequencing, and
synthesis. Pass a quoted command-line goal to replace the built-in architecture
decision. Because typed events are optional and orthogonal to turn results, the
example prints only the root's final answer by default. Set
`NANOCODEX_SUBAGENT_JSONL=1` to send child lifecycle events to stderr using
their native request IDs and sequence numbers, without introducing a merged
event protocol.

[`fork_conversations.rs`](examples/fork_conversations.rs) is the direct API
tour. It configures a cloneable Tower stack, builds ten meaningful checkpoints,
forks exact historical turns while the mainline advances, demonstrates a
latest-checkpoint fork, and proves that later and branch-only facts remain
isolated.

### Configure the agent and Tower stack

`Nanocodex::builder(api_key)` exposes deliberate overrides for the persistent
instructions, thinking level, tools, workspace, stable session ID, and
Responses stack. `.instructions(...)` configures the stable system/developer
instructions; `.prompt(...)` on the built handle submits only user input. The
workspace is fixed for the owned session at build time and cannot be changed by
a later prompt or steer.

Factories are conditional, not the default. Pass a completed `Tools` value to
`.tools(tools)` for stateless or shareable handlers. Use
`.tools_factory(|handle| ...)` only when every root, fork, and clean child needs
a fresh tool collection or a tool must target its invoking agent through that
weak `AgentHandle`. The factory receives no credentials; spawned agents reuse
the builder's private API key and configuration automatically.

The same distinction applies to Responses services. Use `.layer(...)` to wrap
the standard persistent-WebSocket and retry implementation. Use
`.service(|| make_stack())` only when replacing the complete Tower service. A
replacement is always a factory because cancellation recovery and independent
branches require fresh mutable service and connection state; a one-off service
instance could not satisfy those lifecycle guarantees.

The concrete Responses configuration is carried in `NanocodexBuilder`'s type,
so `build()` is available only when the selected layer or service satisfies the
complete `Service<ResponsesAttempt>` contract. This keeps dispatch generic and
unboxed while leaving socket tasks, queue capacities, and mutable run state
private.

Add `tower = { version = "0.5", features = ["limit", "timeout"] }` when
composing the middleware used below.

```rust
use std::time::Duration;

use nanocodex::{AgentEvents, Nanocodex, Responses, Thinking};
use tower::{limit::ConcurrencyLimitLayer, timeout::TimeoutLayer};

fn build_agent(api_key: String) -> nanocodex::Result<(Nanocodex, AgentEvents)> {
    let responses = Responses::builder()
        .layer(TimeoutLayer::new(Duration::from_secs(120)))
        .layer(ConcurrencyLimitLayer::new(1))
        .build();

    Nanocodex::builder(api_key)
        .instructions("You are a concise repository maintenance agent.")
        .thinking(Thinking::Medium)
        .workspace("/work/project")
        .responses(responses)
        .build()
}
```

Tower layers are deferred until the standard persistent-WebSocket service is
created. Callers can add deadlines, concurrency limits, load shedding, tracing,
metrics, circuit breaking, or error mapping without boxing the client or
rebuilding agent orchestration. `Responses::builder().service(|| make_stack())`
replaces the standard service with a factory for any caller-composed
`tower::Service<ResponsesAttempt>`. The factory is invoked for the root and
again whenever cancellation, clean child spawning, or forking needs independent
mutable service and connection state. One-off service stacks are deliberately
not part of the API.

See [`docs/RESPONSES_TOWER.md`](docs/RESPONSES_TOWER.md) for the implemented
operation boundary, layer ordering, retry safety, and benchmark evidence.

### Crate boundaries

The workspace exposes five independently useful library layers, following the
same boundary style as `alloy-core` and Alloy's ergonomic top-level crate:

- `nanocodex-core`: dependency-light prompts, events, model configuration, and
  complete typed Responses wire/domain types.
- `nanocodex-service`: persistent WebSocket transport, stream processing,
  typed errors, Tower service/client, retry middleware, and telemetry.
- `nanocodex-tools`: built-in tools, code mode, heterogeneous tool registry,
  and the public tool trait.
- `nanocodex-mcp`: background MCP transports, discovery catalog, BM25
  `tool_search`, authentication inputs, and deferred Code Mode dispatch.
- `nanocodex`: owned agent lifecycle, builders, and ergonomic re-exports.

`nanocodex-macros` implements `#[tool]`. The `nanocodex-bin` package under
`bin/nanocodex` is an example CLI adapter, not the SDK boundary.
The PyO3 and Rust/WASM packages under `bindings/` are likewise thin embedded
adapters over the owned session and typed event contract.

## Develop this repository

```sh
just bootstrap      # install pinned host dependencies once
just run            # native low-effort smoke; requires local Node.js
just prepare-evals  # build/cache tasks and the shared verifier toolbox
just eval           # fresh full model-driven Terminal-Bench suite
just eval-hosted    # same pinned suite in hosted Daytona sandboxes
just view           # inspect retained Harbor jobs
```

The native CLI defaults to the interactive Ratatui client. Its `run` subcommand
accepts one positional prompt and streams flushed JSONL to stdout for Harbor and
other process integrations. Neither adapter is required by the library.

Harbor builds a static Linux binary, installs it in an unchanged task container,
and derives ATIF from the retained JSONL. Python owns upload/process lifecycle
only; model decisions, API calls, tools, and mutations remain in Rust.

```text
native BuildKit compile -> static Linux binary
                       -> Harbor task container
                       -> /installed-agent/nanocodex
                       -> Rust executes tools in /app
                       -> Harbor verifier
```

Local artifacts use Cargo's `dev` profile. Set
`NANOCODEX_BUILD_PROFILE=profiling` for an optimized build with debug symbols.
The pinned eval selection lives in
[`evals/terminal-bench-2.yaml`](evals/terminal-bench-2.yaml), not the Justfile.

Hosted evals use Harbor's Daytona environment and a separate AMD64 artifact:

```sh
just eval-task-hosted terminal-bench/fix-git
just eval-hosted
```

Retained jobs live under `.nanocodex/harbor/jobs`; `just view` opens them. The
latest full 41-task gate scored 38/41 with zero Responses retries or WebSocket
reconnects and 92.23% cached input. One task hit a transient upstream policy
rejection after producing a verifier-passing artifact and passed an isolated
rerun. The research and measurement log for a focused race-free Rust evaluation
runner lives in [`docs/HARBOR_RS_LOG.md`](docs/HARBOR_RS_LOG.md). Current agent
architecture, validation policy, failure classifications, and ordered future
work live in [`PLAN.md`](PLAN.md).
