# Account-to-tenant passthrough bindings

A passthrough binding is an explicit grant from one upstream account to one
platform tenant. It covers **all models declared on the account**, including
models added later. It does not copy model lists, credentials or endpoints.
There is no model selector, per-model activation or binding enabled switch.
NodeDispatch is a separate execution family at `/nt/v1`; it does not use these
account grants or accept a model-name routing prefix.

Manage bindings under **Upstream Channels → Passthrough Bindings**:
`/admin/upstreams/passthrough`. The sibling tabs are Account Management and
Node Gateway. The binding editor uses the same modal and form styles as
“Add LLM Channel”, with account and tenant dropdowns and two checkboxes.

## Scope and pool participation

Both flags default to false in the database, write API and editor:

| `is_global` | `pool_enabled` | Who may use `/pt`? | Who may use this account through ordinary APIs? |
| --- | --- | --- | --- |
| false | false | The selected active tenant | Nobody |
| false | true | The selected active tenant | The selected active tenant |
| true | false | All active authorized platform tenants | Nobody |
| true | true | All active authorized platform tenants | All active authorized platform tenants |

Global never means anonymous access. A tenant must still be selected for a
global grant; that grant-owning tenant and the account-owning tenant must be
active. The account's master enabled switch and operational health remain
applicable. The account owner does not get a special bypass of a private grant.

An account may have multiple tenant grants, with at most one global grant.
Grants are additive: narrowing or deleting one row does not revoke another
row's effective global/tenant access. Account and tenant choices must therefore
be considered together when reviewing exposure.

Once an account has any passthrough binding, these grants are authoritative;
legacy account visibility does not independently authorize ordinary use.
Ordinary access includes Chat, eligible Responses operations and existing
resource-affinity continuations. A stored resource ID does not bypass a
subsequently restricted grant. NodeDispatch does not use account credentials.

Creating a binding atomically suppresses the account's legacy pool flag.
Deleting the last binding does not automatically restore it. To reopen an
unbound account, an administrator must explicitly opt it into the pool in
Account Management. While bindings exist, pool participation is changed on
those bindings, not by overriding the account editor's pool checkbox.

Ordinary unbound accounts retain normal account-management defaults and
visibility rules. False/false is the default for **passthrough bindings**.
All paths using one account share its RPM, TPM, concurrency and billing identity.

## API contract

Management requires an authorized system-administrator JWT. Generated platform
API keys never acquire management permissions merely because their owner is an
administrator. Account and tenant selectors return bounded, searchable pages;
account choices do not expose endpoints, credentials or key previews.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/admin/passthrough-bindings` | Paginated list, optional tenant and search filter |
| POST | `/api/v1/admin/passthrough-bindings` | Create an account-to-tenant grant |
| GET | `/api/v1/admin/passthrough-bindings/{id}` | Read current grant and account model summary |
| PUT | `/api/v1/admin/passthrough-bindings/{id}` | Update with `expected_revision` |
| DELETE | `/api/v1/admin/passthrough-bindings/{id}?expected_revision=N` | Revoke the specified revision |
| GET | `/api/v1/admin/passthrough-bindings/options` | Search/paginate eligible account choices |
| POST | `/api/v1/admin/passthrough-bindings/{id}/probe` | Optional single-model diagnostic |

Creation body:

```json
{
  "account_id": "ACCOUNT_UUID",
  "tenant_id": "TENANT_UUID",
  "is_global": false,
  "pool_enabled": false
}
```

Update accepts the same four fields optionally, plus required positive
`expected_revision`. Old `model`, `enabled` and `api_capability` configuration
fields are rejected, not silently ignored. Models in read responses are derived
from the account. Duplicate account/tenant grants and stale revisions return
409. The old model-binding administration API is not retained.

## Calling the account

Use a normal authenticated platform key belonging to an authorized tenant:

```bash
curl --fail-with-body "$BASE_URL/pt/v1/chat/completions" \
  -H "Authorization: Bearer $PLATFORM_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"example-model","messages":[{"role":"user","content":"Hello"}]}'

curl --fail-with-body "$BASE_URL/pt/v1/models" \
  -H "Authorization: Bearer $PLATFORM_KEY"
```

The requested model is matched against accessible bound accounts, not a
per-model binding table. Distinct accessible accounts declaring the same model and requested protocol capability
are ambiguous. Supported management writes reject overlapping namespaces;
runtime also returns `409 passthrough_binding_ambiguous` before health-based
filtering, protecting against out-of-band account changes. It never chooses
one by priority or silently uses the healthier account. Multiple grants to the
same account are deduplicated. An ambiguous model is omitted from user discovery.

Passthrough supports native OpenAI Chat Completions, nonstreaming Anthropic
Messages, and nonstreaming stateless OpenAI Responses. Authorization still grants
all account models; the selected account must also declare the requested protocol
and API capability. Distinct operations can have separate account namespaces.
Messages uses `/pt/v1/messages`; Responses uses `/pt/v1/responses` with `store=false`.
Stored response resources, background jobs and native node streaming are added in
separate stages; unsupported stateful fields are rejected, never deleted silently.
Embeddings and image-generation endpoints are not implied by these grants.

One incoming passthrough request makes at most one outbound generation attempt:
no same-account retry, no account fallback and no compatibility retry.
Unknown native JSON fields, caller stream choices and `stream_options` are
preserved. The `/pt` prefix selects the platform route; it is not appended to
the upstream endpoint. Client retries remain distinct requests, not exactly-once.

## Health, diagnostics and configuration changes

Saving a binding does not call an upstream. Missing or expired model-health
observations do not require individual model activation. The account must be
enabled, usable and out of cooldown; a known unhealthy/degraded observation
for the current connection/model configuration blocks that model until recovery.
A failure does not become healthy merely by waiting for its observation to expire.

The optional diagnostic accepts `{ "model": "example-model", "timeout_ms": 5000 }`.
With no model specified, it tests one declared model. This is a paid-capable
upstream operation and is confirmed explicitly in the UI. It does not enable,
disable or broaden any grant, and success for one model is not proof that all
account models are healthy. No automatic paid probing is installed.

Runtime observations capture the account configuration version and health
generation before external I/O. Conditional writes prevent delayed old successes
from overwriting newer failures. `upstream_config_version` changes when endpoint,
credentials or model/capability configuration changes; ordinary labels, priority
and pool settings do not masquerade as a new connection.

After queue admission, execution rechecks the authoritative writer snapshot:
grant scope, revision, tenant/account state, model, health and connection identity.
This check is the authorization point. Subsequent edits do not retroactively
retarget an already admitted request. No database lock spans HTTP or SSE.

## Errors and deployment

Missing or unauthorized model grants return 404; ambiguous accessible accounts
return 409; unavailable accounts, changed queued targets and metadata/capacity
failures use the appropriate 503 response. Local errors have stable
`passthrough_binding_*` codes. Definite upstream errors retain their safe status;
after streaming starts, failure terminates the stream rather than switching
accounts or replacing the already-sent HTTP status.

The repository currently supports fresh database initialization only. Schema is
in `crates/keycompute-db/migrations/001_init.sql`, including grants and independent
account-model health. This refactor does not implement a retained-data upgrade,
reset an existing database or deploy the running production services.

Regression coverage is in `crates/integration-tests/tests/e2e_passthrough_binding.rs`.
It uses isolated PostgreSQL/Redis, the actual server router and loopback upstreams,
including default isolation, four flag combinations, private cross-tenant grants,
existing-resource revocation, dynamic models, ambiguity, shared limits, cancellation
and native body preservation. Never run destructive test fixtures on production.
