#!/usr/bin/env python3
"""Fixed launcher for the opt-in Bitwarden-backed HTTP service."""

from __future__ import annotations

import os
from pathlib import Path
import sys

from bitwarden_secrets_adapter import BitwardenSecretError, load_token


def main() -> int:
    config = os.environ.get("CHAT_HISTORY_BITWARDEN_CONFIG")
    if not config:
        print("Bitwarden config is not set", file=sys.stderr)
        return 2
    if os.environ.get("CHAT_HISTORY_ALLOW_WRITES", "false") != "false":
        print("Bitwarden mode supports only reader or collector-only services", file=sys.stderr)
        return 2
    if os.environ.get("CHAT_HISTORY_COLLECTOR_ONLY", "false") not in ("true", "false"):
        print("Invalid collector mode", file=sys.stderr)
        return 2
    profile = "writer" if os.environ.get("CHAT_HISTORY_COLLECTOR_ONLY") == "true" else "reader"
    try:
        token = load_token(profile, config)
    except BitwardenSecretError as exc:
        print(str(exc), file=sys.stderr)
        return 1
    child_env = {key: value for key, value in os.environ.items()
                 if not key.startswith(("BW_", "BWS_"))}
    child_env["CHAT_HISTORY_HTTP_TOKEN"] = token
    child_env["CHAT_HISTORY_BITWARDEN_LOADED"] = "true"
    script = Path(__file__).with_name("chat-history-http-service")
    os.execve(str(script), [str(script)], child_env)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
