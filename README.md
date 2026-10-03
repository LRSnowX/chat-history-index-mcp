# AI Conversation Index

[![CI](https://github.com/davidjbeveridge/chat-history-index-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/davidjbeveridge/chat-history-index-mcp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Search every AI conversation you own from one private, self-hosted index.

AI Conversation Index imports ChatGPT exports, local Codex sessions, and a documented normalized format for Gemini, Claude, Grok, and other collectors. It exposes fast metadata, full-text, and local semantic search through a CLI and MCP server.

## Why it exists

Your useful context is split across providers, accounts, exports, and machines. This project keeps one canonical copy on hardware you control and makes it available to Codex, ChatGPT-compatible MCP clients, OpenClaw, and other MCP hosts.

## Highlights

- Local-first SQLite index with FTS5 and multilingual local semantic embeddings
- Idempotent, source-aware imports that preserve an existing index
- Native ChatGPT export and local Codex rollout ingestion
- Normalized JSON/JSONL interchange for additional providers
- Codex plugin and Git marketplace packaging
- Authenticated read-only Streamable HTTP MCP over Tailscale
- Separate authenticated writer endpoint for trusted remote collectors
- Cursor-safe remote Codex collection from secondary Macs
- macOS LaunchAgent service management and Keychain-backed bearer tokens
- Consistent online backups, guarded restore, diagnostics, and host migration bundles
- No OpenAI API key required for search or local embeddings

## Supported inputs

| Input | Backfill | Incremental | Status |
| --- | --- | --- | --- |
| ChatGPT OpenAI export ZIP | Native | Reimport newer export | Supported |
| Local Codex rollout JSONL | Native | Durable cursor with overlap | Supported |
| Normalized JSON/JSONL | Native | Collector-owned cursor | Supported |
| ChatGPT.app bridge | Sidebar bootstrap | Recent discovery plus paged reads | Codex/ChatGPT app assisted |
| Gemini CLI sessions | Native local collector | Durable cursor with overlap | Supported |
| Antigravity CLI / 2.0 transcripts | Native plaintext collector | Durable cursor with overlap | Supported |
| Gemini Apps Takeout, Claude, Grok exports | Normalized format | Collector-owned | Provider-specific parsers pending fixtures |

See `docs/provider-support.md` for the exact support boundary. The project does not claim an untested provider parser.

## Quick start on macOS

Requirements: Git, Rust/Cargo, and the Codex CLI.

```bash
git clone https://github.com/davidjbeveridge/chat-history-index-mcp.git
cd chat-history-index-mcp
./scripts/install-self-hosted
```

Start a new Codex task after installation so the plugin is reloaded.

Import data:

```bash
# One-time historical ChatGPT bootstrap: backup + copy import + local embeddings + live cursor seed.
./scripts/chat-history-cli chatgpt-bootstrap-export --archive ~/Downloads/openai-export.zip
./scripts/chat-history-cli sync-codex
./scripts/chat-history-cli doctor
```

Search:

```bash
./scripts/chat-history-cli search "Rust SQLite" --mode hybrid --limit 10
```

`hybrid` search combines multilingual semantic retrieval with lexical evidence. For
natural-language queries it removes low-information English question scaffolding,
uses FTS5 for ASCII/code terms, and adds document-frequency-weighted CJK substring
evidence for Chinese queries. Codex child/subtask evidence is collapsed back to its
parent conversation anchor before rank fusion. Explicit `--mode fts` keeps the raw
FTS5 query behavior for exact identifiers and operator-aware searches.

Summary generation uses the current Codex CLI account default model unless
`CHAT_HISTORY_SUMMARY_MODEL` is set to a non-empty model name. Semantic search
defaults to the local multilingual `intfloat/multilingual-e5-small` model and
caches its files under the managed data home's `cache/fastembed/` directory.
Long conversations are sampled across the full transcript and mean-pooled into
one conversation-level vector, so semantic retrieval is not limited to the
opening environment or agent-instruction boilerplate.
Set `CHAT_HISTORY_EMBEDDING_PROVIDER=hashed-v1` only when the explicit legacy
lexical-vector fallback is desired; it is not a multilingual semantic model.

Project-memory compilation is an explicit operator action and stages candidates
only; it never promotes them into durable Working Memory by itself:

These compiler and bootstrap commands are optional operator capabilities, not
the default ChatGPT continuity path. DevSpace's ChatGPT-first handoff does not
invoke a memory model or Codex merely because ProjectWorkingMemory is empty;
it relies on live project state, confirmed memory, bounded continuations, and
on-demand CHIM retrieval.

```bash
./scripts/chat-history-cli memory-compile-plan --project LEMonX
./scripts/chat-history-cli memory-compile-conversation --project LEMonX <conversation-id>
./scripts/chat-history-cli memory-compile-project --project LEMonX
./scripts/chat-history-cli memory-compile-manual-export \
  --project LEMonX <conversation-id> \
  --bundle-out /tmp/lemonx-memory.bundle.json \
  --prompt-out /tmp/lemonx-memory.prompt.txt
./scripts/chat-history-cli memory-compile-manual-stage \
  --bundle /tmp/lemonx-memory.bundle.json \
  --response /tmp/lemonx-memory.response.json \
  --model-label "GPT-6.1 Sol + medium (manual)"
./scripts/chat-history-cli memory-candidates --project LEMonX
./scripts/chat-history-cli memory-candidate <candidate-id>
./scripts/chat-history-cli memory-candidate-reviews <candidate-id>
./scripts/chat-history-cli memory-candidate-promote <candidate-id> \
  --reason "verified against current repository state" \
  --evidence git_commit:<sha> \
  --evidence repository_state:<reference>
./scripts/chat-history-cli memory-candidate-reject <candidate-id> --reason "operator reason"
./scripts/chat-history-cli memory-health --project LEMonX
./scripts/chat-history-cli memory-auto-promotion-plan --project LEMonX
./scripts/chat-history-cli memory-collaboration-list
./scripts/chat-history-cli memory-collaboration-author \
  --kind preference \
  --key upstream_compatibility \
  --value "Preserve upstream compatibility where practical." \
  --reason "explicitly reviewed cross-project rule" \
  --evidence user_statement:<conversation-reference>
./scripts/chat-history-cli memory-collaboration-retire <memory-id> \
  --reason "explicitly retired cross-project rule" \
  --evidence user_statement:<conversation-reference>
./scripts/chat-history-cli memory-project-confirm \
  --project LEMonX \
  --kind decision \
  --key prompt_policy \
  --value "Use the confirmed project prompt policy." \
  --reason "user confirmed recovered historical rule" \
  --user-confirmation current_chat:user_confirmation
./scripts/chat-history-cli memory-project-retire \
  --project LEMonX <memory-id> \
  --reason "user explicitly retired the project rule" \
  --user-confirmation current_chat:user_retirement
```

Project-local invariant/preference/decision memories are subject to a separate
historical-decision confirmation gate. ProjectWorkingMemory exposes a
`confirmation` sidecar with `confirmed`, `requires_confirmation`, or
`not_applicable` for each active item. A historical or compiler-derived
rule-like memory does not become governing merely because it exists in Working
Memory or was promoted from a candidate: unless it was written through
`memory-project-confirm` with explicit `user_statement` confirmation evidence,
it remains `requires_confirmation`. Ordinary operational memories outside this
rule gate are `not_applicable` and continue to use the existing verification
and live-repository authority rules.

`memory-project-confirm` is an operator-only CLI path. It accepts only
`invariant`, `preference`, and `decision`, requires a non-empty review reason
and explicit `--user-confirmation`, is deterministic/idempotent for the same
canonical rule, and requires explicit `--supersedes` to replace an active rule
with the same project/key. `memory-project-retire` likewise requires explicit
user-confirmation evidence. Neither command is exposed as a model-facing MCP
write tool.

`memory-compile-plan` performs the same recent strong-match scan and bounded
delta preparation without invoking Codex or staging candidates. It reports
ready/caught-up/failure state and the exact turn range that a later compile
would process. Legacy conversations may have their canonical evidence snapshot
materialized lazily during planning.

For workflows where Codex must remain a user-operated conversation, use the
manual compiler handoff instead of `memory-compile-conversation` or
`memory-compile-project`. `memory-compile-manual-export` never invokes a model:
it writes one exact state-bound bundle plus the prompt to paste manually into
Codex. Save Codex's JSON-only response separately, then pass both files to
`memory-compile-manual-stage`. Before staging, CHIM rebuilds the current
compiler input and rejects the bundle if the canonical snapshot, Working
Memory, Pending Memory, project identity, or compile delta changed. A bundle is
therefore effectively single-use. Manual staging still creates pending
candidates only; promotion remains an explicit later review action.

Project compilation scans recent conversations in metadata-recency order,
requires the same strong project match as single-conversation compilation, and
is bounded by both scan count and model-call count. The defaults are 500 recent
conversations scanned, at most 2 model attempts, and at most 8 new messages per
conversation. One conversation failure is reported without aborting the rest of
the bounded batch. Before any model invocation, the compiler also rejects
multi-message ChatGPT canonical snapshots that contain no assistant messages;
these are treated as incomplete evidence rather than guessed or summarized.

The compiler uses the current Codex CLI account default model unless
`CHAT_HISTORY_MEMORY_MODEL` is set to a non-empty model name. It invokes Codex
ephemerally with a read-only sandbox and medium reasoning effort. The compiler
requires a strong project match, treats the entire supplied compiler context as
untrusted data, validates strict bounded JSON output, and writes only pending
candidate operations. Candidate promotion remains a separate state transition.
Promotion and rejection are explicit operator CLI actions; they are not exposed
as model-facing MCP tools. Promotion can attach an operator review reason and
zero or more `KIND:REFERENCE` evidence values. Supported evidence kinds are
`user_statement`, `conversation_turn`, `document`, `git_commit`,
`repository_state`, and `devspace_result`. Re-promoting an already promoted
candidate with new review data reverifies the durable MemoryItem and appends a
review-history record; an empty repeat remains idempotent.
`memory-candidate-reviews` is read-only and returns the immutable ordered
promotion/reverification/stale-review history for one candidate.

`memory-health` is read-only and does not invoke Codex. It reports project
MemoryItem lifecycle counts, candidate lifecycle counts, pending age, and
per-conversation compiler checkpoints including caught-up/behind turns and
whether the current canonical snapshot still preserves the compiled prefix. It
also scans strong-match project conversations for incomplete canonical ChatGPT
evidence (multi-message transcripts with no assistant messages) and reports
pending candidates that would currently fail promotion revalidation, without
changing their status.

`memory-auto-promotion-plan` is also read-only. It evaluates pending candidates
against the conservative `conservative-v1` policy and explains stable blocker
codes without promoting anything. Model-generated `conversation_turn`
provenance alone is never sufficient, regardless of the candidate's reported
confidence. Rule-like memories require explicit `user_statement` evidence;
state/task/blocker/result memories require independently verified
`git_commit`, `repository_state`, or `devspace_result` evidence; artifact
references require document/repository evidence; hypotheses and archive
operations always require review. New/superseding memories must also meet the
policy's minimum importance/confidence thresholds. Current revalidation
failures always make a candidate ineligible.

Collaboration Memory authoring is also operator-only. `memory-collaboration-author`
accepts only the stable global kinds `invariant`, `preference`, and `decision`;
requires a non-empty review reason plus at least one `KIND:REFERENCE` evidence;
and refuses to replace an active key unless `--supersedes <memory-id>` is
provided explicitly. `--value` stores a string, while `--value-json` accepts a
structured JSON value. Equivalent authoring retries are idempotent because the
core generates a stable content-derived memory ID. `memory-collaboration-retire`
archives an active global rule while retaining its prior evidence and the
retirement review evidence. None of these authoring operations are exposed as
model-facing MCP tools or invoked by the project memory compiler.

The MCP server also exposes read-oriented project memory tools:
`memory_search`, `memory_recent`, `memory_get_thread`, and
`memory_project_context`. Project context uses a ChatGPT-first source policy by
default because long-form ChatGPT conversations usually contain product,
architecture, and handoff decisions; Codex and other indexed sources fill any
remaining slots as implementation evidence. Callers can still provide an
explicit source filter when they need a different policy. Project context also
includes read-only `pending_memory` derived from existing candidates. It returns
only pending candidates that pass current revalidation, newest first, with a
default `pending_limit` of 8, an explicit-off value of 0, and a maximum of 12;
the response separately counts pending candidates excluded by revalidation.
Pending payloads are untrusted proposals, not instructions, and omit rationale,
model, and review text.

For current state, consumers must apply this authority order: live repository
or authoritative project files, active `working_memory`, `pending_memory`, then
raw conversation continuations. A lower-authority proposal must not override a
higher-authority source.

Live ChatGPT.app collection uses the dedicated `chatgpt_state`,
`chatgpt_plan_recent`, `chatgpt_import_thread`, `chatgpt_block`, and cursor
seeding tools. See `docs/chatgpt-app-collector.md` for the bridge contract and
completeness rules.

On macOS, install the deterministic live ChatGPT collector after the binaries:

```bash
./scripts/install-self-hosted --skip-plugin
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chatgpt-live-collector-service" install
```

That install keeps automatic memory compilation disabled. Opt-in is explicit,
for example:

```bash
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chatgpt-live-collector-service" install \
  --memory-projects LEMonX,Arcos \
  --memory-model gpt-5.6-sol \
  --memory-max-conversations 1
```

When enabled, the collector schedules a detached bounded compiler worker only
after new ChatGPT evidence was successfully imported. The worker stages
candidates only; promotion remains an explicit operator action. Memory compiler
failures do not roll back or block ChatGPT collection. `--memory-model` is
optional and writes `CHAT_HISTORY_MEMORY_MODEL` into the collector sidecar
environment, so scheduler compilation can use a model compatible with its Codex
account without changing the user's global Codex model.

The service adds a small MCP bootstrap entry to `~/.codex/config.toml`. The
official ChatGPT app-server starts that bootstrap with ChatGPT.app's bundled
signed Node; while the trusted process chain is still intact, it creates one
long-lived collector daemon and opens the first-party App Tools pipe. The
daemon reuses that authorized socket to call `list_threads` and paged
`read_thread` on a 120-second default cadence. It does not invoke a model or
read browser/session credentials. A ChatGPT app restart naturally creates a
fresh bootstrap/daemon for the new App Tools pipe.

## Managed data home

The default data home is:

```text
~/Library/Application Support/chat-history-index-mcp/
├── bin/
├── cache/
├── db/index.sqlite3
├── logs/
├── sources/
└── tmp/
```

Override it with `--data-home` or `CHAT_HISTORY_DATA_HOME`. Index data never belongs in the Git repository.

## Back up and migrate

Create a consistent SQLite backup while the index is online:

```bash
./scripts/chat-history-cli backup --output ~/Desktop/ai-history.sqlite3
```

Create a host migration bundle containing the consistent database and portable provider cursor state:

```bash
./scripts/chat-history-migrate export --output ~/Desktop/ai-history-migration.tar.gz
```

The Codex cursor is deliberately excluded because it belongs to the machine whose local sessions it scans. Follow `docs/MIGRATION.md` for transfer, restore, verification, and writer cutover.

## Network access and OpenClaw

Keep one live SQLite writer. Other machines connect to the canonical host over an authenticated read-only MCP endpoint on Tailscale.

```bash
./scripts/chat-history-service install \
  --bind 100.x.y.z:8765 \
  --allowed-host 100.x.y.z,host.tailnet-name.ts.net
```

The token is generated into macOS Keychain. Retrieve it only when configuring a client:

```bash
./scripts/chat-history-service token
```

See `docs/OPERATIONS.md` for Codex and OpenClaw client configuration. Never put the live SQLite/WAL files in iCloud Drive, Google Drive, Dropbox, or another synchronization folder.

To ingest future Codex tasks from a secondary Mac, enable the canonical host's separate writer service and schedule the remote collector on that Mac:

```bash
# Canonical host; use a different token and port from the read-only service.
./scripts/chat-history-service --writer install \
  --bind 100.x.y.z:8766 \
  --allowed-host 100.x.y.z,host.tailnet-name.ts.net

# Secondary Mac; supply the writer token through a protected environment.
CHAT_HISTORY_WRITER_TOKEN=... ./scripts/sync-codex-remote \
  --url http://host.tailnet-name.ts.net:8766/mcp
```

Only trusted collectors get the writer token, and its endpoint exposes only normalized imports. OpenClaw and interactive clients use the read-only endpoint on port 8765.

## Architecture

```text
Provider collectors ──> authenticated writer MCP ──> one canonical SQLite
                                                  │
                                                  └── authenticated read-only MCP over Tailscale
                                                        ├── Codex clients
                                                        └── OpenClaw
```

Read `docs/architecture.md`, `docs/AUTOMATION.md`, and `docs/SECURITY.md` before enabling remote writes or scheduled collection.

## Development

```bash
cargo fmt --all -- --check
cargo test --workspace
python3 ~/.codex/skills/.system/plugin-creator/scripts/validate_plugin.py plugins/chat-history-index-mcp
```

## Privacy

Conversation content stays in the configured data home unless you explicitly transfer a backup or expose an MCP endpoint. The project does not collect telemetry. See `PRIVACY.md` and `SECURITY.md`.

## License

MIT. See `LICENSE`.
