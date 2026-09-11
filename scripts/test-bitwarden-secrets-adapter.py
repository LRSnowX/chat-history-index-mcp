#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import os
import sys
from unittest import mock
import unittest


ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("bitwarden_secrets_adapter", ROOT / "bitwarden_secrets_adapter.py")
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)


class FakeRun:
    def __init__(self, secret):
        self.calls = []
        self.secret = secret

    def __call__(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        if argv[0].endswith("security"):
            return subprocess.CompletedProcess(argv, 0, "bootstrap", "")
        self.public_config = Path(argv[2]).read_text()
        return subprocess.CompletedProcess(argv, 0, json.dumps(self.secret), "")


class AdapterTests(unittest.TestCase):
    def config(self, profile="reader"):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "metadata.json"
        path.write_text(json.dumps({
            "profile": profile,
            "server_url": "https://api.bitwarden.com",
            "organization_id": MODULE.EXPECTED_ORGANIZATION_ID,
            "project_id": profile + "-project",
            "secret_id": profile + "-secret",
            "expected_key": "CHAT_HISTORY_" + ("HTTP_TOKEN" if profile == "reader" else "WRITER_TOKEN"),
        }))
        path.chmod(0o600)
        return path

    def secret(self, profile="reader", **changes):
        value = {
            "organizationId": MODULE.EXPECTED_ORGANIZATION_ID,
            "projectId": profile + "-project",
            "id": profile + "-secret",
            "key": "CHAT_HISTORY_" + ("HTTP_TOKEN" if profile == "reader" else "WRITER_TOKEN"),
            "value": "super-secret-token",
        }
        value.update(changes)
        return value

    def test_exact_lookup_strips_hostile_environment_and_keeps_profiles_separate(self):
        runner = FakeRun(self.secret("writer"))
        token = MODULE.load_token("writer", self.config("writer"), environ={
            "USER": "david", "BWS_ACCESS_TOKEN": "hostile", "BWS_SERVER_URL": "https://evil.invalid",
            "BW_SESSION": "hostile", "PATH": "/evil",
        }, run=runner)
        self.assertEqual(token, "super-secret-token")
        self.assertEqual(runner.calls[0][0][5], "growthops.chat-history.writer.machine-access")
        bws_argv, bws_kwargs = runner.calls[1]
        self.assertEqual(bws_argv[4:], ["get", "writer-secret", "--output", "json"])
        self.assertNotIn("hostile", bws_kwargs["env"].values())
        self.assertEqual(bws_argv[0], "/opt/homebrew/bin/bws")
        self.assertEqual(bws_argv[1], "--config-file")
        self.assertFalse(Path(bws_argv[2]).exists())
        self.assertEqual(runner.public_config, MODULE.CLOUD_CONFIG)
        self.assertNotIn("bootstrap", runner.public_config)

    def test_identity_or_key_mismatch_fails_without_returning_secret(self):
        for changes in ({"organizationId": "wrong"}, {"projectId": "wrong"}, {"id": "wrong"}, {"key": "wrong"}):
            with self.subTest(changes=changes):
                with self.assertRaises(MODULE.BitwardenSecretError):
                    MODULE.load_token("reader", self.config(), environ={"USER": "david"}, run=FakeRun(self.secret(**changes)))

    def test_keychain_failure_does_not_attempt_bws(self):
        calls = []

        def fail(argv, **kwargs):
            calls.append(argv)
            return subprocess.CompletedProcess(argv, 1, "", "secret bootstrap")

        with self.assertRaises(MODULE.BitwardenSecretError):
            MODULE.load_token("reader", self.config(), environ={"USER": "david"}, run=fail)
        self.assertEqual(len(calls), 1)

    def test_unsafe_endpoint_is_rejected_before_keychain(self):
        path = self.config()
        data = json.loads(path.read_text())
        data["server_url"] = "https://evil.invalid"
        path.write_text(json.dumps(data))
        calls = []
        with self.assertRaises(MODULE.BitwardenSecretError):
            MODULE.load_token("reader", path, environ={"USER": "david"}, run=lambda *a, **k: calls.append(a))
        self.assertEqual(calls, [])

    def test_metadata_must_be_private_regular_file(self):
        path = self.config()
        path.chmod(0o644)
        with self.assertRaises(MODULE.BitwardenSecretError):
            MODULE.load_token("reader", path, environ={"USER": "david"}, run=FakeRun(self.secret()))

    def test_invalid_profile_cannot_use_other_secret(self):
        with self.assertRaises(MODULE.BitwardenSecretError):
            MODULE.load_token("writer", self.config("reader"), environ={"USER": "david"}, run=FakeRun(self.secret()))


    def test_timeout_is_sanitized_and_does_not_return_a_token(self):
        def timeout(argv, **kwargs):
            raise subprocess.TimeoutExpired(argv, 30, output="do-not-expose")
        with self.assertRaises(MODULE.BitwardenSecretError) as caught:
            MODULE.load_token("reader", self.config(), run=timeout)
        self.assertNotIn("do-not-expose", str(caught.exception))

    def test_http_launcher_selects_writer_and_drops_vault_auth(self):
        with mock.patch.dict(sys.modules, {"bitwarden_secrets_adapter": MODULE}):
            spec = importlib.util.spec_from_file_location("launcher", ROOT / "run-chat-history-http-with-bitwarden.py")
            launcher = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(launcher)
        for collector, expected in (("true", "writer"), ("false", "reader")):
            with self.subTest(collector=collector), mock.patch.dict(os.environ, {
                "CHAT_HISTORY_BITWARDEN_CONFIG": "/metadata",
                "CHAT_HISTORY_COLLECTOR_ONLY": collector,
                "BWS_ACCESS_TOKEN": "owner-bootstrap",
                "BW_SESSION": "owner-session",
            }, clear=True), mock.patch.object(launcher, "load_token", return_value="service-token") as load, mock.patch.object(launcher.os, "execve") as execute:
                launcher.main()
                load.assert_called_once_with(expected, "/metadata")
                env = execute.call_args.args[2]
                self.assertEqual(env["CHAT_HISTORY_HTTP_TOKEN"], "service-token")
                self.assertNotIn("BW_SESSION", env)
                self.assertNotIn("BWS_ACCESS_TOKEN", env)
                self.assertNotIn("service-token", repr(execute.call_args.args[:2]))
        with mock.patch.dict(os.environ, {"CHAT_HISTORY_BITWARDEN_CONFIG": "/metadata"}, clear=True), mock.patch.object(launcher, "load_token", side_effect=MODULE.BitwardenSecretError("failed")), mock.patch.object(launcher.os, "execve") as execute:
            with mock.patch("sys.stderr"):
                self.assertEqual(launcher.main(), 1)
            execute.assert_not_called()
        with mock.patch.dict(os.environ, {"CHAT_HISTORY_BITWARDEN_CONFIG": "/metadata", "CHAT_HISTORY_ALLOW_WRITES": "true"}, clear=True), mock.patch.object(launcher, "load_token") as load, mock.patch.object(launcher.os, "execve") as execute:
            with mock.patch("sys.stderr"):
                self.assertEqual(launcher.main(), 2)
            load.assert_not_called()
            execute.assert_not_called()

    def test_http_opt_in_dispatch_precedes_old_keychain_lookup(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            script = home / "chat-history-http-service"
            script.write_text((ROOT / "chat-history-http-service").read_text())
            helper = home / "run-chat-history-http-with-bitwarden.py"
            helper.write_text("import sys; sys.exit(73)\n")
            result = subprocess.run(["/bin/bash", str(script)], env={
                "HOME": directory, "CHAT_HISTORY_BITWARDEN_CONFIG": "/unused",
                "CHAT_HISTORY_KEYCHAIN_SERVICE": "nonexistent-test-service",
            }, capture_output=True, text=True)
            self.assertEqual(result.returncode, 73)
            self.assertEqual(result.stderr, "")

    @unittest.skipUnless(Path("/opt/homebrew/bin/bws").is_file(), "local BWS not installed")
    def test_installed_bws_parses_cloud_configuration_without_credentials(self):
        with tempfile.NamedTemporaryFile(mode="w", dir="/tmp") as config:
            config.write(MODULE.CLOUD_CONFIG)
            config.flush()
            result = subprocess.run([
                "/opt/homebrew/bin/bws", "--config-file", config.name,
                "config", "server-api", "https://api.bitwarden.com", "--output", "none",
            ], env={"PATH": "/opt/homebrew/bin:/usr/bin:/bin"}, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, "installed BWS rejected public configuration")
            saved = Path(config.name).read_text()
            self.assertIn('server_api = "https://api.bitwarden.com"', saved)
            self.assertIn('server_identity = "https://identity.bitwarden.com"', saved)
            self.assertIn('state_opt_out = "true"', saved)


if __name__ == "__main__":
    unittest.main()
