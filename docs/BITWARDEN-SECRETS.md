# Bitwarden Secrets Manager adapter (source-only)

The repository contains an opt-in adapter for the existing chat-history HTTP
reader and remote Codex writer services. It is **not deployed**, does not
migrate any Keychain item, and does not create or modify Bitwarden projects or
secrets.

The operator metadata file is JSON and must declare exactly one profile and its
expected identities:

```json
{
  "profile": "reader",
  "server_url": "https://api.bitwarden.com",
  "organization_id": "a17d6101-b339-48fa-a3c5-b4a90168e972",
  "project_id": "<reader project id>",
  "secret_id": "<reader secret id>",
  "expected_key": "CHAT_HISTORY_HTTP_TOKEN"
}
```

Use `writer` and `CHAT_HISTORY_WRITER_TOKEN` for the remote writer metadata.
The machine bootstrap is read from the fixed Keychain service
`growthops.chat-history.reader.machine-access` or
`growthops.chat-history.writer.machine-access`, respectively. These names are
dedicated Bitwarden machine-access bootstrap items and must not reuse the live
MCP bearer Keychain services.
The adapter calls `bws secret get` for the one declared secret, validates the
organization, project, secret id, and key, and keeps the returned value in the
service process only. Inherited `BWS_*`, `BW_*`, and endpoint settings are
discarded. A private temporary file containing only public configuration sets the exact cloud API and identity endpoints and disables persistent BWS state.

The local `bws 2.1.0` supports `server-api`, `server-identity`, and
`state-opt-out` configuration. The adapter explicitly uses
`https://api.bitwarden.com` and `https://identity.bitwarden.com`, with
`state_opt_out = "true"` (the string form required by installed BWS 2.1.0), and never reads an ambient CLI configuration.
`--server-url` is intentionally avoided: [official BWS source](https://github.com/bitwarden/sdk-sm/blob/main/crates/bws/src/config.rs)
shows that it denotes a base URL to which `/api` and `/identity` are appended.
The metadata `server_url` identifies the approved API origin; it is not passed
as a BWS base URL. A binary string match alone does not verify URL behavior.

Opt in by setting `CHAT_HISTORY_BITWARDEN_CONFIG` to the metadata path. The HTTP
wrapper uses the fixed Python launcher so shell command substitution never
prints the token. The sync script loads the writer secret in process. With that
variable unset, existing explicit-token and Keychain behavior is unchanged.

Before any migration or deployment, provision and record separate reader and
writer project/secret ids under Personal organization
`a17d6101-b339-48fa-a3c5-b4a90168e972`, install the corresponding machine
bootstrap access in the fixed Keychain services, and exercise each service with
fake or approved live requests. Confirm the exact account, endpoint, and
permissions, then separately schedule deployment and Keychain retirement.
