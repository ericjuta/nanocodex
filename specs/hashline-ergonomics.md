# Improve Hashline ergonomics without weakening edit safety

- Branch: `master`
- Status: Ready for Implementation
- Owner(s): Eric Juta / implementing agent
- Created: 2026-07-23
- Last Updated: 2026-07-23
- Links: [Active Plan](../PLAN.md) | [Hashline README](../README.md#tools-mcp-events-and-errors)

This ExecPlan is the source of truth for implementation. Keep `Progress`,
`Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective`
current. A fresh agent should continue the next unchecked milestone instead of
reconstructing this proposal from chat history.

## Purpose / Big Picture

Hashline already provides stale-safe UTF-8 reads, anchored routine patches, and
crash-recoverable typed transactions. A disposable live smoke on 2026-07-23
confirmed bounded reads, Rust/Markdown block lookup, Unicode byte identity,
single- and multi-file patches, stale-evidence rejection, exact preview replay,
transaction create/update/move/delete, and final Rust compilation.

The smoke also exposed friction: the schema says “block forms” without naming
them; patch programs cannot select a compact root; a minimal diff can hide a
matching delimiter as unchanged context; bounded reads cannot separately say
that the file continues; missing transaction parents produce a vague error; and
retained terminal receipts are only tersely described.

After this change:

- The schema and parser errors expose every accepted patch spelling.
- Reads add `has_more` without changing `truncated` semantics.
- Patches may opt into strict root-relative section paths.
- Bounded previews show one line of context and edited-move previews.
- Transaction responses explain bounded recovery receipts.
- A separately gated final milestone may create destination parents only with a
  crash-safe directory journal.

Non-goals are a second editing protocol, generic filesystem API, unbounded
diffs, mutable preview registry, receipt-deletion action, new dependency,
non-Linux durable backend, or weakened file/line/block/digest/root/recovery
validation. `commitPreviewed` remains stateless and continues to require the
typed mutations plus `expectedPlanDigest`.

Success means focused Hashline tests and the complete workspace gate pass, old
callers retain their behavior, and each new contract is directly visible in
tool output or deterministic tests.

## Progress

- [x] (2026-07-23 11:00Z) Run the disposable live smoke and remove its files and
  recovery records.
- [x] (2026-07-23 11:00Z) Inspect parser, schemas, read pagination, previews,
  requests, directory leases, recovery journal, receipt pruning, and tests.
- [x] (2026-07-23 11:00Z) Freeze compatibility decisions, implementation order,
  and acceptance gates in this spec.
- [x] (2026-07-23 12:15Z) Implement Milestone 1: canonical grammar/schema names,
  unsupported-operation guidance, and independent read `has_more` metadata.
- [x] (2026-07-23 13:05Z) Implement Milestone 2: optional rooted patches,
  root-relative lexical validation, contextual/edited-move previews, delete
  metadata, and structural-first routine output bounding.
- [x] (2026-07-23 13:40Z) Implement Milestone 3: bounded receipt recovery
  metadata, shared pruning constants, typed-semantic replay documentation, and
  actionable existing-parent errors.
- [ ] Review Milestone 4's directory journal design; implement it only after the
  go/no-go gate passes.
- [ ] Run focused/workspace validation; update README and changelogs; record
  exact evidence here.

## Surprises & Discoveries

- Observation: canonical block operations are `SWAP.BLK`, `DEL.BLK`,
  `INS.BLK.PRE`, and `INS.BLK.POST`; `INS.BLK` aliases the post form.
  Evidence: `HashlineOperation` and `parse_hashline_patch` in
  `crates/nanocodex-tools/src/hashline/patch_parser.rs`. `SWAP.BLOCK` failed in
  the smoke, while the schema only said “block forms.”

- Observation: explicit `end_line` completion differs from output truncation.
  Evidence: `hashline::read` compares `returned_end` with the requested
  interval. Reading through line 17 of a 29-line file correctly returned
  `truncated=false` and `next_start_line=null`, but gave no continuation signal.
  Implementation evidence: focused schema, parser-suggestion, and read
  continuation tests pass for explicit, capped, byte-truncated, final, and
  empty reads without changing `truncated` or `next_start_line`.

- Observation: previews use a common-prefix/common-suffix minimal diff.
  Evidence: `build_hashline_patch_preview` in `patch.rs`. A newly inserted
  closing brace matching neighboring context was omitted as unchanged, although
  the applied bytes and digest were correct.
  Implementation evidence: the matching delimiter now appears as resulting-file
  context, and the preview digest matched applied bytes in the focused
  regression.

- Observation: raw caller spellings are not sufficient conflict keys in rooted
  mode because `a/./b` and `a/b` address the same target.
  Evidence: rooted routine conflict checks now use normalized resolved path keys
  while response details retain the caller's root-relative spelling.

- Observation: routine patches create missing destination parents, but durable
  transactions lease existing parent directory identities.
  Evidence: patch commit calls `resolve_destination(..., true)`. Transaction
  `resolve`/`open_parent` and `TransactionLease` require existing parents, and
  journals currently model only files. The smoke failed with “resolve
  transaction parent” when `src/` was absent.
  Implementation evidence: missing `src/lib.rs` now names required parent
  `src`, recommends an explicitly authorized operation, and leaves both the
  parent and recovery storage absent.

- Observation: transaction preview is intentionally stateless.
  Evidence: `TransactionRequest` always carries mutations; `commitPreviewed`
  re-prepares them under leases and compares the plan digest. Changed mutations
  and changed files were rejected before writes in the smoke.

- Observation: successful commits retain terminal receipts and reservations,
  not before/after artifact bodies.
  Evidence: `remove_journal` removes artifacts, persists `state=complete` and
  the terminal outcome, and leaves the reservation for ID uniqueness.
  `prune_terminal_receipts` bounds complete receipts to 64 during later
  transaction activity.
  Implementation evidence: preview reports retention `false` without creating
  storage; commit reports `true` only after terminalization. The focused
  receipt test derives its 64-receipt/128-entry assertions from the shared
  policy constant.

- Observation: transaction previews have a 6 KiB aggregate budget; routine
  patch responses have 24 KiB. Structural evidence is already favored over
  textual previews and must remain so.

## Decision Log

- Decision: ship contract/discoverability improvements before durability work.
  Rationale: schema, errors, `has_more`, rooted patching, and previews do not
  expand the transaction crash state machine.
  Date/Author: 2026-07-23 / Codex

- Decision: preserve `truncated` and `next_start_line` and add `has_more`.
  Rationale: `truncated` means the requested interval was not completely
  represented. Reinterpreting it as file continuation would break deliberate
  bounded reads.
  Date/Author: 2026-07-23 / Codex

- Decision: patch `root` is opt-in. Rooted mode rejects absolute paths, `..`,
  and the reserved `.nanocodex/hashline-transactions` subtree; omitted-root
  mode preserves current absolute and parent-relative behavior.
  Rationale: current external-path behavior is public and tested. Rooted mode
  provides a compact lexical base, not a transaction-grade filesystem sandbox:
  it preserves routine patch symlink behavior and does not claim descriptor-
  relative race resistance.
  Date/Author: 2026-07-23 / Codex

- Decision: keep compact diffs and add one bounded context line on each side.
  Rationale: full post-edit files waste tool context. Context clarifies delimiter
  alignment while digests and rereads remain authoritative.
  Date/Author: 2026-07-23 / Codex

- Decision: keep `commitPreviewed` stateless and require mutation resubmission.
  Rationale: digest-only commit requires retained preview bytes, expiry,
  identity, conflict, and cleanup policy. Current typed-semantic replay is
  verbose but deterministic and restart-independent.
  Date/Author: 2026-07-23 / Codex

- Decision: report receipt retention but add no cleanup action.
  Rationale: receipts preserve ID uniqueness/recovery evidence, are bounded to
  64, and are pruned by normal activity. Agent deletion could erase disturbed
  recovery evidence.
  Date/Author: 2026-07-23 / Codex

- Decision: parent creation is opt-in, destination-only, and review-gated.
  Rationale: updates, deletes, and move sources require existing leased parents.
  Created directories must be journaled recovery mutations, not an unjournaled
  `create_dir_all` side effect.
  Date/Author: 2026-07-23 / Codex

## Outcomes & Retrospective

- Outcome: research and the implementation proposal are complete; product code
  has not changed.
  Evidence: the smoke's generated Rust fixture compiled and passed one test;
  all routine mutation kinds and expected rejection paths were exercised.
  Remaining: Milestones 1-3, Milestone 4 review, implementation, validation,
  docs/changelogs, and final evidence.

## Context and Orientation

Hashline lives in `crates/nanocodex-tools/src/hashline/`:

- `mod.rs` owns request decoding, tool schemas, reads, patch/transaction
  preparation, aggregate output bounds, and model-facing errors.
- `patch_parser.rs` owns operation spellings, anchors, payloads, `REM`, and `MV`.
- `patch_sections.rs` owns multi-file section headers.
- `patch.rs` owns routine patch application and textual previews.
- `format.rs` owns bounded read excerpts.
- `transaction_fs.rs` owns roots, parent resolution, leases, journals, recovery,
  rollback, receipt pruning, and Linux/filesystem validation.
- `tests.rs` contains schemas, patches, transactions, recovery, and subprocess
  fault coverage.
- `README.md`, `crates/nanocodex-tools/CHANGELOG.md`, and top-level
  `CHANGELOG.md` document public behavior.

The complete current DSL is:

    SWAP <LINE:HASH or START:HASH-END:HASH>:
    +replacement
    DEL <LINE:HASH or range>
    INS.PRE <LINE:HASH>:
    +inserted
    INS.POST <LINE:HASH>:
    +inserted
    INS.HEAD:
    +inserted
    INS.TAIL:
    +inserted
    SWAP.BLK <LINE:HASH@BLOCK_HASH>:
    +replacement
    DEL.BLK <LINE:HASH@BLOCK_HASH>
    INS.BLK.PRE <LINE:HASH@BLOCK_HASH>:
    +inserted
    INS.BLK.POST <LINE:HASH@BLOCK_HASH>:
    +inserted
    REM
    MV <destination>

Preserve existing inline `|text` forms and `INS.BLK` as an alias for
`INS.BLK.POST`. Do not add `*.BLOCK` aliases merely to hide incomplete
documentation; suggest canonical `*.BLK` spellings instead.

No dependency is needed. Durable transactions remain Linux-only and restricted
to filesystems accepted by `ensure_supported_directory`.

## Execution DAG

    D0 Freeze behavior/compatibility [complete]
     |
     v
    D1 Grammar/schema/errors + read has_more
     |
     v
    D2 Patch root + contextual previews
     |
     v
    D3 Receipt metadata + parent diagnostics
     |
     v
    G4 directory journal accepted?
       | no                    | yes
       v                       v
    ship D1-D3          D4 durable parent creation
                               |
                               v
                         D5 full validation
                               |
                        pass? /   \ no
                             v     v
                           ship   fix/retest

Milestones are sequential because `mod.rs` and `tests.rs` are shared hotspots.
Stopping after D3 is valid if D4 cannot prove crash-safe rollback.

## Plan of Work

### Milestone 1: Discoverability and read continuation

Files: `mod.rs`, `patch_parser.rs`, and `tests.rs`.

Work:

1. Define one canonical operation-name list close to the parser. Use it in the
   schema and unsupported-operation errors so they cannot silently drift.
2. Name all canonical forms and the `INS.BLK` alias. Include copy-ready examples
   for line swap, block swap, head/tail, `REM`, and quoted `MV`. Explain `+`
   payload lines and exact `find_block` anchors.
3. Unsupported operation errors list canonical spellings. Deterministically
   suggest `*.BLK` for close `*.BLOCK` input; do not add fuzzy matching.
4. Add required boolean `has_more` to read output. It is true exactly when
   `returned_end < total_lines`. Preserve existing `truncated` and
   `next_start_line` calculations. Empty files return false.
5. Test empty files, explicit ranges, `max_lines`, byte truncation, and final
   pages.

Acceptance:

- `tool_schemas_explain_external_paths_and_patch_grammar` asserts every exact
  block spelling.
- A parser regression proves `SWAP.BLOCK` suggests `SWAP.BLK`.
- A 29-line file read through line 17 returns `truncated=false`,
  `next_start_line=null`, and `has_more=true`; the final page returns false.

### Milestone 2: Rooted patches and clearer previews

Files: `mod.rs`, `patch_sections.rs`, `patch.rs`, and `tests.rs`.

Work:

1. Add optional `root: Option<String>` to `PatchRequestWire` and
   `PatchRequest`, both for `patch` and `header`/`operations` forms. Relative
   roots resolve from the workspace; absolute roots are accepted; the root must
   exist and be a directory.
2. With no root, preserve current section/`MV` resolution. With root, require
   non-empty root-relative source/destination paths without `..` or the exact
   reserved `.nanocodex/hashline-transactions` subtree.
3. Prepare, conflict-check, preview, and commit against the selected root.
   Returned paths retain caller-supplied root-relative spelling. Do not
   canonicalize through missing destinations. Rooted mode is lexical scoping,
   preserves current routine patch symlink behavior, and must not be described
   as transaction-grade containment.
4. Render at most one unchanged context line before/after the minimal changed
   span as ` <NEW_LINE>:<NEW_HASH>|text`: one leading space followed by the
   resulting file's line number and hash. Context counts against the existing
   40-line, 4 KiB per-preview limits and sets `truncated` if omitted.
5. Include textual preview for edited moves. Unchanged moves retain digest/path
   metadata. Deletes gain bounded structural `old_hash`, `old_bytes`, and
   `old_lines`, not deleted contents.
6. Preserve 24 KiB patch and 6 KiB transaction aggregate budgets, retaining
   structural evidence before optional text.

Acceptance:

- Omitted-root external-path tests pass unchanged.
- `one_patch_mixes_create_update_move_and_delete_sections` covers rooted mode
  and an edited move.
- Rooted mode rejects absolute, traversal, and internal-storage paths.
- A matching-delimiter insertion shows the delimiter as context and applies to
  the previewed digest.
- Dry runs remain non-mutating and output-bound tests retain structural fields.

### Milestone 3: Receipt observability and parent diagnostics

Files: `mod.rs`, `transaction_fs.rs`, `tests.rs`, README, and both changelogs.

Work:

1. Add committed transaction output:

       "recovery": {
         "terminal_receipt_retained": true,
         "relative_directory": ".nanocodex/hashline-transactions",
         "terminal_receipt_limit": 64
       }

   Preview returns the same three-field `recovery` object with
   `terminal_receipt_retained: false`; `relative_directory` and
   `terminal_receipt_limit` describe the policy that a later commit would use.
   Do not expose artifact names, absolute paths, before-images, or journals.
2. Source the directory and limit from shared constants used by pruning.
3. Document that successful commits remove mutation artifacts, retain bounded
   complete receipts/reservations, and prune them during later activity.
4. Document that “identical mutations” means equal deserialized typed semantics,
   not byte-identical JSON. Keep request/digest behavior unchanged.
5. Improve missing-parent errors to name the model path and required parent.
   Before D4, tell callers to create it through an explicitly authorized
   operation. If D4 ships, mention `create_parents`.

Acceptance:

- Changed files/mutations still fail
  `transaction_preview_digest_commits_exact_plan_and_cleans_sidecar`.
- `terminal_receipts_are_bounded_and_oldest_ids_are_pruned` uses the shared
  limit.
- Output tests distinguish stateless preview from committed retention and never
  leak absolute temp paths or recovery bytes.
- Missing `src/lib.rs` parent errors name `src` and give supported recovery.

### Milestone 4: Durable destination-parent creation

Scope: optional and review-gated. Never begin with `create_dir_all` in the
file-only commit path.

Interface: add top-level `create_parents: bool`, default false, to
`TransactionRequest` and its schema. It affects only `Create.path` and
`Move.destination`. Preview and plan digest include the flag and planned
directories. `commitPreviewed` must resubmit the same flag.

Required design:

1. Walk destinations to the nearest existing ancestor without following
   symlinks. Record every missing component, reject non-directory collisions,
   and deduplicate shared plans.
2. Lease the nearest existing ancestor identity and revalidate both that
   identity and all expected missing/existing components before journaling.
3. Journal explicit directory-create mutations before dependent files. Create
   one component at a time, sync its parent, and advance progress only after
   durable publication.
4. Roll back only empty directories created by this transaction, deepest first.
   Never remove pre-existing directories or external content. External content
   forces retained `recoveryRequired` evidence and manual recovery.
5. Recover every directory/file state transition after process death.
   Terminal receipt pruning stays unchanged.
6. Default false executes today's existing-parent path exactly.

Go/no-go gate:

- First add the reviewed journal shape/state transitions to `Decision Log`.
- Add failpoints around directory journal publication, component creation,
  dependent file publication, rollback removal, and terminal cleanup.
- If ancestor replacement cannot be prevented without broader locks or new
  filesystem assumptions, stop after D3 and record why.

Acceptance:

- Nested create/move parents work only with `create_parents=true`.
- Default false is actionable and non-mutating.
- Shared plans deduplicate; files/symlinks and parent replacement fail closed.
- External-content races preserve both parties' state.
- Every new subprocess failpoint converges to committed bytes or before-images,
  with no orphan temporary artifacts; disturbed evidence is retained.
- Existing fault matrices and receipt-bound tests remain green.

## Interfaces and Dependencies

- `hashline__read` response adds required `has_more: bool`; inputs/failures stay
  unchanged.
- `hashline__patch` request adds optional `root: string`; omitted root is
  compatible, while rooted mode supplies strict lexical path scoping without
  claiming transaction-grade symlink or race containment.
- `hashline__patch` response keeps its structure and gains contextual/edited
  move previews plus delete metadata.
- `hashline__transaction` response adds bounded `recovery` metadata.
- Milestone 4 alone adds optional `create_parents`. All stale digest, plan,
  filesystem, race, and disturbed-recovery failures remain fail-closed.
- No external dependency or Codex parity claim is involved.

## Concrete Steps

From the repository root, iterate with:

    cargo test -p nanocodex-tools \
      'hashline::tests::tool_schemas_explain_external_paths_and_patch_grammar'
    cargo test -p nanocodex-tools \
      'hashline::tests::one_patch_mixes_create_update_move_and_delete_sections'
    cargo test -p nanocodex-tools \
      'hashline::tests::transaction_preview_digest_commits_exact_plan_and_cleans_sidecar'
    cargo test -p nanocodex-tools \
      'hashline::tests::transaction_preview_output_is_capped_before_structural_evidence'
    cargo test -p nanocodex-tools \
      'hashline::tests::terminal_receipts_are_bounded_and_oldest_ids_are_pruned'
    cargo test -p nanocodex-tools 'hashline::tests::'

Final gate:

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    cargo check --workspace --all-targets

If D4 changes subprocess coverage, run every affected fault-matrix test exactly
as documented beside it and record pass counts, elapsed time, and retained
recovery evidence.

Expected D1-D3 smoke evidence:

    read: truncated=false, has_more=true for an explicit non-final range
    parser: unsupported SWAP.BLOCK; did you mean SWAP.BLK?
    patch: rooted two-file dry run/apply report matching digests
    preview: trailing matching delimiter appears as context
    transaction preview: terminal_receipt_retained=false
    transaction commit: terminal_receipt_retained=true

## Validation and Acceptance

Manual smoke uses a disposable `mktemp -d` root outside the repository. Seed
UTF-8 Rust/Markdown fixtures, exercise bounded reads, exact block operations,
rooted multi-file dry-run/apply, stale-header rejection, changed-plan rejection,
and receipt metadata. Compile or parse the edited fixture, verify exact bytes,
then remove the disposable root and receipts.

If D4 ships, repeat without pre-creating destination parents and inspect restart
recovery around an injected interruption.

Regression requirements:

- Omitted patch root keeps absolute/parent-relative support.
- Dry runs never mutate; transaction preview creates no receipt.
- Routine patch remains validation-first/best-effort; transactions remain the
  durable batch path.
- Stale file, line, block, and exact digests reject before mutation.
- Complete receipts stay bounded; incomplete/disturbed evidence is not pruned
  as terminal.
- Output bounds favor structural evidence over text.

## Idempotence and Recovery

Tests use disposable roots and are safe to rerun. Rooted patch failures recover
through reread/rebuilt evidence. Transaction retries must enter normally so
`recover_pending` resolves journals before a new plan.

If D4 fails a fault test, retain its fixture for diagnosis; do not delete
disturbed evidence and call it recovered. D1-D3 can be reverted independently.
Do not partially revert D4's request field while leaving journal variants that
the recovery reader cannot deserialize.

## Rollout and Operations

No flag, environment variable, migration, or service is required. `root` and
`create_parents` are opt-in. Update README and both changelogs as public slices
ship. Operators continue treating `.nanocodex/hashline-transactions` as bounded
recovery state, not cache.

Each implementation handoff updates this spec with progress, decisions, exact
validation, residual risks, and revision notes.

## Risks and Open Questions

- Risk: grammar prose inflates the stable tool prompt.
  Mitigation: one compact canonical grammar and tests, not repeated prose.
- Risk: rooted mode creates path aliases or is mistaken for a sandbox.
  Mitigation: strict lexical rules and conflict tests, explicit preservation of
  routine patch symlink behavior, and no transaction-grade containment claim.
- Risk: context consumes preview budget.
  Mitigation: count it under current limits and retain structure first.
- Risk: `has_more` is confused with `truncated`.
  Mitigation: test their independent truth table.
- Risk: receipt metadata invites deletion.
  Mitigation: expose lifecycle, not artifact identifiers or a delete action.
- Risk: D4 expands crash states.
  Mitigation: separate gate, explicit journal mutations, extended fault matrix,
  and permission to ship D1-D3 without it.

Open question:

- Split D4 into a separate spec if journal versioning or the reviewed state
  machine exceeds one focused change.

## Artifacts and Notes

Live smoke summary:

    PASS read pagination, UTF-8 anchors, Rust/Markdown blocks
    PASS SWAP, INS.PRE, INS.POST, DEL, multi-file patch
    PASS stale header and changed preview plan rejected before writes
    PASS transaction create, update, move, delete
    PASS generated Rust fixture: 1 test passed
    DISCOVERY accepted block spelling is SWAP.BLK, not SWAP.BLOCK
    DISCOVERY missing transaction parent error lacks the parent path
    DISCOVERY complete receipts/reservations remain beneath the root

No repository files were changed by the smoke. Its disposable root and recovery
records were removed.

## Revision Notes

- 2026-07-23: Created an implementation-ready handoff from live smoke and source
  evidence. Preserved stateless preview replay and bounded receipts, separated
  low-risk ergonomics from durable parent creation, and defined compatibility,
  recovery, and acceptance gates.
