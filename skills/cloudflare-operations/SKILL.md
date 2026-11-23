---
name: cloudflare-operations
description: "Operational patterns for the Cloudflare Workers AI provider - engine, endpoints, catalog refresh, troubleshooting."
version: 0.0.1
author: PlayForm
license: CC0-1.0
platforms: [linux, macos, windows]
metadata:
    hermes:
        tags: [cloudflare, workers-ai, provider, hermes-plugin, catalog]
---

# Cloudflare Workers AI - Operations

Provider plugin operations for the PlayForm/Auth-Cloudflare Hermes plugin. The
plugin registers the `cloudflare` provider (aliases: `cloudflare-ai`,
`workers-ai`, `cf`).

## Endpoints (the plugin owns this routing)

| Route                                                                                    | Purpose                              |
| :--------------------------------------------------------------------------------------- | :----------------------------------- |
| `POST /client/v4/accounts/<ACCOUNT_ID>/ai/v1/chat/completions`                           | Inference (OpenAI-compatible)        |
| `GET  /client/v4/accounts/<ACCOUNT_ID>/ai/models/search?format=openrouter&per_page=1000` | Account catalog                      |
| `GET  /client/v4/user/tokens/verify`                                                     | Token health check                   |
| `POST /client/v4/accounts/<ACCOUNT_ID>/ai/run/<model>`                                   | Native REST (not used by the plugin) |

## Environment

```sh
export CLOUDFLARE_ACCOUNT_ID="<account id>" # Workers & Pages → Overview
export CLOUDFLARE_API_TOKEN="<token>"       # Account → Cloudflare Workers AI → Edit
```

The account ID is not a secret; the token is. Never paste a `cfut_...` token
into chat, git, or logs - the repo's pre-commit hook blocks `cfut_`/`cfwt_`
prefixes in staged diffs.

## Catalog behavior

- Discovery is **live and account-aware** via `/ai/models/search`.
- The OpenRouter-format response **omits tool metadata**. The plugin keeps a
  verified capability table - capability `unknown` ≠ capability `unsupported`.
- Safety classifiers (`llama-guard-3-8b`) and non-chat modalities are filtered
  from the primary picker.
- Network failure → curated `FALLBACK_MODELS` (same order as the verified
  coding set).
- **Delisting is additive, never subtractive.** An id already shipped in
  `FALLBACK_MODELS` that a fresh snapshot no longer lists stays in the list,
  moves into `DEPRECATED_MODELS`, and is reported `deprecated` with
  `primary_agent_eligible: false` - it still resolves and still classifies, it
  is simply never offered. `scripts/regenerate-fixtures.py` recomputes both
  lists from the snapshot each run, so a model Cloudflare restores drops back
  out of the deprecated set by itself. Never hand-edit those two consts.

## Troubleshooting

| Symptom                                   | Cause                                               | Fix                                                  |
| :---------------------------------------- | :-------------------------------------------------- | :--------------------------------------------------- |
| `hermes model` shows no Cloudflare models | `CLOUDFLARE_API_TOKEN` unset                        | Export the token; restart gateway                    |
| Catalog returns error 7003                | `CLOUDFLARE_ACCOUNT_ID` unset/empty → malformed URL | Export the account ID                                |
| HTTP 403 on inference                     | Token lacks Cloudflare Workers AI Write             | Re-scope the token                                   |
| HTTP 401                                  | Token expired/revoked                               | Roll the token                                       |
| Model not in picker                       | Non-chat or safety model                            | Filtered by design; use chat models                  |
| New model missing from `model_overrides`  | Sync skipped a real change (`unchanged: true`)      | Fixed in 0.0.10 (deep-copy compare); re-run the sync |
| `.../ai/v1/models` 404 in logs            | Hermes default probe                                | Expected - plugin sets `supports_health_check=False` |

## Testing the provider

```sh
# One-shot through the registered provider
hermes chat -q "Reply with exactly: Cloudflare Workers AI connection confirmed."

# Direct endpoint probe (token from env, never echoed)
curl -sS "https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/ai/v1/chat/completions" \
	-H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
	-H "Content-Type: application/json" \
	-d '{"model":"@cf/deepseek-ai/deepseek-v4.1-flash-1017","messages":[{"role":"user","content":"Say OK"}]}'
```

## Local dev with the installed binary

- Install the debug build to `~/.hermes/bin/auth-cloudflare` (canonical locator
  path, checked before plugin `bin/`): `cp target/debug/auth-cloudflare
~/.hermes/bin/ && chmod +x` - reinstall after every `cargo build` so doctor/
  verify commands carry new features.
- Once the binary exists, `cloudflare_doctor`/`fetch_models` go binary-first:
  the python direct-HTTP fallback is only used when the locator returns None.
  Plugin tests that instrument the fallback (test_catalog.py,
  test_token_redaction.py) MUST mock `plugin.locate_auth_cloudflare_binary`
  → None in setUp, or they break with KeyError on the captured request.
- `_run_binary_json` relays the binary's diagnostic JSON on non-zero exit
  (sets `exit_code`); a missing account id surfaces as `account_id:
configured:false` + `exit_code: 2`, never a bare "exited with code 2".
- Plain `hermes` (default profile) has no account env → binary doctor exits 2
  by contract; use the dev-cloudflare profile wrapper or export
  CLOUDFLARE_ACCOUNT_ID.
- The `hermes model` picker list is 100% plugin-generated per opening
  (binary `catalog get` → policy-ordered); disabling the plugin removes the
  provider and every model row.

## Recommended routing

```text
Default:            @cf/deepseek-ai/deepseek-v4.1-flash-1017
Long context:       @cf/thinkingmachines/inkling-2-small    (1,048,576; text+image; tools unknown)
Experimental:       @cf/zai-org/glm-5.3                     (conformance pending)
Cheap fallback:     @cf/openai/gpt-oss-20b
Cheap short:        @cf/cloudflare/clairic-flash            (65,536; $0.10/M in)
```

Delisted ids kept in the bundled catalog as `deprecated` (resolvable, never
offered): `@cf/moonshotai/kimi-k2.7-code`, `@cf/deepseek-ai/deepseek-v4-pro-0813`,
`@cf/deepseek-ai/deepseek-v4-flash-0731`, `@cf/zai-org/glm-5.3-flash`,
`@cf/zai-org/glm-5.2`, `@cf/moonshotai/kimi-k2.6`, `@cf/qwen/qwq-32b`,
`@cf/qwen/qwen3-30b-a3b-fp8`, `@cf/nvidia/nemotron-3-120b-a12b`,
`@cf/ibm-granite/granite-4.0-h-micro`, and the small llama-3.x / r1-distill
models. The list is regenerated - read `DEPRECATED_MODELS` in
`crates/auth-cloudflare/src/catalog.rs` for the current set.
