use client_api::{ApiClient, ClientConfig, api::tenant_pricing::*};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};
fn client(server: &MockServer) -> ApiClient {
    ApiClient::new(
        ClientConfig::new(server.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap()
}
fn row(tenant: Uuid, id: Uuid, version: i64) -> Value {
    json!({"id":id,"scope_type":"tenant","tenant_id":tenant,"model_name":"model-fixture","billing_dimension":"provideraccount","currency":"CNY","input_price_per_1k":"0.0000000001","output_price_per_1k":"9999999999.9999999999","is_default":false,"is_effective":true,"effective_from":"2026-01-01T00:00:00Z","effective_until":null,"created_at":"2026-01-01T00:00:00Z","version":version})
}
fn create() -> CreateTenantPrice {
    CreateTenantPrice {
        model_name: "model-fixture".into(),
        billing_dimension: BillingDimension::ProviderAccount,
        currency: "CNY".into(),
        input_price_per_1k: "0.0000000001".into(),
        output_price_per_1k: "9999999999.9999999999".into(),
        is_default: false,
        effective_from: None,
        effective_until: None,
    }
}
#[tokio::test]
async fn tenant_prices_use_fresh_scoped_reads_and_literal_search() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantPricingApi::new(&client(&server), tenant).unwrap();
    let search = "a&tenant_id=foreign%_*";
    let url = format!("/api/v1/tenants/{tenant}/pricing");
    Mock::given(method("GET"))
        .and(path(&url))
        .and(query_param("page", "1"))
        .and(query_param("page_size", "20"))
        .and(query_param("search", search))
        .and(header("authorization", "Bearer fixture-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"pricing":[row(tenant,id,7)],"total":1,"page":1,"page_size":20,"total_pages":1}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        let result = api.list(1, 20, search, "fixture-token").await.unwrap();
        assert_eq!(result.pricing[0].version, 7);
        assert_eq!(result.pricing[0].input_price_per_1k, "0.0000000001");
    }
    assert!(TenantPricingApi::new(&client(&server), Uuid::nil()).is_err());
    assert!(api.list(0, 20, "", "fixture-token").await.is_err());
    assert!(api.list(1, 101, "", "fixture-token").await.is_err());
    assert!(api.detail(Uuid::nil(), "fixture-token").await.is_err());
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    for req in reqs {
        assert!(req.url.query_pairs().all(|(k, _)| k != "tenant_id"));
    }
}
#[tokio::test]
async fn foreign_platform_missing_version_or_mismatched_resources_fail_closed() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantPricingApi::new(&client(&server), tenant).unwrap();
    for mut value in [
        row(Uuid::new_v4(), id, 1),
        row(tenant, Uuid::new_v4(), 1),
        row(tenant, id, 0),
        row(tenant, id, 1),
    ] {
        if value["tenant_id"] == json!(tenant) && value["id"] == json!(id) && value["version"] == 1
        {
            value["scope_type"] = "platform".into();
            value["tenant_id"] = Value::Null;
        }
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(value))
            .expect(1)
            .mount(&server)
            .await;
        assert!(api.detail(id, "fixture").await.is_err());
        server.verify().await;
    }
    server.reset().await;
    let mut missing = row(tenant, id, 1);
    missing.as_object_mut().unwrap().remove("version");
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(missing))
        .mount(&server)
        .await;
    assert!(api.detail(id, "fixture").await.is_err());
}
#[tokio::test]
async fn create_patch_delete_and_default_match_the_actual_tenant_wire_contract() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantPricingApi::new(&client(&server), tenant).unwrap();
    let base = format!("/api/v1/tenants/{tenant}/pricing");
    let create = create();
    Mock::given(method("POST")).and(path(&base)).and(body_json(json!({"model_name":"model-fixture","billing_dimension":"provideraccount","currency":"CNY","input_price_per_1k":"0.0000000001","output_price_per_1k":"9999999999.9999999999","is_default":false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(row(tenant,id,1))).expect(1).mount(&server).await;
    assert_eq!(api.create(&create, "fixture").await.unwrap().id, id);
    let patch = UpdateTenantPrice {
        expected_version: 1,
        input_price_per_1k: Some("0.0000000002".into()),
        output_price_per_1k: None,
        effective_until: None,
    };
    Mock::given(method("PATCH"))
        .and(path(format!("{base}/{id}")))
        .and(body_json(
            json!({"expected_version":1,"input_price_per_1k":"0.0000000002"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(row(tenant, id, 2)))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(api.update(id, &patch, "fixture").await.unwrap().version, 2);
    let mut default = row(tenant, id, 3);
    default["is_default"] = true.into();
    Mock::given(method("POST"))
        .and(path(format!("{base}/{id}/make-default")))
        .respond_with(ResponseTemplate::new(200).set_body_json(default))
        .expect(1)
        .mount(&server)
        .await;
    assert!(api.make_default(id, "fixture").await.unwrap().is_default);
    Mock::given(method("DELETE"))
        .and(path(format!("{base}/{id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"success":true,"pricing_id":id})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(api.delete(id, "fixture").await.unwrap().success);
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}
#[tokio::test]
async fn control_errors_are_not_automatically_replayed_and_bad_prices_do_not_dispatch() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantPricingApi::new(&client(&server), tenant).unwrap();
    let base = format!("/api/v1/tenants/{tenant}/pricing");
    Mock::given(method("POST"))
        .and(path(&base))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({"error":{"message":"uncertain"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(api.create(&create(), "fixture").await.is_err());
    Mock::given(method("PATCH"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error":{"message":"expired"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut patch = UpdateTenantPrice {
        expected_version: 1,
        input_price_per_1k: Some("1".into()),
        output_price_per_1k: None,
        effective_until: None,
    };
    assert!(api.update(id, &patch, "fixture").await.is_err());
    patch.expected_version = 0;
    assert!(api.update(id, &patch, "fixture").await.is_err());
    for bad in [
        "-1",
        "NaN",
        "Infinity",
        "0.00000000001",
        "10000000000",
        "1.1.1",
        "",
    ] {
        let mut req = create();
        req.input_price_per_1k = bad.into();
        assert!(api.create(&req, "fixture").await.is_err());
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
