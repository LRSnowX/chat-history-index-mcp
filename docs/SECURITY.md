# Threat model and hardening

## Protected assets

- Conversation text and attachments
- Provider identifiers and source URLs
- Import archives and migration bundles
- MCP bearer tokens
- Provider cursor state

## Primary risks

- Public exposure of the MCP endpoint
- A read-only client receiving write tools
- Two writer hosts diverging after migration
- Copying live SQLite/WAL files through a sync service
- Secrets or transcripts entering Git, logs, shell history, or automation prompts
- Incomplete provider reads advancing a cursor

## Required controls

- Bind HTTP only to loopback or a private-network interface.
- Require a high-entropy bearer token stored in macOS Keychain.
- Keep the ordinary endpoint read-only; `--allow-writes` is explicit.
- Use separate tokens and ports for read and write roles.
- Restrict source and destination hosts with Tailscale ACLs.
- Use online backup or migration bundles instead of live database copying.
- Stop the old canonical writer before destination cutover.
- Run `doctor` after imports, restores, upgrades, and cutovers.
- Review logs and artifacts before sharing bug reports.

## Non-goals

The bundled HTTP server is not hardened for direct public-internet exposure and does not implement multi-user authorization. Public plugin-directory deployment requires a separate HTTPS and OAuth architecture.
