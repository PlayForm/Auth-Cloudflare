# Changelog

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