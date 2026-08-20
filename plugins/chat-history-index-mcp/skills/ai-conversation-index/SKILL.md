---
name: ai-conversation-index
description: Search, inspect, import, back up, restore, or maintain a private self-hosted AI conversation index for ChatGPT, Codex, Gemini, Claude, Grok, and normalized provider collectors.
---

# AI Conversation Index

Use the bundled MCP tools for search, conversation retrieval, related-conversation lookup, and index statistics. The MCP server uses the managed data home at `~/Library/Application Support/chat-history-index-mcp` unless `CHAT_HISTORY_DATA_HOME` overrides it.

## Safety

- Preserve existing records and use provider source IDs for idempotent upserts.
- Keep the live SQLite database on exactly one writer host's local disk.
- Never place SQLite, WAL, exports, transcripts, or tokens in Git or a synced folder.
- Expose only the authenticated read-only HTTP endpoint to ordinary clients and OpenClaw.
- Stop the canonical writer before restore or host cutover.
- Never inspect browser cookies, local storage, auth headers, or private provider endpoints.
- Reject truncated or incomplete transcripts and do not advance provider cursors after partial work.

## Operator CLI

The installed CLI is `~/Library/Application Support/chat-history-index-mcp/bin/chat-history-cli`.

Useful commands:

```bash
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chat-history-cli" stats
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chat-history-cli" doctor
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chat-history-cli" sync-codex
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chat-history-cli" search "query" --mode hybrid --limit 10
```

On a secondary Mac, use the installed `bin/sync-codex-remote` with the canonical writer URL and a protected `CHAT_HISTORY_WRITER_TOKEN`. Keep OpenClaw and ordinary clients on the separate read-only endpoint.

Use `scripts/chat-history-migrate` from the cloned repository for host-to-host migration bundles. Follow `docs/MIGRATION.md` and `docs/AUTOMATION.md` in the repository for cutover and scheduled collection.
