# Scheduled collection

Use a Codex automation on the canonical writer host. The automation must run serially and fail closed on incomplete provider reads.

## Recommended schedule

Run the ChatGPT/Gemini/Antigravity heartbeat on its chosen cadence. Codex sync is a separate deterministic LaunchAgent; ChatGPT.app reads are rate-limited, so keep batches small.

## Automation instructions

```text
Maintain the self-hosted AI Conversation Index in the installed repository. Codex rollout collection runs independently through the deterministic `local.chat-history-index-codex-sync` LaunchAgent; do not run `sync-codex` here. Read skills/ai-conversation-index/SKILL.md, docs/chatgpt-app-collector.md, docs/provider-support.md, and docs/architecture.md first. Run scripts/chat-history-cli sync-gemini and scripts/chat-history-cli sync-antigravity. Use supported ChatGPT.app list_threads and paged read_thread operations for incremental ChatGPT discovery. Adapt recent discovery to the documented bridge snapshot and feed it to `chatgpt-plan-recent`; follow every nextCursor until hasMore is false and feed only a complete transcript to `chatgpt-import-thread`. Reject inaccessible or truncated conversations. On Too many requests, stop immediately, use `chatgpt-block` when the thread is incomplete, and preserve pending IDs. If all 50 recent entries are newer than the durable cursor, report discovery overflow and do not advance it. Import newly arrived normalized JSON or JSONL provider inbox files without deleting or rebuilding existing records. Never inspect cookies, local storage, auth headers, or private endpoints. Never advance a provider cursor if any conversation is incomplete. Never place live SQLite/WAL files in a synced folder. Finish with scripts/chat-history-cli doctor. Stay quiet on a clean no-change run; otherwise report concise imported counts and failures.
```

Initial ChatGPT sidebar backfill is a separate bounded workflow. It must enumerate IDs through the signed-in UI, page every transcript to completion, preserve retry state, and stop on rate limits. Do not disguise that backfill as ordinary incremental collection.

## Secondary Macs

Schedule `scripts/sync-codex-remote --url http://CANONICAL_HOST:8766/mcp` on every secondary Mac that creates Codex tasks. Prefer the shared-skill installer `~/.codex/skills/chat-history-index-mcp/scripts/install-remote-codex-sync --writer-url http://CANONICAL_HOST:8766/mcp`, which creates the deterministic `local.chat-history-index-remote-codex-sync` LaunchAgent. Store the token in macOS Keychain service `chat-history-index-mcp-writer-remote`, or supply `CHAT_HISTORY_WRITER_TOKEN` through a protected environment. The remote collector has its own durable cursor and fails without advancing it when parsing or upload is incomplete.

ChatGPT.app collection remains on the canonical host's Codex automation. OpenClaw and interactive MCP clients remain on the separate read-only endpoint.
