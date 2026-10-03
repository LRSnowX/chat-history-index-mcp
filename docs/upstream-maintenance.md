# Upstream maintenance

This branch is an upstream-compatible specialized fork of
`davidjbeveridge/chat-history-index-mcp`, not a replacement product line.
Preserve the upstream AI Conversation Index product model unless a demonstrated
local requirement justifies divergence.

The upstream product remains responsible for conversation collection, canonical
local archival/indexing, normalized ingestion, search, and MCP access. Fork-local
project-memory capabilities are a specialization built on top of that evidence
layer; they should not turn the index into an autonomous agent or opaque
knowledge brain.

## Current checkpoint

As of 2026-10-03:

- upstream remote: `https://github.com/davidjbeveridge/chat-history-index-mcp.git`
- fork remote: `https://github.com/LRSnowX/chat-history-index-mcp.git`
- upstream/main: `e994d1645c42`
- local/integration: `978085ce4436`
- merge base: `e994d1645c42`
- divergence: `0` upstream-only commits, `54` fork-only commits
- `git merge-tree --write-tree local/integration upstream/main`: clean

The fork is therefore currently an additive descendant of upstream main. Do not
rewrite history merely to reduce the local commit count.

## Product boundary

Keep these responsibilities upstream-aligned whenever practical:

- provider conversation collection and normalized interchange;
- canonical archive/index ownership;
- SQLite/FTS search infrastructure;
- source-aware ingestion and conversation identity;
- read-only/search MCP behavior;
- backup, restore, diagnostics, and host migration.

Fork-local specialization may add capabilities such as multilingual retrieval,
ChatGPT live collection, project-scoped memory views, explicit confirmation, and
DevSpace integration, but should prefer additive modules, metadata, CLI/operator
surfaces, and MCP extensions over replacing the core conversation model.

Do not add graph memory, autonomous retain/reflect loops, mental models, or other
knowledge-engine product concepts merely because another memory product has them.
Such a change requires an explicit product decision and evidence that the
conversation-index model can no longer meet the real requirement.

## Upstreamability classes

Use these classes when reviewing local work:

### A. Generic upstream candidates

Capabilities that solve a general AI Conversation Index problem should be
designed so they could be contributed upstream. Current examples include:

- multilingual retrieval quality;
- source/ingestion correctness hardening;
- collector reliability and incomplete-read fail-closed behavior;
- retrieval provenance or diagnostics that do not alter the product model.

### B. Additive fork extensions

Capabilities that are useful locally but can remain behind a clear seam:

- project Working Memory and confirmation metadata;
- operator-only memory authoring/review flows;
- DevSpace-oriented project aliases and handoff support;
- manual memory-compiler handoff;
- ChatGPT live-collector service scripts.

Keep these removable. If upstream later provides an equivalent generic
capability, prefer migrating to upstream and deleting the fork-local duplicate.

### C. Core divergence hotspots

Review these upstream-owned files first whenever upstream moves:

- `crates/chat-history-core/src/search.rs`
- `crates/chat-history-core/src/ingest.rs`
- `crates/chat-history-core/src/openai.rs`
- `crates/chat-history-core/src/sql.rs`
- `crates/chat-history-core/src/db.rs`
- `crates/chat-history-mcp/src/main.rs`
- `crates/chat-history-cli/src/main.rs`

A clean textual merge in one of these files is not enough; verify the same
search, ingestion, database, and MCP contracts semantically.

### D. Dormant capabilities

The automatic memory compiler/scheduler is not part of the default ChatGPT-first
workflow. Keep it dormant unless the user explicitly changes that product
decision. Do not deepen core coupling for a dormant path.

## Database schema compatibility

Upstream currently writes `PRAGMA user_version = 1`. This fork has already
published local schema markers `2` through `6`:

- `2`: project-memory foundation;
- `3`: retained conversation evidence snapshots;
- `4`: staged memory candidates;
- `5`: promotion review evidence;
- `6`: project memory aliases.

Those versions exist in real user databases and are immutable fork history.
Do not renumber or rewrite them.

The fork does not currently maintain a true sequential migration registry:
`open_database` applies idempotent schema creation/migrations and then records
the current schema marker. Because upstream also owns `PRAGMA user_version`,
blindly continuing with fork versions `7`, `8`, `9`, ... would reserve numbers
that upstream may later use for unrelated migrations.

Therefore:

1. Do not increment `CURRENT_SCHEMA_VERSION` beyond `6` as routine fork work.
2. If upstream/main changes its `user_version` or migration model, stop the
   ordinary sync and design an explicit reconciliation before merging.
3. Do not downgrade or rewrite existing v2-v6 databases merely to make the
   numbering prettier.
4. Prefer new fork-local tables/columns that can be created idempotently without
   consuming another shared sequential version until the version-ownership
   strategy is deliberately redesigned.
5. Any future redesign must include legacy v1 and fork v2-v6 upgrade tests,
   backup/restore verification, and a real-data migration check before release.

## Sync trigger

Reassess upstream as soon as `upstream/main` moves. Prefer a small early sync
over accumulating a large semantic merge.

A sync review should answer:

1. Which upstream commits touch the core divergence hotspots above?
2. Did upstream add a capability that should replace local code?
3. Does upstream change SQLite schema/version ownership?
4. Can local behavior remain additive instead of modifying the new upstream
   contract?
5. Do collector, MCP, CLI, install, or data-home assumptions change?

## Safe sync procedure

Use the existing upstream remote and keep the fork branch independently
reviewable:

```bash
git fetch upstream main
git rev-list --left-right --count upstream/main...local/integration
git diff --stat upstream/main...local/integration
git log --oneline local/integration..upstream/main
git merge-tree --write-tree local/integration upstream/main
```

When upstream has moved, inspect overlapping commits before choosing merge or
rebase. Do not force-push shared fork history as routine maintenance.

After resolving a sync:

- run focused tests for every overlapping ingestion/search/database/MCP
  contract;
- run `cargo test --workspace`;
- run `cargo check --workspace --all-targets`;
- run `git diff --check` and inspect the final diff;
- verify the installed/self-hosted path when collector, MCP, or database
  behavior changed.

No upstream PR is required merely because a local enhancement exists. Contribute
only coherent generic changes that fit the upstream product model.
