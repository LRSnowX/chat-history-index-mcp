# Self-hosted architecture

Use exactly one canonical writer host. SQLite remains on that host's local disk in WAL mode.

```text
ChatGPT.app bridge ┐
Codex collectors ──┼──> authenticated writer MCP ──> canonical local SQLite
Provider imports ──┘                                      │
                                                          └──> authenticated read-only MCP over Tailscale
                                                      ├── Codex clients
                                                      └── OpenClaw
```

## Why one writer

SQLite WAL is not a network replication protocol. Synced folders and two active copies can create split-brain state, lost updates, or corruption. Backups are portable; the live database is not shared as a file.

## Access tiers

- Local stdio MCP: read and write, for the canonical host's Codex installation.
- Tailnet HTTP MCP: read-only by default, for other machines and OpenClaw.
- Optional writer HTTP MCP: a separate port and token, enabled only for a trusted remote collector.

The writer service defaults to port 8766 and `local.chat-history-index-mcp-writer`; the reader defaults to port 8765 and `local.chat-history-index-mcp`. They never share a bearer token.

HTTP transport requires both private-network reachability and a bearer token. Store tokens in an OS secret manager. Do not expose the bundled server directly to the public internet.

## ChatGPT collection

ChatGPT.app-assisted collection uses supported `list_threads` and paged `read_thread` capabilities in a signed-in app. The public Conversations API does not enumerate the ChatGPT sidebar. See `chatgpt-app-collector.md`.

## Portability

The Git repository contains code and plugin metadata. The managed data home contains binaries, index data, cursors, exports, logs, and backups. `scripts/chat-history-migrate` creates a consistent transfer bundle while excluding the host-local Codex cursor. After a host migration, each secondary Mac uses `scripts/sync-codex-remote` to feed its future local tasks to the canonical writer.
