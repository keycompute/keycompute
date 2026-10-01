# Tenant Responses and Conversation console

`/tenant/responses` manages two distinct resource families. Passthrough and
`node_dispatch` resources remain KeyCompute-local and enumerable. Account-pool
Responses and Conversations are native upstream resources; their tenant-scoped
indexes enumerate only rows with a proven `resource_kind`, original owner and
owning account affinity. Detail and mutation routes still require the original
owner UUID and opaque resource ID. Root/operator platform labels alone never
mount the tenant page.

## Durable native identity

`response_affinities` now stores an explicit nullable `resource_kind`
(`response` or `conversation`). OpenAI resource IDs are opaque; neither backend,
SDK nor UI classifies them by `resp_`/`conv_` or any future prefix. Newly proven
visible routes persist their kind in the same transaction as routing state. A
legacy NULL kind can become addressable only after an operation whose semantic
route proves the kind; a conflicting explicit kind fails closed. Internal,
reservation and otherwise unproven legacy rows are not native-admin resources.

Fresh databases use the final `001_init.sql` schema directly. An exact
historical V0001 checksum receives the bounded `resource_kind` compatibility
upgrade under the migration lock; unknown checksum drift and non-empty databases
without migration history fail closed. The `resource_kind` column, constraint and
admin index are part of the final baseline.

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

## Account-pool Conversations

Native Conversations use the same tenant, owner, account and `resource_kind`
proof as Responses. The console supports detail, item paging, metadata updates,
item append/removal and deletion. These calls carry the native upstream payload
without a local optimistic revision; the server validates item count/content,
keeps one dispatch for each mutation, and tombstones only the exact conversation
when deletion is confirmed.

## Local resources and UI lifecycle

Passthrough/node Responses still support list/filter/page, detail, input items,
cancel and delete with observed revisions. Local Conversations retain detail,
metadata edit, item paging/append/removal and delete. Their resource identity is
tenant + original owner + execution mode + kind + opaque ID, with revision/item
identity added to mutation state. Late async results cannot publish into another
verified workspace. Private JSON is escaped text and is not stored in URLs or
browser persistent storage.

The account-pool UI exposes tenant-scoped Response and Conversation indexes
backed by proven affinity rows, plus direct owner UUID + opaque ID detail access.
Switching between resource families preserves the selected mode and scope.
Detail/items/cancel/delete and Conversation metadata/item mutations use the native
SDK contract without synthetic revisions; private synthetic browser payloads remain
escaped and absent from browser storage.

## Verification boundary

Real isolated PostgreSQL/Axum tests cover exact owner/account/kind scope, immutable
ownership, credential snapshot rotation, settlement-vs-delete serialization,
pre-dispatch membership/expiry revocation, post-upstream expiry/grant revocation,
and stable-user continuation ownership. SDK tests cover native envelopes, no fake
revision, single dispatch, indexed enumeration and exact scope rejection.
Migration tests cover fresh baseline and exact legacy-V0001 compatibility.
Compiled-browser tests remain UI
evidence with synthetic HTTP and are not a substitute for those server/database
checks.

Native account-pool enumeration and Conversation administration are covered
by the indexed and direct-resource contracts above. No production database,
credential, upstream resource, service restart or deployment is changed by this
development delivery.
