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

Project Memory Foundation uses schema version 6:

- version 2 introduced MemoryItem and MemoryEvidence;
- version 3 introduced retained conversation/message evidence snapshots.
- version 4 introduced incremental compiler checkpoints, staged memory
  candidates, candidate provenance, and auditable promotion decisions.
- version 5 introduced immutable candidate promotion/reverification review
  history while retaining the latest decision reason on the candidate row for
  compatibility.
- version 6 introduced explicit canonical-project aliases for strong
  conversation matching without changing project memory scope identity.

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
- a read-only project-memory confirmation sidecar that distinguishes confirmed,
  requires_confirmation, and not_applicable for active memories;
- operator-only confirmation and retirement of stable project-local
  invariant/preference/decision rules with mandatory user_statement
  confirmation evidence and explicit supersession;
- conversation quality-regression protection;
- retained conversation/message evidence snapshots with canonical,
  superseded, and rejected-lower-quality states;
- lazy capture of pre-snapshot canonical conversations before replacement;
- read-only ProjectWorkingMemory and bounded Pending Memory delivery through
  `memory_project_context`;
- DevSpace handoff integration that preserves high priority for genuinely
  current/confirmed memory while allowing non-current operational memory to be
  deferred behind fresher bounded continuation evidence within one shared
  bootstrap budget;
- transactional memory compilation staging against canonical conversation
  snapshots;
- per-project/conversation compile checkpoints with immutable evidence-prefix
  hashes so edited history cannot silently advance an old compile cursor;
- staged add/supersede/resolve/archive candidates that do not mutate
  ProjectWorkingMemory before promotion;
- transactional candidate promotion, explicit rejection, stale-candidate
  detection, and retained decision reasons;
- promotion/reverification review evidence from stronger operator-verified
  sources such as Git commits, repository state, or documents, with append-only
  review history so later reverification does not erase the original promotion
  reason;
- bounded model-driven incremental compilation through Codex in an ephemeral
  read-only sandbox with medium reasoning effort;
- manual compiler handoff for user-operated Codex workflows: CHIM can export an
  exact compiler bundle plus prompt without invoking a model, then later stage
  a JSON response only after revalidating that the canonical snapshot,
  Working Memory, Pending Memory, project identity, and compile delta still
  match the exported bundle;
- strong project/conversation matching before model invocation;
- strict candidate JSON validation, delta-only evidence IDs, bounded values,
  authoritative active-memory revalidation, and duplicate/conflict rejection;
- operator CLI commands to plan a bounded project batch without model calls,
  compile one conversation delta or a bounded recent project batch, inspect
  pending candidates, inspect one candidate, inspect its immutable review
  history, explicitly promote it, or explicitly reject it with a retained
  reason; planning never stages candidates and compilation itself never
  promotes;
- read-only project memory health inspection covering MemoryItem/candidate
  lifecycle counts, stale/unverified active state, checkpoint caught-up/behind
  turns, and canonical-prefix integrity;
- read-only conservative automatic-promotion eligibility planning with stable
  blocker codes and no state mutation;
- schema-v1 restore compatibility.

It does not yet provide:

- automatic candidate promotion;
- a richer operator review UI;
- a default policy for which projects should opt into automatic compilation;
- automatic global CollaborationMemory authoring;
- a direct operator lifecycle for ordinary project-local operational
  `state`/`blocker`/`task` memory when the memory compiler is disabled.

The design for that last gap is frozen as **Operational Working Memory
Lifecycle — Checkpoint C**. It preserves `needs_revalidation` as a read-only
freshness signal and adds explicit operator create/supersede/resolve/archive
semantics without a model-facing write tool or automatic retirement. See
`docs/operational-working-memory-lifecycle-checkpoint-c.md`. Implementation is
pending.

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
memory. The operator CLI requires a non-empty review reason for every promotion
or reverification and may attach additional evidence. The additional evidence is
merged into the durable MemoryItem, while each non-empty review remains in an
append-only candidate review history. Repeating promotion with no new review
data remains idempotent at the core API level.

