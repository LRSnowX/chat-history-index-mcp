# ChatGPT app collector

ChatGPT.app is the primary live ChatGPT collector. The Codex app bridge exposes two useful operations:

- `list_threads(limit: 50)` discovers the most recently updated ChatGPT and Codex conversations.
- `read_thread(threadId, cursor, turnLimit: 10)` returns a conversation's turns newest-first and a cursor for older turns.

The public OpenAI Conversations API is not this data source. It stores conversations created through API requests; it does not enumerate the ChatGPT sidebar.

## Automatic macOS live collector

The installed macOS collector consumes those first-party App Tools without invoking a model:

```bash
./scripts/install-self-hosted --skip-plugin
"$HOME/Library/Application Support/chat-history-index-mcp/bin/chatgpt-live-collector-service" install
```

The default cadence is 120 seconds. Change it at install time with
`--interval SECONDS` (minimum 30 seconds). No ChatGPT collector LaunchAgent is
created.

The collector intentionally uses ChatGPT.app's bundled, OpenAI-signed runtime chain:

```text
ChatGPT.app
  -> official codex app-server
    -> bundled signed Node (short-lived MCP bootstrap)
      -> bundled signed Node (detached collector daemon)
        -> first-party App Tools native pipe
          -> list_threads / read_thread
```

On production macOS builds the App Tools Unix socket performs native peer/parent/grandparent code
signature authorization. Starting a collector independently from Terminal or a LaunchAgent is
rejected by that security boundary even when it uses the bundled Node, because the parent process
chain is not the ChatGPT-hosted chain. The collector does not bypass or disable the check. Instead,
it is configured as an ordinary MCP server of the official ChatGPT app-server. Its short-lived
bootstrap waits until the detached daemon has opened the authorized pipe before completing MCP
initialization. The daemon keeps that one accepted socket open across polling cycles, even after
the bootstrap process is reclaimed by the app-server.

The MCP server configuration uses `env_vars = ["CODEX_APP_TOOLS_PIPE_PATH"]` so the app-server
explicitly passes its current first-party pipe to the bootstrap. If the whole ChatGPT app restarts
and receives a new pipe, the bootstrap compares a non-secret pipe identity with the prior daemon
and replaces a stale daemon before syncing.

No collector step reads ChatGPT cookies, browser local storage, authentication headers, Keychain
session material, or private HTTP API responses. It only consumes the first-party App Tools MCP
surface already provided by the desktop app.

Each polling cycle:

1. reuses the already-authorized App Tools socket opened during bootstrap;
2. chooses an existing local Codex thread as the App Tools interaction context;
3. calls `list_threads(limit: 50)`;
4. sends normal and pinned entries through the durable `chatgpt-plan-recent` state machine;
5. leaves an `active` ChatGPT thread pending so an in-progress conversation is not snapshotted;
6. for each selected `idle` ChatGPT thread, follows every `read_thread` cursor until `hasMore=false`;
7. imports the complete transcript and builds only its local multilingual embedding;
8. advances the safe cursor only when no pending/blocked item remains.

The App Tools per-message output ceiling is 20,000 characters. Since the current bridge does not
publish a separate truncation flag for ordinary user/assistant items, the collector fails closed
when a returned message reaches the boundary instead of silently indexing an ambiguous partial
message. App Tools attachment metadata is preserved in the compressed raw conversation record;
the live collector does not invent binary attachment rows without stable provider file payloads.

Useful service commands:

```bash
SERVICE="$HOME/Library/Application Support/chat-history-index-mcp/bin/chatgpt-live-collector-service"
"$SERVICE" status
"$SERVICE" restart
"$SERVICE" stop
"$SERVICE" uninstall
```

`install` updates only the dedicated `mcp_servers.chatgpt_live_collector` blocks in
`~/.codex/config.toml`, preserving a timestamped configuration backup before a change. If
ChatGPT.app is already running, the service restarts only its `codex app-server` child so the main
desktop app and open conversations remain in place. `uninstall` removes that config block and
stops the daemon but preserves indexed history and sync state.

Status lives at `cache/chatgpt-live-collector-status.json`. Logs are written under the managed
data home as `logs/chatgpt-live-collector.log` and `logs/chatgpt-live-collector.error.log`. A
clean no-change cycle is intentionally quiet.

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
  ],
  "pinned_threads": []
}
```

`threads` is the normal limited recent list and is the only list used to detect recent-50
discovery overflow. `pinned_threads` is additional: pinned items participate in synchronization
but an old pinned conversation cannot hide an overflow in the limited recent list.

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

Run transcript reads serially or with very low concurrency. The automatic collector is strictly
serial. `read_thread` is rate-limited; on `Too many requests`, stop the batch, preserve the pending
IDs, and retry on a later run. Do not continue issuing requests into the throttle.

The stdin form avoids leaving plaintext transcript staging files on disk.

## Initial backfill

The 50-item recent list is not enough for an arbitrary historical backfill. Use a complete OpenAI
export for the one-time bootstrap, then let the live collector maintain the incremental tail. The
collector never inspects cookies, local storage, auth headers, or private endpoints.

Completeness requires all of the following:

- every discovered ID reaches `hasMore: false`;
- no returned item is truncated;
- every normalized conversation imports successfully;
- the oldest imported creation timestamp is at or before the requested cutoff;
- the newest imported update timestamp reaches the current run.

Keep a retry list for inaccessible IDs. Never move the successful cursor past a failed or incomplete conversation.

Skip blocked IDs during normal backfill batches so one oversized conversation cannot starve later
IDs. Revisit blocked IDs only when a collector path can return the full untruncated item.
