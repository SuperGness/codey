#!/usr/bin/env python3
"""Create a minimal Codey native Rust plugin crate."""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
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
    if (len(value) > 96 or not re.fullmatch(r"[a-z][a-z0-9._-]*", value)
            or ".." in value or value.endswith(".")):
        raise ValueError("plugin id must start with a lowercase letter, use lowercase letters, digits, ._- within 96 characters, and contain neither .. nor a trailing dot")
    return value


def write(path: Path, content: str, force: bool) -> None:
    if path.exists() and not force:
        raise FileExistsError(f"refusing to overwrite {path}; pass --force to replace it")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def main() -> int:
    repository = Path(__file__).resolve().parents[4]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("name", help="crate/folder name, for example header-demo")
    parser.add_argument("--path", type=Path, default=repository / "plugins",
                        help="plugin directory or parent directory (default: repository plugins/)")
    parser.add_argument("--id")
    parser.add_argument("--display-name")
    parser.add_argument("--version", default="0.1.0")
    parser.add_argument("--sdk-path", help="SDK path relative to the generated crate, or an absolute path")
    parser.add_argument("--capability", action="append", choices=["request.lifecycle.v1", "request.lifecycle.auth", "request.lifecycle.api_key", "request.lifecycle.turn_state", "provider.route.v1", "provider.transport.v1", "provider.account.v1", "appserver.call.v1"], default=[])
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    try:
        folder = folder_name(args.name)
        crate = crate_name(folder)
        identifier = plugin_id(args.id if args.id is not None else f"dev.codey.{crate.replace('_', '-')}")
    except ValueError as error:
        parser.error(str(error))
    display = args.display_name or crate.replace("_", " ").title()
    root = args.path.expanduser().resolve()
    if root.name != folder:
        root = root / folder

    lifecycle = "request.lifecycle.v1" in args.capability
    if any(capability.startswith("request.lifecycle.") and capability != "request.lifecycle.v1" for capability in args.capability) and not lifecycle:
        parser.error("request lifecycle extensions require request.lifecycle.v1")
    if len(set(args.capability)) != len(args.capability):
        parser.error("capabilities must not be repeated")
    transport = "provider.transport.v1" in args.capability
    account = "provider.account.v1" in args.capability
    if (transport and "provider.route.v1" not in args.capability) or account != transport:
        parser.error("provider transport requires provider.route.v1, provider.transport.v1, and provider.account.v1")

    sdk_path = args.sdk_path
    if sdk_path is None:
        sdk = repository / "crates/codey-plugin-sdk"
        try:
            sdk_path = os.path.relpath(sdk, root)
        except ValueError:  # Separate Windows drives cannot use a relative path.
            sdk_path = str(sdk)

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
    if transport:
        provider_method = """
            "provider.describe" => Ok(json!({
                "name": "Example",
                "baseUrl": "https://example.invalid/v1",
                "upstreamProtocol": "openaiResponses",
                "models": ["example-model"],
                "headers": [],
                "transport": {"accountEmail": self.account_email, "models": {}, "imageGeneration": false, "imageEdit": false}
            })),
            // 实现有界分块、后台传输及取消后，再替换此明确失败的占位。
            "provider.request.start" => Err("upstream_unavailable".into()),
            "provider.request.write" | "provider.request.read" => Err("request_not_found".into()),
            "provider.request.cancel" | "provider.request.stop" => Ok(json!({})),
"""
    account_field = "    account_email: String," if transport else ""
    account_init = """
        let account_email = config["accountEmail"].as_str().unwrap_or("").trim().to_ascii_lowercase();
        if account_email.len() > 254 || !account_email.contains('@') || account_email.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("请填写需要绑定的账号邮箱".into());
        }
""" if transport else ""
    init_fields = "context, account_email" if transport else "context"

    cargo = textwrap.dedent(f"""\
        [package]
        name = "codey-plugin-{crate}"
        version = {json.dumps(args.version, ensure_ascii=False)}
        edition = "2024"

        [workspace]

        [lib]
        crate-type = ["cdylib"]

        [dependencies]
        codey-plugin-sdk = {{ path = {json.dumps(sdk_path, ensure_ascii=False)} }}
    """)
    source = textwrap.dedent(f"""
        use codey_plugin_sdk::{{Plugin, PluginContext, serde_json::{{json, Value}}}};

        struct {''.join(part.title() for part in crate.split('_'))} {{
            context: PluginContext,
{account_field}
        }}

        impl Plugin for {''.join(part.title() for part in crate.split('_'))} {{
            fn create(config: Value, context: PluginContext) -> Result<Self, String> {{
                if !config.is_object() {{
                    return Err("config must be a JSON object".into());
                }}
{account_init}
                context.log("plugin_created")?;
                Ok(Self {{ {init_fields} }})
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
    if transport:
        config = '{\n  "_comments": {"accountEmail": "必填：唯一匹配 Codey 已保存账号的邮箱，不回退默认账号。"},\n  "accountEmail": ""\n}\n'
    readme = textwrap.dedent(f"""\
        # {display}

        Codey native plugin `{identifier}`. Implement business behavior in `src/lib.rs`, keep configuration in `config.json`, and package the built `cdylib` with the repository `scripts/package-plugin.py`.

        Declared capabilities: {', '.join(args.capability) if args.capability else 'none'}.

        This plugin is trusted native code and runs with the host process permissions.
    """)
    if transport:
        readme += "\nThis scaffold rejects transport requests until implemented. Follow crates/codey-plugin-sdk/PROVIDER_TRANSPORT.md for bounded I/O, cancellation, credential handling, and runtime cleanup.\n"

    files = {"Cargo.toml": cargo, "src/lib.rs": source, "config.json": config, "README.md": readme}
    try:
        # Check every destination before creating anything, including parent files.
        for relative in files:
            destination = root / relative
            if destination.exists() or destination.is_symlink():
                if not args.force or not destination.is_file() or destination.is_symlink():
                    raise FileExistsError(f"refusing to overwrite {destination}; pass --force to replace a regular file")
            for parent in destination.parents:
                if parent.exists() and not parent.is_dir():
                    raise FileExistsError(f"destination parent is not a directory: {parent}")
        for relative, content in files.items():
            write(root / relative, content, args.force)
    except (OSError, ValueError) as error:
        parser.error(str(error))

    print(root)
    print(f"cargo build --manifest-path {shlex.quote(str(root / 'Cargo.toml'))}")
    capability_flags = ''.join(f' --capability {capability}' for capability in args.capability)
    print(f"python3 {shlex.quote(str(repository / 'scripts/package-plugin.py'))} --library <target-library> --config {shlex.quote(str(root / 'config.json'))} --output <desktop>/{folder}/{folder}-<platform>-<arch>-{shlex.quote(args.version)}.codey-plugin --id {identifier} --name {shlex.quote(display)} --version {shlex.quote(args.version)} --platform <platform> --arch <arch>{capability_flags}")
    if "request.lifecycle.api_key" in args.capability:
        print("Add --api-key-url <exact-authorized-Responses-URL> when packaging API Key access.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
