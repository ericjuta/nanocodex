# Single-prompt latency profile — 2026-07-20

This profile measures the headless `nanocodex run` path at
`d2df7bfe25d0` on an Apple M1 Max with 32 GiB RAM, macOS 26.3.1,
Rust 1.94.1, and Node 23.6.0. The binary used the repository's `profiling`
profile and `gpt-5.6-sol` with low thinking.

These are live service observations with small sample counts. They identify
orders of magnitude and request boundaries; they are not a controlled model
runtime comparison. Full event and fmt-subscriber traces are retained under
`.nanocodex/profile-single-prompt/` and intentionally remain outside Git.

The representative 41-task and long-output follow-up is in
[`long_prompt_profile_2026-07-20.md`](long_prompt_profile_2026-07-20.md). It
confirms the API/tool bottleneck, removes a redundant request-sizing pass, and
adds per-attempt pipeline timings.

## Results

### One model call, no tool

Five fresh processes ran `Reply with exactly PONG and no other text.` with the
default tool surface.

| Measurement | Median | Range |
| --- | ---: | ---: |
| End to end | 2.021 s | 1.484–2.912 s |
| Initial WebSocket connection | 454 ms | 442–630 ms |
| Warmup, including the connection | 916 ms | 714–1,077 ms |
| Generation | 1.224 s | 757–1,825 ms |
| First generation protocol event | 86 ms | 50–117 ms |
| First generation output | 1.048 s | 589–1,573 ms |
| Local time outside warmup/generation | 5.2 ms | 2.7–12.0 ms |

The generation used 10,948 input tokens and emitted 6 output tokens in every
trial. The process consumed about 90–110 ms of user CPU, 20–40 ms of system CPU,
and peaked around 26 MiB RSS. Startup through Clap `--version` was 6.3 ms p50
across 50 processes, excluding one 262 ms OS scheduling outlier.

Disabling standalone web search and image generation reduced generation input
from 10,948 to 8,173 tokens (25%) and warmup input from 8,679 to 5,904 tokens
(32%). It did not improve latency in five trials: median end-to-end time was
2.177 s and median generation time was 1.332 s. Service variance was larger
than any local benefit.

### One tool round trip

Three fresh processes asked the agent to execute `pwd` once and report it.

| Measurement | Median | Range |
| --- | ---: | ---: |
| End to end | 6.114 s | 5.906–6.424 s |
| Both model calls | 5.294 s | 5.030–5.510 s |
| First model call | 2.096 s | 1.932–2.462 s |
| Second model call | 3.098 s | 3.048–3.198 s |
| Warmup | 803 ms | 557–824 ms |
| Top-level Code Mode tool wall time | 48 ms | 44–312 ms |
| Actual nested `pwd` work | 8.7 ms | 7.2–8.9 ms |
| Local time outside measured phases | 3.5 ms | 3.3–15.0 ms |

The model calls were the median 85% of total wall time. The actual shell command
was 0.14% of the turn. The first Code Mode cell showed a cold-host mode around
300 ms to the Node host's first event and a warm-OS-cache mode around 36–39 ms.
Within one retained session the host was reused: first-event time fell to
0.57–1.44 ms before nested tool work.

A three-turn retained-session run took 5.495 s for its cold first tool turn,
then 3.084 s and 3.330 s for the two follow-ons. Besides ordinary service
variance, those turns avoided connection/warmup and reused the Node host. The
follow-ons also sent incremental inputs with more than 99% reported cached
input tokens.

### Node-host prewarm

The profile branch now creates the shared Node host when the per-session tool
runtime is constructed. The process initializes concurrently with warmup and
the first model call; the existing lazy spawn remains as a retry when eager
startup fails or the runtime is constructed outside an entered Tokio runtime.

| Measurement | Lazy cold host | Prewarmed host |
| --- | ---: | ---: |
| First Code Mode event | 301 ms | 1.7–2.7 ms |
| Top-level tool wall time | 312 ms | 11–13 ms |
| Nested `pwd` work | 8.9 ms | 8.9–9.8 ms |

All three prewarm trials reported `host.reused=true` on the first cell. Their
end-to-end totals remained model-bound and varied from 4.33 to 7.49 seconds,
so the defensible claim is removal of the measured local cold-host gap rather
than an end-to-end service speedup.

### Warmup A/B

An experimental build skipped only `perform_warmup` and sent the complete first
generation directly. The source patch was restored after measurement.

