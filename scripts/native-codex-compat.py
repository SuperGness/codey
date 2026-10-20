#!/usr/bin/env python3
"""Exercise native Codex against a synthetic loopback Responses provider.

Usage (Python 3.10+, macOS/Linux; pass native executables, not npm wrappers):
  python3 scripts/native-codex-compat.py --codex-bin /path/to/codex \
      --codex-bin /path/to/another/codex --report /tmp/native-compat.json

Every run uses temporary HOME, CODEX_HOME, working directory and session data.
No account is needed. Requests exercise local compaction, not official remote
compaction or encrypted history compatibility across official accounts.
Alias checks cover native wire requests and catalog capabilities, not Codey gateway routing.
The rejecting proxy reports known CLI startup connections without failing route
checks; any other proxied destination fails the scenario. This is not an OS
network sandbox: clients that ignore proxy settings require separate isolation.
"""

import argparse
import contextlib
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import tempfile
import threading
import time


OLD = "fixture-old"
NEW = "fixture-new"
ALIAS_A = "fixture-a/shared-model"
ALIAS_B = "fixture-b/shared-model"
OLD_HASH = "fixture-hash-old"
COMPACTION_MARKER = "CONTEXT CHECKPOINT COMPACTION"
# These pinned clients contact startup services even with updates/analytics off.
# This list permits reporting a denied CONNECT, never opening the connection.
BLOCKED_STARTUP_TARGETS = frozenset(("chatgpt.com:443", "github.com:443", "api.github.com:443"))
CASES = (
    "same-hash-switch",
    "different-hash-compaction",
    "missing-hash-switch",
    "resume-missing-hash",
    "recover-failed-compaction",
    "smaller-window-compaction",
    "explicit-alias-roundtrip",
)


def check(condition, message):
    # Assertions must remain enabled even when Python is run with -O.
    if not condition:
        raise AssertionError(message)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def isolated_environment(directory, proxy=None):
    """Build an allowlist instead of copying secrets, proxies or user config."""
    directory = Path(directory)
    paths = {name: directory / name for name in ("home", "codex", "tmp", "config", "cache", "data")}
    for path in paths.values():
        path.mkdir(parents=True, exist_ok=True)
    env = {
        "PATH": os.defpath,
        "HOME": str(paths["home"]),
        "CODEX_HOME": str(paths["codex"]),
        "TMPDIR": str(paths["tmp"]),
        "TMP": str(paths["tmp"]),
        "TEMP": str(paths["tmp"]),
        "XDG_CONFIG_HOME": str(paths["config"]),
        "XDG_CACHE_HOME": str(paths["cache"]),
        "XDG_DATA_HOME": str(paths["data"]),
        "LANG": "C.UTF-8",
    }
    if proxy:
        # Unexpected requests honoring proxy settings terminate at the fixture.
        for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
            env[name] = proxy
        env["NO_PROXY"] = env["no_proxy"] = "127.0.0.1,localhost,::1"
    return env


def stop_process_group(process, grace=2):
    """Reap the child and kill its group even if the group leader exited early."""
    # Reap an exited leader before signaling its group (including on macOS).
    process.poll()
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    # A child may have inherited stdout and outlived an already exited parent.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=grace)
    return {"reaped": process.returncode is not None, "returncode": process.returncode}


def model(slug, comp_hash, small=False):
    value = {
        "slug": slug, "display_name": slug, "description": "Synthetic compatibility fixture",
        "default_reasoning_level": "low",
        "supported_reasoning_levels": [{"effort": "low", "description": "Fixture low"}],
        "shell_type": "shell_command", "visibility": "list", "supported_in_api": True,
        "priority": 0, "base_instructions": "Reply OK without tools. Synthetic compatibility test.",
        "supports_reasoning_summaries": False, "support_verbosity": False,
        "default_verbosity": "low", "apply_patch_tool_type": "freeform",
        "truncation_policy": {"mode": "tokens", "limit": 10000},
        "supports_parallel_tool_calls": False,
        "context_window": 128000 if small else 1000000,
        "max_context_window": 128000 if small else 1000000,
        "auto_compact_token_limit": 115200 if small else 900000,
        "effective_context_window_percent": 100, "experimental_supported_tools": [],
        "input_modalities": ["text"], "supports_search_tool": False,
    }
    if comp_hash is not None:
        value["comp_hash"] = comp_hash
    return value


