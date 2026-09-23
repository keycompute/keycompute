use client_api::{
    ApiClient, ClientConfig,
    api::settings::{SettingsApi, UpdateNodeTipRatio},
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

#[tokio::test]
async fn ratio_wire_preserves_decimal_revision_reason_and_console_credential() {
    let server = MockServer::start().await;
    let client = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    let api = SettingsApi::new(&client);
    let version = "2026-09-23T00:00:00Z";
    Mock::given(method("GET"))
        .and(path("/api/v1/platform/tips/settings/ratio"))
        .and(header("authorization", "Bearer root-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ratio":"0.9000","updated_at":version})),
        )
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        assert_eq!(
            api.get_node_tip_ratio("root-token").await.unwrap().ratio,
            "0.9000"
        );
    }
    Mock::given(method("PUT"))
        .and(path("/api/v1/platform/tips/settings/ratio"))
        .and(body_json(
            json!({"ratio":"0.1234","expected_updated_at":version,"reason":"new platform policy"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ratio":"0.1234","updated_at":"2026-09-23T00:00:01Z"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let row = api
        .update_node_tip_ratio(
            &UpdateNodeTipRatio {
                ratio: "0.1234".into(),
                expected_updated_at: version.into(),
                reason: "new platform policy".into(),
            },
            "root-token",
        )
        .await
        .unwrap();
    assert_eq!(row.ratio, "0.1234");
    server.verify().await;
}
#[tokio::test]
async fn settings_reads_are_fresh_and_keys_cannot_change_the_target_route() {
    let server = MockServer::start().await;
    let client = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    let api = SettingsApi::new(&client);
    Mock::given(method("GET")).and(path("/api/v1/platform/settings/site_name"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key":"site_name","value":"KeyCompute","value_type":"string","description":null}))).expect(2).mount(&server).await;
    for _ in 0..2 {
        api.get_system_setting_by_key("site_name", "token")
            .await
            .unwrap();
    }
    for key in ["../users", "site_name?tenant=other", "", "site_name/secret"] {
        assert!(api.get_system_setting_by_key(key, "token").await.is_err());
        assert!(
            api.update_system_setting_by_key(key, &json!("x"), "token")
                .await
                .is_err()
        );
    }
    server.verify().await;
}
