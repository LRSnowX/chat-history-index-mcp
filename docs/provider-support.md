# Provider support

| Provider/input | Backfill | Incremental | Status |
| --- | --- | --- | --- |
| ChatGPT OpenAI export ZIP | Native | Reimport newer export | Complete |
| Local Codex rollout JSONL | Native | Durable cursor with overlap | Complete |
| Normalized JSON/JSONL | Native | Collector-owned cursor | Complete |
| ChatGPT.app bridge | Sidebar-ID bootstrap + cursor-paged transcripts | Recent-50 discovery + durable cursor | Native live collector; fails closed on discovery overflow or truncation |
| Gemini CLI sessions | Native local JSON/JSONL collector | `sync-gemini` durable cursor with overlap | Complete for documented Gemini CLI session files under `~/.gemini/tmp/*/chats/` |
| Gemini Apps Takeout | Normalized adapter required | New Takeout archive required | Google provides an official export; a Takeout-specific parser awaits a redacted fixture, so the importer does not guess its archive layout |
| Antigravity CLI / 2.0 transcripts | Native plaintext JSON/JSONL collector | `sync-antigravity` durable cursor with overlap | Complete for documented `brain/<conversation>/.system_generated/logs/transcript*.jsonl` files |
| Antigravity internal `.db` / `.pb` stores | Not parsed | Not parsed | Intentionally unsupported: those formats have no stable public record schema |
| Claude export | Fixture needed | Normalized collector | Not complete |
| Grok export | Fixture needed | Normalized collector | Not complete |

Run `scripts/chat-history-cli sync-gemini` and `scripts/chat-history-cli sync-antigravity` on the canonical writer. Both commands are read-only against provider files, import with stable source IDs, and advance their own cursor only after a clean run. `sync-antigravity` prefers `transcript_full.jsonl` when it exists beside `transcript.jsonl`.

Provider-specific parsers beyond the documented local session/transcript formats must be tested against a real, redacted export fixture. Until then, collectors should emit `docs/normalized-format.md`; do not label format guesses as native support.
