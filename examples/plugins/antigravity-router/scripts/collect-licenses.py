#!/usr/bin/env python3
"""Collect license texts from Cargo's locked runtime/build graph; no credentials."""
import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import tomllib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=pathlib.Path)
parser.add_argument('--target', default='x86_64-pc-windows-gnu')
parser.add_argument('--cargo', default='cargo')
args = parser.parse_args()
example = pathlib.Path(__file__).resolve().parent.parent
repo = example.parents[2]
cache = pathlib.Path(os.environ.get('CARGO_HOME', pathlib.Path.home() / '.cargo')) / 'registry/src'
components, registry = set(), set()
for manifest, lock, package in [
    (example / 'Cargo.toml', repo / 'Cargo.lock', 'codey-plugin-antigravity-router'),
    (example / 'proxy-rust/Cargo.toml', example / 'proxy-rust/Cargo.lock', 'antigravity-proxy'),
]:
    registry.update((p['name'], p['version']) for p in tomllib.loads(lock.read_text(encoding='utf-8'))['package'] if p.get('source', '').startswith('registry+'))
    result = subprocess.run([args.cargo, 'tree', '--manifest-path', str(manifest), '-p', package, '--target', args.target, '--locked', '--offline', '--edges', 'normal,build', '--prefix', 'none'], check=True, capture_output=True, text=True, encoding='utf-8')
    for line in result.stdout.splitlines():
        match = re.match(r'^(\S+) v(\S+)', line)
        if match:
            components.add(match.groups())
args.output.mkdir(parents=True, exist_ok=False)
index = []
for name, version in sorted(components & registry):
    sources = list(cache.glob(f'*/{name}-{version}'))
    if len(sources) != 1:
        raise SystemExit(f'Expected one cached source for {name} {version}')
    source = sources[0]
    notice_source = source
    metadata = tomllib.loads((source / 'Cargo.toml').read_text(encoding='utf-8'))['package']
    files = [p for p in source.iterdir() if p.is_file() and any(s in p.name.lower() for s in ('license', 'copying', 'copyright', 'notice'))]
    for folder in ('LICENSES', 'licenses'):
        if (source / folder).is_dir():
            files.extend(p for p in (source / folder).rglob('*') if p.is_file())
    if metadata.get('license-file'):
        files.append(source / metadata['license-file'])
    provenance = None
    if not files and name == 'winapi-x86_64-pc-windows-gnu' and version == '0.4.0':
        alternatives = list(cache.glob('*/winapi-0.3.9'))
        if len(alternatives) != 1:
            raise SystemExit('Missing winapi license source')
        notice_source = alternatives[0]
        provider = tomllib.loads((notice_source / 'Cargo.toml').read_text(encoding='utf-8'))['package']
        if provider['repository'] != metadata['repository']:
            raise SystemExit('winapi license provenance differs')
        files = list(notice_source.glob('LICENSE*'))
        provenance = 'License texts from winapi 0.3.9 in the same retep998/winapi-rs repository.'
    if not files:
        raise SystemExit(f'No license text for {name} {version}')
    relative = []
    for file in sorted(set(files)):
        if not file.resolve().is_relative_to(notice_source.resolve()):
            raise SystemExit('License path escapes cached crate')
        dest = args.output / f'{name}-{version}' / file.relative_to(notice_source)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(file, dest)
        relative.append(dest.relative_to(args.output).as_posix())
    index.append({'name': name, 'version': version, 'license': metadata.get('license', 'see license-file'), 'files': relative, **({'provenance': provenance} if provenance else {})})
(args.output / 'index.json').write_text(json.dumps({'target': args.target, 'components': index}, indent=2) + '\n', encoding='utf-8')
print(f'Collected {len(index)} locked dependency license notices')
