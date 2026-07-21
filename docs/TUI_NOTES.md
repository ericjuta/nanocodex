# TUI notes

This document is the durable working set for Nanocodex's Ratatui consumer. It
records current behavior, the evidence retained from Amp and Codex, measured
performance constraints, and ideas that still need filtering. It is not a
commitment to implement every candidate.

Nanocodex remains a headless, library-first SDK. The TUI is a thin consumer of
the owned agent/session API and must not reshape the public library contract or
grow into an app-server protocol, approval system, or generic scheduler.

## Keybindings and commands

The implementation in `bin/nanocodex/src/tui/mod.rs` is the source of truth.

### Submission and pane control

| Input | Current behavior |
| --- | --- |
| `Enter` | Submit to the focused pane. While that pane is running, request a steer at the next safe model/tool boundary. |
| `Tab` with a non-empty draft | Explicitly queue the draft as a follow-on prompt. |
| `Tab` or `BackTab` with an empty draft | Toggle focus between the main and `/btw` panes when a side pane exists. |
| `Shift+Enter`, `Alt+Enter`, or `Ctrl+J` | Insert a newline. |
| `Esc` while idle | Clear the draft. |
| `Esc`, then `Esc` within one second while running | Cancel the focused turn. The first press arms target-scoped confirmation and preserves the draft. Repeated key events do not confirm cancellation. |
| `Ctrl+C` | Quit. |
| `Ctrl+D` with an empty draft | Quit. |

`Enter` and `Tab` deliberately differ while work is active: `Enter` is a steer
that may affect the current run, while `Tab` creates a later queued turn.

### Editing and history

| Input | Current behavior |
| --- | --- |
| `Left` / `Right` | Move the cursor by one Unicode scalar. |
| `Home` / `Ctrl+A` | Move to the start of the current input line. |
| `End` / `Ctrl+E` | Move to the end of the current input line. |
| `Backspace` / `Delete` | Delete before/under the cursor. |
| `Up` / `Ctrl+P` | Select the previous submitted draft. History navigation is disabled for multiline input. |
| `Down` / `Ctrl+N` | Select the next submitted draft, then restore the draft that preceded history navigation. |
| Terminal paste | Insert literal pasted text at the cursor after normalizing CRLF and CR to LF. |

### Transcript navigation

| Input | Current behavior |
| --- | --- |
| `PageUp` / `PageDown` | Scroll the focused transcript by 12 rows. |
| Mouse wheel | Scroll the focused transcript by 3 rows. |

### Slash commands

| Command | Current behavior |
| --- | --- |
| `/btw <question>` | Fork the latest safe mainline checkpoint into a side pane and submit the question there. Partial model output and unmatched tool calls are excluded. |
| `/btw` | Open an empty side fork, or focus the existing side pane. |
| `/close` | Close the `/btw` pane once it is idle. A busy pane is retained and reports why it cannot close. |
| `/cancel` | Cancel the focused turn without the two-stage Escape gesture. |
| `/trace` | Open Jaeger filtered to the focused session. A `/btw` trace becomes available after its fork has produced a session ID. |

Unknown slash-prefixed input is sent to the model as an ordinary prompt.

## Current design and retained practices

### State and rendering

- Transcript state is semantic: user, assistant, tool, and error entries are
  retained separately from their rendered cells.
- Streaming assistant deltas mutate the current assistant tail rather than
  adding one transcript entry per delta.
- Tool state is updated by call ID and distinguishes running, completed,
  cancelled, and failed calls.
- Wrapped entry height is cached for the current terminal width. A width change
  recomputes it.
- Rendering clips entry paragraphs to the visible viewport. Ratatui's buffer
  diff then writes only changed cells.
- Main and `/btw` conversations own independent transcripts, queues, statuses,
  and scroll offsets. The composer targets whichever pane has focus.
- Terminal setup uses synchronized updates, bracketed paste, mouse capture, and
  enhanced keyboard reporting where supported. Restoration is drop- and
  panic-safe.

### Scheduling

- Rendering is demand-driven rather than a permanent full-speed loop.
- Streaming events are coalesced behind an approximately 120 Hz maximum frame
  rate (`8.333334 ms`).
- Input and resize request an immediate frame and preempt a pending streaming
  deadline.
- The retained Codex workload's densest 33 ms bucket contained 590
  display-affecting records; the scheduler reduces that burst to frame-rate
  work rather than one render per record.

### Observability

