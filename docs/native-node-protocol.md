# Native node protocol — phase 1

> Historical phase baseline. The current implementation also supports [native event streaming](native-streaming.md).

Node Chat now preserves the complete JSON request and response instead of
projecting it through the legacy text-only Chat task. This phase implements
native **non-streaming Chat Completions** only. Messages, Responses, native
stream events and platform-managed Responses state are later phases, not
capabilities of this release.

The public entry remains `POST /nt/v1/chat/completions`; model IDs are literal.
The worker still initiates registration, heartbeat, task polling and result
submission through `/node/v1/...`. No inbound port is required on node-token.

`NodeNativeRequest` contains an operation, full JSON body and protocol headers.
In phase 1 the operation is `chat`, the fixed local destination is
`/v1/chat/completions`, and request headers must be empty. It cannot contain a
caller-selected destination URL. Platform credentials never reach Ollama.
`NodeNativeHttpResult` carries HTTP status, safe response headers and full JSON.
The wire contract is named `node.native.v1`; the control protocol remains
`node.v1` with additive native payload/result variants.

Only sessions explicitly advertising native Chat can receive native tasks.
The server snapshots permission during registration; changing another session
or the mutable node declaration does not upgrade an older session. Native and
legacy queues are separate, and an authorized worker polls both fairly. Atomic
claim repeats ownership, active-tenant, session, operation and model checks.

Native requests execute once. Completion retries resend the same result and
are deduplicated by task/lease/result identity; they never repeat inference.
Claimed native failures and expiration are terminal, not eligible for requeue.

## Fidelity and bounded behavior

Bodies are preserved as JSON values: tools, tool history, sampling fields,
stop sequences, explicit nulls and unknown extensions are not discarded.
JSON whitespace and object-key ordering are not a byte-preservation promise.
Each serialized native body is limited to 1 MiB; safe result headers are limited
to 16 entries, each at most 256 printable ASCII bytes. Debug output redacts
native bodies. Inline images are retained; remote image URLs are explicitly
unsupported pending a bounded fetch policy. `stream:true` is rejected before
execution in this phase rather than silently changed to non-streaming.

The worker has no automatic inference retries or HTTP redirects. Its task
deadline covers response headers and all body chunks. Control-plane poll and
completion responses are bounded before JSON decoding.

HTTP 200 requires the exact model name, nonempty choices and valid nonnegative
usage within ledger integer limits, with total equal to input plus output.
Missing or invalid usage is an explicit error, not fabricated provider usage.
HTTP 400–599 requires an error object and is returned with the original safe
status/body without a successful-generation charge. Native error completion is
recorded as failed; it does not masquerade as a successful model execution.

A detached owner preserves settlement if the caller disconnects. Transient
result-read errors are retried within the task deadline; completion notifications
trigger immediate writer reads, with a final read to resolve deadline races.
All modes still share normal platform authentication, admission and accounting.

Golden request fixtures are synchronized between KeyCompute and node-token.
Both repositories must be updated before a native-capable worker is registered.
Old workers remain eligible for legacy internal tasks, not public native Chat.
The repository's fresh-schema initialization rule applies to session metadata;
this phase does not upgrade retained production data or deploy running services.
