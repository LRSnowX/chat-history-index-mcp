# ChatGPT app collector

ChatGPT.app is the primary live ChatGPT collector. The Codex app bridge exposes two useful operations:

- `list_threads(limit: 50)` discovers the most recently updated ChatGPT and Codex conversations.
- `read_thread(threadId, cursor, turnLimit: 10)` returns a conversation's turns newest-first and a cursor for older turns.

The public OpenAI Conversations API is not this data source. It stores conversations created through API requests; it does not enumerate the ChatGPT sidebar.

## Local collector state machine

The index owns the durable sync state and completeness checks. The ChatGPT.app bridge only reads
provider data and maps it into the small JSON contracts below. This keeps provider-specific app
details out of the SQLite importer.

Inspect current state:

```bash
scripts/chat-history-cli chatgpt-state
```

The durable state lives at `cache/chatgpt-sync-state.json` under the managed data home. It tracks
the last safe update-time cursor, the current high-water mark, pending threads, blocked threads,
and threads already imported above a blocked cursor.

### Discovery input

Adapt the result of `list_threads(limit: 50)` to:

```json
{
  "requested_limit": 50,
  "threads": [
    {
      "thread_id": "provider-thread-id",
      "kind": "chatgpt",
      "title": "Conversation title",
      "create_time": 1787184000.0,
      "update_time": 1787187600.0
    }
  ]
}
```

Feed it over stdin so no plaintext staging file is required:

```bash
BYTES=$(wc -c < discovery.json | tr -d ' ')
scripts/chat-history-cli chatgpt-plan-recent --path - --stdin-bytes "$BYTES" < discovery.json
```

If all 50 listed items are newer than the last successful cursor, the planner sets
`discovery_overflow: true` and refuses to advance the cursor. This is deliberate: recent-50 is
not proof of complete discovery.

### Transcript input

Follow every `read_thread` cursor first, then adapt the complete result to one document:

```json
{
  "thread_id": "provider-thread-id",
  "title": "Conversation title",
  "create_time": 1787184000.0,
  "update_time": 1787187600.0,
  "model": null,
  "source_url": null,
  "pages": [
    {
      "request_cursor": null,
      "next_cursor": "older-page",
      "has_more": true,
      "messages": [
        {
          "message_id": "newer-message",
          "role": "assistant",
          "create_time": 1787187600.0,
          "text": "Newest message returned by the bridge",
          "truncated": false,
          "inaccessible": false,
          "raw": {}
        }
      ]
    },
    {
      "request_cursor": "older-page",
      "next_cursor": null,
      "has_more": false,
      "messages": []
    }
  ]
}
```

Import only after the final page has `has_more: false`:

```bash
BYTES=$(wc -c < transcript.json | tr -d ' ')
scripts/chat-history-cli chatgpt-import-thread --path - --stdin-bytes "$BYTES" < transcript.json
```

The importer rejects cursor-chain gaps, a non-terminal final page, duplicate/empty message IDs,
and any message marked truncated or inaccessible. Messages are reversed from the bridge's
newest-first order into chronological order. After a clean database import it builds the local
multilingual embedding for that one conversation; it does not invoke Codex summary generation.

If a thread cannot be completed, record it without moving the safe cursor:

```bash
scripts/chat-history-cli chatgpt-block THREAD_ID --reason "read_thread returned truncated content"
```

Successfully imported conversations above a blocked cursor are remembered so they are not
requeued every run. Importing a previously blocked thread clears its block; the cursor advances
only when the whole discovered window is clean.

### One-time historical bootstrap

For an account with substantial archived ChatGPT history, a complete OpenAI export is still the
safest one-time bootstrap when the app bridge cannot enumerate the entire sidebar. The dedicated
bootstrap command creates a consistent SQLite backup first, copies (rather than moves) the export
into managed storage, skips Codex summary generation, builds local multilingual embeddings, and
seeds future live incremental collection from the newest indexed ChatGPT item:

```bash
scripts/chat-history-cli chatgpt-bootstrap-export --archive /path/to/openai-export.zip
```

After this bootstrap, future bridge collection only needs to ingest conversations newer than or
updated after the durable cursor. Re-exporting the full OpenAI archive is not part of the normal
incremental path.

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

Skip blocked IDs during normal backfill batches so one oversized conversation cannot starve later
IDs. Revisit blocked IDs only when a collector path can return the full untruncated item.
