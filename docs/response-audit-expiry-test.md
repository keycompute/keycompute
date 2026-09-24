# Signed expiry and audit-entry regression timing

The previous two-second JWT fixture sometimes expired during authentication/setup
on a loaded CI runner. Its nontransactional marker correctly failed because no
audit INSERT was reached; an early 401 is not post-audit-expiry coverage. An
explicit 2100ms scheduler delay reproduced that precondition failure locally.

Only the isolated test router changes. The fixture verifies the actual signed JWT
with the real AuthService and caches exactly that returned context, the same
request-extension contract used by console admission. It then approaches the
unchanged signed deadline before entering the complete production router. The
six-second admission budget is separate from the audit wait, which remains under
the existing 2500ms SQL and three-second async timeouts. Production authentication,
timeouts, resource authorization, audit behavior and returned data are unchanged.

The deliberately delayed first request remains. A nontransactional timestamp
sequence proves the audit starts before the signed deadline; the original marker
proves the intended INSERT was reached exactly once. Every request must then
return 401 after that deadline, roll back its audit, retain resource revisions,
withhold private data and start no extra inference. There is no HTTP retry or
ignored assertion. Root and tenant-admin detail/list/count cases use separate
throwaway databases and run concurrently with the normal suite.

The initial longer-token-only attempt hit the real SQL timeout and was rejected;
no production limit was relaxed to pass it. The ready-auth barrier separates
fixture setup from the short transaction timing condition under test. Existing
raw-token authentication and cross-tenant tests continue unchanged.
