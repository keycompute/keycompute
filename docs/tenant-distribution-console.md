# Tenant distribution policy console

`/tenant/distribution` is the selected tenant administrator's future-allocation
policy page. It uses the existing tenant distribution-policy handlers and does not
reuse personal `/distribution` views or platform-targeted control routes. The
selected workspace is presentation context only; the server still requires the
current tenant administrator authority for every read and write.

## Policy semantics

A policy belongs to exactly one tenant and has an `everyone` or `tenant_member`
beneficiary. An `everyone` policy never carries a beneficiary ID. A member policy
carries one explicit nonzero user UUID, and an active policy is accepted by the
server only while that user is an active member of the target tenant. The create
form offers active-member suggestions from the current tenant; the UUID input
remains explicit so a large tenant is not silently truncated to one client page.
The backend remains authoritative for membership.

Beneficiary identity is immutable in the editor because the existing PATCH
contract does not allow changing it. Changing an allocation target therefore means
creating a new policy and explicitly disabling or deleting the old one. The editor
never fabricates a beneficiary mutation.

Commission rates remain exact decimal strings. The console accepts plain values
from 0 through 1 with at most four fractional digits, matching the server policy
limit without percentage or floating-point conversion. Priority stays within
`-1000..=1000`. Effective timestamps use RFC3339 with an explicit timezone. A blank
edit end time is an explicit clear; the start time is immutable after creation.

Every create, edit, delete and default-policy command requires a bounded human
reason. Edit and delete send the exact `updated_at` revision displayed by the
server. No client-generated revision is substituted. The default-policy action is
its own server operation; the UI does not emulate it by guessing a rule ID or
priority.

## Scope, reads and command lifecycle

`DistributionPolicyApi` stores an explicit target tenant. Tenant and platform
constructors use different route families and neither falls back to the browser's
selected tenant. Reads bypass the display cache and validate returned tenant IDs,
resource IDs, beneficiary shape, exact rate syntax, unique list IDs and pagination
metadata before rendering. A foreign or malformed row makes the whole result fail
closed instead of being filtered after display.

Control requests are single-dispatch through the verified workspace command guard.
Authentication refresh, conflict, 5xx or an uncertain network result is not
silently replayed. A tenant/session/workspace change after dispatch prevents the
old completion from publishing success or repopulating the new workspace. Global
root/operator role labels without a real selected tenant-admin membership do not
mount the tenant page.

The page configures **future distribution resolution only**. It does not mutate
historical distribution records, move balances, settle earnings, release wallet
reservations, withdraw money or call a payment channel. Those are separate control
planes with their own authorization and transaction contracts.

## Verification boundary

Client wire tests cover fresh reads, exact target/ID/page validation, beneficiary
shape, exact commission-rate input, revisioned patch/delete and explicit platform
targeting. Existing Axum/PostgreSQL distribution-policy tests remain the authority
for transactional authorization, active-member validation, audit rollback and
optimistic conflicts. Code-level Web tests cover draft identity, immutable
beneficiary, nullable fields, route separation, exact revisions and workspace-race
isolation. These tests do not represent a real payout or production deployment.
