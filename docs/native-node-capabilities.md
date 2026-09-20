# Native node capability negotiation

Each native-capable session advertises versioned per-model operation profiles.
A profile records its exact model and operation, functional features, request
and result byte ceilings, and an optional output-token ceiling. The server
computes requirements from the original request without rewriting its body.
Routing and atomic claim use the same profile/limit predicate; queue hints are
not authority. An incapable worker cannot remove another worker's native task.

Registration freezes profiles and registered models into the session. Heartbeat
may narrow the currently accepted model set but cannot introduce new models or
features by editing mutable node metadata. Native and legacy work remain isolated,
and native-capable workers poll both classes fairly. A native execution failure
is terminal; a delivery retry never repeats model inference.

`POST /node/v1/capabilities` negotiates a replacement session under current session
authentication. Identical retries return the same replacement identity. The old
session stops receiving work but may finish already-issued leases; unrelated or
revoked sessions cannot take over them. Exclusion and tenant lifecycle checks are
repeated when accepting new work.

node-token reads local `/api/version` and bounded `/api/show` metadata with at
most four concurrent requests. No generation probe is performed automatically.
Unverified metadata does not become a blanket all-features declaration. Restart
loads and explicitly renegotiates changed capabilities before scheduling. Persisted
session metadata and executor-side validation prevent the client from silently
broadening the server's accepted contract.

The admin Node Gateway shows runtime version and advertised per-model capabilities.
These are execution qualifications, not new tenant/model authorization bindings.
