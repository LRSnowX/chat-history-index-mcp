# Operational Working Memory Lifecycle — Checkpoint C

Status: **frozen design / preflight; implementation pending**

Baseline:

- branch: `local/integration`
- repository HEAD at freeze: `ac9ef6f0071f4352c918121002249318701ca000`
- schema version: unchanged

## Goal

Add an explicit, auditable operator lifecycle for project-local operational
Working Memory without enabling automatic memory mutation.

Checkpoint B made stale operational memory safe to carry in a bounded handoff:
`needs_revalidation` no longer receives the same bootstrap-budget protection as
current memory. It deliberately did not change durable memory status.

Checkpoint C closes the remaining lifecycle gap. Operational `state`,
`blocker`, and `task` items must be maintainable directly by an operator even
when the model/Codex memory compiler remains disabled.

## Governing design

The user explicitly confirmed these semantics:

1. `needs_revalidation` is a verification downgrade only. It must never by
   itself resolve, supersede, archive, or delete a durable MemoryItem.
2. Operational Working Memory uses an explicit lifecycle:
   create/set, supersede, resolve, and archive.
3. The direct operator surface applies only to project-local `state`,
   `blocker`, and `task` memory.
4. Formal checkpoint acceptance is the primary moment to refresh bounded
   operational handoff state, but the operator path is not restricted to that
   workflow.
5. The default model/Codex memory compiler remains disabled.
6. No model-facing memory write tool is added.
7. CHIM owns durable memory lifecycle semantics. DevSpace may orchestrate the
   operator workflow but must not duplicate lifecycle rules in Node.

Historical-decision confirmation remains a separate gate. Operational memory is
not a project rule and therefore does not require
`memory-project-confirm` / user-confirmation evidence.

## Existing primitives to reuse

The implementation must reuse the current MemoryItem model:

- `MemoryStatus::{Active, Resolved, Superseded, Archived}`
- `supersedes_memory_id`
- durable MemoryEvidence
- `last_verified_at`
- transactional `put_memory_item_tx`
- the partial unique index on
  `(scope_type, scope_id, memory_key) WHERE status = 'active'`

No database migration is required.

The existing compiler candidate lifecycle already proves the intended status
semantics, but Checkpoint C must not require a compiler candidate in order for an
operator to maintain operational memory.

## Operator contract

### Set / create / supersede

Add:

```text
memory-project-operational-set
```

Required inputs:

- `--project <project>`
- `--kind state|blocker|task`
- `--key <stable-key>`
- exactly one of `--value <string>` or `--value-json <json>`
- `--reason <review-reason>`
- at least one `--evidence KIND:REFERENCE`

Optional:

- `--supersedes <memory-id>`
- importance/confidence using bounded defaults consistent with current operator
  authoring paths

Semantics:

- when the project/key has no active item, create one active operational item;
- when an active item already exists for the project/key, replacement must be
  explicit through `--supersedes`;
- the supplied predecessor must be the active item for the same project/key;
- supersession is atomic: the predecessor becomes `superseded` in the same
  transaction that creates the replacement;
- exact replay of the same canonical set request is idempotent;
- the operator path generates a deterministic content-derived memory ID rather
  than requiring callers to invent IDs.

The implementation should use a distinct deterministic identity namespace such
as `project-operational-memory-v1:`.

### Resolve

Add:

```text
memory-project-operational-resolve --project <project> <memory-id>
```

It requires a non-empty reason and at least one evidence reference.

Semantics:

- only a project-local operational `state`, `blocker`, or `task` belonging
  to the requested project may be targeted;
- an active target becomes `resolved`;
- an already resolved target is an idempotent replay;
- superseded or archived historical versions cannot be resolved;
- the transition updates verification time and appends operator review evidence.

### Archive

Add:

```text
memory-project-operational-archive --project <project> <memory-id>
```

It requires a non-empty reason and at least one evidence reference.

Semantics:

- an active or resolved operational item may become `archived`;
- an already archived target is an idempotent replay;
- a superseded historical version cannot be rewritten through this operator
  path;
- evidence is appended rather than replacing prior provenance.

## Evidence and audit contract

Every operator mutation requires:

- project scope;
- review reason;
- at least one supported MemoryEvidence reference;
- mutation timestamp.

Operator evidence must record the action (`set`, `supersede`, `resolve`, or
`archive`) and review reason while retaining the supplied evidence detail.

Operational set/transition is an operator assertion, not a historical-rule
confirmation. It must not mark the item `confirmed` or alter the
Historical Decision Confirmation Gate.

## Working Memory behavior

`ProjectWorkingMemory` remains a materialized view of active MemoryItems.

Therefore:

