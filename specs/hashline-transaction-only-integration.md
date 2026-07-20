# Integrate Hashline transactions as Nanocodex's only structured editor

- Branch: `master` (planning document; implementation branch to be created when work starts)
- Status: Draft
- Owner(s): Nanocodex maintainers and Codex
- Created: 2026-07-20
- Last Updated: 2026-07-20
- Links: [Source design](../../codex/specs/hashline-transaction-greenfield.md) | [Source integration](../../codex/codex-rs/core/src/tools/handlers/hashline_transaction.rs) | [Nanocodex plan](../PLAN.md)

This ExecPlan is a living document. Keep `Progress`, `Surprises & Discoveries`,
`Decision Log`, and `Outcomes & Retrospective` current as research,
implementation, validation, and review proceed.
When the next milestone is clear, continue to it and update the spec instead of
asking for generic next steps.

## Purpose / Big Picture

Nanocodex currently exposes `apply_patch` as its structured file editor. The
handler parses a freeform patch and applies add, update, move, and delete hunks
one after another with ordinary filesystem calls. It does not validate every
mutation before the first write, bind a proposed edit to exact observed bytes,
or retain a recovery journal if the process dies partway through a multi-file
change.

After this work, native Nanocodex agents have one structured editing tool:
`hashline__transaction`. A call describes an explicit set of creates, updates,
deletes, and moves. The tool can preview the complete plan without writing,
commit immediately, or commit only the exact previously previewed plan. Linux
commits validate all inputs first, stage durable before/after evidence, journal
progress, apply guarded per-path mutations, and either finish, roll back, or
leave bounded evidence for deterministic recovery on the next transaction call.

This is a greenfield integration of the transaction subsystem only. It does not
port legacy `hashline.read`, `hashline.write`, `hashline.patch`,
`hashline.find_block`, `hashline.remove_file`, or `hashline.rename_file`. It
does not port Codex feature flags, environment selection, exec-server JSON-RPC,
sandbox approvals, or the legacy `hashline_only` visibility mode. There is no
additive mode: once the migration gate passes, `apply_patch` is removed from the
default registry and `hashline__transaction` is the only structured editor.

`exec_command` and the local Node.js Code Mode host remain intentionally
capable of writing files. Therefore “transaction only” means the only dedicated,
schema-described editing tool, not a filesystem security boundary. Enforcing
all writes through transactions would require a separate read-only shell and
Code Mode sandbox design.

Success means:

- Code Mode advertises `hashline__transaction` and no longer advertises
  `apply_patch`; public tool-description tests prove the exact schema and names.
- On Linux ext-family case-sensitive filesystems and tmpfs, preview performs no
  writes, immediate commit works, and `commitPreviewed` rejects a stale plan
  digest without changing any user file.
- One request can safely mix create, update, delete, and move mutations. A
  demonstrated failure after staging or during commit restores all before-images
  or leaves a journal that a fresh runtime recovers deterministically.
- Successful operations remove their staging, backup, reservation, and journal
  artifacts. Interrupted operations retain only the bounded evidence required
  for recovery.
- Non-Linux and unproven filesystem semantics compile and fail closed with a
  typed `Unsupported` result. They do not silently fall back to `apply_patch`.
- `cargo fmt`, warnings-denied workspace Clippy, workspace tests, public examples,
  a native CLI smoke, focused Harbor trials, and the configured milestone
  `just eval` pass with inspected JSONL, trajectories, and verifier output.
- The change does not add `unsafe` code, loosen `unsafe_code = "forbid"`, expose
  a feature flag, or add a provider/environment abstraction.

## Progress

- [x] (2026-07-20 21:20Z) Research current Nanocodex editing behavior, Codex
  transaction code, dependency seams, and relevant source history.
- [x] (2026-07-20 21:20Z) Define the transaction-only behavior contract,
  implementation milestones, baseline/eval strategy, recovery boundary, and
  no-go areas in this ExecPlan.
- [ ] Import and adapt the dependency-light transaction engine with deterministic
  planner, edit, preview, journal, rollback, and recovery tests.
- [ ] Implement the unsafe-free Linux filesystem capability and fail-closed
  unsupported-platform capability.
- [ ] Add the `hashline__transaction` handler and schema, then replace and delete
  the `apply_patch` structured-editor surface.
- [ ] Run focused, workspace, example, native smoke, recovery, and Harbor
  validation; inspect exact artifacts and record evidence here.
- [ ] Update the spec with final outcomes, adopted source provenance, residual
  platform risks, PR, commit, and rollout state.

## Surprises & Discoveries

- Observation: Nanocodex currently has three file-mutation paths, not one.
  `apply_patch` is the structured editor, `exec_command` can run arbitrary
  mutating shell commands, and Code Mode executes normal Node.js with filesystem
  access from the workspace.
  Evidence: `crates/nanocodex-tools/src/runtime.rs`,
  `crates/nanocodex-tools/src/apply_patch/mod.rs`,
  `crates/nanocodex-tools/src/shell/tool.rs`, and
  `crates/nanocodex-tools/src/code_mode/mod.rs`.

- Observation: the Codex transaction engine is separated from its tool adapter
  and native filesystem implementation, but the latter is coupled to
  exec-server environment/RPC types that Nanocodex does not need.
  Evidence: `../codex/codex-rs/hashline-transaction/src/lib.rs`,
  `../codex/codex-rs/core/src/tools/handlers/hashline_transaction.rs`, and
  `../codex/codex-rs/exec-server/src/hashline_transaction_fs.rs`.

