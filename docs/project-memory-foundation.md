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

This is the first protection against a live collector overwriting a more
complete OpenAI export or earlier complete snapshot with a partial transcript.
Longer term, CHIM should retain multiple evidence snapshots and materialize a
canonical conversation view instead of replacing snapshots in place.

## Schema lifecycle

Project Memory Foundation introduces schema version 2.

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
- schema-v1 restore compatibility.

It does not yet provide:

- a Memory Compiler;
- automatic extraction from new conversation turns;
- memory-first semantic search;
- a DevSpace HandoffPacket containing working memory;
- automatic resolution/supersession policies;
- memory health/inspection CLI.

This boundary prevents an LLM from writing long-term state before provenance,
conflict handling, and compiler rules are implemented and tested.

## Next phases

### Phase 2 — Evidence snapshots

Replace destructive conversation refresh semantics with retained evidence
snapshots plus a canonical materialized view. Define quality/source precedence
without assuming that newest or longest is always best.

### Phase 3 — Incremental Memory Compiler

Process only new evidence since the last compiled turn/snapshot and emit
candidate operations:

- add
- update
- resolve
- supersede
- archive

Compiler output must preserve provenance and must not silently override stronger
evidence.

### Phase 4 — Memory-first retrieval

Answer ordinary project-state/history questions from MemoryItems first.
Conversation search becomes a deeper evidence lookup rather than the default
knowledge surface.

### Phase 5 — DevSpace HandoffPacket

DevSpace should combine:

- collaboration memory;
- ProjectWorkingMemory;
- bounded recent episodic continuation;
- live repository snapshot;
- authoritative project references.

The dynamic memory portion should remain bounded. Repository state and
authoritative project files outrank stale historical memory.

### Phase 6 — Memory health

Expose diagnostics for:

- working-memory age;
- active/resolved/superseded item counts;
- unresolved conflicts;
- incomplete conversation evidence;
- handoff byte budget;
- stale/unverified memory;
- skipped lower-quality snapshots.
