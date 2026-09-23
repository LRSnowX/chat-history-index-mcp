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
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chat-history-cli" chatgpt-state
```

## ChatGPT.app collection

On the primary macOS host, normal incremental ChatGPT collection is handled by the deterministic
`local.chat-history-index-chatgpt-sync` LaunchAgent. It uses ChatGPT.app's bundled signed runtime
and the first-party `codex-app-tools` MCP (`list_threads` / `read_thread`) without invoking a
model. Do not duplicate this sync in an interactive maintenance run.

Check it with:

```bash
SERVICE="$HOME/Library/Application Support/chat-history-index-mcp/bin/chatgpt-live-collector-service"
"$SERVICE" status
```

The underlying collector contract remains:

1. Call `list_threads(limit: 50)` and adapt the result to the documented discovery snapshot.
2. Call `chatgpt_plan_recent` before reading transcripts. If it reports discovery overflow, do
   not advance the cursor or pretend the recent-50 list is complete.
3. For every selected ChatGPT thread, call `read_thread` repeatedly through every `nextCursor`
   until `hasMore` is false.
4. Adapt the full paged result to the documented transcript contract and call
   `chatgpt_import_thread`. The index validates the cursor chain, rejects truncated/inaccessible
   content, restores chronological order, imports idempotently, and builds the local embedding.
5. If a thread cannot be read completely, call `chatgpt_block` with the real failure reason and
   stop issuing requests when the bridge rate-limits.

`chatgpt_state` is read-only. `chatgpt_plan_recent`, `chatgpt_import_thread`, `chatgpt_block`, and
cursor-seeding tools are writer/collector operations and must never be proxied through a normal
read-only memory endpoint.

For a first historical bootstrap when the app bridge cannot enumerate the entire sidebar, use one
complete OpenAI export once:

```bash
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chat-history-cli" \
  chatgpt-bootstrap-export --archive /path/to/openai-export.zip
```

That command backs up SQLite first, copies the export into managed storage, skips Codex summary
generation, builds local multilingual embeddings, and seeds the durable live collector cursor.
Repeated full-account exports are not the intended steady-state sync path.

On a secondary Mac, use the installed `bin/sync-codex-remote` with the canonical writer URL and a protected `CHAT_HISTORY_WRITER_TOKEN`. Keep OpenClaw and ordinary clients on the separate read-only endpoint.

Use `scripts/chat-history-migrate` from the cloned repository for host-to-host migration bundles. Follow `docs/MIGRATION.md` and `docs/AUTOMATION.md` in the repository for cutover and scheduled collection.