- Observation: the Codex Linux adapter uses direct `libc` and audited local
  `unsafe` blocks, while Nanocodex forbids unsafe code workspace-wide.
  Evidence: `Cargo.toml` sets `unsafe_code = "forbid"`; direct calls occur in
  `../codex/codex-rs/exec-server/src/hashline_transaction_fs_linux*.rs`.

- Observation: locked `rustix` 1.1.4 and `nix` 0.30.1 expose safe wrappers for
  the required Linux primitives, including descriptor-relative open/stat/rename/
  unlink, `renameat2` flags, directory iteration, file locking, allocation,
  metadata restoration, filesystem inspection, and `FS_IOC_GETFLAGS`.
  Evidence: the local Cargo registry sources for `rustix-1.1.4` and `nix-0.30.1`;
  the repository and `/tmp` currently report ext-family filesystem magic.

- Observation: legacy Hashline supplied compact line hashes, but this integration
  deliberately omits that reader. The existing transaction contract still
  supports every edit variant; default model flows should use `replaceAll` after
  reading through `exec_command` or Node and compute `exactDigest` with SHA-256.
  Hash-anchored line edits remain available when the caller supplies the
  documented `xxh32(line, seed=0) & 0xffff` anchor.
  Evidence: `../codex/codex-rs/hashline-transaction/src/edits.rs` and
  `../codex/codex-rs/core/src/tools/handlers/hashline_transaction_spec.rs`.

- Observation: the last transaction-path change after the initial Linux feature
  chain was `49f8c4c8e5`, which completed fail-closed non-Linux trait
  implementations and gated Linux-only execution tests. No later commit through
  the inspected `../codex` HEAD `eff2c761e2bf3c644730edf795a8055b00818e92`
  changes the transaction paths.
  Evidence: path-limited `git log eca55b9f3e..HEAD` and inspection of
  `git show 49f8c4c8e5`.

## Decision Log

- Decision: expose one flat function tool named `hashline__transaction` and no
  legacy Hashline namespace siblings.
  Rationale: Nanocodex's Code Mode registry uses flat nested-tool names, and the
  product request is a greenfield transaction-only structured editor.
  Date/Author: 2026-07-20 / user and Codex

- Decision: preserve the source transaction actions `preview`, `commit`, and
  `commitPreviewed`, including camelCase `expectedPlanDigest` with the existing
  snake_case compatibility alias at deserialization.
  Rationale: previewed commit is the concurrency-safe review path, and the alias
  preserves the source's already-demonstrated schema/runtime compatibility fix.
  Date/Author: 2026-07-20 / Codex

- Decision: keep create, update, delete, move, `replaceAll`, `replaceLines`,
  `insertBefore`, and `insertAfter` in the engine and schema, while recommending
  `replaceAll` in the tool description when no line-hash producer is available.
  Rationale: this ports the complete transaction edit contract without importing
  the legacy reader family or inventing a second inspection tool.
  Date/Author: 2026-07-20 / Codex

- Decision: place the transaction engine, native filesystem capability, handler,
  and tests in private modules under `nanocodex-tools` rather than adding a new
  public crate.
  Rationale: the project boundary assigns built-in tools to `nanocodex-tools`;
  the engine is concrete implementation support and does not need a separately
  versioned public API in this slice.
  Date/Author: 2026-07-20 / Codex

- Decision: replace Codex `PathUri` and environment identifiers with one
  workspace-owned native transaction root. An optional model `root` is relative
  to the agent workspace; absolute roots and `..` escapes fail before planning.
  Rationale: Nanocodex owns one local workspace and explicitly does not want a
  generic environment/provider transport.
  Date/Author: 2026-07-20 / Codex

- Decision: preserve `unsafe_code = "forbid"` and adapt raw syscalls through safe
  `rustix`/`nix` APIs. Missing safe coverage blocks the Linux adapter milestone.
  Rationale: copying upstream `unsafe` or weakening the lint would violate a
  repository-wide invariant to obtain a pass.
  Date/Author: 2026-07-20 / Codex

- Decision: support commits initially on Linux ext-family case-sensitive
  directories and tmpfs, and use the complete unsupported implementation on
  other platforms/filesystems.
  Rationale: these are the only source semantics with a proven byte-exact path
  key; fail-closed behavior is safer than path aliasing or recovery corruption.
  Date/Author: 2026-07-20 / Codex

- Decision: retain recovery state on the same filesystem below
  `.nanocodex/hashline-transactions/` inside the selected transaction root and
  remove the directory when the last successful/recovered transaction is clean.
  Rationale: staging and atomic descriptor-relative renames require same-filesystem
  ownership. `.nanocodex/` is already ignored by this repository; external
  workspaces see it only while recovery evidence is needed.
  Date/Author: 2026-07-20 / Codex

- Decision: remove `apply_patch` only after transaction engine, native adapter,
  tool-schema, direct execution, and Code Mode integration gates pass in the same
  branch. Do not add a feature flag or compatibility fallback.
  Rationale: this preserves a usable editor while iterating but delivers the
  requested atomic final product and a simple Git-revert rollback.
  Date/Author: 2026-07-20 / user and Codex

