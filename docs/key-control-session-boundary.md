# Original session boundary for key control

This hardening covers the existing tenant key metadata/issuance endpoints,
owner-only issuance claims/declines, and direct personal key creation/deletion.
It changes no role grants, schema, resource owner or inference credential format.

## Reproduced failures

An actual HTTP regression delayed the key audit INSERT until the original signed
JWT expired. Owner rotation claim and direct personal creation both previously
returned HTTP 200 rather than rejecting the expired operation. A separate actual
HTTP test held the tenant row, observed the exact personal-creation database PID
waiting on its shared authorization lock, and changed the original token version.
The prior handler accepted the operation after this revocation as well.

The tests use independent disposable local databases, synthetic users and keys,
and existing schema bootstrap. The audit delay uses a nontransactional sequence
as evidence that the request reached the delayed write; early authentication
failure cannot masquerade as the expected post-audit rejection. No production
credential or database is used. An intermediate membership fixture tried updating
a trigger-owned version directly; that trigger intentionally preserved the old
version. The corrected test performs a real role/status transition and explicitly
asserts the stored version advanced before releasing the blocked request.

## Transaction and response boundary

The shared `key_control_auth` helper requires an expiring JWT and an explicit
selected tenant/user. After the scoped DAO work and audit waits, it compares the
original user token version, tenant and membership authorization versions,
current active states, tenant role and platform role against the writer.
The existing DAO retains its authorization row locks in the outer transaction.

Every HTTP mutation has an outer transaction. Internal DAO transactions are
savepoints, including issuance requests and cancellation/decline which previously
committed independently. Expiry or a changed original proof rolls back the entire
key/intent/audit change rather than returning an error after the inner DAO commit.
Successful credential changes fence display caches through commit. Inert intent
creation or cancellation does not evict inference-key cache entries.

The direct personal key path keeps its shared parent/identity/member locking and
does not acquire a new global exclusive identity fence. One additional bounded
identity-tuple lookup is performed per final control read or mutation, not for
each row in a key list. Inference requests are not routed through this helper.
Existing server key ownership predicates and current-state constraints remain.

The helper also checks the JWT deadline after commit acknowledgement. A commit
acknowledgement itself can be delayed: a transaction accepted while authorized
may already have committed even though its caller receives an expiry error.
That case is an uncertain result, not a promise that committed data was rolled
back. A one-time secret is withheld, and the caller must inspect durable records
before requesting another key; automatic claim replay is not introduced.

Personal and tenant metadata reads recheck the original session after their last
query. Personal key creation now explicitly returns private/no-store and no-cache
headers, including when the handler is used without cache middleware. Its JSON
shape is unchanged. Owner claim keeps its existing no-store headers.

## Tests and boundaries

Four isolated HTTP scenario groups cover ten mutation paths: tenant request,
rotation, metadata patch, revocation, deletion and intent cancellation; owner
claim and decline; direct personal create and delete. Rejected requests leave
all tenant key, issuance and audit fingerprints unchanged. A fresh valid identity
can complete the operation afterwards; rotation then revokes the original key
and creates exactly the owner's replacement.

The queued personal case tests user-token, tenant-version, membership-role and
suspend/regrant changes against an observed backend lock wait. Each mutation must
actually advance its corresponding version. Authorized suspension/role changes
can revoke old keys through existing database triggers; the rejection test keeps
that legitimate revocation while proving the queued command adds no effects.
New and existing key ownership,
concurrency, audit and display-cache tests are retained.

This is a key-control boundary, not a certification of every resource handler.
Provider/pricing and other control transaction deadline coverage, native account-
pool resources, remaining tenant resource pages and production release gates
remain separate. No production data, payment, SMTP, credentials or deployment
are changed by these tests or this delivery.
