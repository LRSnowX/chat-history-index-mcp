# ChatGPT app collector

ChatGPT.app is the primary live ChatGPT collector. The Codex app bridge exposes two useful operations:

- `list_threads(limit: 50)` discovers the most recently updated ChatGPT and Codex conversations.
- `read_thread(threadId, cursor, turnLimit: 10)` returns a conversation's turns newest-first and a cursor for older turns.

The public OpenAI Conversations API is not this data source. It stores conversations created through API requests; it does not enumerate the ChatGPT sidebar.

## Incremental sync

1. Call `list_threads` with `limit: 50` and keep entries whose `kind` is `chatgpt`.
2. Stop at the first entry older than the last successful ChatGPT cursor. If all 50 entries are newer, report discovery overflow and do not advance the cursor.
3. For every selected ID, call `read_thread` repeatedly with each returned `nextCursor` until `hasMore` is false.
4. Reject the entire conversation if any message is truncated or inaccessible.
5. Reverse the returned turns into chronological order and emit the normalized format with `source: "chatgpt"` and the app thread ID as `source_conversation_id`.
6. Import through `scripts/import-normalized-stdin BYTE_COUNT` and advance the cursor only after a clean import.

Run transcript reads serially or with very low concurrency. `read_thread` is rate-limited; on `Too many requests`, stop the batch, preserve the pending IDs, and retry on a later run. Do not continue issuing requests into the throttle.

The stdin form avoids leaving plaintext transcript staging files on disk.

## Initial backfill

The 50-item recent list is not enough for a complete backfill. Discover all sidebar conversation IDs through the signed-in ChatGPT UI, then fetch every transcript through `read_thread`. The UI step collects IDs and titles only; do not inspect cookies, local storage, auth headers, or private endpoints.

Completeness requires all of the following:

- every discovered ID reaches `hasMore: false`;
- no returned item is truncated;
- every normalized conversation imports successfully;
- the oldest imported creation timestamp is at or before the requested cutoff;
- the newest imported update timestamp reaches the current run.

Keep a retry list for inaccessible IDs. Never move the successful cursor past a failed or incomplete conversation.

The durable retry state is `cache/chatgpt-sync-state.json` under the managed data home. Skip its blocked IDs during normal backfill batches so one oversized conversation cannot starve later IDs. Revisit blocked IDs only when a collector path can return the full untruncated item.