- Decision: treat shell and direct Node writes as trusted escape hatches, not as
  violations of the transaction-only structured-editor contract.
  Rationale: constraining those general runtimes requires a separate sandbox and
  would materially broaden this integration.
  Date/Author: 2026-07-20 / user and Codex

## Outcomes & Retrospective

- Outcome: Draft ExecPlan created from live Nanocodex and Codex source evidence.
  Evidence: this file, Nanocodex HEAD
  `d2df7bfe25d05efc235f464aae38117708befe0e`, and inspected Codex HEAD
  `eff2c761e2bf3c644730edf795a8055b00818e92`.
  Remaining: all implementation and validation milestones.

## Context and Orientation

Read the root `AGENTS.md` before implementing. `nanocodex-tools` owns Code Mode,
built-in tools, the heterogeneous registry, subprocess lifecycle, and local file
operations. The higher `nanocodex` crate constructs one private `ToolRuntime` per
agent driver. Only the top-level `exec` and `wait` definitions are sent to the
Responses API; `exec` describes nested tools and dispatches calls such as
`tools.exec_command(...)` through `ToolRegistry`.

Current structured editing is implemented by:

- `crates/nanocodex-tools/src/runtime.rs`: registers `ApplyPatchHandler` in every
  native `ToolRuntime` and reserves the `apply_patch` built-in name.
- `crates/nanocodex-tools/src/apply_patch/mod.rs`: parses and sequentially applies
  add, update, delete, and move hunks with `std::fs`.
- `crates/nanocodex-tools/src/apply_patch/{parser.rs,seek_sequence.rs,streaming_parser.rs}`
  and `apply_patch.lark`: freeform grammar and patch parsing.
- `crates/nanocodex-tools/src/code_mode/description.rs`: renders every registered
  nested tool into the model-visible `exec` description.
- `crates/nanocodex-tools/src/code_mode/tests.rs`: proves freeform
  `tools.apply_patch(...)` dispatch and the generated declaration.

The source transaction system has three layers:

1. `../codex/codex-rs/hashline-transaction/src/` owns typed requests, limits,
   exact-byte observation, planning, line edit compilation, deterministic plan
   digests, previews, journals, execution, rollback, and recovery.
2. `../codex/codex-rs/exec-server/src/hashline_transaction_fs*.rs` owns native
   filesystem semantics, capability negotiation, descriptor-relative path
   traversal, locking, storage, guarded mutations, and durable recovery.
3. `../codex/codex-rs/core/src/tools/handlers/hashline_transaction{,_spec}.rs`
   translates the model schema into engine requests, routes actions, and bounds
   model-visible output to 8 KiB.

This plan adopts the behavior of the feature chain through `eca55b9f3e` and the
non-Linux completeness fix `49f8c4c8e5`. It does not advance the repository's
general Codex parity checkpoint; it pins only the source paths named above. A
future parity review must still follow the root `AGENTS.md` checkpoint rules.

Terms used here:

- A mutation is one explicit create, update, delete, or move operation.
- An expected file digest is the lowercase 64-character SHA-256 digest of the
  exact bytes the caller believes are present.
- A plan digest binds the root identity, canonical paths, exact before/after
  bytes, metadata/identity evidence, and mutation variants.
- Validation atomicity means every mutation is valid before the first visible
  user-file write.
- Recoverable means an interrupted commit converges to all-before, all-after, or
  a durable non-destructive state requiring explicit diagnosis. It does not mean
  simultaneous visibility of every path to unrelated readers.

Relevant repository rules:

- Follow `PLAN.md` in order and build a vertical library slice with a real
  consumer; the CLI and Harbor adapter are validation boundaries.
- Keep `nanocodex-tools` useful without importing `nanocodex`.
- Add focused deterministic tests for public contracts and demonstrated
  regressions, then compile public examples.
- Do not weaken lint, tests, verifiers, benchmark tasks, or acceptance criteria.
- Inspect exact JSONL, Harbor results, trajectories, and verifier output before
  claiming an eval result.
- Do not expose raw transport IDs, secrets, prompts, tool argument contents, or
  file contents through tracing.

Assumptions:

- Initial mutation support targets Linux. Non-Linux builds must remain green and
  return a deterministic unsupported error from the same tool schema.
- The chosen root resides on an ext-family case-sensitive directory or tmpfs. If
  not, planning and commit fail before mutation with the observed filesystem type.
- The source and destination licenses remain compatible (`../codex` Apache-2.0;
  Nanocodex MIT OR Apache-2.0). Preserve source provenance in a concise notice.
- The model can inspect files with `exec_command` or Node and compute exact
  SHA-256 digests. No legacy Hashline reader is added as a fallback.

## Plan of Work

### Milestone 1: Freeze baseline and import the pure transaction engine

Scope: record current tool behavior, then adapt the dependency-light engine
without registering it or changing the default editor. This isolates semantic
porting from filesystem and model-behavior changes.

Files and interfaces:

- `specs/hashline-transaction-only-integration.md`: record baseline commands,
  exact outputs, source classification, and any changed decisions.
- `Cargo.toml` and `Cargo.lock`: add direct `xxhash-rust` and any engine-only
  dependencies at pinned workspace-compatible versions.
