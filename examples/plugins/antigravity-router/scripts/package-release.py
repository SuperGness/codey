#!/usr/bin/env python3
"""Package Windows binaries with exact native schema and reconstructable source."""
import argparse
import hashlib
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('example', type=pathlib.Path)
parser.add_argument('output', type=pathlib.Path)
parser.add_argument('--cargo', default='cargo')
parser.add_argument('--target', choices=['x86_64-pc-windows-gnu', 'x86_64-pc-windows-msvc'], default='x86_64-pc-windows-gnu')
args = parser.parse_args()
example = args.example.resolve()
repo = example.parents[2]
version = '0.9.0'
if args.output.exists():
    raise SystemExit('Output already exists; choose a new directory')
args.output.mkdir(parents=True)
bundle = args.output / f'codey-antigravity-{version}-windows-x64'
(bundle / 'bin').mkdir(parents=True)
native = args.output / f'antigravity-router-{version}-windows-x64.codey-plugin'
subprocess.run([sys.executable, str(repo / 'scripts/package-plugin.py'), '--library', str(repo / 'target' / args.target / 'release/codey_plugin_antigravity_router.dll'), '--config', str(example / 'config.json'), '--output', str(native), '--id', 'dev.codey.antigravity-router', '--name', 'Antigravity', '--version', version, '--platform', 'windows', '--arch', 'x86_64', '--capability', 'request.lifecycle.v1', '--capability', 'provider.route.v1', '--header', 'x-antigravity-provider', '--lifecycle-failure-policy', 'continue'], check=True)
subprocess.run([sys.executable, str(example / 'scripts/verify-native.py'), str(native)], check=True)
shutil.copy2(native, bundle / native.name)
shutil.copy2(example / 'proxy-rust/target' / args.target / 'release/antigravity-proxy.exe', bundle / 'bin/antigravity-proxy.exe')
for filename in ('README.md', 'INSTALL.md', 'BUILDING.md', 'NOTICE.md', 'LICENSE', 'start-proxy.ps1', 'stop-proxy.ps1'):
    shutil.copy2(example / filename, bundle / filename)
(bundle / 'scripts').mkdir()
for filename in ('runtime.ps1', 'install.ps1', 'verify-bundle.ps1', 'LICENSE'):
    shutil.copy2(example / 'scripts' / filename, bundle / 'scripts' / filename)
shutil.copy2(example / 'proxy-rust/LICENSE', bundle / 'bin/LICENSE')
subprocess.run([sys.executable, str(example / 'scripts/collect-licenses.py'), str(bundle / 'third-party-licenses'), '--target', args.target, '--cargo', args.cargo], check=True)
if args.target.endswith('-gnu'):
    shutil.copytree(example / 'toolchain-runtime-licenses', bundle / 'toolchain-runtime-licenses')
# A minimal workspace uses the actual SDK source and pinned versions, without the host.
with tempfile.TemporaryDirectory(prefix='codey-antigravity-source-') as temp:
    source = pathlib.Path(temp)
    source_example = source / 'examples/plugins/antigravity-router'
    for filename in ('Cargo.toml', 'config.json', 'README.md', 'INSTALL.md', 'BUILDING.md', 'NOTICE.md', 'LICENSE', '.gitignore', 'start-proxy.ps1', 'stop-proxy.ps1'):
        dest = source_example / filename
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(example / filename, dest)
    for folder in ('src', 'tests', 'scripts', 'toolchain-runtime-licenses'):
        shutil.copytree(example / folder, source_example / folder)
    shutil.copytree(example / 'proxy-rust/src', source_example / 'proxy-rust/src')
    for filename in ('Cargo.toml', 'Cargo.lock', 'LICENSE', 'README.md'):
        shutil.copy2(example / 'proxy-rust' / filename, source_example / 'proxy-rust' / filename)
    sdk = source / 'crates/codey-plugin-sdk'
    shutil.copytree(repo / 'crates/codey-plugin-sdk/src', sdk / 'src')
    for filename in ('Cargo.toml', 'README.md', 'REQUEST_LIFECYCLE.md', 'PROVIDER_TRANSPORT.md'):
        shutil.copy2(repo / 'crates/codey-plugin-sdk' / filename, sdk / filename)
    shutil.copy2(repo / 'LICENSE', sdk / 'LICENSE')
    # Keep SDK unit tests; omit host-only test dependencies whose sources are not bundled.
    sdk_manifest = (sdk / 'Cargo.toml').read_text(encoding='utf-8').split('\n[dev-dependencies]', 1)[0]
    (sdk / 'Cargo.toml').write_text(sdk_manifest + '\n[dev-dependencies]\ntempfile = "3"\n', encoding='utf-8')
    workspace = re.sub(r'members = \[.*?\]', 'members = ["crates/codey-plugin-sdk", "examples/plugins/antigravity-router"]', (repo / 'Cargo.toml').read_text(encoding='utf-8'), count=1, flags=re.S)
    (source / 'Cargo.toml').write_text(workspace, encoding='utf-8')
    shutil.copy2(repo / 'Cargo.lock', source / 'Cargo.lock')
    shutil.copy2(repo / 'LICENSE', source / 'LICENSE')
    (source / 'scripts').mkdir()
    shutil.copy2(repo / 'scripts/package-plugin.py', source / 'scripts/package-plugin.py')
    subprocess.run([args.cargo, 'metadata', '--manifest-path', str(source / 'Cargo.toml'), '--offline', '--filter-platform', args.target, '--format-version', '1'], check=True, stdout=subprocess.DEVNULL)
    with zipfile.ZipFile(bundle / 'source.zip', 'x', zipfile.ZIP_DEFLATED, strict_timestamps=False) as archive:
        for path in sorted(source.rglob('*')):
            if path.is_file():
                archive.write(path, path.relative_to(source).as_posix())
def sums(directory):
    return ''.join(f'{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(directory).as_posix()}\n' for path in sorted(directory.rglob('*')) if path.is_file() and path.name != 'SHA256SUMS')
(bundle / 'SHA256SUMS').write_text(sums(bundle), encoding='utf-8')
with zipfile.ZipFile(args.output / f'{bundle.name}.zip', 'x', zipfile.ZIP_DEFLATED, strict_timestamps=False) as archive:
    for path in sorted(bundle.rglob('*')):
        if path.is_file():
            archive.write(path, path.relative_to(args.output).as_posix())
(args.output / 'SHA256SUMS').write_text(sums(args.output), encoding='utf-8')
print(f'Portable bundle: {bundle}')
