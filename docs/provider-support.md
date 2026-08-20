# Provider support

| Provider/input | Backfill | Incremental | Status |
| --- | --- | --- | --- |
| ChatGPT OpenAI export ZIP | Native | Reimport newer export | Complete |
| Local Codex rollout JSONL | Native | Durable cursor with overlap | Complete |
| Normalized JSON/JSONL | Native | Collector-owned cursor | Complete |
| ChatGPT.app bridge | Sidebar-ID bootstrap + cursor-paged transcripts | Recent-50 discovery + durable cursor | Native live collector; fails closed on discovery overflow or truncation |
| Gemini Takeout / UI | Fixture needed | Bounded UI collector | Not complete |
| Claude export | Fixture needed | Normalized collector | Not complete |
| Grok export | Fixture needed | Normalized collector | Not complete |

Provider-specific native parsers must be tested against a real, redacted export fixture. Until then, collectors should emit `docs/normalized-format.md`; do not label format guesses as native support.
