# Tenant model bindings

Model binding is an administrative routing constraint for the native Chat
wire format. `POST /pt/v1/chat/completions` authenticates with the same API
keys as `/v1/chat/completions`, then resolves the exact tuple
`(authenticated tenant, chat_completions, requested model)` to one account.
It does not sort the ordinary account pool, retry another account, or use a
mock fallback. The account remains an ordinary `provider_account` for
credentials and pricing; `model_binding` is only the request/attempt route
classification. Internally, `ExecutionTarget::UpstreamAccount` carries
`AccountSelection::ModelBinding { binding_id, binding_revision }`; ordinary
pool selection and Responses resource affinity remain separate provenance
values. `NodeDispatch` is the independent node task path. Existing serialized
`ProviderAccount` and `Node` tags are retained.

## Management

System administrators use the existing JWT admin session and its
`ManageProviders` permission (API keys never gain admin rights). The current
repository's admin middleware is system-admin scoped; tenant-admin delegation
is intentionally not implied by this feature:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/v1/admin/model-bindings` | Bounded list (`page`, `page_size`, optional `tenant_id`, `model`, `enabled`) |
| `POST` | `/api/v1/admin/model-bindings` | Create a `chat_completions` binding |
| `PUT` | `/api/v1/admin/model-bindings/{id}` | Change account/enabled state with `expected_revision` |
| `DELETE` | `/api/v1/admin/model-bindings/{id}?expected_revision=N` | Remove only the current binding revision |
| `POST` | `/api/v1/admin/model-bindings/{id}/probe` | Probe the specified model on the bound account |

Creation validates tenant/account visibility, account owner lifecycle,
`openai` protocol, capability and model support. `tenant_id`, account IDs and
models are opaque identifiers; clients must not submit an endpoint or key.
The unique `(tenant, capability, model)` key covers enabled and disabled
rows. Updates are optimistic: a stale `expected_revision` returns a conflict
and must be reread before retrying.

The probe body always includes the exact model, for example:

```json
{
  "model": "gpt-4o-mini",
  "api_capability": "chat_completions",
  "timeout_ms": 2000
}
```

Probe concurrency is bounded to eight operations per server process; execution
timeout defaults to 2 seconds and is clamped to 100 milliseconds–10 seconds.
Model-health validity is 300 seconds, using the database writer clock. A
completed successful live request refreshes tracked model health only if its
pre-dispatch configuration and health-generation fence is still current.
Idle bindings require another explicit probe after expiry; this feature does
not install an automatic probing scheduler.

A successful probe records model-specific health; an unknown, stale, degraded or unhealthy
record fails closed for bound traffic. Account-level disablement, cooldown,
credential/endpoint changes and capacity still reject with stable `503`
responses. A model-specific 404 updates the tracked model without quarantining
unrelated models on that account; account-wide credential/configuration failures
can still quarantine the account.

## Client behavior

`GET /pt/v1/models` and `GET /pt/v1/models/{model}` list only currently
routable bindings for the authenticated tenant. The list is advisory; the
dispatch path repeats binding, visibility, revision, support and health
checks immediately before the outbound request. The final authoritative
admission after the account queue is the authorization point: changes committed before it are
rechecked, while already admitted requests are not retroactively retargeted
or cancelled. Database locks are never held across upstream HTTP or SSE.
Missing bindings or unsupported models return stable OpenAI `404`; authorization, malformed JSON and unavailable
bound state retain their `401`, `400` and `503` classes.

The body is parsed with native Chat semantics. Unknown fields, tools,
streaming choices and a caller's `stream_options` are preserved; the server
does not inject compatibility fields. There is one outbound generation
attempt, no same-account retry and no fallback. An explicit client retry is a
new request and is not an exactly-once guarantee.

## Error contract

Locally rejected bound requests use the OpenAI error object with a string
`error.code`, `error.param = "model"`, and a sanitized message. Important codes:

| HTTP | Code | Meaning |
| --- | --- | --- |
| 404 | `model_binding_not_found` | No visible binding for this tenant/model |
| 404 | `model_binding_model_not_supported` | Bound account no longer declares the model/capability |
| 503 | `model_binding_unavailable` | Binding, account or owning tenant is unavailable |
| 503 | `model_binding_changed` | A queued request's binding/configuration was changed |
| 503 | `model_binding_health_unknown` | Missing, expired, future-dated or config-stale probe |
| 503 | `model_binding_model_unhealthy` | Model health is unhealthy or degraded |
| 503 | `model_binding_capacity_exhausted` | Fixed account's execution budget is unavailable |
| 503 | `model_binding_dependency_unavailable` | Required state/metadata dependency failed |

These are local admission errors, not a second upstream attempt. Definite
upstream HTTP errors retain their status through existing safe error handling;
after a response stream has started, failures terminate that stream instead
of replacing its already-sent HTTP status.

## Minimal activation example

Use a system administrator JWT for management, not a generated platform API
key. Replace the placeholders with identifiers from the intended test or
freshly initialized deployment. The explicit probe invokes the configured
upstream once and can incur its normal inference cost.

```bash
BASE_URL='http://localhost:3000'
# ADMIN_JWT, TENANT_ID and ACCOUNT_ID are provided by the administrator.
curl --fail-with-body -X POST "$BASE_URL/api/v1/admin/model-bindings" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d "{\"tenant_id\":\"$TENANT_ID\",\"account_id\":\"$ACCOUNT_ID\",\"model\":\"example-model\",\"api_capability\":\"chat_completions\"}"

# Set BINDING_ID to the id returned above.
curl --fail-with-body -X POST "$BASE_URL/api/v1/admin/model-bindings/$BINDING_ID/probe" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H 'Content-Type: application/json' \
  -d '{"model":"example-model","api_capability":"chat_completions","timeout_ms":5000}'

# PLATFORM_KEY is a generated tenant API key; the account's upstream key stays server-side.
curl --fail-with-body "$BASE_URL/pt/v1/chat/completions" \
  -H "Authorization: Bearer $PLATFORM_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"example-model","messages":[{"role":"user","content":"Hello"}]}'
```

The ordinary and bound paths share the real account's quota; changing the
public URL does not allocate an additional RPM/TPM/concurrency budget. Model
selection does not alter the `provideraccount` billing dimension.

## Activation and operations

This feature is intentionally fail-closed. After creating a binding, verify
the account is active, supports the exact model and capability, then run the
model-specific probe from **Admin → Model Bindings**. Keep the account's
configuration and health current; changing a key, endpoint, visibility or
model support invalidates old health snapshots. The schema is greenfield:
fresh deployments initialize it from `crates/keycompute-db/migrations/001_init.sql`.
Do not run the schema change against retained production databases until the
project's upgrade/migration policy is introduced.

The Nginx `/pt/v1/chat/completions` and `/pt/v1/models*` locations preserve the
URI, disable request/response buffering and apply the same body and timeout
budgets as the regular Chat API. No production deployment is performed by
this repository change.

## Validation

`cargo test -p integration-tests --test e2e_model_binding` uses PostgreSQL,
the actual Axum router and loopback mock upstreams; it never needs a real
provider key. The test database must be disposable and initialized from the
current baseline. Tests assert exact outbound account/call counts, native
request preservation, caller and account isolation, strict health gates,
revision changes during queue wait, stale observations, public error classes
and resource cleanup. The ordinary protocol and workspace suites remain
required regression gates.
