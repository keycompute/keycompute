# Upstream channel administration

The administrator has one sidebar entry, **Upstream Channels**, and three
route-backed tabs:

| Tab | Canonical route | Responsibility |
| --- | --- | --- |
| Account Management | `/admin/upstreams/accounts` | Upstream connections, credentials, capabilities, account state and unbound-account pool participation |
| Passthrough Bindings | `/admin/upstreams/passthrough` | Account-to-tenant grants covering every declared account model |
| Node Gateway | `/admin/upstreams/nodes` | Node enrollment, live workers and task execution |

The tabs embed their resource views. Reloading a tab or opening its URL directly
retains the active mode. Historical account/model-binding/node-management UI
addresses redirect here; they are not competing sidebar destinations.

A connection is not a routing mode. One account can serve several explicit
passthrough grants, and ordinary routing is permitted only by the effective
pool policy. Binding-managed accounts show that their pool setting is controlled
by grants; ordinary account edits omit that field so changing a name or credential
does not attempt to override access policy.

The passthrough form matches the existing Add LLM Channel modal. Account and
tenant dropdowns are named, searchable and paginated. Declared models are a
read-only summary, not inputs. The only policy checkboxes are Global and
Participate in Account Pool, both unchecked initially. There is no enabled
switch, draft/publish lifecycle or mandatory per-model probe. Saving never
performs a paid diagnostic.

Node enrollment tokens are not inference API keys. NodeDispatch remains the
technical name of task dispatch; the administrator-facing title is Node Gateway.
Pricing, tenants, platform keys and request monitoring remain distinct workflows.
Monitoring records `passthrough_binding` separately while retaining the real
account identity and shared accounting.

Caller guidance starts with account pool, passthrough or NodeDispatch execution,
then shows only compatible protocol and model choices. Model discovery is
authenticated and tenant-scoped. Empty data, loading and dependency failures are
distinct; examples never manufacture an available model. A generated platform
key identifies a caller, not a fixed execution mode. `/pt` is the passthrough URL
prefix; `/nt/v1` is the NodeDispatch URL prefix and node model IDs remain raw.
See [NodeDispatch](node-dispatch.md) for the supported public surface and
explicit migration from the removed prefix-based routing form.

For grant semantics, the four-option scope matrix, APIs, diagnostics, revocation
and fresh-schema constraints, see [Passthrough bindings](passthrough-bindings.md).
