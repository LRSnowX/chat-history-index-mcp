# Changelog

## 0.4.0 - 2026-08-20

- Added a separate authenticated writer service role with its own LaunchAgent, port, and Keychain token.
- Added cursor-safe remote Codex collection so secondary Macs can feed one canonical index without copying SQLite files.
- Added a read-only Codex export command for collector pipelines.

## 0.3.0 - 2026-08-20

- Package the project as a Git-backed Codex marketplace plugin.
- Add portable macOS installation and LaunchAgent service management.
- Add online SQLite backup, guarded restore, integrity diagnostics, and host migration bundles.
- Remove machine-specific runtime defaults.
- Add self-hosting, migration, operations, automation, privacy, and security documentation.
