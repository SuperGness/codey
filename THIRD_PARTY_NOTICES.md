# Third-Party Notices

Codey is licensed under the GNU Affero General Public License version 3 only.
The complete project license is in `LICENSE`.

Codey contains or depends on third-party software and data. Those components
remain under their respective licenses and copyright notices. This inventory is
based on `Cargo.lock`, `pnpm-lock.yaml`, and the package metadata resolved by
those lockfiles. Upstream package metadata and license files are authoritative.

## Vendored source

`vendor/CodeyRuntime` is distributed as source under `AGPL-3.0-only`. Its
license text is also preserved at `vendor/CodeyRuntime/LICENSE`.

## Windows overlay recovery

The Windows overlay recovery helper adapts MIT-licensed code from
[BaoZiFly-233/codex-tweaks-pet-drag-recovery](https://github.com/BaoZiFly-233/codex-tweaks-pet-drag-recovery)
at commit `3d16eebe50eebee62e37bee97301e28411f6bbd8`, using the native layered-window
reset technique also documented by
[FoegiUpdate/ChatGPT-Overlay-Fix](https://github.com/FoegiUpdate/ChatGPT-Overlay-Fix)
at commit `633323f537cd862d8c83d619d5f755a56966765b`.
Copyright (c) 2026 ct-pet-drag-recovery contributors; Copyright (c) 2026 FoegiUpdate.
The complete MIT notices are preserved in `licenses/PetDragRecovery/LICENSE`
and `licenses/ChatGPTOverlayFix/LICENSE`. Codey's adaptation provides one-shot
recovery through its Rust backend, without the upstream Node host or watcher.

## Bundled context tool

Codey's optional built-in context tool sidecar includes FastCtx.

| Component | Locked version | License | Source or copyright |
| --- | --- | --- | --- |
| `fastctx` | 0.2.6 | Apache-2.0 | [yc-duan/fastctx](https://github.com/yc-duan/fastctx); Copyright (c) 2026 yc-duan <dy2958830371@gmail.com> |

FastCtx's Apache License 2.0 text and NOTICE file are preserved in
`licenses/FastCtx/`.

The following attribution is reproduced verbatim from FastCtx's NOTICE:

    This product includes FastCtx
    (https://github.com/yc-duan/fastctx), Copyright (c) 2026 yc-duan,
    used under the Apache License 2.0.

    FastCtx is redistributed and/or modified here by the maintainer of
    this distribution. Any such change is that maintainer's own work
    and their sole responsibility. It is not endorsed by, not
    supported by, and not attributable to the author of FastCtx, who
    accepts no liability of any kind arising from this distribution or
    from anything built on top of it.

## JavaScript and TypeScript packages

The primary frontend dependencies are:

| Package | Version | License | Copyright or project |
| --- | --- | --- | --- |
| `antd` | 6.6.2 | MIT | Ant Design contributors |
| `@ant-design/cssinjs` | 2.1.2 | MIT | Ant Design contributors |
| `@tabler/icons-react` | 3.45.0 | MIT | Paweł Kuna and Tabler contributors |
| `tailwindcss`, `@tailwindcss/vite` | 4.3.0 | MIT | Tailwind Labs, Inc. |
| `@vitejs/plugin-react` | 4.3.4 | MIT | Vite and Babel contributors |
| `react`, `react-dom` | 19.2.7 | MIT | Meta Platforms, Inc. and affiliates |
| `typescript` | 5.8.2 | Apache-2.0 | Microsoft Corporation |
| `vite` | 6.4.3 | MIT | Evan You and Vite contributors |

The locked dependency graph also contains the following packages whose
licenses are not MIT:

| Package | Version | License | Copyright or project |
| --- | --- | --- | --- |
| `baseline-browser-mapping` | 2.10.43 | Apache-2.0 | Web Platform DX Community Group contributors |
| `detect-libc` | 2.1.2 | Apache-2.0 | Lovell Fuller and contributors |
| `typescript` | 5.8.2 | Apache-2.0 | Microsoft Corporation |
| `electron-to-chromium` | 1.5.393 | ISC | Kilian Valkhof and contributors |
| `lru-cache` | 5.1.1 | ISC | Isaac Z. Schlueter and contributors |
| `picocolors` | 1.1.1 | ISC | Alexey Raspopov |
| `semver` | 6.3.1 | ISC | GitHub, Inc. and contributors |
| `yallist` | 3.1.1 | ISC | Isaac Z. Schlueter and contributors |
| `caniuse-lite` | 1.0.30001806 | CC-BY-4.0 | Ben Briggs; Can I Use data by Alexis Deveria and contributors |
| `lightningcss` | 1.32.0 | MPL-2.0 | Parcel and Lightning CSS contributors |
| `lightningcss-*` platform packages | 1.32.0 | MPL-2.0 | Parcel and Lightning CSS contributors |
| `source-map` | 0.7.0 | BSD-3-Clause | Nick Fitzgerald and Mozilla contributors |
| `source-map-js` | 1.2.1 | BSD-3-Clause | Valentin Semirulnik and contributors |
| `tslib` | 2.8.1 | 0BSD | Microsoft Corporation |

All other JavaScript packages in `pnpm-lock.yaml` declare the MIT license.
The dependency graph includes build-time packages as well as code that can be
included in the distributed frontend bundle.

The `caniuse-lite` data is attributed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). No endorsement by
its authors is implied. Source and change history are available from the
[caniuse-lite project](https://github.com/browserslist/caniuse-lite).

MPL-covered Lightning CSS source and modifications, if any, remain available
from the [Lightning CSS project](https://github.com/parcel-bundler/lightningcss)
and the corresponding locked package source.

## Rust crates and embedded data

The Rust dependency graph is predominantly licensed under MIT,
Apache-2.0, or a choice of those licenses. The following components require
additional notice because they use another license, include separately
licensed data, or require more than one license:

| Component | Locked version(s) | License | Source or copyright |
| --- | --- | --- | --- |
| `option-ext` | 0.2.0 | MPL-2.0 | [option-ext](https://github.com/soc/option-ext) contributors |
| ICU4X crates and data (`icu_*`, `litemap`, `potential_utf`, `tinystr`, `writeable`, `yoke*`, `zerofrom*`, `zerotrie`, `zerovec*`) | 2.2.x / locked transitive versions | Unicode-3.0 | Unicode, Inc. and ICU4X contributors |
| `webpki-roots` | 0.26.11, 1.0.9 | CDLA-Permissive-2.0 | Mozilla root certificate data and rustls contributors |
| `ring` | 0.17.14 | Apache-2.0 AND ISC | Brian Smith and BoringSSL contributors |
| `rustls-webpki` | 0.103.13 | ISC | webpki and rustls contributors |
| `untrusted` | 0.9.0 | ISC | Brian Smith |
| `subtle` | 2.6.1 | BSD-3-Clause | dalek cryptography contributors |
| `encoding_rs` | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause | Henri Sivonen and encoding_rs contributors |
| `unicode-ident` | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 | Unicode, Inc. and unicode-ident contributors |
| `foldhash` | 0.2.0 | Zlib | Orson Peters and contributors |
| bundled SQLite through `libsqlite3-sys` | 3.x amalgamation selected by `rusqlite` 0.32.1 | Public domain (SQLite); wrapper under MIT | SQLite authors and rusqlite contributors |

Where a Cargo package declares alternatives with `OR`, Codey relies on a
permissive MIT or Apache-2.0 option when one is offered. Expressions containing
`AND` require all stated licenses. Exact package names, versions, sources, and
license expressions can be reproduced with:

```text
cargo metadata --format-version 1 --locked
```

## License references

- [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
- [BSD 3-Clause License](https://opensource.org/license/bsd-3-clause)
- [Creative Commons Attribution 4.0](https://creativecommons.org/licenses/by/4.0/)
- [Community Data License Agreement Permissive 2.0](https://cdla.dev/permissive-2-0/)
- [ISC License](https://opensource.org/license/isc-license-txt)
- [MIT License](https://opensource.org/license/mit)
- [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/)
- [Unicode License v3](https://www.unicode.org/license.txt)
- [Zero-Clause BSD License](https://opensource.org/license/0bsd)
- [zlib License](https://opensource.org/license/zlib)

Names and trademarks belong to their respective owners. Their inclusion does
not imply affiliation with or endorsement of Codey.
