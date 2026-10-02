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
- operator CLI commands to compile one conversation delta and inspect pending
  candidates; compilation stages only and never promotes automatically;
- schema-v1 restore compatibility.

It does not yet provide:

- automatic compiler scheduling after new conversation evidence arrives;
- an operator promotion/rejection CLI or review UI;
- automatic candidate promotion;
- memory-first semantic search;
- memory health/inspection CLI.

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
prefix and can emit staged candidate operations:

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

The remaining Phase 3 work is orchestration and review:

- trigger bounded compilation after accepted new evidence without blocking
  ingestion;
- expose operator review/promotion/rejection outside model-facing MCP tools;
- define conservative promotion policy classes if any operation is ever made
  automatic;
- add retry/health visibility for compiler failures and caught-up state.

No MCP model-facing memory write surface exists.

### Phase 4 — Memory-first retrieval

Answer ordinary project-state/history questions from MemoryItems first.
Conversation search becomes a deeper evidence lookup rather than the default
knowledge surface.

### Phase 5 follow-up — Complete DevSpace HandoffPacket

The read path now combines ProjectWorkingMemory with bounded recent episodic
continuation under one shared dynamic-memory budget. The remaining HandoffPacket
work should add and normalize:

- collaboration memory;
- live repository snapshot;
- authoritative project references.

Repository state and authoritative project files already outrank stored memory
in the host instruction contract. The remaining work is to expose those stronger
sources as explicit structured packet sections rather than only as host
instructions and separately loaded project files.

### Phase 6 — Memory health

Expose diagnostics for:

- working-memory age;
- active/resolved/superseded item counts;
- unresolved conflicts;
- incomplete conversation evidence;
- handoff byte budget;
- stale/unverified memory;
- skipped lower-quality snapshots.
