use client_api::{ApiClient, ClientConfig, api::tenant_providers::*};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};
fn client(s: &MockServer) -> ApiClient {
    ApiClient::new(
        ClientConfig::new(s.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap()
}
fn account(t: Uuid, id: Uuid) -> serde_json::Value {
    json!({"id":id,"tenant_id":t,"tenant_active":true,"name":"tenant account","provider":"openai","api_key_preview":"sk-***","api_base":"https://example.invalid/v1","models":["m"],"api_capabilities":["responses"],"rpm_limit":60,"tpm_limit":1000,"current_rpm":0,"is_active":true,"is_healthy":true,"health_status":"healthy","health_penalty":0,"health_reason":null,"routing_eligible":true,"pool_enabled":false,"passthrough_binding_count":0,"last_probe_at":null,"last_probe_status":null,"last_probe_error_code":null,"priority":0,"visibility":"tenant","created_at":"2026-01-01T00:00:00Z","last_used_at":null})
}
fn binding(t: Uuid, id: Uuid, a: Uuid, rev: i64) -> serde_json::Value {
    json!({"id":id,"account_id":a,"account_name":"tenant account","tenant_id":t,"tenant_name":"Tenant","provider":"openai","is_global":false,"pool_enabled":false,"revision":rev,"models_supported":["m"],"health_status":"healthy","health_reason_code":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"})
}
#[tokio::test]
async fn fresh_lists_are_fixed_to_route_tenant_and_reject_foreign_rows() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantProviderApi::new(&client(&s), t).unwrap();
    let base = format!("/api/v1/tenants/{t}/accounts");
    Mock::given(method("GET"))
        .and(path(&base))
        .and(query_param("page", "1"))
        .and(query_param("page_size", "20"))
        .and(query_param("search", "a&tenant_id=foreign"))
        .and(query_param("provider", "openai"))
        .and(query_param("status", "active"))
        .and(header("authorization", "Bearer fixture"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"accounts":[account(t,id)],"total":1,"page":1,"page_size":20,"total_pages":1}),
        ))
        .expect(2)
        .mount(&s)
        .await;
    for _ in 0..2 {
        assert_eq!(
            api.accounts(1, 20, "a&tenant_id=foreign", "openai", "active", "fixture")
                .await
                .unwrap()
                .accounts[0]
                .id,
            id
        );
    }
    assert!(TenantProviderApi::new(&client(&s), Uuid::nil()).is_err());
    s.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(account(Uuid::new_v4(), id)))
        .mount(&s)
        .await;
    assert!(api.account_detail(id, "fixture").await.is_err());
}
#[tokio::test]
async fn account_controls_match_wire_contract_and_do_not_replay() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantProviderApi::new(&client(&s), t).unwrap();
    let base = format!("/api/v1/tenants/{t}/accounts");
    let create = CreateTenantAccount {
        name: "a".into(),
        provider: "openai".into(),
        api_key: "secret".into(),
        api_base: None,
        models: vec!["m".into()],
        api_capabilities: Some(vec!["responses".into()]),
        rpm_limit: Some(60),
        tpm_limit: Some(1000),
        priority: Some(0),
        pool_enabled: Some(false),
    };
    Mock::given(method("POST")).and(path(&base)).and(body_json(json!({"name":"a","provider":"openai","api_key":"secret","models":["m"],"api_capabilities":["responses"],"rpm_limit":60,"tpm_limit":1000,"priority":0,"pool_enabled":false}))).respond_with(ResponseTemplate::new(200).set_body_json(account(t,id))).expect(1).mount(&s).await;
    assert_eq!(api.create_account(&create, "fixture").await.unwrap().id, id);
    s.reset().await;
    Mock::given(method("PUT"))
        .and(path(format!("{base}/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(account(t, id)))
        .expect(1)
        .mount(&s)
        .await;
    assert_eq!(
        api.update_account(
            id,
            &UpdateTenantAccount {
                name: Some("b".into()),
                ..Default::default()
            },
            "fixture"
        )
        .await
        .unwrap()
        .id,
        id
    );
    s.reset().await;
    Mock::given(method("POST"))
        .and(path(format!("{base}/{id}/test")))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({"error":{"message":"uncertain"}})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(api.test_account(id, "fixture").await.is_err());
    s.verify().await;
    assert!(
        api.update_account(
            id,
            &UpdateTenantAccount {
                priority: Some(11),
                ..Default::default()
            },
            "fixture"
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn binding_revision_and_owner_scope_are_preserved() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let a = Uuid::new_v4();
    let api = TenantProviderApi::new(&client(&s), t).unwrap();
    let base = format!("/api/v1/tenants/{t}/passthrough-bindings");
    Mock::given(method("POST"))
        .and(path(&base))
        .and(body_json(json!({"account_id":a,"pool_enabled":false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(binding(t, id, a, 1)))
        .expect(1)
        .mount(&s)
        .await;
    assert_eq!(
        api.create_binding(
            &CreateTenantBinding {
                account_id: a,
                pool_enabled: false
            },
            "fixture"
        )
        .await
        .unwrap()
        .revision,
        1
    );
    s.reset().await;
    Mock::given(method("PUT"))
        .and(path(format!("{base}/{id}")))
        .and(body_json(
            json!({"pool_enabled":true,"expected_revision":1}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(binding(t, id, a, 2)))
        .expect(1)
        .mount(&s)
        .await;
    assert_eq!(
        api.update_binding(
            id,
            &UpdateTenantBinding {
                account_id: None,
                pool_enabled: Some(true),
                expected_revision: 1
            },
            "fixture"
        )
        .await
        .unwrap()
        .revision,
        2
    );
    s.reset().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{base}/{id}")))
        .and(query_param("expected_revision", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"deleted":true,"binding_id":id})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(api.delete_binding(id, 2, "fixture").await.unwrap().deleted);
    assert!(
        api.update_binding(
            id,
            &UpdateTenantBinding {
                account_id: None,
                pool_enabled: None,
                expected_revision: 0
            },
            "fixture"
        )
        .await
        .is_err()
    );
    s.reset().await;
    let mut foreign = binding(t, id, a, 1);
    foreign["is_global"] = true.into();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"bindings":[foreign],"total":1,"page":1,"page_size":20,"total_pages":1}),
        ))
        .mount(&s)
        .await;
    assert!(api.bindings(1, 20, "", "fixture").await.is_err());
}
