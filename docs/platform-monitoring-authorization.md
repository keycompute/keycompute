# Platform monitoring authorization contract

## Trust boundary

Raw diagnostic endpoints are root-only platform resources. Global root sessions
need no default or target tenant membership. A selected session, when present,
must still match its active user, membership, tenant and signed versions.
Operator diagnostics remain on the separately allowlisted platform operations
API; this implementation does not grant raw traces or business detail to operators.

The canonical prefix is `/api/v1/platform/monitoring`. Retained
`/api/v1/admin/monitoring` paths invoke the same guarded handlers, not legacy roles.
Both prefixes expose overview, requests, requests/{request_id}, summary,
targets/health and targets/probe. Tenant selectors are root query filters, not
membership grants. Request pages and financial summaries use the original
consumer tenant; target-health selection uses the resource owner's tenant.

## Audited read snapshots

Every raw handler requires a global console credential and a canonical server
RequestId, then obtains a MonitoringRead on the primary database. The guard
checks the complete original signed FinancialScope and shared-locks only its
relevant authorization rows, in tenant/user/member order. It does not acquire
or increment the identity administrative write fence, and concurrent diagnostic
readers remain compatible. Data queries share one repeatable-read transaction.

The read audit must commit before payload delivery. Expiry is checked before
and after audit insertion; failure or expiry withholds data and rolls back the
audit. Responses are private, no-store. Query targets and request ownership are
included in audit metadata without prompt bodies, response bodies or credentials.
Request-to-usage/node evidence joins include original tenant and user, not only
an independently supplied request ID.

## Console probes versus scheduled runtime probes

Console and internal runtime probe entry points are explicitly distinct.
Manual batch candidate discovery loads account IDs only, bounds the operation
to 50 unique accounts and rejects nil targets. An implicit all-enabled request
with more than 50 targets is rejected rather than silently truncated; callers
must choose an explicit bounded batch. Four-way probe concurrency is unchanged.

The manual request audit commits before network activity, and each target also
uses the existing account-probe audit. No authorization transaction is held
across upstream I/O. Console material is loaded with complete current authority;
model discovery and every capability probe step recheck original authority and
actual connection/configuration material as well as the application version.
Revoked or stale probes do not continue to another capability or penalize account
health. Accepted network work is not retried on an uncertain outcome.

Tenant-admin probes remain limited to owned accounts: global visibility grants
use, not management or secret access. Final result handling rechecks current
console authority. Runtime scheduling keeps its separate trusted internal path;
this is not an inference-key permission or a client-selectable authority mode.

## Client and verification boundaries

The SDK uses canonical fresh read paths and validates detail request UUIDs.
Probe POSTs retain the existing single-dispatch retry policy. The regression
suite includes bare handler mounts with only canonical request-ID injection,
real root/tenant/operator/key credentials, consistent tenant/currency queries,
audit failure withholding, exact-backend queued role/expiry races and real local
mock-upstream multi-capability revocation. A failed lock-timeout is not accepted
as proof that the post-audit expiry check ran.

This is not native account-pool Responses administration, the complete tenant UI,
a release or a production deployment. Final verification results are recorded
separately in tenant-implementation-progress.md only after the gates complete.