- `crates/nanocodex-tools/src/hashline_transaction/mod.rs`: private subsystem
  entry point.
- `crates/nanocodex-tools/src/hashline_transaction/engine/*.rs`: adapted planner,
  edit, preview, journal, executor, rollback, recovery, limits, and typed state.
- `crates/nanocodex-tools/src/hashline_transaction/engine/*_tests.rs`: port the
  complete deterministic engine tests before changing behavior.
- `THIRD_PARTY_NOTICES.md` or the repository's chosen equivalent: record derived
  source paths and checkpoint without copying task history into code comments.

Work:

Record the current `exec` description and focused Code Mode test proving
`apply_patch` is present. Run one current-model baseline on
`terminal-bench/large-scale-text-editing` with the exact existing configuration,
retain the Harbor job, and inspect whether it uses `apply_patch`, shell writes,
or Node writes. This baseline is evidence, not a gate on the port.

Copy semantic behavior rather than Codex orchestration. Preserve typed mutation
variants, hard limits, exact-byte SHA-256 evidence, xxh32 line anchors,
canonical conflict detection, deterministic plan digests, bounded preview
serialization, journal state transitions, rollback, and recovery. Replace
`PathUri` and environment IDs with private Nanocodex root types while retaining
stable digest inputs. Do not import exec-server protocol, RPC error codes,
environment selection, approvals, sandboxing, feature flags, or Codex tool traits.

Acceptance:

- `cargo test -p nanocodex-tools hashline_transaction::engine` passes all ported
  planner, edits, preview, journal, executor, rollback, and recovery tests.
- Repeating a deterministic plan fixture produces the same 64-character plan
  digest; changing exact bytes, metadata evidence, root identity, path, action,
  or mutation changes the digest.
- `rg '\bunsafe\b|libc::' crates/nanocodex-tools/src/hashline_transaction` finds
  no implementation use.
- Existing `freeform_apply_patch_accepts_a_string` and model-description tests
  remain unchanged and pass at this milestone.

### Milestone 2: Implement the unsafe-free native filesystem capability

Scope: make preview, execution, journaling, rollback, and recovery operate on the
local Nanocodex workspace with the source Linux guarantees. No model tool is
registered yet.

Files and interfaces:

- `crates/nanocodex-tools/Cargo.toml`: add direct `rustix` 1.1.4 with required
  filesystem/process features and expand the existing `nix` feature set only for
  safe directory iteration or primitives not exposed by `rustix`.
- `crates/nanocodex-tools/src/hashline_transaction/fs.rs`: platform-neutral
  capability selection and fail-closed unsupported implementation.
- `crates/nanocodex-tools/src/hashline_transaction/fs_linux/*.rs`: root/path
  handles, evidence, semantics, coordination, storage, guarded mutation, and
  recovery, split so production modules remain near the repository's size limit.
- `crates/nanocodex-tools/src/hashline_transaction/fs_*_tests.rs`: native,
  unsupported-platform, race, fault-injection, and restart coverage.

Work:

Translate every direct `libc` operation to a safe API. Use descriptor-relative
operations and owned descriptors throughout; never canonicalize a model path and
later reopen it by ambient string. Reject empty paths, absolute paths, `..`,
symlinks, directories, devices, hard links, non-UTF-8 contents, duplicate source
or destination keys, unstable metadata, and unproven directory lookup semantics.

Store reservations, staged files, backups, and versioned bounded journals below
`.nanocodex/hashline-transactions/` on the transaction root's filesystem. Lock
all participating parent directories in canonical order. Revalidate root-to-parent
edges, file identity, metadata, link count, and exact bytes while holding the
lease. Sync staged files, backups, journals, storage directories, every changed
parent, and terminal state in source order. Clean successful artifacts and empty
internal directories. On startup-equivalent first use, scan bounded pending
journals and recover them before accepting a new commit for that root.

The first implementation must not weaken capability negotiation to accommodate
the development host. A missing safe wrapper, unsupported filesystem, failed
directory flag check, or missing durability primitive returns `Unsupported` and
blocks this milestone rather than introducing local `unsafe` or path-based races.

Acceptance:

- Focused Linux tests prove ext-family and tmpfs capability acceptance and
  deterministic rejection of casefolded/unrecognized filesystems.
- Fault injection at every journal, stage, backup, guarded mutation, sync,
  rollback, and cleanup transition yields all-before, all-after, or a recoverable
  evidence-preserving state; no test accepts a mixed unjournaled result.
- A subprocess restart test kills execution after each durable transition, starts
  a fresh runtime, invokes recovery, and proves terminal convergence plus cleanup.
- Two runtimes racing on overlapping paths serialize or return a typed conflict;
  disjoint transactions can proceed without sharing mutable global state.
- Workspace Clippy with warnings denied still enforces `unsafe_code = "forbid"`.
- Non-Linux compile checks instantiate the complete capability traits and return
  `Unsupported` without conditional test failures.

### Milestone 3: Expose the single transaction tool through Code Mode

Scope: add the complete model schema and in-process handler while retaining
`apply_patch` temporarily for an A/B regression window inside the branch.

Files and interfaces:

- `crates/nanocodex-tools/src/hashline_transaction/tool.rs`: `Tool`
  implementation named `hashline__transaction`.
- `crates/nanocodex-tools/src/hashline_transaction/schema.rs`: closed JSON schema
  for actions, mutations, expected files, line anchors, edit lists, and root.