class MockResponses:
    def __init__(self, input_tokens=1000, accepted_models=(OLD, NEW), quota_model=OLD):
        self.input_tokens = input_tokens
        self.accepted_models = accepted_models
        self.quota_model = quota_model
        self.requests = []
        self.unexpected = []
        self.blocked_outbound = []
        self.phase = "initial"
        self.lock = threading.Lock()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(2)

            def log_message(self, *_args):
                pass

            def respond(self, status, data, content_type="application/json"):
                self.send_response(status)
                self.send_header("content-type", content_type)
                self.send_header("content-length", str(len(data)))
                self.send_header("connection", "close")
                self.end_headers()
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    self.wfile.write(data)

            def reject(self):
                with fixture.lock:
                    target = fixture.blocked_outbound if self.path.startswith(("http://", "https://")) else fixture.unexpected
                    target.append({"method": self.command, "path": self.path})
                self.respond(400, b'{"error":"unexpected fixture request"}')

            def do_CONNECT(self):
                # No tunnel is ever opened, including for CLI startup services.
                with fixture.lock:
                    fixture.blocked_outbound.append({"method": "CONNECT", "path": self.path})
                self.respond(403, b'{"error":"external network disabled by fixture"}')

            do_GET = reject
            do_PUT = reject
            do_DELETE = reject
            do_PATCH = reject
            do_HEAD = reject

            def do_POST(self):
                if self.path != "/v1/responses":
                    self.reject()
                    return
                try:
                    size = int(self.headers.get("content-length", "0"))
                    if not 0 < size <= 4 * 1024 * 1024:
                        raise ValueError("invalid request size")
                    body = json.loads(self.rfile.read(size))
                    if not isinstance(body, dict) or body.get("model") not in fixture.accepted_models:
                        raise ValueError("unexpected model")
                except (ValueError, OSError):
                    self.reject()
                    return
                with fixture.lock:
                    sequence = len(fixture.requests) + 1
                    # Quota scenarios reject the old model after the successful seed.
                    status = 429 if body["model"] == fixture.quota_model and sequence > 1 else 200
                    fixture.requests.append({
                        "phase": fixture.phase, "path": self.path, "model": body["model"],
                        "status": status, "compaction": COMPACTION_MARKER in json.dumps(body.get("input")),
                        "input": body.get("input"),
                        "client_metadata": body.get("client_metadata"),
                        "turn_metadata_header": self.headers.get("x-codex-turn-metadata"),
                    })
                if status == 429:
                    self.respond(429, json.dumps({"error": {
                        "code": "insufficient_quota", "type": "insufficient_quota",
                        "message": "Synthetic old-model quota exhausted",
                    }}).encode())
                    return
                item = {"id": f"msg_{sequence}", "type": "message", "status": "completed",
                        "role": "assistant", "content": [{"type": "output_text", "text": "OK", "annotations": []}]}
                response = {
                    "id": f"resp_{sequence}", "object": "response", "created_at": int(time.time()),
                    "model": body["model"], "status": "completed", "output": [item],
                    "usage": {"input_tokens": fixture.input_tokens, "input_tokens_details": {"cached_tokens": 0},
                              "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0},
                              "total_tokens": fixture.input_tokens + 1},
                }
                events = [
                    {"type": "response.created", "response": {**response, "status": "in_progress", "output": []}},
                    {"type": "response.output_item.added", "output_index": 0,
                     "item": {**item, "status": "in_progress", "content": []}},
                    {"type": "response.output_text.delta", "item_id": item["id"], "output_index": 0,
                     "content_index": 0, "delta": "OK"},
                    {"type": "response.output_item.done", "output_index": 0, "item": item},
                    {"type": "response.completed", "response": response},
                ]
                data = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()
                self.respond(200, data, "text/event-stream")

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        # server_close also joins handlers; each socket has a bounded timeout.
        self.server.daemon_threads = False
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)
        check(not self.thread.is_alive(), "mock server did not stop")


