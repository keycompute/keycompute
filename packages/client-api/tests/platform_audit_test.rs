use client_api::{
    ApiClient, ClientConfig,
    api::platform_audit::{PlatformAuditApi, PlatformAuditQuery, PlatformAuditRecord},
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

#[tokio::test]
async fn platform_audit_uses_exact_tenant_and_request_filters() {
    let server = MockServer::start().await;
    let client = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    let api = PlatformAuditApi::new(&client);
    let tenant = Uuid::new_v4();
    let request = Uuid::new_v4();
    let event = Uuid::new_v4();
    let actor = Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path("/api/v1/platform/audit-events"))
        .and(query_param("tenant_id", tenant.to_string()))
        .and(query_param("request_id", request.to_string()))
        .and(query_param("cursor", "opaque+/cursor="))
        .and(query_param("page_size", "25"))
        .and(header("authorization", "Bearer operator-session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": event,
                "scope_type": "tenant",
                "tenant_id": tenant,
                "tenant_name": "Example",
                "tenant_slug": "example",
                "actor_user_id": actor,
                "actor_email": "operator@example.invalid",
                "actor_name": "Operator",
                "action": "membership.join",
                "resource_type": "tenant_membership",
                "resource_id": actor.to_string(),
                "request_id": request,
                "request_event_count": 2,
                "credential_kind": "jwt",
                "platform_role": "operator",
                "tenant_role": null,
                "metadata": {},
                "result": "success",
                "created_at": "2026-10-09T00:00:00Z"
            }],
            "next_cursor": null,
            "page_size": 25
        })))
        .expect(1)
        .mount(&server)
        .await;
    let page = api
        .list(
            &PlatformAuditQuery {
                tenant_id: Some(tenant),
                request_id: Some(request),
                cursor: Some("opaque+/cursor=".into()),
                page_size: 25,
            },
            "operator-session",
        )
        .await
        .unwrap();
    assert_eq!(page.page_size, 25);
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.items[0].request_event_count, 2);
    assert_eq!(page.items[0].request_id, Some(request));
    server.verify().await;
}

#[tokio::test]
async fn platform_audit_rejects_invalid_filters_paging_and_cursors_before_http() {
    let server = MockServer::start().await;
    let client = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    let api = PlatformAuditApi::new(&client);
    for query in [
        PlatformAuditQuery {
            tenant_id: Some(Uuid::nil()),
            ..Default::default()
        },
        PlatformAuditQuery {
            request_id: Some(Uuid::nil()),
            ..Default::default()
        },
        PlatformAuditQuery {
            page_size: 101,
            ..Default::default()
        },
        PlatformAuditQuery {
            cursor: Some("\n".into()),
            ..Default::default()
        },
    ] {
        assert!(api.list(&query, "operator-session").await.is_err());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn platform_audit_defaults_event_count_for_older_server_payloads() {
    let record: PlatformAuditRecord = serde_json::from_value(json!({
        "id": Uuid::new_v4(),
        "scope_type": "platform",
        "tenant_id": null,
        "tenant_name": null,
        "tenant_slug": null,
        "actor_user_id": Uuid::new_v4(),
        "actor_email": "operator@example.invalid",
        "actor_name": "Operator",
        "action": "tenant.update",
        "resource_type": "tenant",
        "resource_id": Uuid::new_v4().to_string(),
        "request_id": Uuid::new_v4(),
        "credential_kind": "jwt",
        "platform_role": "operator",
        "tenant_role": null,
        "metadata": {},
        "result": "success",
        "created_at": "2026-10-09T00:00:00Z"
    }))
    .unwrap();
    assert_eq!(record.request_event_count, 0);
}
