use client_api::{AdminApi, ClientError, api::admin::MonitoringQuery};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};
mod common;
#[tokio::test]
async fn monitoring_reads_use_canonical_fresh_platform_paths_and_literal_filters() {
    let (client, server) = common::create_test_client().await;
    let api = AdminApi::new(&client);
    Mock::given(method("GET"))
        .and(path("/api/v1/platform/monitoring/requests"))
        .and(query_param("tenant_id", "tenant&user_id=foreign"))
        .and(header("authorization", "Bearer fixture"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"items":[],"next_cursor":null})),
        )
        .expect(2)
        .mount(&server)
        .await;
    let query = MonitoringQuery {
        tenant_id: Some("tenant&user_id=foreign".into()),
        ..Default::default()
    };
    api.monitoring_requests(&query, "fixture").await.unwrap();
    api.monitoring_requests(&query, "fixture").await.unwrap();
    for id in [
        "../summary",
        "not-a-uuid",
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert!(matches!(
            api.monitoring_request(id, "fixture").await,
            Err(ClientError::Config(_))
        ));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
#[tokio::test]
async fn monitoring_probe_remains_single_dispatch_on_uncertain_failure() {
    let (client, server) = common::create_test_client().await;
    let api = AdminApi::new(&client);
    let id = uuid::Uuid::new_v4().to_string();
    Mock::given(method("POST"))
        .and(path("/api/v1/platform/monitoring/targets/probe"))
        .and(body_json(serde_json::json!({"account_ids":[id.clone()]})))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(serde_json::json!({"error":"uncertain"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        api.probe_monitoring_targets(Some(vec![id]), "fixture")
            .await
            .is_err()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
