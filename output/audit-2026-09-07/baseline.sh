#!/bin/zsh
cd /Users/kim/Desktop/codey-f
L=output/audit-2026-09-07/baseline.log
: > $L
t() { local s=$(date +%s); echo "=== $1 (start $(date +%T))" >> $L; shift; "$@" >> $L 2>&1; local rc=$?; echo "=== rc=$rc elapsed=$(( $(date +%s) - s ))s" >> $L; }
t "pnpm check" pnpm run check
t "pnpm test:js" pnpm run test:js
t "vite:build (cold)" pnpm run vite:build
du -sk dist-overlay dist-overlay/inject >> $L
ls -l dist-overlay/assets >> $L 2>&1
t "vite:build (warm)" pnpm run vite:build
t "cargo fmt check" cargo fmt --all -- --check
t "cargo test --workspace" cargo test --workspace
t "cargo clippy" cargo clippy --workspace --all-targets -- -D warnings
t "cargo build --release (incremental/warm?)" cargo build --release
ls -l target/release/codey target/release/codey-fastctx >> $L 2>&1
echo DONE >> $L
