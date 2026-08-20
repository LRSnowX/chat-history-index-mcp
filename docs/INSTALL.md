# Installation

## Requirements

- macOS 13 or newer
- Git
- Rust and Cargo
- Codex CLI for plugin installation
- Tailscale for private multi-machine access

## Install from GitHub

```bash
git clone https://github.com/davidjbeveridge/chat-history-index-mcp.git
cd chat-history-index-mcp
./scripts/install-self-hosted
```

The installer builds release binaries, copies them into the managed data home, registers this repository as the `universal-ai-history` marketplace, and installs `chat-history-index-mcp`.

Use `--skip-plugin` to install only the CLI and MCP binaries. Use `--data-home PATH` to override the default data home.

Start a new Codex task after installing or upgrading a plugin.

## Upgrade

```bash
cd ~/src/chat-history-index-mcp
git pull --ff-only
./scripts/install-self-hosted
codex plugin marketplace upgrade universal-ai-history
codex plugin add chat-history-index-mcp@universal-ai-history
```

The installer does not delete or rebuild the data home.

## Verify

```bash
cargo test --workspace
./scripts/chat-history-cli doctor
codex plugin list
```

## Uninstall

```bash
./scripts/chat-history-service uninstall
codex plugin remove chat-history-index-mcp@universal-ai-history
```

Uninstalling preserves the data home and Keychain token. Delete those separately only when permanent data removal is intended.
