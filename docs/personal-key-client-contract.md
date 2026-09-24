# Personal Key client and server contract

The existing personal `/api/v1/keys` creation SDK previously serialized
`expires_at:null` (or a caller date), while the strict server request accepts only
`name` and `never_expires`. A real SDK/Axum/PostgreSQL test reproduced HTTP 422
from that mismatch before the SDK change. This is not a new backend role grant or
lifetime policy: it repairs the wire contract used by the existing personal page.

## Lifetime and request semantics

`CreateApiKeyRequest::new(name)` now sends `never_expires:false`, preserving the
server's 180-day default. `with_never_expires(true)` requests no expiration. The
unsupported `with_expires_at` SDK method is removed rather than mapped to an
unrelated policy. Explicit custom expiration belongs to the existing tenant
issuance/metadata endpoints. The old expires_at payload remains rejected; no
legacy request fallback is added.

Creation validates a bounded nonempty name without control characters. The
one-time response must declare success, its real nonzero UUID, the requested name
and lifetime flag, compatible nullable expiration, and a bounded nonempty secret
without whitespace/control characters. Missing required response fields are not
invented. Timestamp/calendar enforcement remains authoritative on the server.

The request is single dispatch, including authentication, 503 and transport
failures. Owner issuance and direct personal creation now share one error
sanitizer and secret-shape predicate. A returned error cannot reflect a raw
one-time secret supplied by an upstream response. The known authentication,
permission, rate-limit and availability error categories remain available.

`CreateApiKeyResponse` Debug formatting redacts both the secret and its optional
message. Its successful raw String field remains the existing explicit API; this
is not a claim that applications cannot copy or persist it. Owner issuance's
separate nonserializable secret wrapper is unchanged. Personal control reads now
use fresh requests rather than client display-cache snapshots.

## Ownership and deletion

The server continues to derive tenant and owner from the verified console session.
The personal request has no tenant/owner selector. An administrator's personal
list does not expand to other members, and a peer or foreign tenant cannot remove
this user's key. Inference credentials remain forbidden from console management.

The existing server removal lifecycle is preserved: first removal revokes a live
key and retains its metadata; a later authorized removal can delete the revoked
row when permitted by persistence constraints. The new real test asserts both
steps instead of treating the first operation as unconditional physical deletion.
No key format, database owner, accounting identity or inference grant changes.

## Verification and remaining UI work

The real native client test proves default 180-day and explicit permanent creation,
original tenant/user/hash, owner-only lists, foreign/member deletion rejection,
inference-Key management denial, the revoke/remove lifecycle and no-store headers.
It also confirms the unsupported old field is still rejected. Wire tests assert
both lifetime request shapes, malformed success rejection, error/Debug redaction,
no automatic creation replay and fresh reads with display caching enabled.
Existing owner issuance and backend session/concurrency tests are retained.

The prepared tenant-Key metadata and owner-claim pages are NOT included in this
SDK-only slice. Their initial compilation/native tests ran, but the browser
validation script write was safety-denied in full and not executed. Fifteen UI
files were archived and restored out of the main worktree. The archived page also
uses a deprecated ReadOnlySignal alias that must be replaced before strict UI
acceptance. No production DB, credential, actual payment, notification, service or
deployment was changed. Resource pages and final release gates remain unfinished.
