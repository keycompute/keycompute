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

#[tokio::test]
async fn task_commands_preserve_personal_scope_and_explicit_platform_target() {
    use client_api::api::node_control::{NodeCommand, NodeControlApi, TaskOperation};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, ResponseTemplate};
    let (client, server) = common::create_test_client().await;
    let tenant = uuid::Uuid::new_v4();
    let id = uuid::Uuid::new_v4();
    let owner = uuid::Uuid::new_v4();
    let revision = "2026-09-22T13:00:00.000001Z";
    let command = NodeCommand {
        expected_updated_at: revision.into(),
        reason: "fixture task control".into(),
    };
    let mut result = serde_json::json!({"task":{"id":id,"request_id":uuid::Uuid::new_v4(),"tenant_id":tenant,"user_id":owner,"model":"fixture","status":"failed","assigned_node_id":null,"failure_count":0,"failure_threshold":3,"queued_at":revision,"claimed_at":null,"finished_at":revision,"deadline_at":revision,"created_at":revision,"updated_at":revision,"cancellation_requested_at":revision,"archived_at":null},"changed":true,"cancellation_requested":true,"archived":false});
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/me/tasks/{id}/cancel")))
        .and(body_json(&command))
        .respond_with(ResponseTemplate::new(200).set_body_json(result.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let cancelled = NodeControlApi::personal(&client)
        .operate_task(id, TaskOperation::Cancel, &command, "fixture")
        .await
        .unwrap();
    assert!(cancelled.cancellation_requested);
    assert_eq!(cancelled.task.user_id, owner);
    assert!(!cancelled.archived);
    result["task"]["archived_at"] = revision.into();
    result["archived"] = true.into();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/platform/tenants/{tenant}/tasks/{id}/archive"
        )))
        .and(body_json(&command))
        .respond_with(ResponseTemplate::new(200).set_body_json(result))
        .expect(1)
        .mount(&server)
        .await;
    let archived = NodeControlApi::platform_tenant(&client, tenant)
        .unwrap()
        .operate_task(id, TaskOperation::Archive, &command, "fixture")
        .await
        .unwrap();
    assert!(archived.archived);
    assert_eq!(archived.task.tenant_id, tenant);
}
