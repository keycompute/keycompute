use client_api::api::node_control::{NodeCommand, NodeControlApi, NodeListQuery, NodeOperation};
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};
mod common;
use common::{create_test_client, fixtures};
fn node(id: Uuid, tenant: Uuid) -> serde_json::Value {
    json!({"id":id,"tenant_id":tenant,"owner_user_id":Uuid::new_v4(),"display_name":"Node","status":"excluded","consecutive_failure_count":0,"failure_threshold":3,"last_heartbeat_at":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00.123456Z"})
}
#[tokio::test]
async fn platform_node_operation_keeps_explicit_target_and_exact_revision() {
    let (client, server) = create_test_client().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = NodeControlApi::platform_tenant(&client, t).unwrap();
    let command = NodeCommand {
        expected_updated_at: "2026-01-02T00:00:00.123456Z".into(),
        reason: "maintenance".into(),
    };
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/platform/tenants/{t}/nodes/{id}/exclude"
        )))
        .and(header(
            "authorization",
            format!("Bearer {}", fixtures::TEST_ACCESS_TOKEN),
        ))
        .and(body_json(
            json!({"expected_updated_at":command.expected_updated_at,"reason":command.reason}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"node":node(id,t),"changed":true,"deleted":false})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = api
        .operate(
            id,
            NodeOperation::Exclude,
            &command,
            fixtures::TEST_ACCESS_TOKEN,
        )
        .await
        .unwrap();
    assert_eq!(result.node.id, id);
    assert!(result.changed);
    assert!(NodeControlApi::tenant(&client, Uuid::nil()).is_err());
}
#[tokio::test]
async fn node_list_and_delete_preserve_filters_and_escape_revision_query() {
    let (client, server) = create_test_client().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = NodeControlApi::tenant(&client, t).unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{t}/nodes")))
        .and(query_param("search", "节点_%"))
        .and(query_param("page_size", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"items":[node(id,t)],"total":2,"page":1,"page_size":1,"total_pages":2}),
        ))
        .mount(&server)
        .await;
    assert_eq!(
        api.nodes(
            &NodeListQuery {
                search: Some("节点_%".into()),
                page_size: Some(1),
                ..Default::default()
            },
            fixtures::TEST_ACCESS_TOKEN
        )
        .await
        .unwrap()
        .total,
        2
    );
    let cmd = NodeCommand {
        expected_updated_at: "2026-01-02T08:00:00.123456+08:00".into(),
        reason: "unused & offline".into(),
    };
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/tenants/{t}/nodes/{id}")))
        .and(query_param("expected_updated_at", &cmd.expected_updated_at))
        .and(query_param("reason", &cmd.reason))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"node":node(id,t),"changed":true,"deleted":true})),
        )
        .mount(&server)
        .await;
    assert!(
        api.delete(id, &cmd, fixtures::TEST_ACCESS_TOKEN)
            .await
            .unwrap()
            .deleted
    );
    assert!(
        NodeControlApi::personal(&client)
            .delete(id, &cmd, fixtures::TEST_ACCESS_TOKEN)
            .await
            .is_err()
    );
}
