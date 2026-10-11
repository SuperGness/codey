#!/usr/bin/env python3
"""打包已构建的 Codey 原生插件；不构建、不加载、不执行动态库。"""
import argparse
import hashlib
import json
import ipaddress
import pathlib
import platform
import re
import sys
from typing import Union, cast
import zipfile
from urllib.parse import urlsplit, urlunsplit


def valid_id(value):
    # Keep this rule aligned with codey_plugins::package::valid_id.
    return (len(value) <= 96 and re.fullmatch(r"[a-z][a-z0-9._-]*", value) is not None
            and ".." not in value and not value.endswith("."))


def allowed_header_name(value, response=False):
    # Lifecycle response metadata is readable, while credentials stay private.
    if not value.isascii():
        return False
    name = value.lower()
    if response and name in {"content-type", "content-length", "content-encoding", "retry-after"}:
        return True
    return (len(name) <= 128 and re.fullmatch(r"[a-z0-9!#$%&'*+.^_`|~\-]+", name) is not None
            and name not in {
                "authorization", "proxy-authorization", "cookie", "set-cookie", "host",
                "connection", "keep-alive", "proxy-connection", "te", "trailer",
                "transfer-encoding", "upgrade", "content-length", "content-type",
                "content-encoding", "accept-encoding", "chatgpt-account-id",
                "openai-organization", "openai-project", "api-key", "x-api-key",
            }
            and not name.startswith(("x-codey-", "sec-"))
            and not any(part in name for part in ("token", "credential", "authorization")))


reconfigure = getattr(sys.stderr, "reconfigure", None)
if callable(reconfigure):
    reconfigure(encoding="utf-8")

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--library", type=pathlib.Path, required=True)
parser.add_argument("--config", type=pathlib.Path, help="UTF-8 JSON 对象配置模板（最多 1 MiB，省略时使用空对象）")
parser.add_argument("--output", type=pathlib.Path, required=True)
parser.add_argument("--id", required=True)
parser.add_argument("--name", required=True)
parser.add_argument("--version", required=True)
parser.add_argument("--platform", choices=["macos", "windows", "linux"], default={"Darwin":"macos", "Windows":"windows", "Linux":"linux"}.get(platform.system()))
parser.add_argument("--arch", default={"arm64":"aarch64", "AMD64":"x86_64"}.get(platform.machine(), platform.machine()))
parser.add_argument("--header", action="append", default=[])
parser.add_argument("--capability", action="append", default=[], choices=["request.lifecycle.v1", "request.lifecycle.auth", "request.lifecycle.api_key", "request.lifecycle.turn_state", "provider.route.v1", "provider.transport.v1", "provider.account.v1", "appserver.call.v1"])
parser.add_argument("--api-key-url", action="append", default=[], help="精确授权的 HTTPS Responses endpoint（回环测试允许 HTTP）")
parser.add_argument("--response-header", action="append", default=[])
parser.add_argument("--lifecycle-failure-policy", choices=["abort", "continue"])
parser.add_argument("--lifecycle-max-wait-ms", type=int)
args = parser.parse_args()
if args.output.suffix != ".codey-plugin":
    parser.error("输出文件必须使用 .codey-plugin 扩展名")
if not valid_id(args.id):
    parser.error("插件 ID 无效：须以小写字母开头，只含小写字母、数字及 ._-，最多 96 字符，不含连续点或末尾点")
capabilities = list(args.capability)
transport = "provider.transport.v1" in capabilities
if (transport and "provider.route.v1" not in capabilities) or (("provider.account.v1" in capabilities) != transport):
    parser.error("插件传输须同时声明线路、传输与账号授权能力")
if len(set(capabilities)) != len(capabilities):
    parser.error("扩展能力不能重复声明")
lifecycle = "request.lifecycle.v1" in capabilities
if not lifecycle and ("request.lifecycle.auth" in capabilities or "request.lifecycle.api_key" in capabilities or "request.lifecycle.turn_state" in capabilities or args.response_header
                      or args.lifecycle_failure_policy is not None or args.lifecycle_max_wait_ms is not None):
    parser.error("生命周期参数需要 --capability request.lifecycle.v1")
api_key = "request.lifecycle.api_key" in capabilities
if api_key != bool(args.api_key_url):
    parser.error("--api-key-url 与 --capability request.lifecycle.api_key 必须同时声明，授权列表不能为空")
if len(args.api_key_url) > 32:
    parser.error("API Key URL 最多声明 32 项")
