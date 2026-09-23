# Tenant key metadata and owner issuance client

`client_api::api::key_control` is the typed client for the already existing key
control handlers. This delivery does not add a Web page, change backend grants,
alter the schema, or create a new platform-level key-management bypass.

## Two distinct clients

`TenantKeyApi` requires an explicit real tenant UUID. It lists and reads key
metadata, applies a versioned name/expiration patch, revokes or removes a key,
requests new or rotating issuance, lists pending intents and cancels an intent.
The only key material in its response types is the nonsecret preview. A removal
result may report a revoked row retained for existing evidence rather than a
physical deletion; callers must display the actual `deleted` flag.

`OwnerKeyIssuanceApi` uses only `/api/v1/me/key-issuance` routes. Its expected tenant
and owner are response-validation constraints, not request parameters or authority.
Even an administrator's JWT cannot claim a peer's intent. Claim and decline remain
personal operations, and the backend independently verifies the real caller.

A new/rotation request is not a key. Existing keys remain unchanged until the
owner claims a current intent. Successful rotation returns a different key ID;
the expected original owner/name/expiration and intent identity cannot silently
change in the response. The old key is revoked by the existing transaction.

## Data handling

Reads always bypass the display cache. Page metadata, tenant, resource and optional
owner filters are checked before returning data. A foreign or malformed response
is rejected instead of filtered into an apparently successful list. UUIDs must be
real. Names and timestamp strings are bounded; the server retains authoritative
calendar, lifetime and current-state validation.

Expiration patches preserve all three meanings: omission retains the old value,
explicit null clears it, and a value sets it. A metadata patch must carry the
observed updated_at; an absent or empty version is not invented by the client.

Every command is single dispatch, including after authentication failure, 503 or
uncertain transport results. The ClientError returned by a one-time claim is sanitized before it is handed
to an application: reflected server messages are not returned with a secret. A claim result and
its secret wrapper do not implement Serialize, have redacted Debug output and
require an explicit `expose()` call to use the raw key. This type discipline does
not itself guarantee that an application never copies or persists an exposed
string; the future owner page must still keep it in memory and clear it on exit.

## Verification and remaining work

Wire tests verify fresh scoped reads, query ownership, omitted/null expirations,
correct paths and status results, malformed identities, redacted secret results,
no automatic replay and retained revoked history. A compile-fail doc test verifies
that a claim cannot be passed directly to serde_json serialization.

An actual SDK/Axum/PostgreSQL test covers metadata denial for member/foreign/key
credentials, administrator-created intents, owner-only single claim, metadata CAS,
inert rotation followed by owner activation, revocation/removal, decline/cancel,
original ownership and audits. These are isolated synthetic accounts/keys, not
production credentials or a deployed tenant UI.

Existing server key authorization, audit rollback, concurrency and display-cache
regressions remain part of workspace verification. The separate metadata/owner
Web pages and remaining backend deadline review are not completed by this SDK.
No production data, payment, SMTP, credentials or deployment is changed.
