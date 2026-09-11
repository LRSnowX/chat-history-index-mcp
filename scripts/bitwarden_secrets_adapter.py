#!/usr/bin/env python3
"""Load one pre-declared Bitwarden secret without exposing it to the shell.

This module deliberately has no generic secret lookup interface.  The operator
metadata names exactly one reader or writer secret and its expected identity.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import stat
import pwd
import tempfile
from uuid import UUID
from typing import Callable, Mapping, Sequence


TRUSTED_SERVER_URL = "https://api.bitwarden.com"
CLOUD_CONFIG = '''[profiles.default]
server_api = "https://api.bitwarden.com"
server_identity = "https://identity.bitwarden.com"
state_opt_out = "true"
'''
EXPECTED_ORGANIZATION_ID = "a17d6101-b339-48fa-a3c5-b4a90168e972"
PROFILES = {
    "reader": "growthops.chat-history.reader.machine-access",
    "writer": "growthops.chat-history.writer.machine-access",
}
EXPECTED_KEYS = {"reader": "CHAT_HISTORY_HTTP_TOKEN", "writer": "CHAT_HISTORY_WRITER_TOKEN"}
_AUTH_ENV_NAMES = {
    "BWS_ACCESS_TOKEN",
    "BW_SESSION",
    "BW_PASSWORD",
    "BW_CLIENTID",
    "BW_CLIENTSECRET",
    "BWS_SERVER_URL",
    "BWS_CONFIG_FILE",
}


class BitwardenSecretError(RuntimeError):
    """A sanitized, non-credential-bearing adapter failure."""


def _fail(message: str) -> BitwardenSecretError:
    return BitwardenSecretError(f"Bitwarden secret load failed: {message}")


def _clean_env(source: Mapping[str, str], bootstrap: str) -> dict[str, str]:
    # Do not allow inherited Bitwarden settings or endpoint overrides to affect
    # the exact request.  PATH is intentionally fixed to the known Homebrew
    # and system locations used by the supported macOS installation.
    env = {
        "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        "BWS_ACCESS_TOKEN": bootstrap,
    }
    for name, value in source.items():
        if name.startswith("BWS_") or name.startswith("BW_") or name in _AUTH_ENV_NAMES:
            continue
        if name in {"HOME", "USER", "LOGNAME", "TMPDIR"}:
            env[name] = value
    return env


def _read_json(path: Path) -> dict:
    try:
        info = path.lstat()
        current_uid = os.getuid()
    except OSError:
        raise _fail("metadata could not be read") from None
    if not stat.S_ISREG(info.st_mode) or info.st_uid != current_uid or info.st_mode & 0o077:
        raise _fail("metadata permissions are unsafe")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise _fail("metadata could not be read") from None
    if not isinstance(value, dict):
        raise _fail("metadata must be an object")
    return value


def _validate_metadata(metadata: Mapping[str, object], profile: str) -> dict[str, str]:
    if profile not in PROFILES:
        raise _fail("unsupported profile")
    if metadata.get("profile") != profile:
        raise _fail("metadata profile does not match requested profile")
    required = ("organization_id", "project_id", "secret_id", "expected_key")
    values: dict[str, str] = {}
    for field in required:
        value = metadata.get(field)
        if not isinstance(value, str) or not value or len(value) > 256:
            raise _fail(f"invalid metadata field: {field}")
        values[field] = value
    for field in ("project_id", "secret_id"):
        try:
            canonical = str(UUID(values[field]))
        except ValueError:
            raise _fail(f"invalid metadata field: {field}") from None
        if canonical != values[field]:
            raise _fail(f"invalid metadata field: {field}")
    if values["organization_id"] != EXPECTED_ORGANIZATION_ID:
        raise _fail("metadata organization is not the approved Personal organization")
    server_url = metadata.get("server_url")
    if server_url != TRUSTED_SERVER_URL:
        raise _fail("metadata server_url is not the approved Bitwarden cloud endpoint")
    if values["expected_key"] != EXPECTED_KEYS[profile]:
        raise _fail("metadata key does not match profile")
    values["server_url"] = TRUSTED_SERVER_URL
    return values


def load_token(
    profile: str,
    metadata_path: str | os.PathLike[str],
    *,
    environ: Mapping[str, str] | None = None,
    run: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    security_path: str = "/usr/bin/security",
    bws_path: str = "/opt/homebrew/bin/bws",
) -> str:
    """Return the exact configured token in process memory only."""
    source_env = dict(os.environ if environ is None else environ)
    metadata = _validate_metadata(_read_json(Path(metadata_path)), profile)
    keychain_service = PROFILES[profile]
    account = pwd.getpwuid(os.getuid()).pw_name
    if not account:
        raise _fail("machine account is unavailable")
    try:
        bootstrap_result = run(
            [security_path, "find-generic-password", "-a", account, "-s", keychain_service, "-w"],
            capture_output=True,
            text=True,
            env=_clean_env(source_env, ""),
            check=False,
            timeout=15,
        )
    except (OSError, subprocess.SubprocessError):
        raise _fail("Keychain lookup failed") from None
    if bootstrap_result.returncode != 0 or not bootstrap_result.stdout.strip():
        raise _fail("Keychain lookup failed")
    bootstrap = bootstrap_result.stdout.strip()
    try:
        # --server-url denotes a self-hosted base and appends /api and
        # /identity. Cloud endpoints require these separate explicit fields.
        # This private temporary file contains public configuration only.
        with tempfile.NamedTemporaryFile(mode="w", prefix="chat-history-bws-", dir="/tmp") as config:
            config.write(CLOUD_CONFIG)
            config.flush()
            result = run(
                [bws_path, "--config-file", config.name, "secret", "get", metadata["secret_id"], "--output", "json"],
                capture_output=True,
                text=True,
                env=_clean_env(source_env, bootstrap),
                check=False,
                timeout=30,
            )
    except (OSError, subprocess.SubprocessError):
        raise _fail("Bitwarden lookup failed") from None
    if result.returncode != 0:
        raise _fail("Bitwarden lookup failed")
    try:
        payload = json.loads(result.stdout)
    except (UnicodeError, json.JSONDecodeError):
        raise _fail("Bitwarden returned invalid data") from None
    if not isinstance(payload, dict):
        raise _fail("Bitwarden returned invalid data")
    if (
        payload.get("organizationId") != metadata["organization_id"]
        or payload.get("projectId") != metadata["project_id"]
        or payload.get("id") != metadata["secret_id"]
        or payload.get("key") != metadata["expected_key"]
    ):
        raise _fail("Bitwarden secret identity did not match metadata")
    token = payload.get("value")
    if not isinstance(token, str) or not token:
        raise _fail("Bitwarden secret has no value")
    return token


def main(argv: Sequence[str] | None = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description="Load one configured chat-history Bitwarden secret")
    parser.add_argument("profile", choices=tuple(PROFILES))
    parser.add_argument("metadata", type=Path)
    args = parser.parse_args(argv)
    # This CLI intentionally never prints the token. It performs the exact
    # lookup so an operator can validate access without a shell transport.
    # Service launchers call load_token() directly in process.
    load_token(args.profile, args.metadata)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BitwardenSecretError as exc:
        print(str(exc), file=os.sys.stderr)
        raise SystemExit(1)
