use client_api::{
    ApiClient, ClientConfig,
    api::response_control::{
        AppendItemsCommand, MetadataCommand, ResourceListQuery, ResponseControlApi, ResponseMode,
        RevisionCommand,
    },
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};

async fn client() -> (ApiClient, MockServer) {
    let server = MockServer::start().await;
    let client =
        ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).expect("client");
    (client, server)
}

#[tokio::test]
async fn tenant_response_routes_are_explicit() {
    let (client, server) = client().await;
    let tenant = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let owner = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let api = ResponseControlApi::tenant(&client, tenant).unwrap();

    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{tenant}/responses")))
        .and(query_param("mode", "passthrough"))
        .and(query_param("owner_user_id", owner.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [], "total": 0, "page": 1, "page_size": 20, "total_pages": 0
        })))
        .mount(&server)
        .await;

    let page = api
        .responses(
            &ResourceListQuery {
                mode: Some(ResponseMode::Passthrough),
                owner_user_id: Some(owner),
                page: Some(1),
                page_size: Some(20),
                reason: None,
            },
            "token",
        )
        .await
        .unwrap();
    assert_eq!(page.total, 0);

    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{tenant}/responses/count")))
        .and(query_param("mode", "passthrough"))
        .and(query_param("owner_user_id", owner.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"total": 2})))
        .mount(&server)
        .await;

    let count = api
        .response_count(
            &ResourceListQuery {
                mode: Some(ResponseMode::Passthrough),
                owner_user_id: Some(owner),
                page: None,
                page_size: None,
                reason: None,
            },
            "token",
        )
        .await
        .unwrap();
    assert_eq!(count.total, 2);
}

#[tokio::test]
async fn platform_detail_carries_bounded_reason_in_query() {
    let (client, server) = client().await;
    let tenant = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let owner = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let api = ResponseControlApi::platform_tenant(&client, tenant).unwrap();

    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/platform/tenants/{tenant}/responses/node_dispatch/{owner}/resp_1"
        )))
        .and(query_param("reason", "incident review"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "summary": {
                "id":"resp_1","tenant_id":tenant,"owner_user_id":owner,
                "mode":"node_dispatch","provider":null,"account_id":null,"model":"m",
                "status":"completed","background":false,"store_response":true,"stream":false,
                "previous_response_id":null,"conversation_id":null,"revision":3,
                "created_at":"2026-01-01T00:00:00Z",
                "updated_at":"2026-01-01T00:00:01Z",
                "expires_at":"2026-02-01T00:00:00Z",
                "deleted":false,"local_content_available":true,"native_content_available":false
            },
            "response": {"id":"resp_1"}
        })))
        .mount(&server)
        .await;

    let detail = api
        .response(
            ResponseMode::NodeDispatch,
            owner,
            "resp_1",
            Some("incident review"),
            "token",
        )
        .await
        .unwrap();
    assert_eq!(detail.summary.revision, Some(3));
}

#[tokio::test]
async fn mutations_send_expected_revision_and_reason() {
    let (client, server) = client().await;
    let tenant = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let owner = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let api = ResponseControlApi::platform_tenant(&client, tenant).unwrap();

    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v1/platform/tenants/{tenant}/responses/passthrough/{owner}/resp_2"
        )))
        .and(body_json(
            json!({"expected_revision":7,"reason":"operator cleanup"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"resp_2","object":"response","deleted":true
        })))
        .mount(&server)
        .await;

    let deleted = api
        .delete_response(
            ResponseMode::Passthrough,
            owner,
            "resp_2",
            &RevisionCommand {
                expected_revision: 7,
                reason: Some("operator cleanup".into()),
            },
            "token",
        )
        .await
        .unwrap();
    assert!(deleted.deleted);
}

#[tokio::test]
async fn conversation_item_routes_are_canonical() {
    let (client, server) = client().await;
    let tenant = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let owner = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let api = ResponseControlApi::tenant(&client, tenant).unwrap();

    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/tenants/{tenant}/conversations/node_dispatch/{owner}/conv_1/items"
        )))
        .and(body_json(
            json!({"expected_revision":4,"items":[{"role":"user","content":"hi"}]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"conv_1","object":"conversation","created_at":1760000000,"metadata":{}
        })))
        .mount(&server)
        .await;

    let value = api
        .append_conversation_items(
            ResponseMode::NodeDispatch,
            owner,
            "conv_1",
            &AppendItemsCommand {
                expected_revision: 4,
                items: vec![json!({"role":"user","content":"hi"})],
                reason: None,
            },
            "token",
        )
        .await
        .unwrap();
    assert_eq!(value["id"], "conv_1");

    let body = MetadataCommand {
        expected_revision: 5,
        metadata: json!({"case":"closed"}),
        reason: None,
    };
    assert_eq!(body.expected_revision, 5);
}

#[test]
fn content_commands_have_redacted_diagnostics() {
    use client_api::api::response_control::{AppendItemsCommand, MetadataCommand};
    let metadata = MetadataCommand {
        expected_revision: 1,
        metadata: serde_json::json!({"private":"never-print-control-content"}),
        reason: None,
    };
    let append = AppendItemsCommand {
        expected_revision: 1,
        items: vec![serde_json::json!({"content":"never-print-control-content"})],
        reason: None,
    };
    assert!(!format!("{metadata:?}").contains("never-print"));
    assert!(!format!("{append:?}").contains("never-print"));
}
