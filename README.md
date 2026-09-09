# [Cloudflare] ☁️

> [!NOTE]
>
> Cloudflare AI auth provider for Hermes Agent - a first-class model-provider
> plugin with live, account-aware catalog discovery. No `custom_providers`
> wiring, no bash URL adaptation, no stale model lists: install the plugin,
> export two environment variables, and `hermes model` offers every Cloudflare
> AI chat model your account can invoke.
> _One provider. Two env vars. Zero hand-rolled YAML._

[![release](https://img.shields.io/static/v1?label=release&message=v0.0.1&color=blue)](https://github.com/PlayForm/Cloudflare/releases)
[![plugin](https://img.shields.io/static/v1?label=plugin&message=v0.0.1&color=purple)](https://github.com/PlayForm/Hermes-Cloudflare/blob/Current/plugin.yaml)
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
git clone https://github.com/PlayForm/Hermes-Cloudflare.git
ln -s "$(pwd)/Hermes-Cloudflare" ~/.hermes/plugins/auth-hermes-cloudflare
hermes plugins enable auth-hermes-cloudflare
hermes
```

**`Terminal`**

```sh
export CLOUDFLARE_ACCOUNT_ID="<your account id>"   # Workers & Pages → Overview
export CLOUDFLARE_API_TOKEN="<scoped token>"       # Account → Cloudflare AI → Edit
hermes model                                       # pick: Cloudflare AI
```

The account ID is operational metadata, not a secret. The API token **is** a
secret - scope it to **Account → Cloudflare AI → Edit** and nothing else.

> [!IMPORTANT]
>
> The provider path is pure Python - no compiled dependencies, no binary
> download, works on macOS, Linux, and Windows alike. `download.sh` only
> matters for the optional Rust dylib flow (`binaries/`), which is not
> required to use Cloudflare AI in Hermes.

### From source

**`Terminal`**

```sh
git clone https://github.com/PlayForm/Cloudflare.git
cd Cloudflare
git submodule update --init --recursive
cargo build --release -p auth-cloudflare -p auth-hermes-cloudflare
```

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

1. **Register** - `register_provider()` adds `auth-cloudflare-ai` to the
   Hermes provider registry with aliases `cloudflare`, `cloudflare-ai`,
   `auth-cloudflare-workers-ai`, and more.
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

plugins/auth-hermes-cloudflare/  ← Hermes plugin (submodule → Hermes-Cloudflare)
  __init__.py                    ← register_provider, lazy URLs, fetch_models

profiles/dev-cloudflare/         ← working Hermes profile (provider block)
skills/                          ← cloudflare-* skills
.hermes/                         ← plan + development conversation archive
```

| Route | Purpose |
| :---- | :------ |
| `POST …/accounts/<ACCOUNT_ID>/ai/v1/chat/completions` | OpenAI-compatible inference |
| `GET  …/accounts/<ACCOUNT_ID>/ai/models/search` | Account-aware model catalog (OpenRouter format) |
| `GET  /client/v4/user/tokens/verify` | Token health check |
| `POST …/accounts/<ACCOUNT_ID>/ai/run/<model>` | Native REST inference (not used by the plugin) |

All endpoint/auth logic lives in the Rust core (`crates/auth-cloudflare`);
the Python plugin is a thin in-process provider that mirrors it for the
picker and wizard paths.

> [!NOTE]
>
> `plugins/auth-hermes-cloudflare/` is a separate repo
> ([PlayForm/Hermes-Cloudflare](https://github.com/PlayForm/Hermes-Cloudflare)),
> tracked here as a git submodule.

---

## Provider Surface 🔧

| Aspect | Value |
| :----- | :---- |
| Provider name | `auth-cloudflare-ai` |
| Aliases | `cloudflare`, `cloudflare-ai`, `auth-cloudflare-workers-ai`, `cloudflare-workers-ai`, `workers-ai`, `cf-workers-ai`, `cf` |
| Display name | `Cloudflare AI` |
| API mode | `chat_completions` |
| Auth type | `api_key` |
| Base URL | derived from the account ID - `fixed_base_url`, the setup wizard never prompts for an override |
| Health check | disabled (no `/models` endpoint); token verify is used instead |
| Signup | [dash.cloudflare.com/profile/api-tokens](https://dash.cloudflare.com/profile/api-tokens) |
| Default model | `@cf/deepseek-ai/deepseek-v4-flash-0731` |
| Fallback catalog | 22 curated chat models compiled into the profile |

---

## Configuration 🎛️

Everything is driven by two environment variables - no recompile, no config
file to keep in sync. The `AUTH_CLOUDFLARE_*` names are the canonical ones;
the `CLOUDFLARE_*` names remain as the legacy Hermes-compatible aliases.

| Variable | Role | Secret |
| :------- | :--- | :----- |
| `CLOUDFLARE_ACCOUNT_ID` / `AUTH_CLOUDFLARE_ACCOUNT_ID` | account ID (Workers & Pages → Overview) | no |
| `CLOUDFLARE_API_TOKEN` / `AUTH_CLOUDFLARE_API_TOKEN` | API token (Account → Cloudflare AI → Edit) | **yes** |

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

| Want to…          | Start here                                                                                 |
| ----------------- | ------------------------------------------------------------------------------------------ |
| Report a bug      | [Open an issue](https://github.com/PlayForm/Cloudflare/issues/new?template=bug_report.md)   |
| Suggest a feature | [Start a discussion](https://github.com/PlayForm/Cloudflare/discussions/new?category=ideas) |
| Submit a PR       | [Fork & open a PR](https://github.com/PlayForm/Cloudflare/pulls)                            |
| Ask a question    | [Discussions Q&A](https://github.com/PlayForm/Cloudflare/discussions/new?category=q-a)      |

No contribution is too small.
First-time contributors are especially welcome.

---

## License 📜

Released under [CC0-1.0](LICENSE) - public domain.

---

_Built with ❤️ by PlayForm._

[Cloudflare]: https://github.com/PlayForm/Cloudflare