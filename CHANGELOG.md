# Changelog

## 0.0.3

### Change

- Bumped package version from 0.0.2 to 0.0.3 (crates `auth-cloudflare` and
  `auth-hermes-cloudflare` incl. the path-dep pin, package.json, plugin.yaml,
  BINARY_VERSION, README badges, version fixture).
- Catalog refresh + fixture regeneration tooling:
    - `scripts/regenerate-fixtures.{py,sh}` regenerate the six model fixtures,
      the `catalog.rs` status lists (FALLBACK_MODELS 22 → 26, picking up
      glm-5.2, gemma-4-26b-a4b-it, gemma-sea-lion-v4-27b-it, glm-5.3-flash),
      the plugin fixture, `plugin_defaults.json`, and
      `docs/catalog.generated.{yaml,md}` from a live Workers AI snapshot
      (offline `--snapshot` mode; idempotent; curated fields preserved;
      runs `cargo fmt` + prettier after generation).
    - The catalog docs exporter now emits prettier-clean 2-space YAML.
- Binary shipping trust model (ported from Aphrodite):
    - Build.yml Finalize generates the in-tree
      `plugins/auth-hermes-cloudflare/SHA256SUMS.txt` (BINARY_VERSION header +
      four per-target blocks) and commits it inside the child submodule.
    - Publish.yml: new `Bump-Plugin-Gitlink` job advances the parent gitlink
      to the child tip carrying the fresh checksums.
    - `download.sh` validates release assets against the in-tree
      `SHA256SUMS.txt` first, the per-target release asset second, and warns
      (does not fail) when neither exists; hard-fails on checksum mismatch.
- Plugin (`plugins/auth-hermes-cloudflare` submodule, e760a5a → 80fe1e4):
    - Reasoning-effort vocabulary (`low|medium|high`) + clamping.
    - Lowercase `cloudflare` provider naming consistency.
    - `models sync` + vision model support.
    - Deep Hermes integration (middleware, hooks, error classification).
    - Model defaults now load from generated `fixtures/plugin_defaults.json`
      (defensive fallback; offline-safe; binary-first catalog unchanged).
    - Fixture + catalog refresh; in-tree checksum trust.
- Tooling: `Update.sh` action-version updater; formatting infra
  (`Maintain/Format.sh`, `.prettierignore`, prettier-plugin-sh,
  prettier 3.9.8).
- Chore: removed `.githooks` (recursive submodule self-gitlink commits);
  scrubbed internal references; README/docs cleanup.

## 0.0.2

### Change

- Bumped package version from 0.0.1 to 0.0.2 (crates `auth-cloudflare` and
  `auth-hermes-cloudflare`, package.json, plugin.yaml, BINARY_VERSION,
  README badges, version fixture).
- Plugin (`plugins/auth-hermes-cloudflare` submodule, 4fe36c2 -> e760a5a):
    - Removed the `fixed_base_url` constructor kwarg that crashed plugin
      discovery on stock hermes-agent cores (`TypeError: unexpected keyword
argument`); the legacy flag is now set post-construction only when the
      base class declares it.
    - Declared `CLOUDFLARE_BASE_URL` in the profile `env_vars` (stock
      `*_BASE_URL` -> `ProviderConfig.base_url_env_var` mapping); the setup
      wizard pre-fills the Base URL prompt from the account-derived URL, and
      `inference_base_url()` prefers the env override when set (trailing
      slash normalized).
    - `hermes cloudflare setup` writes the derived URL to `~/.hermes/.env`
      under `CLOUDFLARE_BASE_URL` (merged from the parallel implementation).
    - Fixed README title (`Auth-Auth-Hermes-Cloudflare` -> `Auth-Hermes-Cloudflare`).
- Githooks:
    - `pre-commit` refuses commits on a detached HEAD (branch guard) so
      submodule auto-commits can never dangle.
    - New `post-commit` + `post-checkout` + `lib/bump-submodule-gitlink.sh`:
      the parent's gitlink is auto-bumped to the latest submodule commit, and
      a detached submodule HEAD is re-attached to `Current` automatically.
    - `post-merge` made executable (submodule re-sync).
- Release process: `Publish.yml` converted to crates.io trusted publishing
  (OIDC via `crates-io-auth-action`), `Cloudflare/v0.0.1` -> `v0.0.2` tag
  examples in `skills/cloudflare-release-workflow/SKILL.md`.

## 0.0.1

### Change

- Initial release: `auth-cloudflare` core + JSON CLI, `auth-hermes-cloudflare`
  Hermes adapter crate, `auth-hermes-cloudflare` model-provider plugin, and
  the GitHub release `Cloudflare/v0.0.1` (12 assets, per-target checksums).
