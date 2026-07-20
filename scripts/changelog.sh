#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

exec git -C "$root" cliff \
    --config "$root/cliff.toml" \
    "$@" \
    --output "$root/CHANGELOG.md"