class Client:
    def __init__(self, command, directory, env, timeout=25):
        self.timeout = timeout
        self.inbox = queue.Queue()
        self.pending = []
        self.number = 0
        self.cleanup = None
        self.stderr = tempfile.TemporaryFile(mode="w+t", encoding="utf-8")
        try:
            self.process = subprocess.Popen(
                command, cwd=directory, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=self.stderr, text=True, encoding="utf-8", bufsize=1, start_new_session=True,
            )
        except BaseException:
            self.stderr.close()
            raise
        self.reader = threading.Thread(target=self.read_output, daemon=True)
        self.reader.start()

    def read_output(self):
        try:
            for line in self.process.stdout:
                try:
                    self.inbox.put(json.loads(line))
                except ValueError:
                    self.inbox.put({"_protocol_error": line[:300]})
        finally:
            self.inbox.put({"_eof": True})

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def until(self, predicate):
        # Preserve notifications that arrive before their RPC response.
        for index, value in enumerate(self.pending):
            if predicate(value):
                return self.pending.pop(index)
        deadline = time.monotonic() + self.timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("native Codex RPC/turn deadline exceeded")
            try:
                value = self.inbox.get(timeout=remaining)
            except queue.Empty as error:
                raise TimeoutError("native Codex RPC/turn deadline exceeded") from error
            if "_eof" in value or "_protocol_error" in value:
                raise RuntimeError(f"native Codex terminated or sent invalid JSON: {value}")
            if predicate(value):
                return value
            self.pending.append(value)
            check(len(self.pending) < 10000, "native Codex emitted too many unmatched events")

    def rpc(self, method, params):
        self.number += 1
        number = self.number
        self.send({"id": number, "method": method, "params": params})
        value = self.until(lambda event: event.get("id") == number)
        check("error" not in value, f"{method}: {value.get('error')}")
        return value["result"]

    def initialize(self):
        self.rpc("initialize", {"clientInfo": {"name": "codey_native_compat", "version": "1.0"},
                                "capabilities": {"experimentalApi": True}})
        self.send({"method": "initialized"})

    def turn(self, thread_id, selected_model, text, client_metadata=None):
        params = {"threadId": thread_id, "model": selected_model,
                  "input": [{"type": "text", "text": text, "text_elements": []}]}
        if client_metadata is not None:
            params["responsesapiClientMetadata"] = client_metadata
        result = self.rpc("turn/start", params)
        turn_id = result["turn"]["id"]
        value = self.until(lambda event: event.get("method") == "turn/completed"
                           and event.get("params", {}).get("threadId") == thread_id
                           and event.get("params", {}).get("turn", {}).get("id") == turn_id)
        return value["params"]["turn"]["status"]

    def close(self):
        if self.cleanup is not None:
            return self.cleanup
        self.cleanup = stop_process_group(self.process)
        self.reader.join(timeout=3)
        self.cleanup["reader_stopped"] = not self.reader.is_alive()
        check(self.cleanup["reader_stopped"], "stdout reader did not stop after process cleanup")
        self.process.stdin.close()
        self.process.stdout.close()
        self.stderr.seek(0)
        self.cleanup["stderr_tail"] = self.stderr.read()[-3000:]
        self.stderr.close()
        return self.cleanup


def rollout_snapshot(home):
    paths = sorted((home / "sessions").rglob("*.jsonl"))
    check(paths, "native Codex did not persist a rollout")
    contents = {str(path.relative_to(home)): path.read_bytes() for path in paths}
    contexts = [item["payload"] for data in contents.values() for line in data.splitlines()
                if (item := json.loads(line)).get("type") == "turn_context"]
    check(any(item.get("model") == OLD and item.get("comp_hash") == OLD_HASH for item in contexts),
          "seed rollout did not persist the old model comp_hash")
    return {name: hashlib.sha256(data).hexdigest() for name, data in contents.items()}


def expect_requests(mock, expected):
    actual = [(item["model"], item["status"], item["compaction"]) for item in mock.requests]
    check(actual == expected, f"request sequence mismatch: expected {expected}, got {actual}")
    check(not mock.unexpected, f"unexpected network requests: {mock.unexpected}")
    unexpected_outbound = [item for item in mock.blocked_outbound
                          if item["method"] != "CONNECT" or item["path"] not in BLOCKED_STARTUP_TARGETS]
    check(not unexpected_outbound, f"unexpected blocked outbound requests: {unexpected_outbound}")


def route_hints(request):
    """Inspect native wire metadata without simulating Codey's route selection."""
    body = request.get("client_metadata") or {}
    values = {"body": body, "body_turn_metadata": body.get("x-codex-turn-metadata"),
              "header_turn_metadata": request.get("turn_metadata_header")}
    hints = {}
    for source, value in values.items():
        if isinstance(value, str):
            value = json.loads(value)
        if isinstance(value, dict) and "codey_route" in value:
            hints[source] = value["codey_route"]
    return hints