- `crates/nanocodex-tools/src/hashline_transaction/tool_tests.rs`: decode,
  output-bound, failure mapping, tracing, and direct handler tests.
- `crates/nanocodex-tools/src/runtime.rs`: register the handler with the workspace
  root and reserve its built-in name.
- `crates/nanocodex-tools/src/code_mode/tests.rs`: prove generated TypeScript,
  nested dispatch, preview/commit, and stale-preview behavior.
- `crates/nanocodex-tools/tests/tracing.rs`: prove transaction spans contain only
  structural counts, durations, digest-safe status, and no contents or arguments.

Work:

Preserve the source wire contract minus `environment_id`. The top-level input is:

    {
      "action": { "type": "preview" }
        | { "type": "commit" }
        | { "type": "commitPreviewed", "expectedPlanDigest": "<sha256>" },
      "root": "optional/relative/root",
      "mutations": [create | update | delete | move]
    }

Existing files require `{ "exactDigest": "<exact-byte-sha256>" }`. Updates and
moves accept ordered `replaceAll`, `replaceLines`, `insertBefore`, and
`insertAfter` edits. The handler converts strings to UTF-8 bytes, applies trusted
internal limits rather than model-supplied limits, runs blocking filesystem work
off the async runtime, normalizes typed failures without flattening conflicts,
and caps exact serialized model output at 8 KiB by truncating preview content
before structural evidence.

The description must say that this is Nanocodex's structured editing tool,
recommend preview plus `commitPreviewed` for reviewed or high-risk batches,
recommend `replaceAll` when only exact digest evidence is available, state the
recovery/visibility guarantee precisely, and avoid claiming database isolation.

Acceptance:

- `ToolRuntime::model_specs()` includes one declaration for
  `tools.hashline__transaction(args)` with the complete closed schema.
- A Code Mode cell previews a mixed transaction with no write, commits it using
  the returned camelCase `planDigest`, and observes exactly one nested tool call
  per invocation with bounded output.
- Both `expectedPlanDigest` and the compatibility input
  `expected_plan_digest` deserialize, while generated schema advertises only
  camelCase.
- Invalid JSON, unknown fields, invalid digest width, stale exact content,
  conflicting paths, unsupported roots/filesystems, and output overflow return
  bounded failed tool results without partial mutation.
- `apply_patch` tests still pass during this milestone so A/B failures can be
  attributed to the new system rather than simultaneous deletion.

### Milestone 4: Remove `apply_patch` and prove transaction-only behavior

Scope: make the new tool the only structured editor, delete dead patch code, and
exercise a real consumer before broad validation.

Files and interfaces:

- `crates/nanocodex-tools/src/runtime.rs`: remove `ApplyPatchHandler` and the
  reserved `apply_patch` name; keep `hashline__transaction` registered by default.
- `crates/nanocodex-tools/src/lib.rs`: remove the `apply_patch` module.
- `crates/nanocodex-tools/src/apply_patch/`: delete the handler, parser, grammar,
  seek helper, streaming parser, and their tests after replacement gates pass.
- `crates/nanocodex-tools/src/code_mode/tests.rs`: replace apply-patch assertions
  with exact transaction-only declaration and behavior assertions.
- `bin/nanocodex/` and `examples/`: add or update the thinnest real consumer smoke
  needed to prove the default runtime; do not move tool behavior into the CLI.

Work:

Delete the complete `apply_patch` surface rather than retaining an adapter or
hidden fallback. Assert that model-visible Code Mode metadata lacks `apply_patch`
and includes `hashline__transaction`. Preserve `exec_command`, `write_stdin`,
plan, image, web, and application-defined tools unchanged. Run a temporary
workspace CLI smoke that reads exact bytes through shell, previews a transaction,
commits the previewed digest, verifies the result, and removes both the user file
and any empty transaction sidecar.

Run focused Harbor trials on existing frozen tasks that exercise different edit
shapes without modifying task instructions or verifiers:

- `terminal-bench/large-scale-text-editing` for sustained file changes;
- `terminal-bench/fix-git` for repository-aware mutation;
- `terminal-bench/polyglot-rust-c` for multi-file source and build validation.

For each retained job, inspect the agent JSONL, ATIF trajectory, result, verifier
output, timing, token/cache usage, and tool calls. Record whether the model used
`hashline__transaction`, shell writes, or Node writes. A task pass without a
transaction call is still useful product evidence but does not prove adoption;
at least one representative task must demonstrate the new tool end to end.

Acceptance:

- `rg -n 'apply_patch' crates/nanocodex-tools/src` returns no production surface;
  intentional historical/spec references are separately reviewed.
- Code Mode advertises `hashline__transaction` and not `apply_patch` in exact
  serialized model input.
- The native CLI smoke completes a previewed create/update/delete cycle, leaves
  the workspace and transaction sidecar clean, and emits no tool diagnostics on
  stdout outside contractual JSONL.
- The three focused Harbor jobs have retained, inspected evidence; at least one
  successful trajectory calls `hashline__transaction` and no trajectory can call
  the removed `apply_patch` tool.

### Milestone 5: Complete regressions, release proof, and documentation

Scope: run the full repository gate, inspect artifacts, and leave a reviewable
rollback/provenance record.

Files and interfaces:

- `PLAN.md`: record the completed vertical slice at the correct active-roadmap
  position only after implementation and gates finish.
- `crates/nanocodex-tools/CHANGELOG.md` and root `CHANGELOG.md`: add release-facing
  behavior when the branch enters release preparation.
- This ExecPlan: fill Progress, discoveries, exact commands, outcomes, residual
  risks, source classification, PR, and commit evidence.

Work:

Run rustfmt, warnings-denied Clippy, workspace tests, public-example compilation,
adapter tests, native smoke, and the full configured `just eval` milestone gate.
Inspect the final git diff/status and every generated or lockfile change. Compare
the full eval to the recorded baseline without claiming causality from one task;
report pass/fail deltas, tool adoption, cost, latency, and unsupported-platform
coverage separately.

Acceptance:

- `just check` passes without skipped transaction tests on the Linux gate host.
- Public examples compile and the standard native `just run` smoke succeeds.
- Full `just eval` finishes with exact retained job path and inspected per-task
  results; failures are classified rather than hidden or retried into a pass.
- `cargo package --locked --allow-dirty --no-verify -p nanocodex-tools` succeeds
  if packaging is part of the current release gate and contains every required
  source/notice file.
- Final `git diff --check`, `git diff --stat`, and `git status --short` show only
  intentional source, test, spec, notice, changelog, and lockfile changes.

## Interfaces and Dependencies

Local interfaces:

- `hashline_transaction::engine::TransactionRequest`:
  - Inputs: trusted root identity, action, explicit mutation list, and internal
    hard limits.
  - Outputs: an immutable planned transaction with exact before/after evidence,
    canonical summary, and deterministic SHA-256 plan digest.
  - Failures: typed invalid request, unsupported capability, conflict/staleness,
    limit, filesystem, execution, rollback, or recovery errors.

- `hashline_transaction::engine::PlanningFileSystem`:
  - Inputs: native root plus model-relative paths and observation limits.
  - Outputs: executor-owned handles, canonical path keys, exact bytes, file
    identity, metadata fingerprint, link evidence, and root identity.
  - Failures: fail-closed unsupported path semantics, symlink/non-file/hard-link
    rejection, bounded-read failure, or observed race.

- `hashline_transaction::engine::TransactionFileSystem`:
  - Inputs: canonical transaction plan and guarded storage/mutation/recovery
    operations.
  - Outputs: durable staged files, backups, journal generations, mutation
    receipts, rollback receipts, and terminal cleanup.
  - Failures: typed conflict, platform, capacity, sync, recovery-required, or
    unsupported capability. No silent best-effort mutation is allowed.

- `HashlineTransactionHandler` implementing `Tool`:
  - Inputs: function JSON matching the closed camelCase schema.
  - Outputs: `ToolExecution` containing bounded JSON preview/result and success
    status; Code Mode receives the structured JSON value.
  - Failures: model-recoverable bounded text/JSON with a stable error category;
    no panic, `unwrap`, hidden partial write, or raw internal handle.

- `ToolRuntime`:
  - Inputs: the agent workspace at construction.
  - Outputs: Code Mode registry containing `hashline__transaction` by default.
  - Failures: unsupported platform/filesystem is reported when the tool executes,
    not hidden by omitting the schema or falling back to another editor.

External dependencies:

- `openai/codex` transaction source:
  - Version/source checked: feature chain through `eca55b9f3e`, non-Linux fix
    `49f8c4c8e5`, and path review through local HEAD
    `eff2c761e2bf3c644730edf795a8055b00818e92`.
  - Expected behavior: validation-first mixed file transactions, exact-byte and
    metadata binding, durable journal, guarded mutation, rollback, and recovery.
  - Failure handling: port semantics and tests; do not import unrelated runtime,
    RPC, feature, environment, or approval layers.

- `rustix`:
  - Version/source checked: locked local registry source `1.1.4`.
  - Expected behavior: safe owned-fd wrappers for Linux descriptor-relative file
    operations, filesystem flags/type, locking, allocation, and metadata changes.
  - Failure handling: translate `Errno` into typed platform/unsupported/conflict
    categories. Missing required safe API blocks the milestone.

- `nix`:
  - Version/source checked: direct workspace dependency `0.30.1` and local
    registry source.
  - Expected behavior: safe `Dir` iteration and only those fd-based operations
    not cleanly supplied by `rustix`.
  - Failure handling: avoid duplicate wrappers; do not invoke generated unsafe
    ioctl functions when `rustix::fs::ioctl_getflags` is available.

- `xxhash-rust`:
  - Version/source checked: source uses workspace `0.8`; pin a compatible exact
    workspace requirement and enable `xxh32` only.
  - Expected behavior: four-hex line anchor from `xxh32(line bytes, 0) & 0xffff`.
  - Failure handling: deterministic engine tests freeze the algorithm and casing.

- `sha2`:
  - Version/source checked: existing workspace dependency `0.10.9`.
  - Expected behavior: exact-byte and plan SHA-256 encoded as fixed-width
    lowercase hexadecimal.
  - Failure handling: invalid input digest widths/casing decode to typed invalid
    requests before filesystem mutation.

## Concrete Steps

