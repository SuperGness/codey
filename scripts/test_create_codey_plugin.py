"""Exercise the native plugin scaffold without creating repository plugins."""
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import tomllib
import unittest


REPOSITORY = Path(__file__).resolve().parents[1]
SCRIPT = REPOSITORY / ".agents/skills/codey-plugin-creator/scripts/create_codey_plugin.py"


class ScaffoldTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="codey-scaffold-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "plugins/demo"

    def create(self, *arguments):
        return subprocess.run([sys.executable, str(SCRIPT), "demo", "--path", str(self.root),
                               *arguments], capture_output=True, text=True)

    def test_default_sdk_resolves_and_crate_is_independent(self):
        result = self.create()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = tomllib.loads((self.root / "Cargo.toml").read_text())
        sdk = manifest["dependencies"]["codey-plugin-sdk"]["path"]
        self.assertEqual((self.root / sdk).resolve(), REPOSITORY / "crates/codey-plugin-sdk")
        self.assertEqual(manifest["workspace"], {})
        self.assertTrue((self.root / "src/lib.rs").is_file())

    def test_ids_follow_host_rules_before_writing(self):
        for identifier in ("", "Dev.Foo", "9foo", "dev..foo", "dev.foo.", "a" * 97):
            with self.subTest(identifier=identifier):
                result = self.create("--id", identifier)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("Traceback", result.stderr)
                self.assertFalse(self.root.exists())
        result = self.create("--id", "dev._demo-")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_default_destination_is_repository_plugins_from_any_directory(self):
        repository = Path(self.temp.name) / "repository"
        script = repository / SCRIPT.relative_to(REPOSITORY)
        script.parent.mkdir(parents=True)
        shutil.copyfile(SCRIPT, script)
        result = subprocess.run([sys.executable, str(script), "demo"],
                                cwd=self.temp.name, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        generated = repository / "plugins/demo/Cargo.toml"
        self.assertTrue(generated.is_file())
        self.assertFalse((Path(self.temp.name) / "demo").exists())
        manifest = tomllib.loads(generated.read_text())
        self.assertEqual((generated.parent / manifest["dependencies"]["codey-plugin-sdk"]["path"]).resolve(),
                         (repository / "crates/codey-plugin-sdk").resolve())

    def test_explicit_sdk_and_version_are_toml_strings(self):
        sdk = '../SDK "quoted"/windows\\path'
        version = '0.1.0"\n[unexpected]'
        result = self.create("--sdk-path", sdk, "--version", version)
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = tomllib.loads((self.root / "Cargo.toml").read_text())
        self.assertEqual(manifest["dependencies"]["codey-plugin-sdk"]["path"], sdk)
        self.assertEqual(manifest["package"]["version"], version)
        self.assertNotIn("unexpected", manifest)

    def test_conflicting_file_does_not_create_partial_scaffold(self):
        self.root.mkdir(parents=True)
        config = self.root / "config.json"
        config.write_text('{"user": true}')
        result = self.create()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(config.read_text(), '{"user": true}')
        self.assertEqual(list(self.root.iterdir()), [config])

    def test_parent_file_does_not_create_partial_scaffold(self):
        self.root.mkdir(parents=True)
        source = self.root / "src"
        source.write_text("user data")
        result = self.create("--force")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(self.root.iterdir()), [source])

    def test_lifecycle_extensions_require_base_capability(self):
        for capability in ("request.lifecycle.auth", "request.lifecycle.api_key",
                           "request.lifecycle.turn_state"):
            with self.subTest(capability=capability):
                result = self.create("--capability", capability)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.root.exists())
                result = self.create("--capability", "request.lifecycle.v1",
                                     "--capability", capability)
                self.assertEqual(result.returncode, 0, result.stderr)
                for file in (self.root / "src/lib.rs", self.root / "Cargo.toml",
                             self.root / "config.json", self.root / "README.md"):
                    file.unlink()
                (self.root / "src").rmdir()
                self.root.rmdir()

    def test_cargo_metadata_accepts_scaffold_inside_another_workspace(self):
        result = self.create()
        self.assertEqual(result.returncode, 0, result.stderr)
        parent = self.root.parent
        (parent / "Cargo.toml").write_text('[workspace]\nmembers = []\n')
        result = subprocess.run(["cargo", "metadata", "--no-deps", "--offline",
                                 "--format-version", "1", "--manifest-path",
                                 str(self.root / "Cargo.toml")], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
