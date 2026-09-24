use client_api::{ApiClient, ClientConfig, api::tenant_reporting::*};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};
fn client(s: &MockServer) -> ApiClient {
    ApiClient::new(
        ClientConfig::new(s.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap()
}
fn window() -> ReportWindow {
    ReportWindow {
        from: "2026-01-01T00:00:00Z".into(),
        to: "2026-02-01T00:00:00Z".into(),
    }
}
fn usage(t: Uuid, u: Uuid, id: Uuid) -> Value {
    json!({"id":id,"request_id":Uuid::new_v4(),"tenant_id":t,"user_id":u,"produce_ai_key_id":Uuid::new_v4(),"model_name":"fixture","provider_name":"openai","account_id":Uuid::new_v4(),"input_tokens":1,"output_tokens":2,"total_tokens":3,"input_unit_price_snapshot":"1E-10","output_unit_price_snapshot":"0.0000000001","user_amount":"9999999999.9999999999","currency":"CNY","usage_source":"gateway_accumulated","status":"success","started_at":"now","finished_at":"now","created_at":"now"})
}
fn order(t: Uuid, u: Uuid, id: Uuid) -> Value {
    json!({"id":id,"tenant_id":t,"user_id":u,"amount":"123.0000000001","currency":"CNY","status":"pending","payment_method":"wechatpay","payment_scene":"native","paid_at":null,"closed_at":null,"expired_at":"later","created_at":"now","updated_at":"now","pay_url":"https://secret.invalid/private-payment","notify_data":{"secret":"do-not-expose"},"subject":"private"})
}
fn page(rows: Vec<Value>) -> Value {
    json!({"total":rows.len(),"items":rows,"page":1,"page_size":20,"total_pages":1})
}
#[tokio::test]
async fn tenant_financial_reads_are_fresh_exact_and_metadata_only() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let u = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantReportingApi::new(&client(&s), t).unwrap();
    let q = ReportQuery {
        owner_user_id: Some(u),
        ..Default::default()
    };
    let base = format!("/api/v1/tenants/{t}");
    Mock::given(method("GET"))
        .and(path(format!("{base}/billing/records")))
        .and(query_param("owner_user_id", u.to_string()))
        .and(query_param("from", window().from))
        .and(query_param("to", window().to))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![usage(t, u, id)])))
        .expect(2)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{base}/payments/orders")))
        .and(query_param("status", "pending"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![order(t, u, id)])))
        .expect(2)
        .mount(&s)
        .await;
    for _ in 0..2 {
        let r = api.usage(&q, &window(), "fixture").await.unwrap();
        assert_eq!(r.items[0].user_amount, "9999999999.9999999999");
        assert_eq!(r.items[0].input_unit_price_snapshot, "1E-10");
        let orders = api
            .payments(&q, Some(PaymentState::Pending), "fixture")
            .await
            .unwrap();
        let value = serde_json::to_value(&orders.items[0]).unwrap();
        assert_eq!(value["amount"], "123.0000000001");
        for name in ["pay_url", "notify_data", "subject"] {
            assert!(value.get(name).is_none());
        }
    }
    for req in s.received_requests().await.unwrap() {
        assert_eq!(req.method, "GET");
        assert!(!req.url.query_pairs().any(|(key, _)| key == "tenant_id"));
    }
}
#[tokio::test]
async fn foreign_members_rows_pages_and_numeric_float_fallback_are_rejected() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let u = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantReportingApi::new(&client(&s), t).unwrap();
    let q = ReportQuery {
        owner_user_id: Some(u),
        ..Default::default()
    };
    for field in [
        "tenant_id",
        "user_id",
        "user_amount",
        "page_size",
        "duplicate",
    ] {
        let mut row = usage(t, u, id);
        if field == "tenant_id" || field == "user_id" {
            row[field] = json!(Uuid::new_v4());
        }
        if field == "user_amount" {
            row[field] = json!(1.2);
        }
        let mut value = page(vec![row.clone()]);
        if field == "page_size" {
            value[field] = 100.into();
        }
        if field == "duplicate" {
            value = page(vec![row.clone(), row]);
        }
        s.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(value))
            .expect(1)
            .mount(&s)
            .await;
        assert!(
            api.usage(&q, &window(), "fixture").await.is_err(),
            "{field}"
        );
        s.verify().await;
    }
    s.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(order(t, u, id)))
        .mount(&s)
        .await;
    assert!(api.payment(Uuid::new_v4(), u, "fixture").await.is_err());
    assert!(api.payment(id, Uuid::new_v4(), "fixture").await.is_err());
}
#[tokio::test]
async fn summary_currencies_remain_separate_and_uninitialized_wallet_is_not_an_error_default() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let u = Uuid::new_v4();
    let api = TenantReportingApi::new(&client(&s), t).unwrap();
    let base = format!("/api/v1/tenants/{t}");
    let group = |c: &str| json!({"currency":c,"total_requests":9007199254740993i64,"total_input_tokens":1,"total_output_tokens":2,"total_tokens":3,"total_amount":"12345678901234567890.1234567890"});
    Mock::given(method("GET"))
        .and(path(format!("{base}/billing/stats")))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"from":window().from,"to":window().to,"currencies":[group("CNY"),group("USD")]}),
        ))
        .expect(2)
        .mount(&s)
        .await;
    Mock::given(method("GET")).and(path(format!("{base}/balances/{u}"))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"tenant_id":t,"user_id":u,"available_balance":"0","frozen_balance":"0","total_recharged":"0","total_consumed":"0","initialized":false,"as_of":"now"}))).expect(2).mount(&s).await;
    for _ in 0..2 {
        let totals = api.totals(&window(), Some(u), "fixture").await.unwrap();
        assert_eq!(totals.currencies.len(), 2);
        assert_eq!(totals.currencies[0].total_requests, 9007199254740993);
        assert_eq!(
            totals.currencies[0].total_amount,
            "12345678901234567890.1234567890"
        );
        assert!(!api.wallet(u, "fixture").await.unwrap().initialized);
    }
    s.verify().await;
    s.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error":{"message":"no accessible wallet"}})),
        )
        .mount(&s)
        .await;
    assert!(api.wallet(u, "fixture").await.is_err());
}
#[tokio::test]
async fn invalid_scope_and_pagination_never_dispatch_and_query_values_cannot_select_tenants() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let api = TenantReportingApi::new(&client(&s), t).unwrap();
    assert!(TenantReportingApi::new(&client(&s), Uuid::nil()).is_err());
    assert!(api.wallet(Uuid::nil(), "fixture").await.is_err());
    for q in [
        ReportQuery {
            page: 0,
            ..Default::default()
        },
        ReportQuery {
            page_size: 101,
            ..Default::default()
        },
        ReportQuery {
            owner_user_id: Some(Uuid::nil()),
            ..Default::default()
        },
    ] {
        assert!(api.usage(&q, &window(), "fixture").await.is_err());
    }
    assert!(s.received_requests().await.unwrap().is_empty());
    let mut w = window();
    w.to = "2026-02-01T00:00:00+08:00&tenant_id=foreign".into();
    Mock::given(method("GET"))
        .and(query_param("to", w.to.clone()))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error":{"message":"invalid timestamp"}})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.usage(&ReportQuery::default(), &w, "fixture")
            .await
            .is_err()
    );
    let requests = s.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]
            .url
            .query_pairs()
            .any(|(key, _)| key == "tenant_id")
    );
}
