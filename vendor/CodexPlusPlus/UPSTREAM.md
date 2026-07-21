# Vendored CodexPlusPlus Components

This directory contains only the CodexPlusPlus crates used by Codey:

- `codex-plus-core`
- `codex-plus-data`

Upstream repository: https://github.com/BigPizzaV3/CodexPlusPlus

Upstream version: `v1.2.36`

Upstream commit: `91a83acd8bdf79a388a106c7c1ea76f9df6bcea9`

Local Codey changes:

- Adds an owned CDP bridge pump with explicit shutdown and per-session
  request tokens, preventing stale bridge listeners after reinjection.
- Uses bounded parallel rollout inspection and metadata-only change records
  to reduce startup latency and peak memory when synchronizing sessions.
- Extends regression coverage for the Codey integrations above.
- Omits two upstream Manager UI source assertions because the Manager app is
  not vendored or used by Codey.

The vendored sources remain licensed under `AGPL-3.0-only`. See `LICENSE`.
