# Security policy

## Supported versions

Security fixes are applied to the latest tagged release and the `main` branch.

## Reporting a vulnerability

Do not open a public issue containing conversation data, credentials, private hostnames, or exploit details. Report vulnerabilities privately through GitHub's private vulnerability reporting for this repository.

## Deployment boundary

- Keep the live database on one writer host's local disk.
- Use Tailscale or another private network plus bearer authentication.
- Give ordinary clients and OpenClaw the read-only endpoint.
- Use a different token and port for any explicitly enabled writer endpoint.
- Store tokens in macOS Keychain or another OS secret manager.
- Treat migration bundles and provider exports as sensitive data.
- Do not expose the bundled HTTP server directly to the public internet.

See `docs/SECURITY.md` for the threat model and operator checklist.
