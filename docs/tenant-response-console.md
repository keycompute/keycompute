# Tenant Responses and Conversation console

`/tenant/responses` manages two distinct resource families. Passthrough and
`node_dispatch` resources remain KeyCompute-local and enumerable. Account-pool
Responses are native upstream resources and are deliberately **direct-ID only**:
an administrator must provide the original owner UUID and opaque Response ID.
Account-pool Conversations and account-pool list/count are not exposed by this
delivery. Root/operator platform labels alone never mount the tenant page.

## Durable native identity

`response_affinities` now stores an explicit nullable `resource_kind`
(`response` or `conversation`). OpenAI resource IDs are opaque; neither backend,
SDK nor UI classifies them by `resp_`/`conv_` or any future prefix. Newly proven
visible routes persist their kind in the same transaction as routing state. A
legacy NULL kind can become addressable only after an operation whose semantic
route proves the kind; a conflicting explicit kind fails closed. Internal,
reservation and otherwise unproven legacy rows are not native-admin resources.

Fresh databases use the final `001_init.sql` schema directly. Databases carrying
the exact previously accepted V0001 checksum receive one bounded compatibility
upgrade under the existing migration advisory transaction: add `resource_kind`,
its check constraint and admin index, then advance the recorded V0001 checksum to
the current final baseline. Unknown or modified checksums still fail rather than
being silently rewritten. This is schema compatibility, not a production migration
run performed by this development change.

## Account-pool Responses

Supported native operations are detail, input-item paging, cancellation and
deletion. They use the exact tenant + original user + opaque resource ID + owning
account affinity. The server first discovers the account from that exact affinity,
then takes the same account -> affinity lock order used by account updates/deletion,
re-reads the route, verifies `resource_kind=response`, and re-applies current
non-passthrough account authorization. It never reselects the account pool and it
never probes other accounts or users. The encrypted credential is decrypted only
for the locked owning account; connection/credential rotation waits until that one
upstream operation finishes.

Native resources do not have a KeyCompute-local optimistic revision. SDK and UI use
separate native commands and never manufacture `expected_revision`. All mutations
are single-dispatch, including uncertain transport outcomes. Deletion is refused
while durable billing settlement remains pending; a confirmed 2xx/404 upstream
delete tombstones only the exact tenant/user/account Response affinity. No local
Conversation mutation is implied by a native Response operation.

Tenant access requires the current selected tenant administrator membership. Root
platform access requires an explicit target tenant plus bounded reason. The strong
control audit/revalidation runs before the resource operation, a read-only provenance
check runs again immediately before upstream dispatch after any account-lock wait,
and the original session/account grant is revalidated after the upstream result. A
revoked/expired old request therefore cannot begin a late upstream call or receive
private content. Global identity locks are never retained across the network call.

Successful native detail JSON goes through the same bounded Responses JSON working-
set admission and sanitization used by the public endpoint; its memory permit is
retained for the response-body lifetime. All reads are private/no-store. Raw API
keys and private bodies are excluded from control audit metadata and error text.

## Local resources and UI lifecycle

Passthrough/node Responses still support list/filter/page, detail, input items,
cancel and delete with observed revisions. Local Conversations retain detail,
metadata edit, item paging/append/removal and delete. Their resource identity is
tenant + original owner + execution mode + kind + opaque ID, with revision/item
identity added to mutation state. Late async results cannot publish into another
verified workspace. Private JSON is escaped text and is not stored in URLs or
browser persistent storage.

The account-pool UI exposes only a direct Response form (owner UUID + opaque ID).
It does not issue a hidden account-pool list/count call. Switching to Conversations
removes account-pool mode. Detail/items/cancel/delete use the native SDK contract,
and private synthetic browser payloads remain escaped and absent from browser
storage.

## Verification boundary

Real isolated PostgreSQL/Axum tests cover exact owner/account/kind scope, immutable
ownership, credential snapshot rotation, settlement-vs-delete serialization,
pre-dispatch membership/expiry revocation, post-upstream expiry/grant revocation,
and stable-user continuation ownership. SDK tests cover native envelopes, no fake
revision, single dispatch and direct-ID/list rejection. Migration tests cover fresh
baseline and exact legacy-V0001 compatibility. Compiled-browser tests remain UI
evidence with synthetic HTTP and are not a substitute for those server/database
checks.

Native account-pool enumeration and native Conversation administration remain
separate work. No production database, credential, upstream resource, service
restart or deployment is changed by this development delivery.
