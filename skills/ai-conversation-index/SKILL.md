---
name: ai-conversation-index
description: Search, inspect, import, back up, restore, or maintain a private self-hosted AI conversation index for ChatGPT, Codex, Gemini, Claude, Grok, and normalized provider collectors.
---

# AI Conversation Index

Use this skill for the self-hosted conversation index. The default managed data home is `~/Library/Application Support/chat-history-index-mcp`; `CHAT_HISTORY_DATA_HOME` overrides it.

## Search and inspect

Prefer the bundled MCP tools for search, conversation retrieval, related-conversation lookup, and index statistics. Use the repository CLI for operator work:

```bash
./scripts/chat-history-cli stats
./scripts/chat-history-cli doctor
./scripts/chat-history-cli search "query" --mode hybrid --limit 10
./scripts/chat-history-cli show CONVERSATION_ID
```

## Ingest

```bash
./scripts/chat-history-cli import --archive ~/Downloads/openai-export.zip --mode copy --run-api-jobs false
./scripts/chat-history-cli sync-codex
./scripts/chat-history-cli import-normalized --path conversations.jsonl
```

For app-assisted ChatGPT collection, follow `docs/chatgpt-app-collector.md`. Fetch every `read_thread` cursor until `hasMore` is false, reject truncated or inaccessible conversations, and import through `scripts/import-normalized-stdin BYTE_COUNT`. Stop immediately on rate limiting and preserve pending IDs.

## Backup and migration

Use `scripts/chat-history-migrate` and follow `docs/MIGRATION.md`. The restore command requires `--confirm-writer-stopped`, creates a rollback backup, and verifies integrity.

## Safety

- Preserve existing records and use provider source IDs for idempotent upserts.
- Keep the live SQLite database on exactly one writer host's local disk.
- Never place SQLite, WAL, exports, transcripts, migration bundles, or tokens in Git or a synced folder.
- Expose only the authenticated read-only HTTP endpoint to ordinary clients and OpenClaw.
- Stop the canonical writer before restore or host cutover.
- Never inspect browser cookies, local storage, auth headers, or private provider endpoints.
- Never advance provider cursors after incomplete work.
