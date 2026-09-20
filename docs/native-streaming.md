# Native event streaming

The scoped native generation surface supports `stream: true` for node Chat,
Messages and Responses, and for passthrough Messages and Responses. Existing
account-pool routes and passthrough Chat retain their own execution paths.
The URL still selects `/v1`, `/pt/v1` or `/nt/v1`; model names are never routing
instructions and are not rewritten to add a `node:` prefix.

## Negotiation and transport

A node must advertise `sse` in the immutable profile for the requested model and
operation. Model discovery with `/nt/v1/models?...&stream=true` applies the same
requirement. Older workers remain eligible only for their previously declared
operations and features. No unsupported streaming request is downgraded to a
buffered response.

Task acquisition remains outbound polling. Workers post event envelopes to
`/node/v1/tasks/{task_id}/events`, using their authenticated session and the
issued task/lease identity. Each envelope has one contiguous sequence number:
`Start`, `Data`, then `Terminal` or `Failed`. Delivery retries reuse the same
serialized event and do not repeat inference.

The server waits for the upstream response head before sending an HTTP success
head. A native upstream HTTP error follows the existing bounded JSON result
path instead of being hidden inside an already-successful SSE response.

## Fidelity and accounting

Node data frames retain their original UTF-8 SSE bytes, including comments,
multiline data, line endings and extension fields. The parser recognizes a
leading byte-order mark without deleting it from the forwarded frame. Native
passthrough events retain their event type and complete protocol JSON rather
than being converted into generic text deltas.

Protocol completion is separate from transport EOF. Chat requires `[DONE]`
after every emitted choice has finished; its usage chunk may follow the
finish-reason chunk. Messages combines initial input usage with later output
usage. Responses reads usage from the terminal response object and distinguishes
completed, incomplete and failed outcomes. Mismatched models, response identities,
conflicting token totals and malformed terminal events cannot establish success.

Input and output accounting provenance are independent. An interrupted Messages
stream can have exact input usage and estimated output usage. Estimates are
marked as estimates, not presented as provider-reported counts. Partial output
and failed client delivery do not erase already observed execution usage.

## Bounds and delivery behavior

The native parser caps a frame at 256 KiB, a stream at 4,096 frames and raw stream
bytes at 8 MiB; negotiated model profiles can impose lower limits. The durable
unread window and HTTP delivery channel are bounded. Slow readers apply
backpressure, and cancellation or a deadline stops delivery rather than growing
unbounded buffers. A failure after the HTTP head cannot change that status;
it terminates the stream without synthesizing a successful protocol terminal.
