# Nanocodex

Nanocodex is a small, headless Rust agents SDK. It is a library first: embed it
in your process, configure the agent and its tools, submit turns through a cheap
handle, and decide whether to consume every event or only typed final results.
There is no required app server, JSON-RPC layer, global runtime, or UI. The CLI
and Harbor integration in this repository are thin adapters over the same
public library API.

The scope is deliberately narrow. Nanocodex currently runs `gpt-5.6-sol` over
the OpenAI Responses WebSocket API, preserves one stateful session across
follow-on prompts, and exposes its transport as a caller-composable Tower
service. Model-generated code mode runs in local Node.js and calls the Rust tool
registry.

## Use it as a library

Until the crates are published, depend on the repository directly:

```toml
[dependencies]
nanocodex = { git = "https://github.com/gakonst/nanocodex" }
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

`Nanocodex::new` uses the standard prompt, medium thinking, built-in tools,
persistent WebSocket, and retry/reconnect policy. Node.js 12.22 or newer must be
available on `PATH` for model-generated code mode.

### Follow-on prompts and events

`build()` spawns the stateful agent driver and returns `(Nanocodex,
AgentEvents)`. `Nanocodex` is a cheap, cloneable command handle. Calling
`prompt(...)` accepts and queues a turn, then immediately returns a `Turn`; the
agent continues independently until `turn.result()` is awaited.

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

For dynamic state, freeform inputs, multimodal outputs, metadata, or custom
decoding, implement the public `Tool` trait directly and register the value with
the same `.tool(...)` method. Internal and external tools use the same
heterogeneous registry. See
[`custom_tool.rs`](crates/nanocodex/examples/custom_tool.rs) for a runnable
example.

### Configure the agent and Tower stack

`Nanocodex::builder(api_key)` exposes deliberate overrides for the system
prompt, thinking level, tools, workspace, stable session ID, and Responses
stack. `.prompt(...)` on the builder replaces the system/developer prompt;
`.prompt(...)` on the built handle submits a user turn.

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
        .prompt("You are a concise repository maintenance agent.")
        .thinking(Thinking::Medium)
        .workspace("/work/project")
        .responses(responses)
        .build()
}
```

Tower layers are deferred until the standard persistent-WebSocket service is
created. Callers can add deadlines, concurrency limits, load shedding, tracing,
metrics, circuit breaking, or error mapping without boxing the client or
rebuilding agent orchestration. `Responses::builder().service(stack)` replaces
the standard service with any caller-composed
`tower::Service<ResponsesAttempt>`.

See [`docs/RESPONSES_TOWER.md`](docs/RESPONSES_TOWER.md) for the implemented
operation boundary, layer ordering, retry safety, and benchmark evidence.

### Crate boundaries

The workspace exposes four independently useful library layers, following the
same boundary style as `alloy-core` and Alloy's ergonomic top-level crate:

- `nanocodex-core`: dependency-light prompts, events, model configuration, and
  complete typed Responses wire/domain types.
- `nanocodex-service`: persistent WebSocket transport, stream processing,
  typed errors, Tower service/client, retry middleware, and telemetry.
- `nanocodex-tools`: built-in tools, code mode, heterogeneous tool registry,
  and the public tool trait.
- `nanocodex`: owned agent lifecycle, builders, and ergonomic re-exports.

`nanocodex-macros` implements `#[tool]`. The `nanocodex-bin` package under
`bin/nanocodex` is an example CLI adapter, not the SDK boundary.

## Develop this repository

```sh
just bootstrap      # install pinned host dependencies once
just run            # native low-effort smoke; requires local Node.js
just prepare-evals  # build/cache tasks and the shared verifier toolbox
just eval           # fresh full model-driven Terminal-Bench suite
just eval-hosted    # same pinned suite in hosted Daytona sandboxes
just view           # inspect retained Harbor jobs
```

The native CLI accepts one positional prompt and streams flushed JSONL to
stdout. It demonstrates the process adapter; it is not required by the library.

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
latest full 41-task gate scored 40/41 with zero errored trials, Responses
retries, or WebSocket reconnects and 95.16% cached input. Current architecture,
validation policy, baseline details, and ordered future work live in
[`PLAN.md`](PLAN.md).