TUI telemetry correlates each event's request ID and sequence across API delta
emission, event receipt, state application, and frame presentation. Frame
records include coalesced delta count, payload bytes, render duration, and
first/last-event-to-presentation latency. Full conversation content remains in
the agent lifecycle traces described in `docs/OBSERVABILITY.md`.

## Representative workload evidence

TUI work should be evaluated against retained representative workloads rather
than visual intuition alone. Raw traces and Amp exports stay outside Git;
committed fixtures may retain deterministic structural summaries only.

The benchmark shapes below were derived on 2026-07-20 from a long local Codex
rollout and the longest thread then returned by
`amp threads list --include-archived --json`. No prompts, arguments, results,
or other user content were retained.

| Shape | User messages / chars | Assistant messages / chars | Tool calls / argument chars |
| --- | ---: | ---: | ---: |
| `codex_long` | 78 / 30,486 | 964 / 308,701 | 3,471 / 1,438,038 |
| `amp_long` | 38 / 4,716 | 199 / 69,676 | 241 / 162,209 |

The Criterion suite in `bin/nanocodex/benches/tui_render.rs` measures:

- steady trace rendering at `80x24`, `120x40`, and `200x60`;
- first-frame construction at `120x40`; and
- a streaming delta appended to a 2 KiB assistant tail.

Run it with:

```sh
cargo bench -p nanocodex-bin --bench tui_render
```

Every performance slice should select applicable gates before implementation:

- event/state-update throughput;
- frame construction and layout time;
- frames rendered per event burst;
- changed-cell count and terminal output volume;
- allocations and retained memory;
- input-to-frame latency; and
- resize/reflow behavior.

Validate claimed improvements at multiple terminal sizes and at both the
streaming head and long-history tail. Use a focused synthetic case only to
isolate a demonstrated boundary.

## Amp findings retained

What survived from the Amp reverse-engineering work:

- The two-stage, one-second Escape gesture was adopted. Cancellation is scoped
  to the focused target, repeated key events do not accidentally confirm it,
  and an in-progress draft is preserved.
- Immediate steer and explicit queue are separate input intents.
- The side-question flow informs `/btw`: a question can branch from a safe
  checkpoint without interrupting the main line.
- Mature, long interactive threads informed the `amp_long` workload shape,
  particularly message wrapping, tool density, and long-session behavior.
- Input history, multiline composition, visible pending input, and concise
  footer hints are treated as daily-driver behavior rather than decoration.

What did not survive:

- There is no consolidated prose export of the earlier Amp discussion.
- The exact Amp thread ID used to derive `amp_long` was not retained.
- The current Amp thread titled "Nanocodex assistance" does not contain the
  missing research.

The structural benchmark summary and adopted behavior are therefore the durable
evidence. Future Amp research should record the export ID, date, observations,
and any sanitized derived fixture here while keeping the raw export outside
Git.

## Codex reference ideas

These are ideas observed in the local Codex checkout at the reviewed upstream
checkpoint `openai/codex@35eaf3ffb0bf2001486c68c47a3d946b34d16634`.
They are evidence and design input, not API requirements or automatic parity
work. The local checkout may be newer; advancing the reviewed checkpoint still
requires classifying every later upstream commit.

Relevant reference areas under `~/github/openai/codex/codex-rs/tui/src`:

- `app/agent_message_consolidation.rs`: consolidate transient streamed cells
  into canonical finalized message source while preserving resize re-rendering.
- `app/resize_reflow.rs`: explicit reflow state and resize behavior.
- `app/history_ui.rs`: history cells and terminal-native scrollback.
- Paste-burst handling that treats a large paste as one interaction instead of
  a rapid sequence of normal keypresses.
- Smooth streaming that drains display work at a controlled cadence.
- Markdown rendering, diff rendering, and table-aware presentation.
- Status indicator/shimmer and completion notifications such as BEL or OSC 9.
- Message-history lookup and rebuilding scrollback after clear or rollback.
- Compatibility handling for terminals with materially different scrollback or
  escape-sequence behavior, including Terminal.app and Warp.

Do not import Codex's app server, approval flow, generic history manager, or
multi-agent scheduler. Nanocodex should copy useful invariants and operational
behavior while retaining its much smaller consumer surface.

## Candidate backlog

Candidate IDs are stable handles for later filtering. `Now` means the idea fits
the current narrow Ratatui consumer; `Evaluate` requires evidence or a product
choice; `Defer` is intentionally outside the next slice.

