#!/usr/bin/env bash
# Run the chaos / disaster-recovery suite (item 7, v0.7.4).
#
# These tests are #[ignore] in the codebase so a normal `cargo test` run
# stays fast — they:
#   - open and reopen RocksDB stores against tempdirs
#   - corrupt SST files on disk
#   - exercise the persist pipeline failure path
#
# Run them on demand before a release cut, or as part of a dedicated
# nightly job. Each test prints what it's doing so the operator can
# see the durability / atomicity guarantee being checked.
#
# Usage:
#   ./scripts/run-chaos-tests.sh [<extra cargo args>]
#
# Examples:
#   ./scripts/run-chaos-tests.sh
#   ./scripts/run-chaos-tests.sh --test-threads=1
#   ./scripts/run-chaos-tests.sh s3_corrupted_sst_fails_loud_or_recovers
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building chaos test binary in release mode (faster S3 corruption test)…"
cargo build --release -p pms-server --tests >/dev/null

echo "==> Running chaos_recovery suite (--ignored)…"
exec cargo test --release \
    -p pms-server \
    --test chaos_recovery \
    -- \
    --ignored \
    --nocapture \
    --test-threads=1 \
    "$@"