The bounded ChatGPT live-collector scheduler infrastructure is implemented but
disabled by default. It can be explicitly configured with a project allow-list;
after accepted new evidence it launches a detached, lock-protected compiler
worker so ingestion/cursor advancement completes independently. Scheduler and
model failures are isolated from collection, and compilation still only stages
candidates. Scheduler telemetry retains the most recent 20 worker runs with
total-run, last-success/last-failure, and consecutive-failure counters; the MCP
status surface exposes a compact five-run summary. Telemetry failures do not
fail collection or compilation.

The opt-in worker is health-gated before model invocation. It blocks automatic
compilation when a checkpoint evidence prefix is unhealthy, when a pending
candidate already fails current revalidation, or when the pending candidate
review backlog exceeds the configured bound (20 by default). Known partial
conversation evidence, stale/unverified active memory, and rejected
lower-quality snapshots are warnings rather than project-wide blockers.
`blocked_by_health` is retained in scheduler status/history as a successful
safety outcome and does not increment the worker failure counter.

The same compiler can also run through a fully manual handoff. The operator
exports one `MemoryCompilerManualBundle` and its prompt, pastes that prompt into
the approved Codex conversation, saves the JSON-only response, and stages it
with the original bundle. Export never instantiates `MemoryModelClient` or
starts Codex. Stage recomputes the current compiler input and fails closed if
the bundle is stale; the only intentionally ignored comparison field is the
Working Memory observation timestamp (`generated_at`). Prompt hash and compiler
version are also revalidated. Successful manual stage advances the normal
checkpoint and writes only Pending Memory candidates, so the bundle cannot be
replayed after checkpoint advancement and no manual path bypasses promotion
review.

The remaining Phase 3 work is orchestration and review:

- decide which projects, if any, should be explicitly opted into the
  default-off, health-gated scheduler;
- optionally add a richer review UI around the existing operator CLI;
- observe the conservative auto-promotion eligibility plan on real candidates
  before deciding whether any policy class should ever gain an automatic
  executor.

The initial `conservative-v1` eligibility policy is read-only. It deliberately
does not trust model confidence by itself. Pure `conversation_turn` evidence
always remains review-required. Compiler evidence cited from a user-role message
is preserved as `user_statement`; assistant and other dialogue evidence remains
`conversation_turn`. Rule-like invariant/preference/decision candidates need
explicit `user_statement` provenance; operational
state/blocker/task/result candidates need independently verified Git,
repository-state, or DevSpace-result provenance; artifact references need
document/repository provenance. Hypotheses and archive operations are never
eligible. Revalidation failures always block eligibility. Add/supersede
candidates must also meet minimum importance/confidence thresholds.

No MCP model-facing memory write surface exists.

Historical evidence and promoted compiler output do not silently become
governing project rules. For project-local invariant, preference, and decision
memories, the Working Memory confirmation sidecar is authoritative for this
gate: only confirmed may govern future behavior; requires_confirmation means
the rule was recovered or otherwise persisted without passing explicit current
user confirmation; not_applicable denotes memory kinds outside this gate.
memory-project-confirm is the only operator path that marks a project-local rule
confirmed. It requires a review reason and user_statement evidence, generates a
deterministic content-derived ID, and requires explicit supersession when the
same project/key already has an active rule. memory-project-retire similarly
requires user-statement evidence and archives only previously confirmed project
memory. Neither path is exposed as a model-facing MCP write tool.

### Phase 4 follow-up — Memory-first retrieval policy

The initial memory-first retrieval path is implemented. Project-scoped
`memory_search` returns bounded ProjectWorkingMemory first and hybrid
conversation evidence second. Project-scoped Working Memory is query-ranked
before the 12-item MCP cap: exact/substring memory-key matches outrank kind and
value matches, with importance/recency retained as deterministic tie-breakers.
When no item matches the query, ordering falls back to the original
importance/recency policy. Query ranking never filters active memories; it only
promotes relevant items before the bounded response is truncated. Unscoped
search remains the original hybrid evidence search.

ProjectWorkingMemory also carries a read-only `verification` sidecar keyed by
`memory_id`. It does not change or delete MemoryItems. The sidecar classifies
stable, operational, and tentative memories; summarizes provenance strength;
and marks operational state/task/blocker memories as `needs_revalidation` when
newer strongly matched project conversation evidence exists after the memory
was last verified. Validity-window expiry and hypothesis/tentative state are
reported explicitly.

