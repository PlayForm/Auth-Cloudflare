#!/bin/sh
# regenerate-fixtures.sh - regenerate the catalog fixtures and plugin defaults
# from a Workers AI catalog snapshot through scripts/regenerate-fixtures.py.
#
# Usage:
#   ./scripts/regenerate-fixtures.sh                 # live: fresh snapshot via the auth-cloudflare binary
#   ./scripts/regenerate-fixtures.sh --snapshot path # offline: recorded snapshot file
#   AUTH_CLOUDFLARE_SNAPSHOT=path ./scripts/regenerate-fixtures.sh
#
# Requires (live mode only): auth-cloudflare binary on PATH or
# AUTH_CLOUDFLARE_BIN, plus the real account env (AUTH_CLOUDFLARE_ACCOUNT_ID
# and the Cloudflare token; the binary redacts everything it prints).
# Offline mode needs only a recorded snapshot JSON.
#
# Live mode also regenerates docs/catalog.generated.{yaml,md} (the combined
# refresh: catalog + fixtures + defaults + docs). Offline snapshot mode
# regenerates fixtures/defaults only.
#
# Always finishes by running the full auth-cloudflare crate suite and
# reporting pass/fail via the exit code. Never echoes the token.
set -eu

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

SNAPSHOT="${AUTH_CLOUDFLARE_SNAPSHOT:-}"
case "${1:-}" in
"")
	;;
--snapshot)
	if [ $# -lt 2 ] || [ -z "$2" ]; then
		echo "ERROR: --snapshot requires a path argument" >&2
		exit 1
	fi
	SNAPSHOT="$2"
	;;
*)
	echo "ERROR: unknown argument '$1' (usage: [--snapshot <path>])" >&2
	exit 1
	;;
esac

if [ -n "$SNAPSHOT" ]; then
	if [ ! -r "$SNAPSHOT" ]; then
		echo "ERROR: snapshot not readable: $SNAPSHOT" >&2
		exit 1
	fi
	echo "Using recorded snapshot: $SNAPSHOT"
	python3 scripts/regenerate-fixtures.py --snapshot "$SNAPSHOT"
else
	BIN="$(command -v auth-cloudflare || true)"
	if [ -z "$BIN" ]; then
		BIN="${AUTH_CLOUDFLARE_BIN:-}"
	fi
	if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
		echo "ERROR: auth-cloudflare binary not found (PATH or AUTH_CLOUDFLARE_BIN)" >&2
		exit 1
	fi
	echo "Fetching a fresh snapshot through: $BIN"
	python3 scripts/regenerate-fixtures.py --live
fi

# Regenerate the docs catalog exports (docs/catalog.generated.{yaml,md})
# from the account cache - the combined refresh covers catalog + fixtures +
# defaults + docs. Only possible with a live account cache; skipped in
# offline snapshot mode.
if [ -z "$SNAPSHOT" ]; then
	if [ -x "$REPO_ROOT/scripts/regenerate-catalog-docs.sh" ]; then
		AUTH_CLOUDFLARE_BIN="$BIN" "$REPO_ROOT/scripts/regenerate-catalog-docs.sh"
	fi
else
	echo "Offline snapshot mode - docs/ regeneration skipped (needs the account cache)."
fi

echo "Running the full auth-cloudflare crate suite..."
cargo test -p auth-cloudflare
