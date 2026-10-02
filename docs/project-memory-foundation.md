# Project Memory Foundation

## Purpose

CHIM should not equate raw conversation history with memory.

The target architecture separates four concepts:

1. **Evidence** — raw conversations, Git state, documents, tests, and other
   source material.
2. **MemoryItem** — one durable, traceable fact, decision, state, blocker,
   task, result, preference, invariant, hypothesis, or artifact reference.
3. **ProjectWorkingMemory** — the current active project MemoryItems,
   materialized from durable state rather than maintained as a second copy.
4. **HandoffPacket** — the bounded context a host such as DevSpace supplies
   when a new project conversation starts.

Raw conversations remain evidence. They are not themselves the canonical
working memory.

## Ownership boundary

CHIM is the memory engine. It owns:

- conversation/evidence ingestion;
- evidence quality and provenance;
- durable MemoryItems;
- memory lifecycle and supersession;
- working-memory materialization;
- later semantic retrieval and memory compilation.

DevSpace remains the memory orchestrator. It owns:

- project/workspace identity;
- live repository state;
- project instructions;
- context budgeting;
- host-facing memory authorization;
- construction of the final handoff packet.

This boundary intentionally keeps CHIM separately usable and avoids making the
DevSpace fork the owner of memory storage semantics.

## MemoryItem contract

Every durable MemoryItem has:

- an immutable ID;
- scope: global or one project;
- kind;
- stable key;
- structured JSON value;
- lifecycle status;
- importance and confidence;
- optional validity interval;
- optional superseded predecessor;
- creation/update/verification timestamps;
- zero or more evidence records.

Supported initial kinds:

- invariant
- preference
- decision
- state
- blocker
- task
- result
- hypothesis
- artifact_reference

Supported lifecycle states:

- active
- resolved
- superseded
- archived

Only one active MemoryItem may exist for the same scope and key.

Replacing an active memory requires explicit supersession. Supersession is
transactional: the predecessor becomes superseded in the same transaction that
writes the replacement. A repeated identical write is safe.

## Evidence contract

Initial evidence kinds are:

- user_statement
- conversation_turn
- document
- git_commit
- repository_state
- devspace_result

Evidence is retained when the MemoryItem later becomes resolved, superseded, or
archived.

Memory provenance is required so a host can later explain where a state or
decision came from and revalidate it against stronger evidence.

## Working-memory contract

ProjectWorkingMemory is not a separate mutable truth source.

It is materialized from active project-scoped MemoryItems ordered by importance
and recency. Resolving or superseding an item therefore changes the working
memory automatically.

Later phases may add bounded rendering and category-aware layout, but should not
introduce a second manually maintained working-memory table.

## Conversation snapshot quality

Conversation ingestion is evidence ingestion and therefore must be monotonic in
quality where degradation can be proven.

An incoming snapshot is rejected as lower quality when, for the same canonical
conversation:

- it would replace a non-empty transcript with an empty transcript;
- an existing user+assistant dialogue would become a multi-message
  single-role transcript;
- the incoming message-ID set is a strict subset of the existing set while
  containing fewer messages.

Incomparable changes are not rejected merely because they are shorter. This is
intentional so edits, branches, and source-specific representations are not
silently classified as worse without evidence.

The import report exposes lower-quality rejections explicitly.

Each new ingest is also retained as an evidence snapshot with its own message
rows. Accepted snapshots become the canonical materialized view, previous
canonical snapshots become superseded, and rejected lower-quality snapshots are
retained as rejected evidence rather than discarded.

For conversations that predate snapshot support, the existing canonical
conversation is captured lazily immediately before the first reimport that
could replace it. This avoids a large eager migration while ensuring the legacy
canonical evidence is preserved before mutation.

## Schema lifecycle

Project Memory Foundation uses schema version 4:

- version 2 introduced MemoryItem and MemoryEvidence;
- version 3 introduced retained conversation/message evidence snapshots.
- version 4 introduced incremental compiler checkpoints, staged memory
  candidates, candidate provenance, and auditable promotion decisions.

The new tables are created by normal database opening. Legacy version-1
databases and backups remain restorable: pre-migration health inspection keeps
the original required-table baseline, and the restored database is upgraded
when opened by current CHIM.

## Current implementation boundary

Phase 1 deliberately does **not** expose model-facing write tools.

The current implementation provides:

- durable MemoryItem and MemoryEvidence schema;
- core read/write APIs;
- explicit transactional supersession;
- active ProjectWorkingMemory materialization;
- conversation quality-regression protection;
- retained conversation/message evidence snapshots with canonical,
  superseded, and rejected-lower-quality states;
- lazy capture of pre-snapshot canonical conversations before replacement;
- read-only ProjectWorkingMemory delivery through `memory_project_context`;
- DevSpace handoff integration that prioritizes bounded working memory before
  recent conversation continuation within one shared bootstrap budget;
- transactional memory compilation staging against canonical conversation
  snapshots;
- per-project/conversation compile checkpoints with immutable evidence-prefix
  hashes so edited history cannot silently advance an old compile cursor;
- staged add/supersede/resolve/archive candidates that do not mutate
  ProjectWorkingMemory before promotion;
- transactional candidate promotion, explicit rejection, stale-candidate
  detection, and retained decision reasons;
- bounded model-driven incremental compilation through Codex in an ephemeral
  read-only sandbox with medium reasoning effort;
- strong project/conversation matching before model invocation;
- strict candidate JSON validation, delta-only evidence IDs, bounded values,
  authoritative active-memory revalidation, and duplicate/conflict rejection;
