# Cloudflare - Cloudflare AI auth provider for Hermes Agent

> PlayForm/Cloudflare - a Hermes model-provider plugin that adds **Cloudflare
> Cloudflare AI** as a first-class authentication provider. No manual
> `custom_providers` wiring, no bash URL adaptation: install the plugin, export
> two environment variables, and `hermes model` offers every Cloudflare AI model
> your account can invoke.

## Why

Hermes discovers custom-provider catalogs at the OpenAI-standard `GET …/models`.
Cloudflare's Cloudflare AI surface instead exposes:

| Route | Purpose |
| :--- | :--- |
| `POST /client/v4/accounts/<ACCOUNT_ID>/ai/v1/chat/completions` | OpenAI-compatible inference |
| `GET  /client/v4/accounts/<ACCOUNT_ID>/ai/models/search` | Account-aware model catalog (OpenRouter format) |
| `GET  /client/v4/user/tokens/verify` | Token health check |
| `POST /client/v4/accounts/<ACCOUNT_ID>/ai/run/<model>` | Native REST inference (not used by the plugin) |

This plugin registers a `cloudflare` provider that owns that routing: the
account ID is injected from the environment, the catalog is fetched from the
`/ai/models/search` endpoint, and safety/non-chat models are filtered out of
the primary picker.

## Install

**`Terminal`**

```sh
git clone --recurse-submodules https://github.com/PlayForm/Cloudflare.git
cd Cloudflare
ln -s "$(pwd)/plugins/cloudflare" ~/.hermes/plugins/cloudflare
```

## Configure

**`Terminal`**

```sh
export CLOUDFLARE_ACCOUNT_ID="<your account id>"   # Workers & Pages → Overview
export CLOUDFLARE_API_TOKEN="<scoped token>"       # Account → Cloudflare AI → Edit
hermes gateway restart
hermes model                                       # pick: Cloudflare AI
```

> [!NOTE]
>
> The account ID is operational metadata, not a secret. The API token **is** a
> secret - scope it to **Account → Cloudflare AI → Edit** and nothing else.

## Default model

`@cf/zai-org/glm-5.3-flash` - 1,310,720-token context, function calling,
reasoning, multimodal; $0.15/M input and $0.50/M output.

## Models

Discovery is live and account-aware (27 Cloudflare-hosted chat models at the
time of writing). When the network is unavailable the plugin falls back to a
curated catalog compiled into the profile. Safety classifiers
(`llama-guard-3-8b`) and non-chat modalities (embedding, image, audio, video)
are always excluded from the picker.

> [!WARNING]
>
> The OpenRouter-format catalog response omits tool-calling metadata entirely.
> The plugin maintains a verified capability table instead of inferring
> `unsupported` from absent metadata - capability unknown ≠ capability
> unsupported.

## Layout

```
Cloudflare/
├── crates/cloudflare/         core crate: auth, catalog, cache (Rust)
├── crates/cloudflare-hermes/  Hermes integration crate (Rust)
├── plugins/cloudflare/        the Hermes plugin (submodule → Hermes-Cloudflare)
├── profiles/                  Hermes profiles (dev-cloudflare, …)
├── skills/                    cloudflare-* skills
└── .hermes/                   plan + development conversation archive
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[CC0-1.0](LICENSE) - see the [license file](LICENSE).
