"""Exercise API Key scope validation through the packaging CLI."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile


SCRIPT = Path(__file__).with_name("package-plugin.py")
LIFECYCLE = "request.lifecycle.v1"
API_KEY = "request.lifecycle.api_key"


class PackageScopeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="codey-package-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.library = self.root / "fixture.dylib"
        self.library.write_bytes(b"test library; never executed")
        self.sequence = 0

    def package(self, urls=(), capabilities=(LIFECYCLE, API_KEY), extra=()):
        self.sequence += 1
        output = self.root / f"fixture-{self.sequence}.codey-plugin"
        command = [sys.executable, str(SCRIPT), "--library", str(self.library),
                   "--output", str(output), "--id", "dev.codey.test",
                   "--name", "Test", "--version", "0.1.0"]
        for capability in capabilities:
            command.extend(["--capability", capability])
        for url in urls:
            command.extend(["--api-key-url", url])
        command.extend(extra)
        result = subprocess.run(command, capture_output=True, text=True)
        manifest = None
        if result.returncode == 0:
            with zipfile.ZipFile(output) as archive:
                manifest = json.loads(archive.read("manifest.json"))
        else:
            self.assertFalse(output.exists())
        return result, manifest

    def test_invalid_ids_fail_before_writing_a_package(self):
        for identifier in ("", "Dev.Foo", "9foo", "dev..foo", "dev.foo.", "a" * 97):
            with self.subTest(identifier=identifier):
                result, _ = self.package(capabilities=(), extra=("--id", identifier))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("ID", result.stderr)
        result, manifest = self.package(capabilities=(), extra=("--id", "dev._demo-"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(manifest["id"], "dev._demo-")

    def test_sensitive_and_invalid_headers_fail_before_writing(self):
        for flag in ("--header", "--response-header"):
            for header in ("Authorization", "COOKIE", "x-codey-private", "sec-test",
                           "x-access-token", "x-credential", "bad name", "X-K", "x" * 129):
                with self.subTest(flag=flag, header=header):
                    result, _ = self.package(capabilities=(LIFECYCLE,), extra=(flag, header))
                    self.assertNotEqual(result.returncode, 0)
        result, _ = self.package(capabilities=(LIFECYCLE,), extra=("--header", "Content-Type"))
        self.assertNotEqual(result.returncode, 0)

    def test_safe_request_headers_and_readable_response_metadata_are_preserved(self):
        result, manifest = self.package(capabilities=(LIFECYCLE,), extra=(
            "--header", "X-Test", "--response-header", "Content-Type",
            "--response-header", "content-length", "--response-header", "Retry-After"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(manifest["headerNames"], ["X-Test"])
        self.assertEqual(manifest["responseHeaderNames"], ["Content-Type", "content-length", "Retry-After"])

    def test_unicode_hosts_cannot_change_the_authorized_domain(self):
        for url in ["https://faß.de/v1/responses", "https://例子.测试/responses"]:
            with self.subTest(url=url):
                result, _ = self.package([url])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Punycode", result.stderr)

    def test_ascii_and_punycode_scopes_preserve_the_target(self):
        urls = ["https://TOKEN.sensenova.cn:443/v1/responses",
                "https://xn--fa-hia.de/v1/responses",
                "http://[::1]:8765/responses/compact"]
        result, manifest = self.package(urls)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(manifest["apiKeyUrls"], [
            "https://token.sensenova.cn/v1/responses", urls[1], urls[2]])

    def test_unsafe_or_non_response_targets_are_rejected(self):
        for url in ["http://example.com/responses", "https://user@example.com/responses",
                    "https://@example.com/responses", "https://*.example.com/responses",
                    "https://example.com/responses?", "https://example.com/responses#",
                    "https://example.com/v1/chat/completions", "https://example.com/responses/",
                    "https://example.com/foo/../responses", "https://example.com/foo/%2e%2e/responses",
                    "https://example.com/%72esponses", "https://example.com\\/responses",
                    "https://example.com/ responses", "ftp://localhost/responses", "not-a-url"]:
            with self.subTest(url=url):
                result, _ = self.package([url])
                self.assertNotEqual(result.returncode, 0)

    def test_scope_and_capability_must_be_declared_together(self):
        url = "https://example.com/responses"
        for urls, capabilities in [([], (LIFECYCLE, API_KEY)),
                                   ([url], (LIFECYCLE,)), ([url], (API_KEY,))]:
            with self.subTest(capabilities=capabilities):
                result, _ = self.package(urls, capabilities)
                self.assertNotEqual(result.returncode, 0)

    def test_duplicate_normalized_targets_are_rejected(self):
        result, _ = self.package(["https://EXAMPLE.com:443/responses",
                                  "https://example.com/responses"])
        self.assertNotEqual(result.returncode, 0)

    def test_scope_count_is_bounded(self):
        result, _ = self.package([f"https://example.com:{9000 + i}/responses" for i in range(33)])
        self.assertNotEqual(result.returncode, 0)

    def test_existing_transport_capabilities_still_package(self):
        capabilities = ("provider.route.v1", "provider.transport.v1", "provider.account.v1")
        result, manifest = self.package(capabilities=capabilities)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(manifest["capabilities"], list(capabilities))
        self.assertNotIn("apiKeyUrls", manifest)


if __name__ == "__main__":
    unittest.main()
