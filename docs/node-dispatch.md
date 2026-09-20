# NodeDispatch ingress

NodeDispatch is selected by the public URL family, not by a model-name prefix.
The supported public surface is intentionally small:

| Method | Path | Boundary |
| --- | --- | --- |
| POST | `/nt/v1/chat/completions` | OpenAI Chat Completions only |
| GET | `/nt/v1/models` | Ready node models, returned with raw IDs |
| GET | `/nt/v1/models/{model}` | One ready node model; a colon in the raw ID is valid |

`/v1` remains AccountPool and `/pt/v1` remains account-to-tenant Passthrough.
The URL sets the trusted access mode. A request body, header, query parameter or
model name cannot switch between families, and there is no fallback between them.
Node registration, heartbeat, task polling and completion continue to use the
existing `/node/v1/...` worker protocol.

## Calling a node

Use a platform API key for inference and keep the worker model ID unchanged:

```bash
curl --fail-with-body "$BASE_URL/nt/v1/chat/completions" \
  -H "Authorization: Bearer $PLATFORM_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"gemma3:270m","messages":[{"role":"user","content":"Hello"}]}'
```

The response uses the existing completion-buffered SSE behavior when `stream`
is enabled. It is not native token-by-token worker streaming. Registration
tokens are for `/node/v1/register`; the exchanged session token is for worker
heartbeat/poll/complete calls, not for public inference.

## Pricing and errors

Pricing and cost previews carry an explicit `mode`; omitted mode means
`account_pool`, while NodeDispatch uses `node_dispatch`. The same raw model can
therefore have separate `provideraccount` and `node` prices. Node prices are
stored against the raw model (`gemma3:270m`) and the admin UI does not add a
prefix; `node` is the billing dimension.

Only Chat and the two model-discovery routes are supported for `/nt/v1`.
Responses, Anthropic Messages, images, embeddings and WebSockets are not
aliases for NodeDispatch. Unsupported `/nt/v1` paths return a backend JSON
error instead of being routed to `/v1` or the console SPA. Empty node capacity
and metadata/readiness failures remain distinct errors.

## Migration

The old `node:<model>` form is removed rather than silently aliased. Replace:

```text
POST /v1/chat/completions  model=node:gemma3:270m
```

with:

```text
POST /nt/v1/chat/completions  model=gemma3:270m
```

The `node:` prefix remains valid only where it is an internal queue/module
label; it is not a public API model name. Existing clients must update their
base URL and model value together.