Update these commands if implementation reveals a more precise focused target.
From the branch checkout root
(`/home/ericjuta/.openclaw/workspace/repos/nanocodex`):

    git status --short --branch
    git -C ../codex log --oneline eca55b9f3e..HEAD -- \
      codex-rs/hashline-transaction \
      codex-rs/core/src/tools/handlers/hashline_transaction.rs \
      codex-rs/core/src/tools/handlers/hashline_transaction_spec.rs

    cargo test -p nanocodex-tools hashline_transaction::engine
    cargo test -p nanocodex-tools hashline_transaction::fs
    cargo test -p nanocodex-tools hashline_transaction::tool
    cargo test -p nanocodex-tools code_mode

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    cargo check --workspace --all-targets

    just run
    just eval-task task=terminal-bench/large-scale-text-editing effort=low
    just eval-task task=terminal-bench/fix-git effort=low
    just eval-task task=terminal-bench/polyglot-rust-c effort=low
    just eval

    rg -n '\bunsafe\b|libc::' crates/nanocodex-tools/src/hashline_transaction
    rg -n 'apply_patch' crates/nanocodex-tools/src
    git diff --check
    git diff --stat
    git status --short

Expected evidence after implementation:

    test result: ok. <focused transaction test counts recorded here>
    Finished `dev` profile ... cargo clippy ... -D warnings
    hashline__transaction preview: planDigest=<64 lowercase hex>, no files changed
    hashline__transaction commitPreviewed: outcome=<committed>, sidecar removed
    unsupported: native Hashline transactions require proven Linux filesystem semantics
    Harbor evidence is not available in this draft. Replace this line after the
    run with the exact retained job path, task, reward, and observed tool calls.

## Validation and Acceptance

Automated validation:

- Engine tests cover normal create/update/delete/move, mixed transactions, exact
  digest mismatch, all edit variants, BOM/EOL preservation, invalid UTF-8,
  overlapping edits, path conflicts, limits, preview truncation, journal decode,
  rollback, recovery, and deterministic digests.
- Native adapter tests cover symlinks, directories, hard links, parent replacement,
  byte-exact lookup, casefold rejection, descriptor identity, metadata changes,
  cross-process locks, allocation, sync ordering, cleanup, and every injected
  durable transition.
- Tool tests cover the exact model schema, camelCase compatibility, root
  containment, output bounds, structured Code Mode return, tool events, tracing
  redaction, and typed failures.
- Workspace tests and examples prove shell sessions, custom tools, MCP, web/image
  tools, event ordering, cancellation, reconnect/replay, compaction, forks, and
  bindings remain unchanged.

Manual or runtime validation:

- Start: build the current branch with `cargo build --locked -p nanocodex-bin`.
- Exercise: use a disposable Git workspace and the CLI/agent to preview and
  preview-commit one mixed create/update/delete/move transaction.
- Observe: preview makes no changes; commit output contains the same plan digest;
  file bytes and Git diff match the request; no transaction sidecar remains.
- Interrupt: run the deterministic subprocess harness at a documented durable
  transition, terminate it, start a fresh runtime, and invoke a transaction.
- Observe: recovery converges to a declared terminal outcome, retains unknown
  external content rather than overwriting it, and cleans recoverable artifacts.

Model-behavior evaluation set:

- Representative: existing `large-scale-text-editing`, `fix-git`, and
  `polyglot-rust-c` tasks, using frozen task definitions and the same model,
  effort, provider, tool, and verifier settings for baseline and candidate.
- Edge: stale expected bytes, stale preview digest, mixed operations, large
  escaped preview content, and cancellation after a durable journal transition.
- Adversarial: symlink/hard-link replacement, parent directory swap, duplicate
  destinations, casefolded/unproven filesystems, malformed journals, and external
  file modification during planning/commit.
- Holdout: reserve at least one existing multi-file Harbor task not named in the
  model tool description or focused examples; record it here before the candidate
  run and do not tune from its output.

Acceptance rubric:

- Correctness: no accepted request violates exact preconditions or leaves an
  unjournaled mixed result.
- Recovery: every injected interruption reaches all-before, all-after, or a
  typed evidence-preserving state on a fresh runtime.
- Behavior: the only structured editor in exact model input is
  `hashline__transaction`; at least one retained model trajectory uses it
  successfully.
- Regression: focused and full repository gates pass; benchmark deltas are
  reported honestly and no task/verifier is changed to obtain a pass.
- Safety: no new unsafe code, secret/content tracing, absolute/root-escape path,
  silent fallback, or unsupported-filesystem mutation.
- Operations: successful/recovered transactions clean their internal artifacts,
  while unresolved evidence remains bounded and diagnosable.

Regression checks:

- Sequential turns retain the same tool runtime, shell sessions, Code Mode host,
  prompt/cache identity, response chain, and event behavior.
- Cancellation still terminates subprocess descendants; transaction recovery
  state survives cancellation or process death independently.
- Caller-defined tools and dynamic MCP providers still compose with built-ins and
  reject duplicate tool names deterministically.
- WASM builds continue excluding native tools without importing Linux-only types.
- CLI stdout remains contractual JSONL only and diagnostics remain on stderr.

## Idempotence and Recovery

- Re-running preview is safe because it performs bounded observation and planning
  only. Identical root/file evidence and request yield the same plan digest.
- Re-running immediate commit after success normally fails its original
  preconditions rather than applying the same mutation twice.
