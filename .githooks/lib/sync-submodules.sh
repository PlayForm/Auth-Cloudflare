#!/usr/bin/env bash
# Sync git submodules after checkout/merge (mirrors Aphrodite's prepare hook).
# Re-entrant: safe to run from any hook; ignores missing submodule dirs.

set -euo pipefail
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [[ -f .gitmodules ]]; then
	# `submodule update --init` is idempotent; --recursive covers nested plugins.
	git submodule update --init --recursive 2>/dev/null || true
fi
