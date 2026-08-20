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

## Backups

```bash
./scripts/chat-history-cli backup --output /secure/path/ai-history.sqlite3
```

Backups contain conversation content. Encrypt and retain them according to your own data policy.
