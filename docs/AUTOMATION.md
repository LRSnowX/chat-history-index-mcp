# Scheduled collection

Use a Codex automation on the canonical writer host. The automation must run serially and fail closed on incomplete provider reads.

## Recommended schedule

Run every four hours. Codex sync is inexpensive and idempotent. ChatGPT.app reads are rate-limited, so keep batches small.

## Automation instructions

```text
Maintain the self-hosted AI Conversation Index in the installed repository. Read skills/ai-conversation-index/SKILL.md, docs/chatgpt-app-collector.md, docs/provider-support.md, and docs/architecture.md first. Run scripts/chat-history-cli sync-codex. Use supported ChatGPT.app list_threads and paged read_thread operations for incremental ChatGPT discovery and import complete normalized transcripts through scripts/import-normalized-stdin BYTE_COUNT. Follow every nextCursor until hasMore is false. Reject inaccessible or truncated conversations. On Too many requests, stop immediately and preserve pending IDs. If all 50 recent entries are newer than the durable cursor, report discovery overflow and do not advance it. Import newly arrived normalized JSON or JSONL provider inbox files without deleting or rebuilding existing records. Never inspect cookies, local storage, auth headers, or private endpoints. Never advance a provider cursor if any conversation is incomplete. Never place live SQLite/WAL files in a synced folder. Finish with scripts/chat-history-cli doctor. Stay quiet on a clean no-change run; otherwise report concise imported counts and failures.
```

Initial ChatGPT sidebar backfill is a separate bounded workflow. It must enumerate IDs through the signed-in UI, page every transcript to completion, preserve retry state, and stop on rate limits. Do not disguise that backfill as ordinary incremental collection.
