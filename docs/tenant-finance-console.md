# Tenant financial reporting console

`/tenant/finance` is the selected tenant administrator's read-only reporting page.
It exposes usage/billing records and their stored unit-price snapshots, currency
summaries, payment metadata and an explicitly selected member's wallet. It does
not reuse platform financial routes or derive access from root/operator labels.

## Boundaries and values

The payment channels remain platform-wide; order and wallet ownership remain the
real tenant/user tuple. A member wallet is not a tenant-wide shared balance pool.
The client requires a real tenant and validates returned tenant/owner/resource IDs,
page metadata and currency groups. Invalid responses fail instead of being filtered
into apparent success. Order DTOs project metadata only: no pay URL, subject/body,
callback payload or payment credential is displayed or serialized by the inspector.

Amounts and stored prices are exact strings, including server scientific notation.
No currency conversion, float parsing or cross-currency total is introduced.
64-bit request/token counters remain integers in Rust, including in WASM; browser
checks include values beyond JavaScript's exact-number range. Wallet values have
no currency field in the existing DTO and are shown without inventing a currency.
A missing wallet for an existing member is shown as uninitialized; a failed or
foreign member lookup is an error rather than a synthetic zero balance.

Usage reads use an explicit applied RFC3339 window, at most31 days in this UI.
Input and applied filters are separate. Payment status/owner filters do not
silently inherit a usage time filter unsupported by the payment endpoint. Lists
are paginated, and an independent memoized summary key avoids refetching heavy
currency totals when only the page number changes. Refresh does not move money.
The SDK preserves server-side calendar validation and exact query encoding.

## Original session after database waits

An isolated actual HTTP regression blocked the exact reporting connection at a
table read, then revoked its original token version. The previous handler returned
HTTP200 with financial data after release. All six report handlers now use the
existing ConsoleSessionProof to recheck the original user, platform and selected
membership versions/states on the writer after their final DAO query. Expiry is
checked before and after that lookup. The existing mandatory tenant/owner DAO
predicates remain in place; the proof is not an additional permission grant.

This adds one bounded provenance lookup per report, not one per row. No global
exclusive authorization lock, wallet initialization, settlement or other financial
write is added. The report router marks responses private/no-store and no-cache.
The same handlers serve the usage/billing aliases. Independent disposable test
DBs cover six handler families after signed-expiry waits and token, tenant-version,
role-change and suspend/regrant races. A new authorized request succeeds afterward.

## UI lifecycle and verification

The verified workspace tuple owns the page and asynchronous results. Submitted
filters identify lists; tenant/member/kind/resource identity owns the inspector.
Late old-workspace details cannot publish into a different tenant. Metadata is
escaped text in an opaque, bounded, focused dialog with Escape support. No financial
metadata is saved by this page in browser storage or its route parameters.

Wire tests cover fixed scopes, exact values, malformed/foreign results, currency
separation, fresh reads and read-only missing-wallet behavior. The actual Rust
SDK/Axum/PostgreSQL test covers filters, paging, detail, totals, metadata projection,
member/inference/foreign rejection and no wallet creation. Native UI tests cover
calendar windows, selectors and detail identity. The production-compiled Chromium
runner uses synthetic HTTP and is not represented as a production payment test.

Withdrawal decisions, reservation recovery and distribution-rule management are
separate existing control APIs and are not granted by this read-only page. Provider
UI, native account-pool resources and final endpoint/release gates remain separate.
No production database, credentials, real payment, SMTP or deployment is changed.
