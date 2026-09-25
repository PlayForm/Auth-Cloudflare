# Changelog

## 0.0.7

### Change

- Credential env vars conflated to exactly two: `CLOUDFLARE_ACCOUNT_ID`
  and `CLOUDFLARE_API_TOKEN`. The `AUTH_CLOUDFLARE_*` aliases and the
  legacy `HERMES_CUSTOM_API_CLOUDFLARE_COM_API_KEY` key are removed from
  the Rust core (config precedence, CLI usage, doctor output), the plugin
  (constants, `plugin.yaml` env_vars, error messages), docs and CI
  (fixtures job). `plugin.yaml` env_vars now mirrors the profile
  contract; the account-id pool-seeding guard is unchanged.
- Adopted the Aphrodite parent-side hook set (post-commit / post-merge /
  post-checkout / pre-push auto-bump parent gitlinks; `.gitattributes`
  pins LF for hook files) - the submodule's gitlink can no longer drift.
- Bumped package version from 0.0.6 to 0.0.7 (crates `auth-cloudflare`
  and `auth-hermes-cloudflare` incl. the path-dep pin, package.json,
  plugin.yaml, BINARY_VERSION, README badges, version fixture).

## 0.0.6

### Feature

- New `hermes cloudflare auth` command: registers the configured API token
  into Hermes' auth store (the credential pool, same mechanism as
  `hermes auth add`) under both provider keys, so delegated subagents -
  which are spawned with a fresh environment - resolve the key from disk
  instead of failing with 401. It prunes the junk account-id-as-key entry
  the pool seeded, clears exhaustion, and persists `.env` when missing.
- `api_token()` / `account_id()` now fall back to `~/.hermes/.env` and the
  credential pool after the process env, matching stock Hermes'
  `get_env_value_prefer_dotenv` + pool resolution.

### Fix

- The account id is no longer declared in the profile's `env_vars`: stock
  `_api_key_env_fields()` treats every non-URL env var as an api-key
  credential, so the account id was seeded into the credential pool and
  tried as a key after the real token got rate-limited - the exact 401
  `Authentication error` agents saw. It is read directly (`account_id()`)
  for the derived base URL; the autocompletion is unchanged.

### Change

- Bumped package version from 0.0.5 to 0.0.6 (crates `auth-cloudflare` and
  `auth-hermes-cloudflare` incl. the path-dep pin, package.json, plugin.yaml,
  BINARY_VERSION, README badges, version fixture).
- Test suite: hermetic credential-pool registration tests
  (`tests/test_auth_registration.py`), env/url disk-fallback isolation.
- CI-only parent-side fixes since 0.0.5 (no crate source changes): ureq
  pinned back to 2.12 (3.x renamed the tls feature), plugin pytest jobs
  provision `HERMES_AGENT_SRC`.

## 0.0.5

### Fix

- Compatibility-check error hints now point at the real command:
  `auth-hermes-cloudflare upgrade` (alias of `install`) instead of the
  non-existent `install --upgrade` option.

### Change

- Bumped package version from 0.0.4 to 0.0.5 (crates `auth-cloudflare` and
  `auth-hermes-cloudflare` incl. the path-dep pin, package.json, plugin.yaml,
  BINARY_VERSION, README badges, version fixture).

## 0.0.4

### Change

- Bumped package version from 0.0.3 to 0.0.4 (crates `auth-cloudflare` and
  `auth-hermes-cloudflare` incl. the path-dep pin, package.json, plugin.yaml,
  BINARY_VERSION, README badges, version fixture).
- CI/release machinery only (no crate or plugin source changes):
    - Build.yml Finalize now checks out submodules (in-tree `SHA256SUMS.txt`
      generation previously failed on the missing child tree) and pushes the
      child commit via a Release-environment fine-grained PAT
      (`PLAYFORM_RELEASE_PAT`), failing loudly when the secret is missing.
    - Check.yml fixture validation recognizes the generated
      `plugin_defaults.json` fixture.

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
