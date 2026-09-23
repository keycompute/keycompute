use client_api::{
    ApiClient, ClientConfig,
    api::platform_operations::{PlatformOperationsApi, TenantHealthQuery, UsageOperationsQuery},
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};
#[tokio::test]
async fn operational_tenant_metadata_reads_are_fresh_and_filter_values_are_encoded() {
    let server = MockServer::start().await;
    let client = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    let api = PlatformOperationsApi::new(&client);
    Mock::given(method("GET"))
        .and(path("/api/v1/platform/operations/tenants"))
        .and(query_param("search", "literal%_&=tenant"))
        .and(header("authorization", "Bearer operator-session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"items":[],"total":0,"limit":20,"offset":0,"as_of":"2026-09-23T00:00:00Z"}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    let query = TenantHealthQuery {
        search: Some("literal%_&=tenant".into()),
        ..Default::default()
    };
    for _ in 0..2 {
        assert_eq!(
            api.tenants(&query, "operator-session").await.unwrap().total,
            0
        );
    }
    server.verify().await;
}
#[tokio::test]
async fn platform_and_named_tenant_usage_have_distinct_routes_without_float_amounts() {
    let server = MockServer::start().await;
    let client = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    let api = PlatformOperationsApi::new(&client);
    let tenant = Uuid::new_v4();
    let output = json!({"from":"2026-09-22T00:00:00Z","to":"2026-09-23T00:00:00Z","as_of":"2026-09-23T00:00:00Z","currencies":[{"currency":"CNY","requests":1,"successful_requests":1,"total_tokens":"10000000000000000000","billed_amount":"0.1234567890"}]});
    for target in [
        "/api/v1/platform/operations/usage".to_owned(),
        format!("/api/v1/platform/operations/tenants/{tenant}/usage"),
    ] {
        Mock::given(method("GET"))
            .and(path(target))
            .respond_with(ResponseTemplate::new(200).set_body_json(output.clone()))
            .expect(1)
            .mount(&server)
            .await;
    }
    let query = UsageOperationsQuery::default();
    assert_eq!(
        api.platform_usage(&query, "operator")
            .await
            .unwrap()
            .currencies[0]
            .billed_amount,
        "0.1234567890"
    );
    assert_eq!(
        api.tenant_usage(tenant, &query, "operator")
            .await
            .unwrap()
            .currencies[0]
            .total_tokens,
        "10000000000000000000"
    );
    assert!(
        api.tenant_usage(Uuid::nil(), &query, "operator")
            .await
            .is_err()
    );
    assert!(api.tenant(Uuid::nil(), "operator").await.is_err());
    server.verify().await;
}
