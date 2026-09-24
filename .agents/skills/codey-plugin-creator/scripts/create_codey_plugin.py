#!/usr/bin/env python3
"""Create a minimal Codey native Rust plugin crate."""

from __future__ import annotations

import argparse
import re
import textwrap
from pathlib import Path


def folder_name(value: str) -> str:
    normalized = re.sub(r"[^a-zA-Z0-9]+", "-", value.strip()).strip("-").lower()
    normalized = re.sub(r"-+", "-", normalized)
    if not normalized or not re.match(r"[a-zA-Z]", normalized):
        raise ValueError("plugin name must start with a letter and use letters, digits, or hyphens")
    return normalized


def crate_name(value: str) -> str:
    normalized = re.sub(r"[^a-zA-Z0-9]+", "_", value.strip()).strip("_").lower()
    normalized = re.sub(r"_+", "_", normalized)
    if not normalized or not re.match(r"[a-zA-Z_]", normalized):
        raise ValueError("plugin name must contain letters, digits, or underscores")
    return normalized


def plugin_id(value: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9]+(?:[._-][A-Za-z0-9]+)*", value):
        raise ValueError("plugin id must use dot, dash, underscore, and alphanumeric segments")
    return value


def write(path: Path, content: str, force: bool) -> None:
    if path.exists() and not force:
        raise FileExistsError(f"refusing to overwrite {path}; pass --force to replace it")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("name", help="crate/folder name, for example header-demo")
    parser.add_argument("--path", type=Path, default=Path.cwd())
    parser.add_argument("--id")
    parser.add_argument("--display-name")
    parser.add_argument("--version", default="0.1.0")
    parser.add_argument("--sdk-path", default="../../../crates/codey-plugin-sdk")
    parser.add_argument("--capability", action="append", choices=["request.lifecycle.v1", "request.lifecycle.auth", "provider.route.v1", "appserver.call.v1"], default=[])
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    folder = folder_name(args.name)
    crate = crate_name(folder)
    identifier = plugin_id(args.id or f"dev.codey.{crate.replace('_', '-')}")
    display = args.display_name or crate.replace("_", " ").title()
    root = args.path.expanduser().resolve()
    if root.name != folder:
        root = root / folder

    lifecycle = "request.lifecycle.v1" in args.capability
    auth = "request.lifecycle.auth" in args.capability
    if auth and not lifecycle:
        parser.error("request.lifecycle.auth requires request.lifecycle.v1")

    lifecycle_methods = """
            "request.beforeSend" | "request.afterHeaders" => Ok(json!({"action": "continue"})),
            "request.completed" | "request.failed" | "request.cancelled" => Ok(json!({})),
""" if lifecycle else ""
    provider_method = """
            "provider.describe" => Ok(json!({
                "name": "Example",
                "baseUrl": "https://example.invalid/v1",
                "upstreamProtocol": "openaiResponses",
                "models": ["example-model"]
            })),
""" if "provider.route.v1" in args.capability else ""

    cargo = textwrap.dedent(f"""\
        [package]
        name = "codey-plugin-{crate}"
        version = "{args.version}"
        edition = "2024"

        [lib]
        crate-type = ["cdylib"]

        [dependencies]
        codey-plugin-sdk = {{ path = "{args.sdk_path}" }}
    """)
    source = textwrap.dedent(f"""
        use codey_plugin_sdk::{{Plugin, PluginContext, serde_json::{{json, Value}}}};

        struct {''.join(part.title() for part in crate.split('_'))} {{
            context: PluginContext,
        }}

        impl Plugin for {''.join(part.title() for part in crate.split('_'))} {{
            fn create(config: Value, context: PluginContext) -> Result<Self, String> {{
                if !config.is_object() {{
                    return Err("config must be a JSON object".into());
                }}
                context.log("plugin_created")?;
                Ok(Self {{ context }})
            }}

            fn invoke(&mut self, method: &str, params: Value) -> Result<Value, String> {{
                match method {{
                    "ping" => Ok(json!({{"plugin": "{identifier}", "params": params}})),
                    "storage.context" => codey_plugin_sdk::serde_json::to_value(&self.context).map_err(|e| e.to_string()),
{lifecycle_methods}{provider_method}                    _ => Err(format!("unknown method: {{method}}")),
                }}
            }}
        }}

        codey_plugin_sdk::export_plugin!({''.join(part.title() for part in crate.split('_'))});
    """)
    config = '{\n  "_comments": {\n    "enabled": "Example configuration field; replace with plugin-specific settings."\n  },\n  "enabled": true\n}\n'
    readme = textwrap.dedent(f"""\
        # {display}

        Codey native plugin `{identifier}`. Implement business behavior in `src/lib.rs`, keep configuration in `config.json`, and package the built `cdylib` with the repository `scripts/package-plugin.py`.

        Declared capabilities: {', '.join(args.capability) if args.capability else 'none'}.

        This plugin is trusted native code and runs with the host process permissions.
    """)

    try:
        write(root / "Cargo.toml", cargo, args.force)
        write(root / "src/lib.rs", source, args.force)
        write(root / "config.json", config, args.force)
        write(root / "README.md", readme, args.force)
    except (FileExistsError, ValueError) as error:
        parser.error(str(error))

    print(root)
    print(f"cargo build --manifest-path {root / 'Cargo.toml'}")
    print(f"python3 scripts/package-plugin.py --library <target-library> --config {root / 'config.json'} --output /tmp/{crate}.codey-plugin --id {identifier} --name {display!r} --version {args.version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
