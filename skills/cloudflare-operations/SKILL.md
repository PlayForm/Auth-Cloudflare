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

Provider plugin operations for the PlayForm/Cloudflare Hermes plugin. The
plugin registers the `cloudflare` provider (aliases: `cloudflare-workers-ai`,
`workers-ai`, `cf`).

## Endpoints (the plugin owns this routing)

| Route | Purpose |
| :--- | :--- |
| `POST /client/v4/accounts/<ACCOUNT_ID>/ai/v1/chat/completions` | Inference (OpenAI-compatible) |
| `GET  /client/v4/accounts/<ACCOUNT_ID>/ai/models/search?format=openrouter&per_page=1000` | Account catalog |
| `GET  /client/v4/user/tokens/verify` | Token health check |
| `POST /client/v4/accounts/<ACCOUNT_ID>/ai/run/<model>` | Native REST (not used by the plugin) |

## Environment

```sh
export CLOUDFLARE_ACCOUNT_ID="<account id>"   # Workers & Pages → Overview
export CLOUDFLARE_API_TOKEN="<token>"         # Account → Workers AI → Edit
```

The account ID is not a secret; the token is. Never paste a `cfut_…` token
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

## Troubleshooting

| Symptom | Cause | Fix |
| :--- | :--- | :--- |
| `hermes model` shows no Cloudflare models | `CLOUDFLARE_API_TOKEN` unset | Export the token; restart gateway |
| Catalog returns error 7003 | `CLOUDFLARE_ACCOUNT_ID` unset/empty → malformed URL | Export the account ID |
| HTTP 403 on inference | Token lacks Workers AI Write | Re-scope the token |
| HTTP 401 | Token expired/revoked | Roll the token |
| Model not in picker | Non-chat or safety model | Filtered by design; use chat models |
| `.../ai/v1/models` 404 in logs | Hermes default probe | Expected - plugin sets `supports_health_check=False` |

## Testing the provider

```sh
# One-shot through the registered provider
hermes chat -q "Reply with exactly: Workers AI connection confirmed."

# Direct endpoint probe (token from env, never echoed)
curl -sS "https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/ai/v1/chat/completions" \
	-H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
	-H "Content-Type: application/json" \
	-d '{"model":"@cf/zai-org/glm-5.3-flash","messages":[{"role":"user","content":"Say OK"}]}'
```

## Recommended routing

```text
Default:            @cf/zai-org/glm-5.3-flash
Code-heavy:         @cf/moonshotai/kimi-k2.7-code
DeepSeek reasoning: @cf/deepseek-ai/deepseek-v4-flash-0731
Premium escalation: @cf/zai-org/glm-5.3
Cheap fallback:     @cf/openai/gpt-oss-20b
```