| Workload | Baseline median | No-warmup median | Interpretation |
| --- | ---: | ---: | --- |
| Five one-call turns | 2.021 s | 1.977 s | No material latency win |
| Three tool turns | 6.114 s | 5.272 s | Too variable at n=3; means differed by only 3.8% |
| First-call TTFE, one-call turns | 86 ms | 661 ms | Warmup moves connection/prefix work ahead of generation |

Removing warmup made the first generation absorb connection and prefix setup.
For the one-call workload, baseline mean was 2.106 s and no-warmup mean was
2.163 s. Warmup should not be described as approximately 900 ms of removable
latency. It does, however, report an additional 8,679 input tokens on every new
default session, with no cached or cache-write tokens in these trials. That is
worth a separate cost/accounting investigation.

### Local microbenchmarks

Selected Criterion results against the retained 195,172-byte one-turn event
stream:

| Operation | Time |
| --- | ---: |
| Encode a send-ready 128 KiB request | 67.5 us |
| Decode an 8 KiB streaming event | 1.58 us |
| Decode the complete trace with raw payloads | 118 us |
| Encode the complete trace with raw payloads | 10.2 us |
| Generic Tower dispatch | 12.7 ns |
| Tower concurrency-limit + timeout stack | 72.3 ns |

The JSON fmt subscriber produced roughly 4 KiB across 12 lines for the simple
turn. The contractual event stream was much larger because it preserved all
API events, but its complete raw-payload encoding cost was still about 10 us.
Neither tracing, event JSONL, request encoding, parsing, nor Tower dispatch is a
single-prompt bottleneck at these sizes.

A 1 kHz Samply profile of a 3.907 s tool turn recorded approximately 174 ms of
Nanocodex CPU, 42 ms of Node CPU, and 10 ms of shell CPU. That is about 6% of
one core in aggregate; the profile is retained as
`.nanocodex/profile-single-prompt/tool-cpu-profile.json` and can be opened with
`samply load`.

## Bottleneck ranking

1. **Model sampling and the extra post-tool sample.** Tool turns spend about
   5.3 s in two model calls. This is the dominant boundary by more than an order
   of magnitude over local parsing or dispatch.
2. **Fresh-session work.** A new process pays roughly 450 ms to connect, builds
   a server-side prefix, and has no generation cache hit. Callers that create
   one agent per prompt leave the main lifecycle/cache optimization unused.
3. **Cold Code Mode host: addressed on this branch.** Starting the host with the
   session reduced first-event latency from 301 ms to 1.7–2.7 ms and first tool
   wall time from 312 ms to 11–13 ms. The session contract assumes Code Mode is
   used, so there is no need to preserve lazy startup as the normal path.
4. **Warmup token accounting, not proven latency.** The warmup A/B does not
   justify deleting it for speed. Its extra input usage deserves verification
   for billing and cache behavior.
5. **Tool-surface size.** Removing two default tools saved 2,775 generation
   input tokens but did not win wall time in this sample. Keep the token saving
   in mind for cost/context, but do not claim a latency optimization yet.

Local Rust hot-path optimization is not currently supported by the evidence.
The next useful experiments are a larger paired warmup A/B with randomized arm
order, a longer first-tool prewarm distribution, and service TTFO distributions
over representative retained sessions.

## Reproduction

Build the symbolized optimized binary:

```sh
cargo build --locked --profile profiling --bin nanocodex
```

Capture events and the JSON fmt-subscriber trace separately:

```sh
./target/profiling/nanocodex run \
  --thinking low \
  --log-format json \
  --log-file .nanocodex/profile-single-prompt/run.trace.jsonl \
  'Reply with exactly PONG and no other text.' \
  > .nanocodex/profile-single-prompt/run.events.jsonl
```

Run the selected local benchmarks against that retained event stream:

```sh
NANOCODEX_BENCH_EVENTS=.nanocodex/profile-single-prompt/run.events.jsonl \
  cargo bench --locked -p nanocodex-service --bench tower_responses -- \
  'responses_request_encoding/encoded_send_ready/131072|responses_event_decoding/serde_json/8192|retained_agent_event_trace|responses_dispatch' \
  --noplot
```

Stdout remains contractual event JSONL. Fmt-subscriber diagnostics belong on
stderr or in `--log-file`; OTLP export adds visualization and aggregation but
does not expose timing data absent from these local spans.
