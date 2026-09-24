# Tenant Key metadata and personal issuance console

`/tenant/keys` is a tenant-administrator page, not a platform shortcut. It uses
TenantKeyApi and the verified selected membership. `/api-keys/issuance` is a
separate personal page using OwnerKeyIssuanceApi. Its expected tenant/user only
validate the response; no owner override is sent in a personal request. A root or
operator label without an administrator membership cannot mount tenant controls.

## Administration

The page lists metadata and pending requests, with explicit owner filtering and
bounded pagination. It supports name/expiration edits, revocation, removal, new
issuance, rotation requests and pending cancellation. The member picker disables
inactive users/members; the backend remains authoritative even for typed UUIDs.
Only a name/expiration edit carries the observed updated_at CAS value. Revocation,
removal and issuance commands retain their existing server transaction semantics;
this page does not invent additional optimistic locking or physical deletion.

Expiration editing distinguishes keeping the value, explicitly clearing it, and
setting a future RFC3339 time within the existing ten-year limit. Revoked/expired
keys may be renamed but cannot be reactivated by extending expiration. A new or
rotation request is inert until its original owner claims it. The administrator
never receives a claim secret. A removal that only revokes retained history is
shown as revocation, not a successful physical deletion.

## Personal lifecycle and secrets

The owner explicitly confirms a claim or decline. A successful claim returns the
one-time nonserializable ClaimedKey in a workspace-local signal. The value is not
written to URL parameters, persistent stores or diagnostic output. Only explicit
copy exposes it to the browser clipboard, and the clipboard Promise must succeed
before the page reports copied. Failure offers manual copy without logging the key.
Closing the value, leaving the page or changing verified workspace destroys this
view's secret state. This is not a memory-zeroization or OS clipboard-erasure claim.

The existing personal `/api-keys` page now uses the same verified workspace keyed
boundary and read/command guards. Its creation policy remains the existing default
180 days; this delivery does not add a custom-date fallback to the personal API.
Delayed personal-create/claim responses cannot publish secrets into another tenant.
The same-workspace token refresh is not treated as a new owner or workspace.

All mutations are single dispatch, including 401/409/503 and uncertain transport
results. A lost claim response is not retrievable through another claim. Closing
or navigating away does not cancel a request already accepted by the server;
refresh persistent metadata before an explicit retry or replacement request.
Existing DAO ownership, authorization, audit and cache rules are unchanged.

## Verification

Native editor tests cover UUIDs, dates, exact nullable expiry, observed revisions,
immutable ownership, state-key identity, routes and translated labels. A checked-in
Chromium runner exercises the compiled WASM with synthetic intercepted HTTP:
metadata and pending controls, member selection, retained history, current scopes,
clipboard success/rejection, secret redaction/nonpersistence, direct personal
creation, late results across workspace changes and narrow-viewport dialogs.
It is UI evidence, not a production payment, upstream or security deployment.
Real key SDK/Axum/PostgreSQL, expiry, audit and concurrent-owner tests are retained
in full workspace verification. No backend/schema/production credential changes.

Provider/financial resource UI, account-pool native administration and complete
endpoint/release gates remain separate from these Key pages.
