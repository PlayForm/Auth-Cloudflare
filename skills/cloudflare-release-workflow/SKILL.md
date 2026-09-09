---
name: cloudflare-release-workflow
description: "Release process for the Cloudflare plugin - version sync, tag naming (Cloudflare/v*), BINARY_VERSION, download scripts."
version: 0.0.1
author: PlayForm
license: CC0-1.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [release, cloudflare, plugin, versioning]
---

# Cloudflare plugin - Release workflow

> [!IMPORTANT]
>
> Tag format is `Cloudflare/v<semver>` (e.g. `Cloudflare/v0.1.0`) - the
> `Build.yml` workflow triggers on `Cloudflare/v*` and attaches per-target
> `cloudflare-hermes-<target>.tar.gz` assets. Never use a bare `v*` tag.

## Release checklist

1. **Version sync** - one version across all three:
   - `crates/cloudflare/Cargo.toml`
   - `crates/cloudflare-hermes/Cargo.toml`
   - `package.json` (`@playform/cloudflare`)
   - `plugins/cloudflare/plugin.yaml`
2. **BINARY_VERSION** - update `plugins/cloudflare/BINARY_VERSION` with the
   plain version (`0.1.0`, no `v`, no tag prefix). `download.sh` reads this
   first; the GitHub-API fallback parses `Cloudflare/v<tag>`.
3. **Changelog** - add a `.hermes/release-notes/<version>.md` entry.
4. **Tag + push**:
   ```sh
   git tag Cloudflare/v0.1.0
   git push Source Cloudflare/v0.1.0
   ```
5. **Verify** - the workflow uploads one artifact per target; check the
   release page lists `aarch64-apple-darwin`, `x86_64-apple-darwin`,
   `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.

## Version bumps

- Rust API/behavior change → bump minor, keep the plugin shim in lockstep.
- Plugin-only fix (no crate change) → bump `plugin.yaml` version only; the
  crates may trail.
- dylib ABI is not stable across versions - `download.sh` always fetches the
  exact `BINARY_VERSION`, never "latest" for an existing install.