def run_scenario(binary, directory, name, timeout):
    directory.mkdir()
    work = directory / "work"
    work.mkdir()
    smaller = name == "smaller-window-compaction"
    alias_switch = name == "explicit-alias-roundtrip"
    old_model, new_model = (ALIAS_A, ALIAS_B) if alias_switch else (OLD, NEW)
    must_compact = name in ("different-hash-compaction", "recover-failed-compaction", "smaller-window-compaction")
    target_hash = OLD_HASH if name == "same-hash-switch" else (
        "fixture-hash-new" if name in ("different-hash-compaction", "recover-failed-compaction") else None)
    env = isolated_environment(directory)
    home = Path(env["CODEX_HOME"])
    catalog = {"models": [model(old_model, None if alias_switch else OLD_HASH),
                          model(new_model, target_hash, smaller)]}
    if alias_switch:
        catalog["models"][1].update(context_window=512000, max_context_window=512000,
                                    auto_compact_token_limit=450000)
    write_json(home / "catalog.json", catalog)
    result = {"case": name, "passed": False, "cleanup": [], "synthetic_input_tokens": 200000 if smaller else 1000,
              "target_context_window": catalog["models"][1]["context_window"]}
    client = None

    def start_client():
        return Client([binary, "app-server", "--stdio"], work, env, timeout)

    def stop_client():
        nonlocal client
        if client is not None:
            result["cleanup"].append(client.close())
            client = None

    def resume(thread_id):
        nonlocal client
        client = start_client()
        client.initialize()
        client.rpc("thread/resume", {"threadId": thread_id, "model": NEW, "modelProvider": "fixture",
                                     "cwd": str(work), "approvalPolicy": "never", "sandbox": "read-only"})

    expected = [(old_model, 200, False)]
    mock = MockResponses(input_tokens=result["synthetic_input_tokens"],
                         accepted_models=(old_model, new_model), quota_model=None if alias_switch else OLD)
    try:
        env = isolated_environment(directory, mock.url)
        (home / "config.toml").write_text(
            f'model = "{old_model}"\nmodel_provider = "fixture"\n'
            f'model_catalog_json = {json.dumps(str(home / "catalog.json"))}\n'
            'model_reasoning_effort = "low"\ncheck_for_update_on_startup = false\n'
            'cli_auth_credentials_store = "file"\n'
            '[model_providers.fixture]\nname = "fixture"\n'
            f'base_url = "{mock.url}/v1"\nwire_api = "responses"\nrequires_openai_auth = false\n'
            'supports_websockets = false\nstream_max_retries = 0\nrequest_max_retries = 0\n'
            'stream_idle_timeout_ms = 5000\n'
            '[analytics]\nenabled = false\n[feedback]\nenabled = false\n', encoding="utf-8")
        client = start_client()
        client.initialize()
        if alias_switch:
            listed = client.rpc("model/list", {})["data"]
            for selected in (old_model, new_model):
                entry = next((item for item in listed if item.get("model") == selected), None)
                check(entry is not None, f"native catalog did not list alias {selected}")
                check(entry.get("defaultReasoningEffort") == "low", f"alias reasoning default changed: {entry}")
                check([level["reasoningEffort"] for level in entry.get("supportedReasoningEfforts", [])] == ["low"],
                      f"alias reasoning capabilities changed: {entry}")
            result["catalog_aliases"] = [old_model, new_model]
        thread = client.rpc("thread/start", {"model": old_model, "modelProvider": "fixture", "cwd": str(work),
                            "approvalPolicy": "never", "sandbox": "read-only", "persistExtendedHistory": True})
        thread_id = thread["thread"]["id"]
        check(client.turn(thread_id, old_model, "Synthetic seed turn. Return OK without tools.",
                          {"codey_route": "fixture-a"} if alias_switch else None) == "completed",
              "seed turn failed")
        expect_requests(mock, expected)
        if name == "resume-missing-hash":
            stop_client()
            before = rollout_snapshot(home)
            check(before == rollout_snapshot(home), "rollout changed before resume")
            result["rollout_unchanged_before_resume"] = True
            resume(thread_id)
        mock.phase = "switch"
        result["switch_status"] = client.turn(thread_id, new_model, "Synthetic switch turn. Return OK without tools.",
                                              {"codey_route": "fixture-a"} if alias_switch else None)
        check(result["switch_status"] == ("failed" if must_compact else "completed"), "unexpected switch outcome")
        expected.append((OLD, 429, True) if must_compact else (new_model, 200, False))
        expect_requests(mock, expected)
        if alias_switch:
            mock.phase = "switch-back"
            result["switch_back_status"] = client.turn(thread_id, old_model, "Return to the first synthetic route.",
                                                      {"codey_route": "fixture-b"})
            check(result["switch_back_status"] == "completed", "switch back to first alias failed")
            expected.append((old_model, 200, False))
            expect_requests(mock, expected)
            for request, hint in zip(mock.requests, ("fixture-a", "fixture-a", "fixture-b")):
                check(set(route_hints(request).values()) == {hint},
                      f"native request lost stale route metadata: {request['model']}, {route_hints(request)}")
            # Context limits must follow each alias, not an unknown-model fallback.
            usage = [client.until(lambda event: event.get("method") == "thread/tokenUsage/updated"
                                  and event.get("params", {}).get("threadId") == thread_id)["params"]
                     for _ in range(3)]
            check(len({item["turnId"] for item in usage}) == 3, "missing distinct per-turn token usage")
            result["context_windows"] = [item["tokenUsage"]["modelContextWindow"] for item in usage]
            check(result["context_windows"] == [1000000, 512000, 1000000],
                  f"native context limits did not follow the aliases: {result['context_windows']}")
        stop_client()
        if not alias_switch:
            result["old_hash_persisted"] = bool(rollout_snapshot(home))
        if name == "recover-failed-compaction":
            before = rollout_snapshot(home)
            catalog["models"][1].pop("comp_hash")
            write_json(home / "catalog.json", catalog)
            check(before == rollout_snapshot(home), "removing catalog hash changed persisted rollout")
            result["rollout_unchanged_before_resume"] = True
            mock.phase = "recover"
            resume(thread_id)
            result["recovery_status"] = client.turn(thread_id, NEW, "Continue the failed synthetic turn. Return OK.")
            check(result["recovery_status"] == "completed", "failed session did not recover")
            expected.append((NEW, 200, False))
            expect_requests(mock, expected)
        stop_client()
        expect_requests(mock, expected)
        result["passed"] = True
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        try:
            stop_client()
        finally:
            mock.close()
        result["mock_closed"] = not mock.thread.is_alive()
        result["requests"] = []
        for item in mock.requests:
            summary = {key: item[key] for key in ("phase", "path", "model", "status", "compaction")}
            if hints := route_hints(item):
                summary["route_hints"] = hints
            result["requests"].append(summary)
        result["unexpected_requests"] = mock.unexpected
        result["blocked_outbound"] = mock.blocked_outbound
    return result


