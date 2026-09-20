# Native scoped generation — phase 3

> Historical phase baseline. The current implementation also supports [native event streaming](native-streaming.md).

The `/pt/v1/messages`, `/pt/v1/responses`, `/nt/v1/messages` and
`/nt/v1/responses` endpoints now support non-streaming native requests.
Existing account-pool endpoints and passthrough Chat behavior are unchanged.
The URL selects the execution family; model IDs remain literal.

Passthrough grants remain account-to-tenant grants over declared account models.
Protocol and operation capability determine which bound accounts can serve a
request; health is checked only after unique-account resolution. Messages uses
an Anthropic account and Responses an OpenAI Responses-capable account. There
is no protocol conversion, account fallback or automatic inference retry.

Nodes advertise immutable per-model operation profiles. Messages and Responses
are dispatched only to matching profiles and are checked again at atomic claim,
client execution and result submission. node-token sends the original JSON to
its configured local `/v1/messages` or `/v1/responses`, never an arbitrary URL.
All three operations carry complete native response bodies and bounded safe
headers. Caller credentials are never part of node tasks or local requests.

Node model discovery uses bounded Ollama version/model metadata. The initial
Messages/Responses runtime floor is Ollama 0.14.0; per-model tools, vision and
thinking still require metadata support. This is not a blanket claim of complete
Anthropic/OpenAI feature equivalence. Known unsupported local semantics such as
prompt caching, PDFs, forced tool choice and thinking budgets return explicit
errors instead of being discarded. Tool schemas and tool arguments are treated
as data, not recursively mistaken for these protocol controls.

Phase 3 explicitly rejects native `stream:true`, stateful Responses references,
`background:true` and `store:true`. Native event transport and platform-owned
Responses state are subsequent stages; these are not silently downgraded.
Passthrough does not inherit Ollama-only feature restrictions, but its new
Messages/Responses ingress currently has the same non-stream/stateless boundary.
Missing/null optional controls remain distinguishable in forwarded JSON.

Model discovery and the usage guide offer the operation actually supported by
an account or node. `/nt/v1/models?protocol=anthropic&capability=messages` and
`/nt/v1/models?protocol=openai&capability=responses` select capability views;
they cannot change the URL's execution family. The same applies to `/pt`.

Native Messages accepts a platform key through Bearer auth or `x-api-key` on
Messages paths only. The supported node protocol header is
`anthropic-version: 2023-06-01`; unsupported beta headers are rejected, not lost.
API-body limits, account quotas, node limits, cancellation and settlement retain
their authenticated lifecycle. A native HTTP error preserves status and body
without retrying generation; only successfully validated node results are billed.

Validation includes shared JSON fixtures, real-router protocol tests and a
cross-repository server/pull/node-token/local-mock acceptance harness. Tests do
not call a real model or a production database. See `native-node-capabilities.md`
for negotiation, and `native-node-protocol.md` for the phase-1 wire baseline.
