# Operations

## Diagnostics

```bash
./scripts/chat-history-cli doctor
./scripts/chat-history-cli stats
./scripts/chat-history-service status
tail -n 100 "$HOME/Library/Application Support/chat-history-index-mcp/logs/http-service.error.log"
```

`doctor` reports SQLite integrity, journal mode, totals, and per-source counts and newest timestamps.

## Read-only Tailscale MCP

```bash
./scripts/chat-history-service install \
  --bind TAILSCALE_IP:8765 \
  --allowed-host TAILSCALE_IP,TAILSCALE_DNS_NAME
```

The LaunchAgent label is `local.chat-history-index-mcp`. Its plist is `~/Library/LaunchAgents/local.chat-history-index-mcp.plist`. Logs are in the managed data home.

Stop it with `./scripts/chat-history-service stop`; remove it with `./scripts/chat-history-service uninstall`. Uninstall preserves data and the Keychain token.

## Codex client

```bash
codex mcp add ai-conversation-index \
  --url http://TAILSCALE_DNS_NAME:8765/mcp \
  --bearer-token-env-var CHAT_HISTORY_HTTP_TOKEN
```

Do not enable the plugin's local stdio writer on a read-only client unless it has its own deliberately separate index.

## OpenClaw

```bash
openclaw mcp set ai-history '{"url":"http://TAILSCALE_DNS_NAME:8765/mcp","transport":"streamable-http","headers":{"Authorization":"Bearer ${CHAT_HISTORY_HTTP_TOKEN}"}}'
openclaw mcp doctor ai-history --probe
```

The read-only server exposes search, get-conversation, related-conversations, and index-stats tools. It omits import and rebuild tools.

## Trusted remote Codex collectors

Enable a separate writer endpoint on the canonical host. It has its own LaunchAgent, Keychain token, and default port:

```bash
./scripts/chat-history-service --writer install \
  --bind TAILSCALE_IP:8766 \
  --allowed-host TAILSCALE_IP,TAILSCALE_DNS_NAME
```

The writer label is `local.chat-history-index-mcp-writer`. Inspect it with `./scripts/chat-history-service --writer status` and retrieve its token with `./scripts/chat-history-service --writer token` only while configuring a trusted collector. This endpoint exposes only `import_conversations`; it cannot search, export, rebuild, or inspect the index.

On each secondary Mac, run:

```bash
./scripts/sync-codex-remote --url http://TAILSCALE_DNS_NAME:8766/mcp
```

The collector reads `CHAT_HISTORY_WRITER_TOKEN` when provided, otherwise it reads macOS Keychain service `chat-history-index-mcp-writer-remote` for the current account. It parses local Codex rollouts in memory, sends normalized batches, and advances its per-destination cursor only after every batch succeeds. Do not give this token to OpenClaw or ordinary MCP clients.

## Backups

```bash
./scripts/chat-history-cli backup --output /secure/path/ai-history.sqlite3
```

Backups contain conversation content. Encrypt and retain them according to your own data policy.
