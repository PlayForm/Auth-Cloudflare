#!/bin/sh
# regenerate-catalog-docs.sh - regenerate docs/catalog.generated.{yaml,md}
# from the account-scoped catalog cache through the auth-cloudflare binary.
#
# Usage:
#   ./scripts/regenerate-catalog-docs.sh            # from repo root
#   AUTH_CLOUDFLARE_EXPORT_DIR=/tmp/out ./scripts/regenerate-catalog-docs.sh
#
# Requires: auth-cloudflare binary on PATH/AUTH_CLOUDFLARE_BIN, and either
# the real account env (AUTH_CLOUDFLARE_ACCOUNT_ID + token, or the legacy
# CLOUDFLARE_* aliases) with a populated cache, or AUTH_CLOUDFLARE_CACHE_DIR
# pointing at a seeded cache (offline/CI mode).
#
# Never echoes the token; the binary redacts everything it prints.
set -eu

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXPORT_DIR="${AUTH_CLOUDFLARE_EXPORT_DIR:-$REPO_ROOT/docs}"
mkdir -p "$EXPORT_DIR"

BIN="${AUTH_CLOUDFLARE_BIN:-}"
if [ -z "$BIN" ]; then
	BIN="$(command -v auth-cloudflare || true)"
fi
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
	echo "ERROR: auth-cloudflare binary not found (PATH or AUTH_CLOUDFLARE_BIN)" >&2
	exit 1
fi

export AUTH_CLOUDFLARE_EXPORT_DIR="$EXPORT_DIR"
"$BIN" catalog export yaml
"$BIN" catalog export markdown

for f in catalog.generated.yaml catalog.generated.md; do
	if [ ! -s "$EXPORT_DIR/$f" ]; then
		echo "ERROR: $EXPORT_DIR/$f missing or empty" >&2
		exit 1
	fi
	echo "OK $EXPORT_DIR/$f"
done
