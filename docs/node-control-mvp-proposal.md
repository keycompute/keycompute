# Node Control MVP Proposal

## Background

KeyCompute currently works well as a centralized gateway for cloud providers and self-hosted endpoints that are directly reachable by the server. This model is suitable for:

- Cloud APIs such as OpenAI, Claude, Gemini, and DeepSeek
- Self-hosted endpoints deployed in the same machine, the same LAN, or a directly reachable server

However, this model does not map well to consumer PCs running long-lived local models behind NAT or home networks.

Example:

- A user keeps a multimodal model such as `qwen3-vl-8b` running on a home PC
- The public KeyCompute server cannot reliably call that machine through a static HTTP endpoint
- Even when reachable through tunneling, direct server-to-node calls increase operational complexity and weaken stability

This proposal introduces a new capability called `Node Control`, where user-owned nodes actively register themselves to KeyCompute, report capabilities, and pull tasks from the server for local execution.

## Goals

- Enable user-owned consumer PCs to participate as execution nodes
- Allow nodes to proactively register and advertise model capabilities
- Let KeyCompute coordinate API requests with appropriate nodes based on capability matching
- Keep the first version simple by using node pull-based task claiming instead of server push
- Reuse KeyCompute as the central control plane for auth, scheduling, task state, and result aggregation

## Non-Goals

- Full decentralized compute marketplace
- NAT traversal platform
- Token-level streaming in the first version
- Revenue settlement, reputation, or sandbox isolation in the first version
- Replacing the existing provider/account routing model for directly reachable endpoints

## High-Level Design

The proposal adds a new execution mode alongside existing provider routing:

1. A `node-agent` runs on a user machine
2. The node registers itself to KeyCompute and periodically sends heartbeats
3. The node reports its supported models and capabilities
4. When KeyCompute receives an API request, it can route the request to a matching execution node
5. Nodes poll the server for claimable tasks
6. A node claims a task, executes it locally through Ollama or vLLM, and uploads the result
7. KeyCompute returns the final response or exposes async task lookup

This keeps KeyCompute as the control plane and turns consumer PCs into pull-based workers.

## Why Pull-Based Nodes For MVP

For the first version, node pull is intentionally preferred over server push:

- Easier to implement and debug
- Works behind NAT without requiring inbound connectivity
- Avoids introducing long-lived bidirectional connections at the start
- Simplifies task recovery when a node goes offline
- Fits consumer PC reliability characteristics better

## Example Use Case

- A user runs `qwen3-vl-8b` on a home PC
- The local agent registers the machine as a node with multimodal capability
- KeyCompute receives a multimodal request
- The scheduler identifies that this node supports `qwen3-vl-8b`
- The node polls and claims the task
- The local agent executes the request via the local model runtime
- The node posts the result back to KeyCompute

## Proposed Architecture

### Control Plane: KeyCompute

KeyCompute should coordinate:

- Node registration
- Node heartbeat and liveness tracking
- Capability matching
- Task creation and assignment
- Task state persistence
- Result aggregation
- API response handling

### Execution Plane: Node Agent

The node agent should handle:

- Node bootstrap and registration
- Capability reporting
- Periodic heartbeat
- Polling for claimable tasks
- Claiming tasks atomically
- Local execution via Ollama or vLLM
- Result upload

## Minimal Data Model

### nodes

- `id`
- `name`
- `owner_user_id`
- `status` (`online`, `offline`, `busy`)
- `last_heartbeat_at`
- `max_concurrency`
- `current_load`
- `labels_json`
- `created_at`
- `updated_at`

### node_capabilities

- `id`
- `node_id`
- `provider_type` (`ollama`, `vllm`, etc.)
- `models_supported`
- `supports_multimodal`
- `supports_stream`
- `metadata_json`

For MVP this can also be embedded into a single JSON field on `nodes`.

### node_tasks

- `id`
- `request_id`
- `task_type`
- `required_provider_type`
- `required_model`
- `required_capabilities_json`
- `status` (`queued`, `claimed`, `running`, `succeeded`, `failed`, `timeout`)
- `assigned_node_id`
- `payload_json`
- `result_json`
- `error_message`
- `created_at`
- `claimed_at`
- `started_at`
- `finished_at`
- `expires_at`

## Minimal API Surface

### Node APIs

- `POST /api/v1/nodes/register`
- `POST /api/v1/nodes/heartbeat`
- `POST /api/v1/nodes/tasks/request`
- `POST /api/v1/nodes/tasks/{id}/claim`
- `POST /api/v1/nodes/tasks/{id}/running`
- `POST /api/v1/nodes/tasks/{id}/complete`

### Task Query API

- `GET /api/v1/tasks/{id}`

This allows clients to inspect task state for long-running jobs.

## Suggested Task Flow

### Fast Path

1. Client sends request to KeyCompute
2. KeyCompute creates a task
3. Matching node claims and executes quickly
4. KeyCompute waits briefly for completion
5. If the result arrives within the wait window, KeyCompute returns it synchronously

### Slow Path

1. Client sends request to KeyCompute
2. KeyCompute creates a task
3. KeyCompute waits for a short timeout
4. If no result arrives, KeyCompute returns `task_id`
5. Client polls `GET /api/v1/tasks/{id}` later

This hybrid model preserves simple API ergonomics while supporting slower consumer nodes.

## Matching Strategy For MVP

A task is eligible for a node when:

- The node is online
- The node has free concurrency
- The node declares the required provider type
- The node supports the requested model
- The node satisfies required capability flags such as multimodal support

This keeps scheduling intentionally simple for the first release.

## Integration With Existing Architecture

This proposal does not need to replace the current provider/account model.

Instead, it can add a second execution path:

- Existing path: route to directly reachable provider account
- New path: route to node-backed execution target

That allows KeyCompute to support both:

- Standard cloud/API gateway use cases
- User-owned node execution use cases

## Suggested Implementation Phases

### Phase 1: MVP

- Add node registration and heartbeat
- Add capability reporting
- Add task table and polling-based claim flow
- Add a minimal node agent
- Support non-streaming task execution and final result upload

### Phase 2

- Add richer scheduling rules
- Add better node load tracking
- Add retries and lease expiration recovery
- Add admin UI for node visibility

### Phase 3

- Add streaming result support
- Add settlement and usage attribution
- Add reputation, trust, and node policy controls

## Benefits

- Makes consumer PCs a first-class execution option
- Avoids requiring public inbound access to home machines
- Preserves KeyCompute as the central coordinator
- Opens a path toward hybrid cloud plus edge execution
- Provides a simpler MVP path than a full message-broker marketplace architecture

## Open Questions

- Should node capabilities live in a dedicated table or JSON for the first release
- Should task claiming use SQL row locking first, or introduce Redis-backed leases immediately
- Should node agents authenticate with static tokens, signed registration, or short-lived credentials
- Should node-backed execution appear as a new provider type in the admin UI, or as a separate resource

## Summary

This proposal adds a practical control-plane feature for user-owned execution nodes.

It enables a home PC running a model such as `qwen3-vl-8b` to participate in KeyCompute without requiring the public server to directly call that machine. By using pull-based task claiming, KeyCompute can remain the central coordinator while nodes perform local inference and report results back to the server.
