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

The MCP server also exposes read-oriented project memory tools:
`memory_search`, `memory_recent`, `memory_get_thread`, and
`memory_project_context`. Project context uses a ChatGPT-first source policy by
default because long-form ChatGPT conversations usually contain product,
architecture, and handoff decisions; Codex and other indexed sources fill any
remaining slots as implementation evidence. Callers can still provide an
explicit source filter when they need a different policy.

Live ChatGPT.app collection uses the dedicated `chatgpt_state`,
`chatgpt_plan_recent`, `chatgpt_import_thread`, `chatgpt_block`, and cursor
seeding tools. See `docs/chatgpt-app-collector.md` for the bridge contract and
completeness rules.

On macOS, install the deterministic live ChatGPT collector after the binaries:

```bash
./scripts/install-self-hosted --skip-plugin
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chatgpt-live-collector-service" install
```

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