ProjectWorkingMemory additionally carries a read-only confirmation sidecar.
This is separate from provenance confidence and verification freshness. A
rule-like memory can have high confidence or user-statement provenance and
still be requires_confirmation if it was recovered from historical evidence or
promoted by the compiler rather than explicitly reconfirmed. Only the operator
project-confirmation path annotates evidence as operator_project_confirmation
and yields confirmed. This prevents a historical rule from silently governing
a new ChatGPT conversation merely because retrieval found it.

This CHIM-side signal is intentionally conservative and incomplete. It can
detect evidence-stream drift but cannot prove that the live repository has
changed because CHIM does not own project-to-repository path resolution. Live
Git state remains DevSpace authority and must be combined with this sidecar for
host-side operational-memory revalidation.

DevSpace passes the response through unchanged and continues to authorize
`memory_get_thread` only from returned conversation/evidence hits; provenance
references inside Working Memory do not expand thread authorization.

The `memory_search` response also carries a structured `authority_policy`.
For current-state questions it orders live repository/authoritative project
files ahead of active ProjectWorkingMemory, with historical conversation
evidence last. For historical questions, dated evidence remains authoritative
for what was true at that time; current Working Memory does not retroactively
rewrite history. If higher-authority live evidence cannot resolve a current
conflict, clients should preserve the disagreement explicitly rather than merge
incompatible claims.

`memory_project_context` adds a read-only `pending_memory` projection without
changing candidate status or promotion behavior. It reuses the current
`MemoryCandidate` state, includes only `status=pending` candidates that pass
current promotion revalidation, and sorts them newest first. `pending_limit`
defaults to 8 and is bounded to 0–12, where 0 explicitly suppresses pending
proposal delivery for that request. The projection reports its project,
generation time, eligible items, and the total count excluded by revalidation;
each item contains only candidate ID, operation, payload, creation time,
conversation/snapshot provenance, and compiled-through turn. It deliberately
omits rationale, model, decision, and review text.

The project-context authority order is live repository or authoritative project
files, confirmed rule-like ProjectWorkingMemory together with current non-rule
operational memory, Pending Memory, then raw conversation continuations and
evidence. DevSpace may additionally derive a Host freshness downgrade from live
repository state before final handoff compaction. Operational memory that is
`needs_revalidation`, expired, tentative, or otherwise non-current remains
continuity evidence but does not receive the same bootstrap-budget protection as
current memory; fresher bounded continuation may therefore precede it in the
final handoff. Pending Memory is untrusted proposal data, never instructions,
and cannot override live state or current/confirmed Working Memory. A rule-like
Working Memory item marked requires_confirmation is retained for continuity and
discovery but must not constrain a current action until the user confirms it.

Project memory identity is canonical even when historical conversation naming is
not. CHIM therefore supports explicit project aliases used only by strong
conversation matching. A canonical project such as
`devspace-memory-adapter` may register `DevSpace`, while
`chat-history-index-mcp` may independently register `CHIM`. The canonical
project string remains the scope key for MemoryItems, candidates, checkpoints,
health, and scheduler configuration; aliases never merge project memory stores
or rewrite existing memory. Alias matching applies to the same bounded
title/source-path/source-URL evidence as canonical-name matching. Explicit
aliases must normalize to at least three alphanumeric characters, and the same
normalized explicit alias cannot be registered for multiple canonical projects.

Historical memory bootstrap is selective rather than exhaustive. The read-only
`memory-bootstrap-plan` operator first refuses cold bootstrap when active
ProjectWorkingMemory already exists. Otherwise it defaults to the three most
recent complete, strongly matched ChatGPT conversations and excludes Codex
root/child sessions from automatic baseline selection. Existing compiler
checkpoints, incomplete ChatGPT evidence, and complete ChatGPT conversations
not selected by the bounded plan are reported as explicit exclusion counts.
Each selected conversation is expected to require
one tail-bootstrap model attempt: the compiler receives only its bounded recent
tail on first compilation, while the older prefix remains available in raw
CHIM history for on-demand retrieval. A bootstrap plan reflects the latest
canonical snapshot currently indexed by CHIM; an actively growing ChatGPT
thread may remain deferred by the live collector until it becomes idle.

