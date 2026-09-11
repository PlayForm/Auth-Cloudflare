# [Auth-Cloudflare] ☁️

> [!NOTE]
>
> Auth Cloudflare Workers AI adds direct, OpenAI-compatible access to
> Cloudflare-hosted Workers AI text-generation models available to your
> Cloudflare account. No `custom_providers` wiring, no bash URL adaptation,
> no stale model lists: install the plugin, export two environment variables,
> and `hermes model` offers the account's Workers AI chat models.
> _One provider. Two env vars. Zero hand-rolled YAML._

[![release](https://img.shields.io/static/v1?label=release&message=v0.0.1&color=blue)](https://github.com/PlayForm/Auth-Cloudflare/releases)
[![plugin](https://img.shields.io/static/v1?label=plugin&message=v0.0.1&color=purple)](https://github.com/PlayForm/Auth-Hermes-Cloudflare/blob/Current/plugin.yaml)
[![hermes](https://img.shields.io/static/v1?label=hermes&message=%E2%89%A50.16.0&color=blue)](https://github.com/NousResearch/hermes-agent)
[![rust](https://img.shields.io/static/v1?label=rust&message=1.88%2B&color=orange)](https://www.rust-lang.org)
[![license](https://img.shields.io/static/v1?label=license&message=CC0-1.0&color=lightgrey)](LICENSE)

---

## Install ⚡

Cloudflare ships as a Hermes model-provider plugin (pure Python) backed by two
Rust crates. No Rust toolchain is required for the common path.

### As a Hermes plugin (recommended)

**`Terminal`**

```sh
git clone https://github.com/PlayForm/Auth-Hermes-Cloudflare.git
ln -s "$(pwd)/Auth-Hermes-Cloudflare" ~/.hermes/plugins/auth-hermes-cloudflare
hermes plugins enable auth-hermes-cloudflare
hermes
```

**`Terminal`**

```sh
export CLOUDFLARE_ACCOUNT_ID="<your account id>"   # Workers & Pages → Overview
export CLOUDFLARE_API_TOKEN="<scoped token>"       # Account → Workers AI → Write
hermes model                                       # pick: Auth Cloudflare Workers AI
```

The account ID is operational metadata, not a secret. The API token **is** a
secret - scope it to **Account → Workers AI → Write** (some dashboard versions
label the same permission **Workers AI → Edit**) and nothing else. Do not
request DNS, Workers Scripts, R2, D1, KV, Pages, Zero Trust, or account
administration permissions.

> [!IMPORTANT]
>
> The picker works pure-Python (no compiled dependencies). The
> `auth-cloudflare` executable backs the full command surface - `hermes
> cloudflare doctor / catalog refresh / model inspect`, catalog caching, and
> the conformance commands (`model verify --suite smoke|tool-loop`, `model
> health`). `download.sh` installs that executable (checksum-verified,
> atomic, fail-closed) to a managed path; without it the picker still works
> through the in-process fallback, but the diagnostics and conformance
> commands are unavailable.

### Binary CLI (optional but recommended)

The Rust core ships as a single executable. Install it any of these ways:

**`Terminal`** - cargo install (needs a Rust toolchain):

```sh
cargo install auth-cloudflare --locked           # engine executable
cargo install auth-hermes-cloudflare --locked    # optional plugin utility CLI
```

`auth-cloudflare` is the engine the plugin talks to; `auth-hermes-cloudflare`
is the optional utility (`install`, `upgrade`, `doctor`, `status`, `link`,
`uninstall`) that wraps the same installer.

**`Terminal`** - via the plugin installer (checksum-verified download from the
GitHub release, no Rust toolchain needed):

```sh
bash plugins/auth-hermes-cloudflare/download.sh
```

The binary is discovered in this order (plugin `locate_auth_cloudflare_binary`):
`AUTH_CLOUDFLARE_BIN` env → `PATH` → `~/.hermes/bin/auth-cloudflare` → plugin
`bin/` → plugin `binaries/`. `cargo install` puts it on `PATH`; `download.sh`
installs to `~/.hermes/bin/auth-cloudflare` by default (override with
`BINARY_DIR`, or set `AUTH_CLOUDFLARE_BIN` for an exact path).

**`Terminal`** - verify the install:

```sh
auth-cloudflare doctor --format json    # redacted status, binary-backed
hermes cloudflare doctor                # same report through the plugin CLI
```

### From source

**`Terminal`**

```sh
git clone https://github.com/PlayForm/Auth-Cloudflare.git
cd Cloudflare
git submodule update --init --recursive
cargo build --release -p auth-cloudflare -p auth-hermes-cloudflare
```

---

## JSON CLI contract 🧾

The `auth-cloudflare` binary emits machine-readable JSON for every command
that honors `--format json`: `version`, `doctor`, `catalog get`, `catalog
list`, and `policy get`. This section pins the stable envelope shape that the
Hermes plugin consumes.

### `catalog get` envelope

| Field                   | Type             | Meaning                                                          |
| :---------------------- | :--------------- | :--------------------------------------------------------------- |
| `schema_version`        | int              | Catalog schema version (`1`)                                     |
| `source`                | string           | `live` \| `cache` \| `fallback` - where the records came from    |
| `fetched_at`            | string           | RFC 3339 timestamp of the snapshot                               |
| `cache_status`          | string           | `fresh` \| `stale` \| `none`                                     |
| `default_model`         | string           | Provider default model id (`@cf/...`)                            |
| `model_count`           | int              | Number of records in the resolved snapshot                       |
| `experimental_included` | bool             | `true` - the fetch runs `hide_experimental=false`                |
| `deprecated_included`   | bool             | `false` - the fetch runs `include_deprecated=false`              |
| `models`                | array of objects | One object per model (fields below)                              |

Each `models[]` object carries:

| Field                    | Type   | Meaning                                                       |
| :----------------------- | :----- | :------------------------------------------------------------ |
| `id`                     | string | Model id (`@cf/...`)                                          |
| `display_name`           | string | Human-readable model name                                     |
| `status`                 | string | Policy status (`recommended`, `available`, `experimental`, …) |
| `primary_agent_eligible` | bool   | Whether the model may serve as the primary agent model        |
| `context_tokens`         | int    | Context window size in tokens                                 |
| `pricing_per_million`    | object | `input`, `cached_input`, `output` per-million USD (nullable)  |
| `capabilities`           | object | `chat`, `tools`, `reasoning` verdicts                         |

`catalog list` returns the same envelope with `models` as an ordered array of
model-id strings (picker order). The other JSON commands are `version`
(name/version handshake), `doctor` (redacted status), and `policy get`
(current model policy).

---

## The Problem 🔥

Hermes discovers provider catalogs at the OpenAI-standard `GET …/models`.
Cloudflare's Workers AI surface does not implement that endpoint - the
account-aware catalog lives at `/ai/models/search` and answers in the
OpenRouter format instead.

So every manual integration ends up hand-rolled: an account ID pasted into a
base URL by hand, a model list maintained as YAML, embedding and
safety-classifier models polluting the picker, and no reliable way to know
which models actually support tool calling. The agent spends its budget
maintaining configuration instead of using the models.

The plugin registers a real `cloudflare` provider that owns that routing: the
account ID is injected from the environment, the catalog is fetched from the
`/ai/models/search` endpoint, non-chat models are filtered out, and a curated
fallback catalog keeps the picker alive when the network is unavailable.

---

## How It Works ⚙️

**`Pipeline`**

```text
 ENV VARS ──────────────► auth-hermes-cloudflare ────────► Hermes model picker
                             │
                             ├─ CLOUDFLARE_ACCOUNT_ID   → derived base URL (…/ai/v1)
                             ├─ CLOUDFLARE_API_TOKEN    → Bearer header only, never logged
                             ├─ catalog discovery      → GET …/ai/models/search (OpenRouter)
                             ├─ chat filter            → @cf/ chat models only, no guards
                             └─ offline fallback       → curated catalog in the profile

    Hermes decides:
    • Online  → live, account-aware catalog
    • Offline → curated fallback, picker still works
```

Four steps:

1. **Register** - `register_provider()` adds `auth-cloudflare-workers-ai` to
   the Hermes provider registry (aliases include `cloudflare`,
   `cloudflare-workers-ai`, `workers-ai`, `cf-workers-ai`, and more).
2. **Resolve** - the account ID from the environment derives every endpoint
   (`base_url`, `models_url`, `verify_url`) - one source of truth, shared by
   the Python plugin and the Rust core.
3. **Discover** - the catalog is fetched from the real search endpoint
   (`format=openrouter&per_page=1000`), not from a nonexistent `/models`.
4. **Filter** - safety classifiers (`llama-guard-3-8b`) and non-chat
   modalities (embedding, image, audio, video) never reach the primary picker.

---

## Architecture 🏗️

**`Layout`**

```text
crates/auth-cloudflare/          ← Core: auth, catalog, cache (cdylib + rlib)
  auth.rs                        ← account/token resolution → endpoint construction
  catalog.rs                     ← ModelRecord, ModelRole, CapabilityState
  cache.rs                       ← account-scoped cache paths

crates/auth-hermes-cloudflare/   ← Hermes integration (cdylib + rlib)
  lib.rs                         ← re-exports core types for tool schemas/hooks

plugins/auth-hermes-cloudflare/  ← Hermes plugin (submodule → Auth-Hermes-Cloudflare)
  __init__.py                    ← register_provider, lazy URLs, fetch_models

profiles/dev-cloudflare/         ← working Hermes profile (provider block)
skills/                          ← cloudflare-* skills
.playform/                        ← plan + development conversation archive
```

| Route                                                 | Purpose                                         |
| :---------------------------------------------------- | :---------------------------------------------- |
| `POST …/accounts/<ACCOUNT_ID>/ai/v1/chat/completions` | OpenAI-compatible inference                     |
| `GET  …/accounts/<ACCOUNT_ID>/ai/models/search`       | Account-aware model catalog (OpenRouter format) |
| `GET  /client/v4/user/tokens/verify`                  | Token health check                              |
| `POST …/accounts/<ACCOUNT_ID>/ai/run/<model>`         | Native REST inference (not used by the plugin)  |

All endpoint/auth logic lives in the Rust core (`crates/auth-cloudflare`);
the Python plugin is a thin in-process provider that mirrors it for the
picker and wizard paths.

> [!NOTE]
>
> `plugins/auth-hermes-cloudflare/` is a separate repo
> ([PlayForm/Auth-Hermes-Cloudflare](https://github.com/PlayForm/Auth-Hermes-Cloudflare)),
> tracked here as a git submodule.

---

## Provider Surface 🔧

| Aspect           | Value                                                                                                                     |
| :--------------- | :------------------------------------------------------------------------------------------------------------------------ |
| Provider name    | `auth-cloudflare-workers-ai`                                                                                              |
| Aliases          | `cloudflare`, `cloudflare-ai`, `auth-cloudflare-workers-ai`, `cloudflare-workers-ai`, `workers-ai`, `cf-workers-ai`, `cf` |
| Display name     | `Auth Cloudflare Workers AI`                                                                                              |
| API mode         | `chat_completions`                                                                                                        |
| Auth type        | `api_key`                                                                                                                 |
| Base URL         | derived from the account ID - `fixed_base_url`, the setup wizard never prompts for an override                            |
| Health check     | disabled (no `/models` endpoint); token verify is used instead                                                            |
| Signup           | [dash.cloudflare.com/profile/api-tokens](https://dash.cloudflare.com/profile/api-tokens)                                  |
| Default model    | `@cf/deepseek-ai/deepseek-v4-flash-0731`                                                                                  |
| Fallback catalog | 22 curated chat models compiled into the profile                                                                          |

---

## Skills 🧠

The repo ships three Hermes skills alongside the plugin:

| Skill                         | Purpose                                                                                        |
| :---------------------------- | :--------------------------------------------------------------------------------------------- |
| `cloudflare-dev-workflow`     | Reverse-PR git workflow - `feat-dev`/`trunk` integration, `Source` remote, no direct pushes    |
| `cloudflare-operations`       | Operational patterns - endpoints, catalog refresh, troubleshooting                             |
| `cloudflare-release-workflow` | Release process - version sync, `Cloudflare/v*` tag naming, `BINARY_VERSION`, download scripts |

---

## Configuration 🎛️

Everything is driven by two environment variables - no recompile, no config
file to keep in sync. The `AUTH_CLOUDFLARE_*` names are the canonical ones;
the `CLOUDFLARE_*` names remain as the legacy Hermes-compatible aliases.

| Variable                                               | Role                                                                    | Secret  |
| :----------------------------------------------------- | :---------------------------------------------------------------------- | :------ |
| `CLOUDFLARE_ACCOUNT_ID` / `AUTH_CLOUDFLARE_ACCOUNT_ID` | account ID (Workers & Pages → Overview)                                 | no      |
| `CLOUDFLARE_API_TOKEN` / `AUTH_CLOUDFLARE_API_TOKEN`   | API token (Account → Workers AI → Write; some dashboards label it Edit) | **yes** |

The token is only ever sent as a `Bearer` header - never logged, never echoed,
never rendered by `Debug`/`Display` (the Rust core redacts it in both).

**`config.yaml`**

```yaml
model:
    default: "@cf/deepseek-ai/deepseek-v4-flash-0731"
    provider: cloudflare
providers:
    cloudflare:
        api_key_env: CLOUDFLARE_API_TOKEN
        base_url: https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/ai/v1
        api_mode: chat_completions
```

> [!TIP]
>
> A ready-made working profile lives at `profiles/dev-cloudflare/`
> (`config.yaml` with the provider block above plus
> `cloudflare-dev-workflow` as the default skill). Copy it and set your own
> env vars.

---

## Models 📊

The default is `@cf/deepseek-ai/deepseek-v4-flash-0731` - DeepSeek V4 Flash:
1,310,720-token context, function calling, reasoning, multimodal; $0.44/M
input and $1.32/M output at Cloudflare's published rates. It is the
development default; GLM-5.3 Flash stays experimental until conformance
thresholds are met.

Discovery is live and account-aware: the catalog is fetched from
`/ai/models/search` in the OpenRouter format and filtered to chat-capable
`@cf/` models. When the network is unavailable the plugin falls back to a
curated catalog compiled into the profile (22 chat models).

> [!WARNING]
>
> The OpenRouter-format catalog response omits tool-calling metadata entirely.
> The plugin maintains a verified capability table instead of inferring
> `unsupported` from absent metadata - capability unknown ≠ capability
> unsupported.

**`Curated fallback catalog (22 chat models)`**

```text
@cf/deepseek-ai/deepseek-v4-flash-0731          @cf/moonshotai/kimi-k2.7-code
@cf/deepseek-ai/deepseek-v4-pro-0813           @cf/openai/gpt-oss-120b
@cf/openai/gpt-oss-20b                         @cf/zai-org/glm-5.3
@cf/qwen/qwen3.8-27b                           @cf/qwen/qwen3-30b-a3b-fp8
@cf/qwen/qwen2.5-coder-32b-instruct            @cf/meta/llama-4-scout-17b-16e-instruct
@cf/meta/llama-3.3-70b-instruct-fp8-fast       @cf/mistralai/mistral-small-3.1-24b-instruct
@cf/nvidia/nemotron-3-120b-a12b                @cf/ibm-granite/granite-4.0-h-micro
@cf/zai-org/glm-4.7-flash                      @cf/moonshotai/kimi-k2.6
@cf/deepseek-ai/deepseek-r1-distill-qwen-32b  @cf/meta/llama-3.1-8b-instruct-fp8
@cf/meta/llama-3.2-1b-instruct                 @cf/meta/llama-3.2-3b-instruct
@cf/meta/llama-3.2-11b-vision-instruct         @cf/qwen/qwq-32b
```

---

## Scope 🎯

Supported:

- Direct Cloudflare-hosted `@cf/...` Workers AI text-generation models.
- OpenAI-compatible Chat Completions.
- Hermes-owned tools: terminal, filesystem, browser, Git, and installed skills.
- Account-aware Workers AI catalog discovery.

Not supported in this release:

- Cloudflare AI Gateway third-party models (partial: Workers AI
  `auth-cloudflare-workers-ai` works end-to-end; the AI Gateway adapter
  `auth-cloudflare-ai-gateway` is parked until explicitly re-opened).
- Anthropic Messages, Gemini-native, or provider-specific API protocols.
- Image, video, embedding, speech, reranking, or safety-only models as
  the primary Hermes agent.
- Cloudflare MCP account operations.
- Automatic model failover.

---

## Troubleshooting ❓

| Symptom                                                      | Cause / fix                                                                                                                                                                 |
| :----------------------------------------------------------- | :-------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `could not verify this endpoint via …/ai/v1/models`          | Expected - Cloudflare has no OpenAI `/models` endpoint. The provider disables health probing (`supports_health_check=False`) and discovers via `/ai/models/search` instead. |
| `Missing environment variable CLOUDFLARE_ACCOUNT_ID`         | The error names the exact variable and prints the fix: export it from Workers & Pages → Overview → Account ID.                                                              |
| `Missing environment variable CLOUDFLARE_API_TOKEN`          | Export a custom token scoped to Account → Workers AI → Write (some dashboards label it Edit).                                                                               |
| Token verify returns 401 / 403                               | Wrong, expired, or under-scoped token - check `GET /client/v4/user/tokens/verify` and re-create the token with Account → Workers AI → Edit.                                 |
| Catalog fetch fails (API error, missing data array, timeout) | Non-fatal by design - the provider falls back to the curated 22-model catalog and the picker keeps working.                                                                 |

---

## Development 🛠️

**`Terminal`**

```sh
cargo test --workspace          # unit tests: endpoint stability, token redaction
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
pnpm FormatCheck               # prettier --check over the repo
```

- CI `Check.yml` runs fmt, clippy, and tests on every push/PR.
- CI `Build.yml` builds release executables for four targets
  (aarch64/x86_64 macOS + Linux) on `Cloudflare/v*` tags and attaches them
  to the release - the exact assets `download.sh` fetches.
- Git flow is a reverse-PR workflow: `feat-dev`/`trunk` branches, `Source`
  remote, no direct pushes - see the `cloudflare-dev-workflow` skill.

---

## Relationship to Hermes Agent 🔗

Hermes intentionally keeps third-party vendor providers out of the core tree -
the maintainers' preferred extension path is a standalone model-provider
plugin. Cloudflare implements exactly that contract:

- `ProviderProfile` subclass + `register_provider()` - the same import
  side-effect pattern every bundled Hermes provider follows.
- `fixed_base_url=True` - the base URL is derived from the account ID, so the
  setup wizard never asks for a manual Base URL override.
- Lazy URL properties - plugin discovery runs before the profile `.env` is
  loaded, so URLs compute from `os.environ` at access time, never at import.

→ [Model-provider plugin developer guide](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/developer-guide/model-provider-plugin.md)

---

## Contributing 🤝

| Want to…          | Start here                                                                                  |
| ----------------- | ------------------------------------------------------------------------------------------- |
| Report a bug      | [Open an issue](https://github.com/PlayForm/Auth-Cloudflare/issues/new?template=bug_report.md)   |
| Suggest a feature | [Start a discussion](https://github.com/PlayForm/Auth-Cloudflare/discussions/new?category=ideas) |
| Submit a PR       | [Fork & open a PR](https://github.com/PlayForm/Auth-Cloudflare/pulls)                            |
| Ask a question    | [Discussions Q&A](https://github.com/PlayForm/Auth-Cloudflare/discussions/new?category=q-a)      |

No contribution is too small.
First-time contributors are especially welcome.

---

## License 📜

Released under [CC0-1.0](LICENSE) - public domain.

---

_Built with ❤️ by PlayForm._

[Auth-Cloudflare]: https://github.com/PlayForm/Auth-Cloudflare
