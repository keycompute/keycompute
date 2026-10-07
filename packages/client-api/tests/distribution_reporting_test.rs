use client_api::api::distribution_reporting::DistributionReportingApi;
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, query_param},
};

mod common;
use common::{create_test_client, fixtures};

fn record(tenant: Uuid, id: Uuid, currency: &str, amount: &str) -> Value {
    json!({
        "id": id,
        "tenant_id": tenant,
        "beneficiary_scope": "everyone",
        "beneficiary_id": null,
        "usage_log_id": Uuid::new_v4(),
        "referred_id": Uuid::new_v4(),
        "amount": amount,
        "currency": currency,
        "commission": "0.1234000000",
        "share_ratio": "0.1000",
        "level": "direct",
        "status": "settled",
        "created_at": "2026-10-07T09:12:07.077086Z"
    })
}

#[tokio::test]
async fn platform_records_use_the_explicit_tenant_route_and_preserve_money_strings() {
    let (client, server) = create_test_client().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = DistributionReportingApi::platform_tenant(&client, tenant).unwrap();

    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/platform/distribution/tenants/{tenant}/records"
        )))
        .and(query_param("page", "2"))
        .and(query_param("page_size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "records": [record(tenant, id, "USD", "12.3400000000")],
            "total": 21,
            "page": 2,
            "page_size": 20,
            "total_pages": 2
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = api
        .records(2, 20, fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();
    assert_eq!(page.records[0].id, id);
    assert_eq!(page.records[0].currency, "USD");
    assert_eq!(page.records[0].amount, "12.3400000000");
    assert_eq!(page.records[0].commission, "0.1234000000");
}

#[tokio::test]
async fn platform_records_reject_cross_tenant_duplicate_and_invalid_page_data() {
    let (client, server) = create_test_client().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = DistributionReportingApi::platform_tenant(&client, tenant).unwrap();

    let invalid_pages = [
        json!({
            "records": [record(Uuid::new_v4(), id, "CNY", "1.00")],
            "total": 1, "page": 1, "page_size": 20, "total_pages": 1
        }),
        json!({
            "records": [record(tenant, id, "CNY", "1.00"), record(tenant, id, "CNY", "2.00")],
            "total": 2, "page": 1, "page_size": 20, "total_pages": 1
        }),
        json!({
            "records": [], "total": i64::MAX, "page": 1, "page_size": 20,
            "total_pages": i64::MAX
        }),
    ];

    for body in invalid_pages {
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/platform/distribution/tenants/{tenant}/records"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            api.records(1, 20, fixtures::TEST_ACCESS_TOKEN)
                .await
                .is_err()
        );
        server.reset().await;
    }

    assert!(
        api.records(0, 20, fixtures::TEST_ACCESS_TOKEN)
            .await
            .is_err()
    );
    assert!(
        api.records(1, 101, fixtures::TEST_ACCESS_TOKEN)
            .await
            .is_err()
    );
    assert!(DistributionReportingApi::platform_tenant(&client, Uuid::nil()).is_err());
}