def binary_version(binary, directory, timeout):
    env = isolated_environment(directory)
    process = subprocess.Popen([binary, "--version"], env=env, cwd=directory,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
        check(process.returncode == 0, f"version probe failed: {stderr[-1000:]}")
        check(stdout.strip().startswith("codex-cli "), f"not a native Codex CLI: {stdout[:200]}")
        return stdout.strip()
    finally:
        stop_process_group(process)
        process.stdout.close()
        process.stderr.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--codex-bin", action="append", required=True, type=Path,
                        help="native executable; repeat to check multiple versions")
    parser.add_argument("--case", action="append", choices=CASES, help="run only selected cases")
    parser.add_argument("--timeout", type=float, default=25, help="per RPC/turn deadline in seconds")
    parser.add_argument("--report", type=Path, help="write a synthetic JSON report; never contains user data")
    args = parser.parse_args(argv)
    if os.name != "posix":
        parser.error("this runner requires POSIX process groups (macOS/Linux)")
    if not 0 < args.timeout <= 120:
        parser.error("--timeout must be greater than zero and at most 120")
    binaries = [str(path.expanduser().resolve()) for path in args.codex_bin]
    for binary in binaries:
        if not Path(binary).is_file() or not os.access(binary, os.X_OK):
            parser.error(f"binary is not executable: {binary}")
    report = {"passed": False, "scope": "synthetic loopback Responses and local compaction; no official accounts",
              "runs": []}
    with tempfile.TemporaryDirectory(prefix="codey-native-compat-") as temporary:
        for index, binary in enumerate(binaries):
            directory = Path(temporary) / str(index)
            directory.mkdir()
            run = {"binary": binary, "passed": False, "cases": []}
            report["runs"].append(run)
            try:
                run["version"] = binary_version(binary, directory / "probe", min(args.timeout, 10))
                print(f"Checking {run['version']}", flush=True)
                for name in dict.fromkeys(args.case or CASES):
                    result = run_scenario(binary, directory / name, name, args.timeout)
                    run["cases"].append(result)
                    print(json.dumps({"version": run["version"], "case": name, "passed": result["passed"],
                                      "requests": result["requests"], "error": result.get("error"),
                                      "blocked_outbound_count": len(result["blocked_outbound"])}), flush=True)
                run["passed"] = all(case["passed"] for case in run["cases"])
            except Exception as error:
                run["error"] = f"{type(error).__name__}: {error}"
                print(run["error"], file=sys.stderr)
        report["passed"] = all(run["passed"] for run in report["runs"])
    report["temporary_data_removed"] = not Path(temporary).exists()
    if args.report:
        write_json(args.report, report)
    return 0 if report["passed"] and report["temporary_data_removed"] else 1


if __name__ == "__main__":
    def interrupted(signum, _frame):
        raise KeyboardInterrupt(f"received signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