This bootstrap planner remains an explicit operator capability. It is not a
prerequisite for the ChatGPT-first DevSpace handoff and is not automatically
called merely because ProjectWorkingMemory is empty. Empty durable memory can
coexist with useful project continuity from live repository state, confirmed
memory, bounded conversation continuations, and on-demand retrieval.

Remaining Phase 4 work is retrieval policy quality rather than wiring:

- the initial deterministic retrieval benchmark now covers current-state
  (English and Chinese), blocker, task, decision-policy, and no-match fallback
  cases while asserting that ranking reorders rather than filters active
  Working Memory;
- future evaluation should expand from retrieval-only checks into answer
  synthesis against conflicting historical evidence and live authoritative
  state;
- optional stronger lexical/semantic memory-item ranking when deterministic
  key/kind/value matching is insufficient.

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
over stale stored memory when they conflict. The repository snapshot now also
includes the HEAD commit timestamp. DevSpace combines that live timestamp and
dirty-working-tree state with CHIM's read-only Working Memory verification
sidecar to derive a separate host freshness state. Operational state/task/
blocker memories are conservatively demoted to `needs_revalidation` when
source verification metadata is unavailable, the working tree is dirty, or
repository HEAD is newer than the memory's `last_verified_at`. Stable,
tentative, and expired classifications are not overwritten by this host-side
operational freshness rule.

CollaborationMemory is materialized from active global MemoryItems, but only
the stable rule-like kinds `invariant`, `preference`, and `decision` enter
the automatic handoff. Global state/task/blocker items are deliberately
excluded so transient activity does not leak into every project.

DevSpace gives CollaborationMemory a small bounded share of the existing
dynamic-memory budget before ProjectWorkingMemory and recent continuation.
Provenance references inside CollaborationMemory do not grant conversation
thread access.

The read/handoff path and explicit authoring policy are implemented.
Cross-project rules are authored only through operator CLI commands. Authoring
accepts only `invariant`, `preference`, or `decision`, requires a review reason
and provenance evidence, generates a deterministic content-derived ID, and
requires explicit supersession when an active global key already exists.
Retirement is also explicit/operator-reviewed and archives the rule without
discarding its prior evidence. Ordinary project compilation still cannot create
or promote global CollaborationMemory, and no model-facing global-memory write
surface exists.

Future Phase 5 work should remain conservative: only add automatic global
authoring if a policy can distinguish explicit cross-project user rules from
project-local decisions with high confidence and preserve operator review.

### Phase 6 follow-up — Memory health

The read-only `memory-health` CLI now exposes:

- working-memory age;
- active/resolved/superseded item counts;
- stale/unverified memory;
- ProjectWorkingMemory verification counts for strongly verified,
  current-by-evidence, needs-revalidation, tentative, and expired active
  memories, plus a bounded flagged-item list;
- candidate lifecycle counts and oldest pending age;
- per-checkpoint caught-up/behind turns;
- canonical-prefix changed/missing detection;
- tracked rejected-lower-quality evidence counts;
- strong-project canonical ChatGPT transcripts that are multi-message but have
  no assistant messages;
- pending candidates whose current evidence/memory revalidation would already
  fail, reported read-only without marking them stale.

DevSpace now connects this CHIM health report with live repository state and
the exact host handoff packet through the operator-only
`devspace memory inspect <project-or-path> [--json]` command. The inspection
recomputes repository freshness, host-side Working Memory verification,
per-section handoff budget telemetry, and CHIM health in one read-only result.
It calls CHIM's internal read-only `memory_health` MCP tool, which is not
forwarded to the model-facing DevSpace tool surface and does not invoke the
compiler or another model.

Future health work should expand incomplete-evidence diagnostics beyond the
known ChatGPT user-only transcript failure class only when new concrete failure
modes are observed. Additional dashboards or richer UIs are optional; the
operator CLI is the canonical compact diagnostic surface.