- Re-running `commitPreviewed` after any observed change fails the expected plan
  digest and performs no user-file mutation.
- If execution fails before the first durable journal, remove only verified
  unreferenced staging artifacts. If it fails after journaling, do not manually
  delete evidence; invoke bounded recovery through a fresh runtime.
- Recovery must be idempotent. Repeating it after any partial recovery transition
  resumes from the latest valid journal generation and never trusts an
  unvalidated artifact name or path.
- Successful terminal cleanup may be repeated and treats already-removed owned
  artifacts as success only when journal evidence proves ownership.
- For manual test cleanup, first prove all user files match a terminal state,
  then remove only the disposable workspace. Never teach operators to delete a
  live `.nanocodex/hashline-transactions/` directory in place.
- Backout plan: revert the cohesive integration commits to restore the previous
  `apply_patch` default. There is no runtime feature toggle. Before reverting a
  deployed binary, let the new binary recover all pending journals or retain it
  as a recovery utility; the old binary cannot interpret new transaction state.

## Rollout and Operations

- Feature flags/config/env vars: none. `hashline__transaction` replaces
  `apply_patch` in the default native tool registry atomically at release.
- Migration/backfill: none for user data. Existing workspaces normally contain
  no transaction state. Do not deploy an older binary over unresolved new
  journals.
- Platform rollout: Linux proven filesystems first. Non-Linux and unproven
  filesystems expose the same schema but fail closed; no compatibility editor is
  restored automatically.
- Monitoring: add tracing spans for planning, recovery scan, staging, commit,
  rollback, cleanup, and total tool duration. Record operation counts, byte
  counts, outcome/error category, recovery-required state, and durations; do not
  record paths, contents, tool arguments, or journal payloads.
- Healthy values: zero unresolved recovery-required outcomes after burn-in, zero
  successful calls with retained artifacts, bounded preview/output sizes, and no
  mixed-state invariant failures.
- PR workflow: implement milestone-sized commits, keep this plan current per
  commit, inspect the complete diff, run the milestone gates, then open one
  reviewable PR with explicit source provenance and rollback notes. Do not push,
  merge, or deploy without user direction.

## Risks and Open Questions

- Risk: safe wrappers may not reproduce every upstream descriptor and durability
  semantic exactly.
  Mitigation: map every direct `libc` call to a reviewed safe primitive, port the
  fault/race tests, and block the native milestone rather than allow `unsafe` or
  path-based substitutes.

- Risk: removing legacy Hashline read means the model has no built-in producer
  for compact line anchors.
  Mitigation: preserve all engine variants, recommend `replaceAll` with exact
  SHA-256 in the initial tool description, and evaluate real token/cost impact.
  Add no inspection action in this scope; revisit only from measured failures.

- Risk: removing `apply_patch` on unsupported platforms leaves no structured
  editor.
  Mitigation: make this limitation explicit in schema output, CLI documentation,
  release notes, and tests. Cross-platform transaction adapters are separate
  milestones, not a hidden fallback.

- Risk: internal recovery artifacts can appear in external repository status
  after a crash.
  Mitigation: remove empty sidecars after success/recovery, label artifacts
  clearly, bound them, document diagnosis, and never auto-add repository ignore
  entries.

- Risk: cancellation can drop the caller while blocking filesystem work continues.
  Mitigation: once a journal is durable, make completion/recovery ownership
  independent of the response future; on drop, leave a valid journal rather than
  claiming cancellation rolled back.

- Risk: the model may continue editing through shell or Node rather than using
  the new structured tool.
  Mitigation: make the tool description direct, inspect trajectories, and report
  adoption separately from task pass rate. Enforcement is explicitly out of scope.

- Risk: large `replaceAll` requests may increase tokens and disturb stable prompt
  prefixes or tool-output budgets.
  Mitigation: tool definitions remain byte-stable across turns, request/output
  limits are hard, previews truncate content before evidence, and Harbor compares
  token/cache/latency data against the frozen baseline.

- Open question: which existing multi-file Harbor task should be reserved as the
  holdout before implementation begins?
  Owner/next step: implementer selects one frozen task before candidate runs,
  records it in this spec, and does not tune from it.

- Open question: does packaging require a root notice file or per-module source
  attribution under the repository's release process?
  Owner/next step: inspect current packaging contents and license practice during
  Milestone 1, then record the chosen artifact in the Decision Log.

## Artifacts and Notes

Initial live evidence gathered for this draft:

    Nanocodex HEAD: d2df7bfe25d05efc235f464aae38117708befe0e
    Codex HEAD inspected: eff2c761e2bf3c644730edf795a8055b00818e92
    Last relevant non-Linux fix: 49f8c4c8e5a849d5efa1c8f423dfd0971c574a30
    Source digest-field fix: eca55b9f3e
    Development repo filesystem: ext-family magic 0xef53
    Nanocodex unsafe lint: unsafe_code = "forbid"
    Safe syscall sources inspected: rustix 1.1.4, nix 0.30.1

The baseline model run has not been executed for this draft. Record its exact
job path, model/provider/settings, tool calls, score, cost, latency, and retained
artifacts before changing model-visible tool registration.

## Revision Notes

- 2026-07-20: Created the transaction-only integration ExecPlan from the Codex
  template, live Nanocodex/Codex source inspection, source-history classification,
  and current dependency/test/eval surfaces.
