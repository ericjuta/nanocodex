# harness

A small Rust coding-agent harness built around Harbor and the OpenAI API.
The current milestone proves the local JSONL process and Harbor InstalledAgent
boundary without calling a model yet.

```sh
just bootstrap  # install pinned dependencies once
just run        # native cargo run; no Python, Docker, or Harbor
just eval       # fresh Terminal-Bench trial with canonical assertions
just view       # inspect retained Harbor jobs
```

`just eval` performs this path:

```text
native BuildKit compile -> static Linux binary
                       -> Harbor task container
                       -> /installed-agent/harness
                       -> Rust executes tools in /app
                       -> Harbor verifier
```

The Python `BaseInstalledAgent` shim only uploads and starts the executable,
then converts its retained JSONL to ATIF. It never dispatches tool calls.

For the local `fix-git` loop, Harbor builds a content-addressed native task
image with the pinned verifier dependencies already installed. The downloaded
benchmark task and its assertion file remain unchanged; only its dependency-
installing shell launcher is replaced by a direct `pytest` invocation.

## Build profiles

Local artifacts use Cargo's `dev` profile by default. Set this in `.env` for an
optimized build with full debug symbols:

```env
HARNESS_BUILD_PROFILE=profiling
```

## Eval selection

[`evals/terminal-bench-2.yaml`](evals/terminal-bench-2.yaml) selects datasets
and tasks. The current `fix-git` mode deliberately copies the two verifier
fixtures through a Rust shell call. Reward `1` therefore validates plumbing,
not autonomous task solving.

Every trial retains `input.jsonl`, `events.jsonl`, `stderr.log`, and
`trajectory.json` under `.harness/harbor/jobs`.