- operator CLI commands to plan a bounded project batch without model calls,
  compile one conversation delta or a bounded recent project batch, inspect
  pending candidates, inspect one candidate, explicitly promote it, or
  explicitly reject it with a retained reason; planning never stages candidates
  and compilation itself never promotes;
- read-only project memory health inspection covering MemoryItem/candidate
  lifecycle counts, stale/unverified active state, checkpoint caught-up/behind
  turns, and canonical-prefix integrity;
- schema-v1 restore compatibility.

It does not yet provide:

- automatic compiler scheduling after new conversation evidence arrives;
- automatic candidate promotion;
- a richer operator review UI;
- memory-first semantic search;
- automatic scheduler/retry telemetry beyond the read-only health snapshot.

This boundary prevents an LLM from writing long-term state before provenance,
conflict handling, and compiler rules are implemented and tested.

## Next phases

### Phase 2 follow-up — Snapshot policy expansion

The initial evidence snapshot layer is implemented. Follow-up work should add:

- attachment snapshots where attachment provenance matters;
- richer source/quality precedence beyond the current provable-regression
  rules;
- snapshot inspection/health APIs;
- optional historical backfill when complete snapshot coverage is useful.

### Phase 3 follow-up — Compiler orchestration and review

The durable state machine and model-driven incremental compiler are implemented.
The compiler processes only new evidence since the last compiled snapshot
prefix and can emit staged candidate operations. Before model invocation it
rejects incomplete multi-message ChatGPT canonical snapshots that contain no
assistant messages, so known partial collector evidence cannot become durable
memory through model guesswork.

- add
- supersede
- resolve
- archive

An ordinary update is represented as an explicit supersession so the previous
memory remains auditable.

Compiler output preserves provenance and cannot write MemoryItems directly.
Candidates become durable memory only through promotion. Promotion revalidates
the candidate evidence prefix against the current canonical conversation
snapshot; edited or branched evidence becomes stale instead of mutating current
memory.

The bounded ChatGPT live-collector scheduler infrastructure is implemented but
disabled by default. It can be explicitly configured with a project allow-list;
after accepted new evidence it launches a detached, lock-protected compiler
worker so ingestion/cursor advancement completes independently. Scheduler and
model failures are isolated from collection, and compilation still only stages
candidates.

The remaining Phase 3 work is orchestration and review:

- decide whether and where to opt projects into the default-off scheduler;
- optionally add a richer review UI around the existing operator CLI;
- define conservative promotion policy classes if any operation is ever made
  automatic;
- add richer scheduler retry history beyond the current last-run worker status
  and read-only checkpoint health.

No MCP model-facing memory write surface exists.

### Phase 4 follow-up — Memory-first retrieval policy

The initial memory-first retrieval path is implemented. Project-scoped
`memory_search` returns bounded ProjectWorkingMemory first and hybrid
conversation evidence second. Working Memory is ordered by the core
importance/recency policy and capped at 12 items at the MCP boundary with an
explicit truncation flag. Unscoped search remains the original hybrid evidence
search.

DevSpace passes the response through unchanged and continues to authorize
`memory_get_thread` only from returned conversation/evidence hits; provenance
references inside Working Memory do not expand thread authorization.

Remaining Phase 4 work is retrieval policy quality rather than wiring:

- query-aware selection among large active Working Memory sets instead of only
  top importance/recency;
- explicit preference between working-memory facts and conflicting historical
  evidence in answer-generation guidance;
- retrieval/answer evaluation benchmarks for current-state, decision-history,
  blocker, and task questions;
- optional memory-key search/ranking before conversation evidence when the
  active set grows beyond the bounded MCP response.

### Phase 5 follow-up — Collaboration Memory

The DevSpace HandoffPacket now combines:

- bounded CollaborationMemory for stable cross-project rules;
- bounded ProjectWorkingMemory;
- bounded recent episodic continuation;
- a live repository snapshot refreshed on every workspace open;
- explicit authoritative project-instruction references.

The repository snapshot includes branch/HEAD, optional upstream divergence,
dirty-state counters, and a bounded changed-path sample. DevSpace exposes these
live/authoritative sections explicitly and instructs the host to prefer them
over stale stored memory when they conflict.

CollaborationMemory is materialized from active global MemoryItems, but only
the stable rule-like kinds `invariant`, `preference`, and `decision` enter
the automatic handoff. Global state/task/blocker items are deliberately
excluded so transient activity does not leak into every project.

DevSpace gives CollaborationMemory a small bounded share of the existing
dynamic-memory budget before ProjectWorkingMemory and recent continuation.
Provenance references inside CollaborationMemory do not grant conversation
thread access.

The remaining Phase 5 work is authoring policy: the current read/handoff path is
complete, but ordinary project conversation compilation does not automatically
promote statements into global CollaborationMemory. Cross-project rules should
remain explicit/operator-reviewed until a conservative global-memory policy is
defined.

### Phase 6 follow-up — Memory health

The read-only `memory-health` CLI now exposes:

- working-memory age;
- active/resolved/superseded item counts;
- stale/unverified memory;
- candidate lifecycle counts and oldest pending age;
- per-checkpoint caught-up/behind turns;
- canonical-prefix changed/missing detection;
- tracked rejected-lower-quality evidence counts.

Future health work should add scheduler retry/error history, handoff byte-budget
telemetry, richer unresolved-conflict reporting, and broader incomplete
conversation evidence diagnostics.
