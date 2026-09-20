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

Native Chat carries the original JSON through an explicitly native-capable
worker. This protocol version accepts non-streaming requests only; `stream:true`
is rejected explicitly, never silently rewritten. Registration
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

## Model lookup and errors

The request URL is the only execution-mode selector. Model IDs are matched
literally against that mode's available models, including colons and a leading
`node:`. The server neither strips the prefix nor returns a migration-specific
error. For example, `node:gemma3:270m` does not refer to `gemma3:270m` unless an
upstream or worker actually declares that exact full name.

An undeclared name follows the normal mode-specific failure path: account-pool
and passthrough Chat return their ordinary model/grant-not-found errors; node
Chat returns its ordinary no-ready-node error. Model-detail endpoints return
normal not-found responses. If the full literal name is declared, discovery,
execution and pricing use that name unchanged in the URL-selected mode.

To invoke the worker model `gemma3:270m`, use
`POST /nt/v1/chat/completions` with `model=gemma3:270m`. Model text never switches
an account request to a node or enables a fallback to another family.
