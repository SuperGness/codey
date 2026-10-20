"""Offline checks for the native runner's failure paths and isolation.

Run with: python3 -m unittest discover -s tests -p 'test_native_codex_compat.py' -v
These tests never require Codex or download anything.
"""

import importlib.util
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock
import urllib.error
import urllib.request


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "native-codex-compat.py"
SPEC = importlib.util.spec_from_file_location("native_codex_compat", SCRIPT)
compat = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(compat)


@unittest.skipUnless(os.name == "posix", "runner uses POSIX process groups")
class NativeRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="codey-native-selftest-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = compat.isolated_environment(self.root)

    def client(self, source, timeout=0.2):
        client = compat.Client([sys.executable, "-c", textwrap.dedent(source)], self.root, self.env, timeout)
        self.addCleanup(client.close)
        return client

    def fake_binary(self, source):
        binary = self.root / "fake-codex"
        binary.write_text(f"#!{sys.executable}\n" + textwrap.dedent(source), encoding="utf-8")
        binary.chmod(0o700)
        return binary

    def test_environment_does_not_inherit_credentials_or_user_paths(self):
        with mock.patch.dict(os.environ, {"HOME": "/not-the-test-home", "CODEX_HOME": "/not-the-test-codex",
                                          "OPENAI_API_KEY": "synthetic-secret", "HTTPS_PROXY": "https://example.invalid",
                                          "CODEX_API_KEY": "synthetic-secret", "NODE_OPTIONS": "untrusted"}):
            env = compat.isolated_environment(self.root, "http://127.0.0.1:12345")
        self.assertNotIn("OPENAI_API_KEY", env)
        self.assertNotIn("CODEX_API_KEY", env)
        self.assertNotIn("NODE_OPTIONS", env)
        self.assertEqual(env["HTTPS_PROXY"], "http://127.0.0.1:12345")
        for name in ("HOME", "CODEX_HOME", "TMPDIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME"):
            self.assertTrue(Path(env[name]).is_relative_to(self.root))

    def test_early_exit_is_reported_and_reaped(self):
        client = self.client("raise SystemExit(7)")
        with self.assertRaisesRegex(RuntimeError, "terminated"):
            client.until(lambda _event: False)
        cleanup = client.close()
        self.assertTrue(cleanup["reaped"])
        self.assertTrue(cleanup["reader_stopped"])
        self.assertEqual(cleanup["returncode"], 7)

    def test_timeout_kills_uncooperative_child(self):
        client = self.client("""
            import signal, time
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            print('{"ready":true}', flush=True)
            time.sleep(60)
        """)
        client.until(lambda event: event.get("ready"))
        with self.assertRaises(TimeoutError):
            client.until(lambda _event: False)
        started = time.monotonic()
        cleanup = client.close()
        self.assertLess(time.monotonic() - started, 5)
        self.assertEqual(cleanup["returncode"], -9)
        self.assertTrue(cleanup["reader_stopped"])

    def test_cleanup_kills_descendant_after_leader_exit(self):
        child_source = ('import signal,time,json; signal.signal(signal.SIGTERM,signal.SIG_IGN); '
                        'print(json.dumps({"ready":True}),flush=True); time.sleep(60)')
        client = self.client(f"""
            import subprocess, sys
            subprocess.Popen([sys.executable, '-c', {child_source!r}])
        """)
        client.until(lambda event: event.get("ready"))
        client.process.wait(timeout=2)
        cleanup = client.close()
        self.assertTrue(cleanup["reaped"])
        # The descendant owns stdout too; stopping the reader proves it exited.
        self.assertTrue(cleanup["reader_stopped"])

    def test_notifications_before_rpc_response_are_retained(self):
        client = self.client("""
            import json, sys
            for line in sys.stdin:
                request = json.loads(line)
                print(json.dumps({'method':'early-notification'}), flush=True)
                print(json.dumps({'id':request['id'],'result':{'ok':True}}), flush=True)
        """)
        self.assertEqual(client.rpc("fixture", {}), {"ok": True})
        self.assertEqual(client.until(lambda event: event.get("method") == "early-notification"),
                         {"method": "early-notification"})

    def test_mock_rejects_unexpected_route_and_releases_port(self):
        server = compat.MockResponses()
        port = server.server.server_port
        try:
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            with self.assertRaises(urllib.error.HTTPError) as error:
                opener.open(server.url + "/external-destination", timeout=2)
            self.assertEqual(error.exception.code, 400)
            self.assertEqual(server.unexpected, [{"method": "GET", "path": "/external-destination"}])
        finally:
            server.close()
        with self.assertRaises(OSError):
            socket.create_connection(("127.0.0.1", port), timeout=0.2)

    def test_alias_fixture_records_wire_metadata_and_rejects_raw_model(self):
        server = compat.MockResponses(accepted_models=(compat.ALIAS_A, compat.ALIAS_B), quota_model=None)
        self.addCleanup(server.close)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        for selected in (compat.ALIAS_A, compat.ALIAS_B, compat.ALIAS_A):
            request = urllib.request.Request(server.url + "/v1/responses",
                data=json.dumps({"model": selected, "input": [], "client_metadata": {"codey_route": "fixture-a"}}).encode(),
                headers={"Content-Type": "application/json",
                         "x-codex-turn-metadata": json.dumps({"codey_route": "fixture-b"})})
            with opener.open(request, timeout=2) as response:
                self.assertEqual(response.status, 200)
                response.read()
        compat.expect_requests(server, [(selected, 200, False)
                                        for selected in (compat.ALIAS_A, compat.ALIAS_B, compat.ALIAS_A)])
        for request in server.requests:
            self.assertEqual(compat.route_hints(request),
                             {"body": "fixture-a", "header_turn_metadata": "fixture-b"})
        raw = urllib.request.Request(server.url + "/v1/responses",
                                     data=json.dumps({"model": "shared-model", "input": []}).encode())
        with self.assertRaises(urllib.error.HTTPError) as error:
            opener.open(raw, timeout=2)
        self.assertEqual(error.exception.code, 400)
        self.assertEqual(len(server.requests), 3)
        self.assertEqual(server.unexpected, [{"method": "POST", "path": "/v1/responses"}])

    def test_failed_scenario_returns_nonzero_report_and_removes_temporary_home(self):
        binary = self.fake_binary("""
            import sys
            if '--version' in sys.argv:
                print('codex-cli synthetic-offline-fixture')
            else:
                print('not JSON', flush=True)
        """)
        report = self.root / "report.json"
        self.assertEqual(compat.main(["--codex-bin", str(binary), "--case", "missing-hash-switch",
                                      "--timeout", "0.5", "--report", str(report)]), 1)
        result = json.loads(report.read_text())
        self.assertFalse(result["passed"])
        self.assertTrue(result["temporary_data_removed"])
        case = result["runs"][0]["cases"][0]
        self.assertTrue(case["mock_closed"])
        self.assertTrue(case["cleanup"][0]["reaped"])
        self.assertIn("invalid JSON", case["error"])

    def test_configuration_failure_closes_mock(self):
        original = Path.write_text

        def fail_config(path, *args, **kwargs):
            if path.name == "config.toml":
                raise OSError("synthetic configuration failure")
            return original(path, *args, **kwargs)

        with mock.patch.object(Path, "write_text", fail_config):
            result = compat.run_scenario("unused", self.root / "scenario", "missing-hash-switch", 0.2)
        self.assertFalse(result["passed"])
        self.assertIn("synthetic configuration failure", result["error"])
        self.assertTrue(result["mock_closed"])
        self.assertEqual(result["cleanup"], [])

    def test_proxy_never_opens_external_tunnels(self):
        server = compat.MockResponses()
        connection = http.client.HTTPConnection("127.0.0.1", server.server.server_port, timeout=2)
        try:
            connection.set_tunnel("should-never-resolve.invalid", 443)
            with self.assertRaisesRegex(OSError, "403"):
                connection.request("GET", "/")
            self.assertEqual(server.blocked_outbound,
                             [{"method": "CONNECT", "path": "should-never-resolve.invalid:443"}])
            self.assertEqual(server.requests, [])
            with self.assertRaisesRegex(AssertionError, "unexpected blocked outbound"):
                compat.expect_requests(server, [])
        finally:
            connection.close()
            server.close()

    def test_known_startup_tunnels_remain_blocked_without_failing_route_checks(self):
        server = compat.MockResponses()
        self.addCleanup(server.close)
        for target in sorted(compat.BLOCKED_STARTUP_TARGETS):
            connection = http.client.HTTPConnection("127.0.0.1", server.server.server_port, timeout=2)
            try:
                host, port = target.rsplit(":", 1)
                connection.set_tunnel(host, int(port))
                with self.assertRaisesRegex(OSError, "403"):
                    connection.request("GET", "/")
            finally:
                connection.close()
        self.assertEqual(len(server.blocked_outbound), len(compat.BLOCKED_STARTUP_TARGETS))
        compat.expect_requests(server, [])
        server.blocked_outbound.append({"method": "POST", "path": "https://chatgpt.com/v1/responses"})
        with self.assertRaisesRegex(AssertionError, "unexpected blocked outbound"):
            compat.expect_requests(server, [])

    def test_sigterm_cleans_active_process_group_and_temporary_home(self):
        marker = self.root / "marker.json"
        binary = self.fake_binary(f"""
            import json, os, pathlib, signal, sys, time
            if '--version' in sys.argv:
                print('codex-cli synthetic-offline-fixture')
            else:
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                pathlib.Path({str(marker)!r}).write_text(json.dumps({{'pid':os.getpid(), 'home':os.environ['CODEX_HOME']}}))
                time.sleep(60)
        """)
        runner = subprocess.Popen([sys.executable, str(SCRIPT), "--codex-bin", str(binary),
                                   "--case", "missing-hash-switch"], stdout=subprocess.DEVNULL,
                                  stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(marker.exists(), "fake app-server did not start")
            child = json.loads(marker.read_text())
            runner.terminate()
            _, stderr = runner.communicate(timeout=8)
            self.assertEqual(runner.returncode, 130, stderr)
            self.assertFalse(Path(child["home"]).exists())
            with self.assertRaises(ProcessLookupError):
                os.kill(child["pid"], 0)
        finally:
            if runner.poll() is None:
                runner.kill()
                runner.wait(timeout=3)
            runner.stderr.close()


if __name__ == "__main__":
    unittest.main()