| ID | Priority | Candidate | Evidence and acceptance boundary |
| --- | --- | --- | --- |
| `TUI-PERF-01` | Now | Add a long-history height/index strategy. | Rendering clips visible entries but still sums every entry and walks from the oldest on every frame. Compare a width-keyed cumulative index or bottom-up tail traversal on both representative shapes, scrolled and unscrolled, including resize invalidation. |
| `TUI-SCROLL-01` | Now | Preserve reading position while output streams. | New output should not drag a user who has scrolled upward back to the tail. Show that unseen output exists and provide an explicit jump-to-bottom path. Define anchoring across wrapped-height changes and resize. |
| `TUI-STREAM-01` | Evaluate | Make streaming versus sealed transcript entries explicit. | Finalization should stop invalidating old content and should retain canonical source suitable for resize/re-render. Measure allocations and streaming-tail frame time before adding abstraction. |
| `TUI-PASTE-01` | Evaluate | Handle paste bursts and very large drafts deliberately. | Preserve pasted bytes after newline normalization, keep the UI responsive, and decide whether the composer shows a compact placeholder for unusually large pastes. Do not silently truncate. |
| `TUI-RENDER-01` | Evaluate | Render assistant Markdown and useful tables. | Preserve selectable/copyable source and deterministic reflow. Benchmark long messages and avoid turning presentation into a new transcript contract. |
| `TUI-TOOL-01` | Evaluate | Improve tool-call presentation. | Explore collapsed/expanded arguments and results, duration, and clearer status without hiding failures or changing event semantics. |
| `TUI-NOTIFY-01` | Evaluate | Notify on completion while unfocused. | Detect focus state, make BEL/OSC 9 behavior configurable or terminal-safe, and never emit noisy notifications for every streaming event. |
| `TUI-SEARCH-01` | Evaluate | Add transcript search and copy-oriented navigation. | First define how matches interact with semantic entries, wrapped rows, two panes, and streaming updates. |
| `TUI-BTW-01` | Defer | Support multiple named `/btw` panes. | Product candidate already recorded in `docs/ORCHESTRATION_DECISION_CONTEXT.md`; preserve fresh driver/tool runtime ownership and explicit cleanup. This is broader than a rendering slice. |
| `TUI-BTW-02` | Evaluate | Make branch cancellation and close cleanup explicit. | Cancellation exists, but busy `/close` is rejected. Decide whether close should offer cancel-and-close while guaranteeing subprocess and driver cleanup. |
| `TUI-SNAPSHOT-01` | Defer | Restore durable conversations in the TUI. | Depends on the library's durable serializable conversation snapshot contract; the TUI must consume rather than invent it. |

## Suggested order

1. Baseline and implement `TUI-PERF-01`.
2. Specify scroll anchoring and unseen-output behavior in `TUI-SCROLL-01`.
3. Evaluate `TUI-STREAM-01` alongside those measurements; implement only if it
   removes demonstrated invalidation or allocation cost.
4. Choose one interaction slice from paste, Markdown, tool presentation, or
   notifications based on representative-session evidence.
5. Revisit multiple `/btw` panes only after the single-pane lifecycle and
   cleanup behavior are unambiguous.

## Source map

- Current input behavior: `bin/nanocodex/src/tui/mod.rs`
- TUI state and Amp Escape invariant: `bin/nanocodex/src/tui/app.rs`
- Transcript rendering and height cache: `bin/nanocodex/src/tui/transcript.rs`
- Layout and footer help: `bin/nanocodex/src/tui/view.rs`
- Render scheduling: `bin/nanocodex/src/tui/scheduler.rs`
- Terminal setup/restoration: `bin/nanocodex/src/tui/terminal.rs`
- Timing instrumentation: `bin/nanocodex/src/tui/telemetry.rs`
- Representative benchmarks: `bin/nanocodex/benches/tui_render.rs`
- TUI performance policy: `AGENTS.md`
- Branching context: `docs/ORCHESTRATION_DECISION_CONTEXT.md`
- Trace behavior: `docs/OBSERVABILITY.md`

Relevant implementation history:

- `f65e358 feat(cli): add ratatui daily driver`
- `6329113 perf(tui): coalesce streaming renders`
- `3c310bc test(tui): cover escape cancellation`
- `3957fb7 fix(tui): distinguish cancelled tools`
- `15a4c1d fix(tui): suppress cancellation error rows`
- `4adccc5 fix(tui): reconcile pending steer state`
