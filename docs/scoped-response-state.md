# Platform-managed Responses state

KeyCompute now manages Responses resources for both `/pt/v1` and `/nt/v1`.
Ollama and other selected execution targets still receive native stateless
Responses requests. This does not assert that Ollama implements stored responses
or conversations. Ordinary `/v1` Responses retain their separate implementation.

## Public operations

Use the same mode-specific base URL throughout a resource's lifetime:

| Method | Relative path | Operation |
| --- | --- | --- |
| POST | `/responses` | Generate, store, continue, or start background work |
| GET | `/responses/{id}` | Retrieve a retained response |
| GET | `/responses/{id}?stream=true&starting_after=N` | Replay a retained event stream |
| DELETE | `/responses/{id}` | Delete the resource; stop active execution |
| GET | `/responses/{id}/input_items` | Paginate the resolved input |
| POST | `/responses/{id}/cancel` | Cancel active execution |
| POST | `/conversations` | Create a conversation |
| GET, POST/PATCH, DELETE | `/conversations/{id}` | Retrieve, update metadata, or delete |
| GET, POST | `/conversations/{id}/items` | List or append items |
| GET, DELETE | `/conversations/{id}/items/{item_id}` | Retrieve or remove one item |

List operations accept `after`, `limit` (1–100), and `order` (`asc` or `desc`).
Streaming replay uses the platform event sequence, not an upstream SSE ID.
Invalid or foreign cursors are rejected rather than restarting a generation.

## Request semantics and isolation

A foreground request defaults to stored mode. Explicit `store:false` without a
previous response or conversation preserves the existing raw stateless path.
`previous_response_id` and `conversation` are mutually exclusive. Previous input,
previous output and new input are replayed exactly once; previous top-level
instructions are not inherited. A supplied current instruction remains intact.

Conversation turns append current input and completed/incomplete output once.
An active conversation rejects concurrent item edits or another turn with 409.
Cancellation/deletion does not append a late successful result. Public item IDs
are separate from the original native bodies, so tool-call IDs and arguments
remain unchanged when replayed to the executor.

KeyCompute consumes its state references and sends the resolved input with
`store:false` and `background:false` to the selected execution target. Other
native fields, tool structures and extension values are preserved. Caller-supplied
resource metadata remains authoritative even if the stateless backend returns
null metadata. Platform
response IDs replace only protocol response identity fields, not arbitrary
nested vendor values or tool IDs. SSE retains native events and publishes its
terminal event after durable resource completion and accounting handoff.

Resources are private to the authenticated tenant, user and execution mode.
Another key belonging to the same user may access them; another user, tenant or
mode receives 404. A PT resource also requires a current grant to its original
account, independently of that grant's account-pool participation setting.
Continuations never silently move private history to another account.

## Background execution, retention and recovery

`background:true` returns a queued resource promptly. Poll its ID or subscribe to
its stored events. Disconnecting that subscription does not cancel background
work. `POST .../cancel` explicitly ends it; repeated cancellation is idempotent.
A cancelled resource cannot later become completed. Already observed inference
usage can still be settled; cancellation is not a promise of zero cost.

Stored responses are retained for 30 days after completion. Conversations use a
30-day retention window refreshed by explicit metadata/item mutations. Background requests without explicit `store:true`
use temporary 10-minute polling retention. Explicit foreground `store:false`
results are not publicly retrievable after completion. Idempotency-key replay is
scoped by user, tenant and mode while the resource/tombstone remains retained;
a different request body with the same live key conflicts instead of generating
again. Deletion and expiry never themselves trigger inference.

Storage is bounded: 32 active responses, 1,024 retained responses and 256
conversations per user/mode; 512 history items and 2 MiB reconstructed request;
8 MiB response/event storage and at most 4,096 events per response. Conversation
item mutations accept at most 20 new items. Limits return errors, not silent
history truncation. Cleanup removes expired payloads in bounded batches.

A short ownership heartbeat fences the execution owner. The maintenance loop
claims stale owners, reconciles a persisted native result/observed usage, or
marks an uncertain interrupted execution failed. It never submits a replacement
model request. Accounting remains independently idempotent by request identity.
This provides bounded recovery, not a distributed exactly-once execution claim.

## Worker compatibility and client examples

Managed non-stream NodeDispatch requires a worker advertising the `cancellation`
feature for the exact model/operation. Such workers check the issued lease via
`POST /node/v1/tasks/{id}/lease-status` and close the single local HTTP operation
when it is cancelled. Session credentials are frozen per leased task even when
the active registration rotates. SSE retains its event/lease cancellation path.
Old workers remain eligible for stateless operations they actually support.
`/nt/v1/models?capability=responses&managed=true` exposes this narrower view;
combine `stream=true` to select native SSE workers.

The API-key guide offers stateless, stored, conversation and background examples
under the existing account-pool / passthrough / node selection. Resource polling
uses GET and does not submit another generation request. A stored two-turn flow:

```python
from openai import OpenAI

client = OpenAI(base_url="https://your-service.example/nt/v1",
                api_key="YOUR_PLATFORM_KEY", max_retries=0)
first = client.responses.create(model="your-raw-model", input="Hello", store=True)
second = client.responses.create(model="your-raw-model", input="Continue",
                                 previous_response_id=first.id,
                                 instructions="Answer briefly", store=True)
print(client.responses.retrieve(second.id))
```

This phase updates the greenfield `001_init.sql` baseline and schema assertions;
it is not a retained-data upgrade migration. No deployment or live model probe
is performed by applying this source change.
