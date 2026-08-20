# Move the canonical index to another Mac

This procedure preserves the existing index and avoids two active writers.

## 1. Install on the destination

Clone the repository and run `./scripts/install-self-hosted`. Do not start its network service or scheduled collectors yet.

## 2. Export on the source

An online SQLite backup includes committed WAL data, so the read-only service can remain online during export:

```bash
./scripts/chat-history-migrate export \
  --output ~/Desktop/ai-history-migration.tar.gz
```

Add `--include-source-archive` only when the canonical ChatGPT export ZIP is also needed. The bundle includes portable provider cursor state and excludes `codex-sync-cursor.json` because that cursor describes the source Mac's local Codex sessions.

Treat the bundle as sensitive conversation data. It is created with mode `0600`.

## 3. Transfer over Tailscale

```bash
scp ~/Desktop/ai-history-migration.tar.gz DESTINATION_HOST:~/Downloads/
```

Compare the SHA-256 value printed during export with `shasum -a 256` on the destination.

## 4. Stage the restore

Stop any destination writer service and make sure no destination collector is running:

```bash
./scripts/chat-history-service stop 2>/dev/null || true
./scripts/chat-history-migrate import \
  --bundle ~/Downloads/ai-history-migration.tar.gz
```

Restore verifies every bundle checksum, verifies SQLite integrity, creates a pre-restore rollback backup when a destination database exists, restores the database, copies portable cursor state, and runs `doctor`.

## 5. Compare source and destination

Run `./scripts/chat-history-cli doctor` on both machines. Compare:

- `integrity_check` equals `ok`
- total conversations and messages
- per-source conversation/message counts
- newest update timestamp per source
- representative searches for ChatGPT and Codex conversations

## 6. Cut over

Only after comparison succeeds:

1. Stop and disable the source writer and its collection automation.
2. Install the destination read-only HTTP service.
3. Start destination collection automation.
4. Update Codex and OpenClaw clients to the destination MCP URL and token.
5. Run one incremental sync and `doctor`.

Keep the source database and migration bundle until the destination has completed at least one clean scheduled run.

## Rollback

Stop the destination writer, restart the source service and automation, and point clients back to the source URL. A restore also stores the prior destination database under `db/backups/`.
