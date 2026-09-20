//! Durable native stream storage against the isolated PostgreSQL fixture.
//! No provider or node process is mocked: claims and event authorization use
//! the same NodeGatewayStore transactions as production.

use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::db::{
    TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::DbRouter;
use keycompute_db::models::{
    node::{CreateNodeRequest, Node},
    node_session::{CreateNodeSessionRequest, NodeSession},
    node_task::NodeTask,
};
use keycompute_types::{
    node::{
        NodeCapabilities, NodeModelCapability, NodeNativeStreamEvent, NodeTaskPayload,
        NodeTaskResult, NodeTaskStreamEventRequest,
    },
    node_capability::{NativeFeature, NativeModelProfile},
    node_native::{NodeNativeHttpResult, NodeNativeOperation, NodeNativeRequest},
};
use node_gateway::{config::NodeGatewayAppConfig, store::NodeGatewayStore};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use serial_test::serial;
use uuid::Uuid;

const MODEL: &str = "native-stream-store-model";

struct Fixture {
    pool: sea_orm::DatabaseConnection,
    _cleanup: TestDataGuard,
    store: NodeGatewayStore,
    caller_id: Uuid,
    node: Node,
    session: NodeSession,
}

impl Fixture {
    async fn new(with_sse: bool) -> Self {
        let pool = create_test_pool().await;
        let run = Uuid::new_v4().to_string();
        let cleanup = TestDataGuard::new(pool.clone(), run.clone());
        let owner_tenant = create_test_tenant(&pool, "native-stream-owner", &run).await;
        let caller_tenant = create_test_tenant(&pool, "native-stream-caller", &run).await;
        let owner = create_test_user(&pool, owner_tenant.id, "native-stream-owner", &run).await;
        let caller = create_test_user(&pool, caller_tenant.id, "native-stream-caller", &run).await;
        let mut profile =
            NativeModelProfile::plain_chat(MODEL).with_features(vec![NativeFeature::Sse]);
        if !with_sse {
            profile.features.clear();
        }
        let caps = NodeCapabilities {
            runtime: "ollama".into(),
            runtime_version: Some("test".into()),
            models: vec![NodeModelCapability {
                model: MODEL.into(),
            }],
            native_operations: vec![NodeNativeOperation::Chat],
            native_profiles: vec![profile.clone()],
        };
        let node = Node::create(
            &pool,
            &CreateNodeRequest {
                owner_user_id: owner.id,
                client_instance_id: format!("native-stream-{run}"),
                display_name: "native stream test node".into(),
                capabilities_json: serde_json::to_value(&caps).unwrap(),
            },
        )
        .await
        .unwrap();
        let session = NodeSession::create(
            &pool,
            &CreateNodeSessionRequest {
                node_id: node.id,
                session_token_hash: format!("test-{run}"),
                expires_at: Utc::now() + ChronoDuration::hours(1),
                accepted_models_json: json!([MODEL]),
                native_operations_json: json!(["chat"]),
                native_profiles_json: json!([profile]),
            },
        )
        .await
        .unwrap();
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE nodes SET status='online',last_heartbeat_at=NOW() WHERE id=$1",
            [node.id.into()],
        ))
        .await
        .unwrap();
        let store = NodeGatewayStore::new(
            DbRouter::single(pool.clone()),
            NodeGatewayAppConfig::default(),
        );
        Self {
            pool,
            _cleanup: cleanup,
            store,
            caller_id: caller.id,
            node,
            session,
        }
    }

    async fn leased_task(&self) -> (NodeTask, Uuid) {
        let task = self
            .store
            .create_and_enqueue_task(self.caller_id, MODEL.into(), payload(Uuid::new_v4()))
            .await
            .unwrap();
        let (leased, envelope) = self
            .store
            .claim_task(task.id, self.node.id, self.session.id)
            .await
            .unwrap()
            .expect("SSE profile should claim the task");
        assert_eq!(leased.id, task.id);
        (leased, envelope.lease_id)
    }
}

fn payload(request_id: Uuid) -> NodeTaskPayload {
    NodeTaskPayload {
        request_id,
        chat: None,
        image_generation: None,
        image_edit: None,
        native: Some(NodeNativeRequest {
            operation: NodeNativeOperation::Chat,
            body: json!({
                "model": MODEL,
                "messages": [{"role":"user","content":"hello"}],
                "stream": true
            }),
            headers: vec![],
        }),
    }
}