api_key_urls = []
for value in args.api_key_url:
    try:
        parsed = urlsplit(value)
        host = parsed.hostname
        port = parsed.port
        loopback = host == "localhost"
        try:
            loopback = loopback or (host is not None and ipaddress.ip_address(host).is_loopback)
        except ValueError:
            pass
        if (not host or len(value.encode("utf-8")) > 2048 or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in value)
                or any(c in value for c in ("*", "\\", "?", "#")) or "@" in parsed.netloc
                or not (parsed.scheme == "https" or parsed.scheme == "http" and loopback)
                or parsed.path not in ("/responses", "/responses/compact", "/v1/responses", "/v1/responses/compact")):
            raise ValueError()
        # Python's built-in IDNA codec can map a domain to a different target
        # than the host's URL parser. Require explicit ASCII/Punycode instead.
        if not host.isascii():
            parser.error("API Key URL 的域名必须使用 ASCII 或 Punycode，避免改变授权目标")
        host = host.lower()
        authority = f"[{host}]" if ":" in host else host
        if port is not None and port != (443 if parsed.scheme == "https" else 80):
            authority += f":{port}"
        normalized = urlunsplit((parsed.scheme, authority, parsed.path, "", ""))
    except (ValueError, UnicodeError):
        parser.error("API Key URL 必须是精确的 HTTPS Responses URL（回环测试允许 HTTP）")
    if normalized in api_key_urls:
        parser.error("API Key URL 不能重复")
    api_key_urls.append(normalized)
if args.lifecycle_max_wait_ms is not None and not 1 <= args.lifecycle_max_wait_ms <= 600000:
    parser.error("生命周期等待上限必须是 1–600000 毫秒")
if args.header and not lifecycle:
    parser.error("请求头参数需要 --capability request.lifecycle.v1")
for names, response in ((args.header, False), (args.response_header, True)):
    if len(names) > 32 or len({name.lower() for name in names}) != len(names):
        parser.error("请求头和响应头各最多声明 32 项，名称不能重复")
    if any(not allowed_header_name(name, response) for name in names):
        parser.error("插件声明了禁止使用或无效的请求头或响应头")
library = args.library.read_bytes()
config = b"{}\n"
if args.config is not None:
    if args.config.is_symlink() or not args.config.is_file():
        parser.error("配置模板必须是普通文件，不能是符号链接")
    with args.config.open("rb") as source:
        config = source.read(1024 * 1024 + 1)
if len(config) > 1024 * 1024:
    parser.error("配置模板超过 1 MiB")
try:
    def reject_constant(value):
        raise ValueError(f"无效 JSON 常量: {value}")
    value = json.loads(config.decode("utf-8"), parse_constant=reject_constant)
except (UnicodeDecodeError, ValueError, RecursionError):
    parser.error("配置模板必须是有效的 UTF-8 JSON")
if not isinstance(value, dict):
    parser.error("配置模板必须是 JSON 对象")
pending: list[tuple[Union[dict[str, object], list[object]], str]] = [
    (cast(Union[dict[str, object], list[object]], value), "$")
]
while pending:
    current, path = pending.pop()
    entries = enumerate(current) if isinstance(current, list) else current.items()
    for key, child in entries:
        child_path = f"{path}[{key}]" if isinstance(current, list) else f"{path}[{json.dumps(key, ensure_ascii=False)}]"
        if isinstance(current, dict) and key == "_comments":
            if not isinstance(child, dict):
                parser.error(f"配置注释 {child_path} 必须是对象，且每项说明必须是字符串。")
            for name, description in child.items():
                if not isinstance(description, str):
                    parser.error(f"配置注释 {child_path}[{json.dumps(name, ensure_ascii=False)}] 必须是字符串。")
        elif isinstance(child, (dict, list)):
            pending.append((child, child_path))
entry = "lib/" + args.library.name
manifest = {
    "id": args.id, "name": args.name, "version": args.version,
    "abiVersion": 1, "platform": args.platform, "arch": args.arch,
    "entry": entry, "librarySha256": hashlib.sha256(library).hexdigest(),
    "capabilities": capabilities,
    "headerNames": args.header
}
if lifecycle:
    manifest["responseHeaderNames"] = args.response_header
    if api_key:
        manifest["apiKeyUrls"] = api_key_urls
    if args.lifecycle_failure_policy is not None:
        manifest["lifecycleFailurePolicy"] = args.lifecycle_failure_policy
    if args.lifecycle_max_wait_ms is not None:
        manifest["lifecycleMaxWaitMs"] = args.lifecycle_max_wait_ms
args.output.parent.mkdir(parents=True, exist_ok=True)
with zipfile.ZipFile(args.output, "x", compression=zipfile.ZIP_DEFLATED) as package:
    package.writestr("manifest.json", json.dumps(manifest, ensure_ascii=False, indent=2))
    package.writestr(entry, library)
    package.writestr("config.json", config)
print(args.output)
