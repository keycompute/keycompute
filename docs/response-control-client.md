# Scoped managed Responses client

The existing `ResponseControlApi` now verifies the explicit target tenant,
original owner, resource family and object ID in managed-resource responses.
This is client-side contract validation, not a replacement for backend action,
tenant or object authorization. The server's role grants, stored ownership,
pricing and settlement behavior are unchanged by this slice.

## Queries and identity

Lists and counts require an explicit mode and bounded pagination. Optional owner
filters narrow the selected tenant; they never mean another tenant or a platform
query. Platform resource access also requires an explicit bounded reason. Reads
use fresh HTTP requests rather than the display cache.

Local passthrough/node summaries require their real positive revision. No absent
version is synthesized. Detail bodies must match the requested resource ID and
object kind; a local Response cannot silently become native content or an absent
body. Returned rows with foreign tenants, owners, modes or object IDs fail closed.
Two owners may legitimately have equal opaque resource IDs. Their rows are kept
separate; duplicated logical owner/ID rows are rejected, not collapsed or filtered.

Native account-pool summaries and Conversations are now supported through
proven affinity rows. Native list/count responses require the account-pool mode and
validate tenant, owner, account, kind and duplicate logical identity. Native
Conversation envelopes also reject a local revision or a missing owning account.
Native detail and mutation calls use the opaque owner/resource address; native
mutations carry no local revision and are single-dispatch. Metadata updates use the
management route's PATCH contract while their upstream native operation remains
POST. The client never substitutes native resources with node dispatch.

## Items and commands

`ResourceAddress` carries execution family, original owner and opaque ID; its API
instance supplies the tenant. `ItemQuery` provides bounded limit, order and cursor.
All path/cursor values remain encoded data. Typed item pages check object kind,
first/last IDs, item IDs, length and has-more consistency. Duplicate items or a
returned input cursor fail instead of producing a pagination loop. Default-page
convenience methods now call this same validated path.

Local content commands require a positive observed revision; native
Conversation commands carry only their upstream metadata/items payload. All
mutations are sent once, including authentication or uncertain network failures.
Results are checked for the matching object identity and actual deleted flag before
success is reported. Append requests contain 1-512 objects and are bounded to
2 MiB; the server retains its metadata and total-history validation. Existing
backend cancellation and archive/settlement semantics are not replaced by a client
success message.

Private content response types retain redacted Debug formatting. Item pages also
redact their content. This is not a promise that an application cannot explicitly
serialize or retain resource content. No new UI or browser-storage policy is
certified by this SDK-only delivery.

## Verification

Wire tests cover fresh reads, explicit scopes, malformed objects, real revisions,
identity collisions, literal cursor encoding, duplicate pages and no automatic
mutation replay. An actual SDK/Axum/PostgreSQL regression exercises both local
families, original-owner lists/details, member and inference-Key denial, metadata
CAS, item pagination/append/removal, conversation/Response deletion and retained
ownership. Deletion does not initiate extra upstream inference.

The `/tenant/responses` UI now exposes local and indexed native modes, including
native Response input-item inspection and indexed cancellation. Its production
Chromium checks pass; synthetic browser fixtures are still not backend
authorization evidence. Provider/Key/financial pages and final release gates
remain separate. No production database, credentials, payment, notification,
service or deployment is changed.