- resolve/archive removes the target from active Working Memory automatically;
- supersede replaces the active version without losing the predecessor;
- historical versions remain durable and auditable;
- `needs_revalidation` remains read-only verification metadata and does not
  perform lifecycle mutation.

No second operational-state table or hand-maintained project summary is added.

## Implementation allowlist

Expected implementation is bounded to:

- `crates/chat-history-core/src/memory.rs`
- `crates/chat-history-core/src/lib.rs`
- `crates/chat-history-core/tests/memory_foundation.rs`
- `crates/chat-history-cli/src/main.rs`
- `crates/chat-history-cli/tests/operational_memory.rs` (new, preferred)
- `crates/chat-history-cli/tests/collaboration_memory.rs` only if sharing the
  existing operator-CLI fixture is materially simpler
- `README.md`
- `docs/project-memory-foundation.md`
- this document

No change is expected in:

- `crates/chat-history-core/src/sql.rs`
- memory compiler/candidate code
- memory promotion policy
- MCP server/model-facing tool schemas
- DevSpace
- dependencies or `Cargo.lock`

If a correct implementation materially requires a file outside the allowlist or
one of the explicitly excluded surfaces, stop and report the reason before
expanding scope.

## Required regression coverage

Core tests must prove:

1. create of a project operational item;
2. duplicate active project/key fails without explicit supersession;
3. supersession is atomic and preserves the predecessor as `superseded`;
4. exact set replay is idempotent;
5. resolve removes an active item from ProjectWorkingMemory while preserving it
   durably as `resolved`;
6. archive accepts active/resolved, removes it from active Working Memory, and is
   idempotent when already archived;
7. cross-project targets fail closed;
8. invariant/preference/decision/result/hypothesis/artifact_reference are
   rejected by the operational operator path;
9. transition evidence is appended and prior provenance retained;
10. newer project evidence may mark an item `needs_revalidation` without
    changing its durable `active` status.

CLI tests must prove:

- the three operator commands emit the expected JSON;
- reason and evidence are mandatory;
- only `state|blocker|task` are accepted;
- replacement requires explicit `--supersedes`;
- project mismatch and invalid lifecycle transitions fail closed;
- no `--user-confirmation` is required for ordinary operational state.

## Verification

Codex implementation validation should stay focused:

```bash
cargo fmt --all -- --check
cargo test -p chat-history-core --test memory_foundation
cargo test -p chat-history-cli --test operational_memory
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

If the CLI regression is implemented in
`crates/chat-history-cli/tests/collaboration_memory.rs` instead of a new test
file, run that target in place of `operational_memory`.

The independent DevSpace acceptance pass owns broad CI-parity validation:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/validate-package
python3 -m py_compile scripts/sync-codex-remote
scripts/test-remote-codex-sync
bash -n \
  scripts/backfill-all \
  scripts/chat-history-cli \
  scripts/chat-history-http-service \
  scripts/chat-history-mcp \
  scripts/chat-history-service \
  scripts/import-normalized-stdin \
  scripts/install-self-hosted \
  scripts/monitor-backfill \
  scripts/start-monitor \
  scripts/stop-monitor \
  scripts/sync-codex-remote \
  scripts/test-remote-codex-sync \
  plugins/chat-history-index-mcp/scripts/chat-history-mcp
git diff --check
```

## Real-data acceptance

After code review and regression, use the existing LEMonX stale operational
memory as the real acceptance sample.

The acceptance must demonstrate that:

- `needs_revalidation` alone does not mutate either stale item;
- the obsolete institutional-onboarding task can be explicitly resolved;
- the obsolete institutional-onboarding state can be explicitly archived (or
  explicitly superseded only if a semantically equivalent current key is
  deliberately chosen);
- both items disappear from active ProjectWorkingMemory;
- their historical records and evidence remain durable;
- a fresh LEMonX handoff no longer carries them as active Working Memory;
- current continuation/retrieval behavior remains unchanged.

Do not manufacture a same-key supersession merely to exercise the feature on
real data. Synthetic regression tests own that case.

## Stop conditions

Stop instead of improvising if implementation would require:

- automatically mutating memory because it is `needs_revalidation`;
- a schema migration or second operational-state store;
- enabling the model/Codex compiler by default;
- a model-facing MCP write tool;
- weakening the Historical Decision Confirmation Gate;
- changing compiler candidate semantics to make the operator path work;
- duplicating lifecycle semantics in DevSpace;
- broadening the operator path beyond project-local `state|blocker|task`.

## Acceptance outcome

Checkpoint C is complete only when:

1. the operator lifecycle exists and is independently reviewed;
2. focused and CI-parity validation pass;
3. real LEMonX stale operational memory is explicitly retired through the new
   lifecycle without data loss;
4. fresh project handoff shows the retired items are no longer active;
5. the implementation is committed/pushed only after independent DevSpace
   acceptance.