fn event(
    fixture: &Fixture,
    task_id: Uuid,
    lease_id: Uuid,
    seq: u64,
    event: NodeNativeStreamEvent,
) -> NodeTaskStreamEventRequest {
    NodeTaskStreamEventRequest {
        protocol_version: "node.v1".into(),
        node_id: fixture.node.id,
        session_id: fixture.session.id,
        task_id,
        lease_id,
        seq,
        event,
    }
}

fn start() -> NodeNativeStreamEvent {
    NodeNativeStreamEvent::Start {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        body: None,
    }
}

fn data(content: &str, finish_reason: Option<&str>) -> NodeNativeStreamEvent {
    NodeNativeStreamEvent::Data {
        frame: format!(
            "data: {}\n\n",
            json!({
                "id": "chatcmpl-native-store",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": MODEL,
                "choices": [{"index":0,"delta":{"content":content},"finish_reason":finish_reason}]
            })
        ),
    }
}

fn done() -> NodeNativeStreamEvent {
    NodeNativeStreamEvent::Data {
        frame: "data: [DONE]\n\n".into(),
    }
}

#[tokio::test]
#[serial(native_stream_store)]
async fn durable_stream_cursor_window_dedup_and_terminal_completion() -> anyhow::Result<()> {
    let fixture = Fixture::new(true).await;
    let (task, lease_id) = fixture.leased_task().await;

    let start_request = event(&fixture, task.id, lease_id, 0, start());
    assert!(
        fixture
            .store
            .accept_native_stream_event(start_request.clone())
            .await?
            .accepted
    );
    let duplicate = fixture
        .store
        .accept_native_stream_event(start_request.clone())
        .await?;
    assert!(duplicate.accepted);
    let conflict = fixture
        .store
        .accept_native_stream_event(event(&fixture, task.id, lease_id, 0, data("wrong", None)))
        .await;
    assert_eq!(
        conflict.unwrap_err().to_string(),
        "native_stream_sequence_conflict"
    );
    let gap = fixture
        .store
        .accept_native_stream_event(event(&fixture, task.id, lease_id, 2, data("gap", None)))
        .await;
    assert_eq!(gap.unwrap_err().to_string(), "native_stream_sequence_gap");

    let first_data = event(&fixture, task.id, lease_id, 1, data("hello", None));
    assert!(
        fixture
            .store
            .accept_native_stream_event(first_data.clone())
            .await?
            .accepted
    );
    let read = fixture
        .store
        .read_native_stream_events(task.id, None)
        .await?;
    assert_eq!(read.len(), 2);
    fixture
        .store
        .acknowledge_native_stream_event(task.id, lease_id, 0)
        .await?;
    assert!(
        fixture
            .store
            .read_native_stream_events(task.id, None)
            .await?
            .iter()
            .all(|item| item.seq != 0)
    );
    // The hash remains after JSON consumption, so an identical retry is ACKed
    // without creating a second row.
    assert!(
        fixture
            .store
            .accept_native_stream_event(start_request)
            .await?
            .accepted
    );
    assert_eq!(
        fixture
            .store
            .read_native_stream_events(task.id, None)
            .await?
            .len(),
        1
    );
    let wrong_lease = fixture
        .store
        .acknowledge_native_stream_event(task.id, Uuid::new_v4(), 1)
        .await;
    assert_eq!(
        wrong_lease.unwrap_err().to_string(),
        "native_stream_lease_mismatch"
    );

    // Fill the bounded window: Start + 63 data events is full; the next data
    // event is a retryable backpressure response and succeeds after an ACK.
    for seq in 2..=65 {
        let response = fixture
            .store
            .accept_native_stream_event(event(&fixture, task.id, lease_id, seq, data("x", None)))
            .await?;
        if seq < 65 {
            assert!(response.accepted);
        } else {
            assert!(!response.accepted && response.retry_after_ms == Some(50));
        }
    }
    fixture
        .store
        .acknowledge_native_stream_event(task.id, lease_id, 1)
        .await?;
    assert!(
        fixture
            .store
            .accept_native_stream_event(event(&fixture, task.id, lease_id, 65, data("x", None)))
            .await?
            .accepted
    );
    fixture
        .store
        .acknowledge_native_stream_event(task.id, lease_id, 2)
        .await?;

    let finish = fixture
        .store
        .accept_native_stream_event(event(
            &fixture,
            task.id,
            lease_id,
            66,
            data("", Some("stop")),
        ))
        .await?;
    assert!(finish.accepted);
    fixture
        .store
        .acknowledge_native_stream_event(task.id, lease_id, 3)
        .await?;
    assert!(
        fixture
            .store
            .accept_native_stream_event(event(&fixture, task.id, lease_id, 67, done()))
            .await?
            .accepted
    );
    let summary = fixture.store.native_stream_summary(task.id).await?.unwrap();
    assert!(
        fixture
            .store
            .accept_native_stream_event(event(
                &fixture,
                task.id,
                lease_id,
                68,
                NodeNativeStreamEvent::Terminal {
                    summary: summary.clone()
                },
            ))
            .await?
            .accepted
    );
    let completion = fixture
        .store
        .complete_task(
            task.id,
            lease_id,
            fixture.node.id,
            fixture.session.id,
            NodeTaskResult::NativeStreamSucceeded {
                summary: summary.clone(),
            },
        )
        .await?;
    assert_eq!(
        completion.action,
        keycompute_types::node::NodeTaskCompleteAction::Succeeded
    );
    let stored = NodeTask::find_by_id(&fixture.pool, task.id).await?.unwrap();
    assert_eq!(stored.status, "succeeded");
    // A successful task cannot be revived by a different terminal variant.
    let late = fixture.store.complete_task(
        task.id,
        lease_id,
        fixture.node.id,
        fixture.session.id,
        NodeTaskResult::NativeSucceeded { response: NodeNativeHttpResult { status: 200, headers: vec![], body: json!({"model":MODEL,"choices":[],"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}}) } },
    ).await;
    assert!(late.is_err());
    Ok(())
}

#[tokio::test]
#[serial(native_stream_store)]
async fn stream_authorization_profile_revocation_and_cancel_persist_partial_usage()
-> anyhow::Result<()> {
    let fixture = Fixture::new(true).await;
    let (task, lease_id) = fixture.leased_task().await;
    assert!(
        fixture
            .store
            .accept_native_stream_event(event(&fixture, task.id, lease_id, 0, start()))
            .await?
            .accepted
    );
    assert!(
        fixture
            .store
            .accept_native_stream_event(event(
                &fixture,
                task.id,
                lease_id,
                1,
                data("partial", None)
            ))
            .await?
            .accepted
    );
    let partial = fixture.store.native_stream_summary(task.id).await?.unwrap();
    assert!(partial.usage.is_some());
    fixture
        .store
        .cancel_native_stream(task.id, "client_disconnect")
        .await?;
    let canceled = NodeTask::find_by_id(&fixture.pool, task.id).await?.unwrap();
    assert_eq!(canceled.status, "failed");
    let persisted = fixture.store.native_stream_summary(task.id).await?.unwrap();
    assert!(persisted.usage.is_some());

    let late = fixture
        .store
        .complete_task(
            task.id,
            lease_id,
            fixture.node.id,
            fixture.session.id,
            NodeTaskResult::NativeStreamSucceeded { summary: persisted },
        )
        .await;
    assert!(late.is_err(), "canceled task must not be revived");

    let (revoked_task, revoked_lease) = fixture.leased_task().await;
    NodeSession::revoke(&fixture.pool, fixture.session.id).await?;
    let denied = fixture
        .store
        .accept_native_stream_event(event(&fixture, revoked_task.id, revoked_lease, 0, start()))
        .await;
    assert_eq!(
        denied.unwrap_err().to_string(),
        "native_stream_session_inactive"
    );
    Ok(())
}

#[tokio::test]
#[serial(native_stream_store)]
async fn stream_profile_and_preheader_native_error_variants_are_distinct() -> anyhow::Result<()> {
    let no_sse = Fixture::new(false).await;
    let task = no_sse
        .store
        .create_and_enqueue_task(no_sse.caller_id, MODEL.into(), payload(Uuid::new_v4()))
        .await?;
    assert!(
        no_sse
            .store
            .claim_task(task.id, no_sse.node.id, no_sse.session.id)
            .await?
            .is_none()
    );

    let fixture = Fixture::new(true).await;
    let (task, lease_id) = fixture.leased_task().await;
    let mut wrong_session = event(&fixture, task.id, lease_id, 0, start());
    wrong_session.session_id = Uuid::new_v4();
    assert_eq!(
        fixture
            .store
            .accept_native_stream_event(wrong_session)
            .await
            .unwrap_err()
            .to_string(),
        "native_stream_lease_mismatch"
    );
    let wrong = fixture
        .store
        .accept_native_stream_event(event(&fixture, task.id, Uuid::new_v4(), 0, start()))
        .await;
    assert_eq!(
        wrong.unwrap_err().to_string(),
        "native_stream_lease_mismatch"
    );
    let json_error = fixture
        .store
        .complete_task(
            task.id,
            lease_id,
            fixture.node.id,
            fixture.session.id,
            NodeTaskResult::NativeSucceeded {
                response: NodeNativeHttpResult {
                    status: 429,
                    headers: vec![],
                    body: json!({"error":{"message":"rate limited"}}),
                },
            },
        )
        .await?;
    assert_eq!(
        json_error.action,
        keycompute_types::node::NodeTaskCompleteAction::Failed
    );
    assert!(
        fixture
            .store
            .native_stream_summary(task.id)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
#[serial(native_stream_store)]
async fn concurrent_duplicate_event_has_one_durable_record() -> anyhow::Result<()> {
    let f = Fixture::new(true).await;
    let (task, lease) = f.leased_task().await;
    let request = event(&f, task.id, lease, 0, start());
    let (a, b) = tokio::join!(
        f.store.accept_native_stream_event(request.clone()),
        f.store.accept_native_stream_event(request)
    );
    assert!(a?.accepted && b?.accepted);
    assert_eq!(
        f.store
            .read_native_stream_events(task.id, None)
            .await?
            .len(),
        1
    );
    let row = f
        .pool
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT next_seq,unread_frames FROM node_native_streams WHERE task_id=$1",
            [task.id.into()],
        ))
        .await?
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "next_seq")?, 1);
    assert_eq!(row.try_get::<i32>("", "unread_frames")?, 1);
    Ok(())
}

#[tokio::test]
#[serial(native_stream_store)]
async fn empty_delivery_window_accepts_one_maximum_escaped_frame() -> anyhow::Result<()> {
    let f = Fixture::new(true).await;
    let (task, lease) = f.leased_task().await;
    f.store
        .accept_native_stream_event(event(&f, task.id, lease, 0, start()))
        .await?;
    f.store
        .acknowledge_native_stream_event(task.id, lease, 0)
        .await?;
    let raw = format!(
        ": {}\n\n",
        "\\".repeat(keycompute_types::node_stream::MAX_NATIVE_SSE_FRAME_BYTES - 4)
    );
    let request = event(
        &f,
        task.id,
        lease,
        1,
        NodeNativeStreamEvent::Data { frame: raw.clone() },
    );
    assert!(serde_json::to_vec(&request.event)?.len() > 512 * 1024);
    assert!(f.store.accept_native_stream_event(request).await?.accepted);
    f.store
        .acknowledge_native_stream_event(task.id, lease, 1)
        .await?;
    assert!(
        f.store
            .accept_native_stream_event(event(&f, task.id, lease, 2, data("after", None)))
            .await?
            .accepted
    );
    Ok(())
}

#[tokio::test]
#[serial(native_stream_store)]
async fn advertised_native_response_budget_is_enforced() -> anyhow::Result<()> {
    let f = Fixture::new(true).await;
    let mut profile = NativeModelProfile::plain_chat(MODEL).with_features(vec![NativeFeature::Sse]);
    profile.max_response_bytes = 512;
    f.pool
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET native_profiles_json=$2 WHERE id=$1",
            [f.session.id.into(), json!([profile]).into()],
        ))
        .await?;
    let (task, lease) = f.leased_task().await;
    f.store
        .accept_native_stream_event(event(&f, task.id, lease, 0, start()))
        .await?;
    let error = f
        .store
        .accept_native_stream_event(event(&f, task.id, lease, 1, data(&"x".repeat(600), None)))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "native_stream_total_limit");
    assert_eq!(
        f.store
            .read_native_stream_events(task.id, None)
            .await?
            .len(),
        1
    );
    Ok(())
}
