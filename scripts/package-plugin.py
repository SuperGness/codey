#!/usr/bin/env python3
"""打包已构建的 Codey 原生插件；不构建、不加载、不执行动态库。"""
import argparse
import hashlib
import json
import pathlib
import platform
import sys
from typing import Union, cast
import zipfile


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
parser.add_argument("--capability", action="append", default=[], choices=["request.lifecycle.v1", "request.lifecycle.auth", "provider.route.v1", "appserver.call.v1"])
parser.add_argument("--response-header", action="append", default=[])
parser.add_argument("--lifecycle-failure-policy", choices=["abort", "continue"])
parser.add_argument("--lifecycle-max-wait-ms", type=int)
args = parser.parse_args()
if args.output.suffix != ".codey-plugin":
    parser.error("输出文件必须使用 .codey-plugin 扩展名")
capabilities = list(args.capability)
if len(set(capabilities)) != len(capabilities):
    parser.error("扩展能力不能重复声明")
lifecycle = "request.lifecycle.v1" in capabilities
if not lifecycle and ("request.lifecycle.auth" in capabilities or args.response_header
                      or args.lifecycle_failure_policy is not None or args.lifecycle_max_wait_ms is not None):
    parser.error("生命周期参数需要 --capability request.lifecycle.v1")
if args.lifecycle_max_wait_ms is not None and not 1 <= args.lifecycle_max_wait_ms <= 600000:
    parser.error("生命周期等待上限必须是 1–600000 毫秒")
if args.header and not lifecycle:
    parser.error("请求头参数需要 --capability request.lifecycle.v1")
for names in (args.header, args.response_header):
    if len(names) > 32 or len({name.lower() for name in names}) != len(names):
        parser.error("请求头和响应头各最多声明 32 项，名称不能重复")
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
