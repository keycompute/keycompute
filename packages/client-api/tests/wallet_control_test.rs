use client_api::{
    ApiClient, ClientConfig,
    api::wallet_control::{MoneyCommand, RecoveryCommand, WalletAction, WalletControlApi},
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};
async fn client() -> (ApiClient, MockServer) {
    let server = MockServer::start().await;
    (
        ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap(),
        server,
    )
}
#[tokio::test]
async fn exact_money_and_idempotency_key_use_only_the_explicit_platform_wallet() {
    let (client, server) = client().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = WalletControlApi::platform_tenant(&client, tenant).unwrap();
    for (action, suffix) in [
        (WalletAction::Adjust, ""),
        (WalletAction::Freeze, "/freeze"),
        (WalletAction::Unfreeze, "/unfreeze"),
    ] {
        Mock::given(method("POST")).and(path(format!("/api/v1/platform/tenants/{tenant}/users/{owner}/balance{suffix}")))
            .and(header("Idempotency-Key","same-logical-operation")).and(header("Authorization","Bearer token"))
            .and(body_json(json!({"amount":"0.0000000001","reason":"exact adjustment"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success":true,"message":"done","user_id":owner,
                "amount":"0.0000000001","reason":"exact adjustment","new_balance":"10.0000000001","updated_by":Uuid::new_v4()}))).expect(1).mount(&server).await;
        let result = api
            .change(
                owner,
                action,
                &MoneyCommand {
                    amount: "0.0000000001".into(),
                    reason: "exact adjustment".into(),
                },
                "same-logical-operation",
                "token",
            )
            .await
            .unwrap();
        assert_eq!(result.amount, "0.0000000001");
    }
}
#[tokio::test]
async fn tenant_reservation_reads_are_fresh_and_recovery_carries_its_original_version() {
    let (client, server) = client().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let request = Uuid::new_v4();
    let version = Uuid::new_v4();
    let api = WalletControlApi::tenant(&client, tenant).unwrap();
    Mock::given(method("GET")).and(path(format!("/api/v1/tenants/{tenant}/users/{owner}/balance/reservations")))
        .and(query_param("limit","1")).and(query_param("cursor","opaque+/="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user_id":owner,"available_balance":"7","total_frozen_balance":"3",
            "request_reserved_balance":"3","manually_frozen_balance":"0","reservations":[],"next_cursor":null}))).expect(2).mount(&server).await;
    for _ in 0..2 {
        assert_eq!(
            api.reservations(owner, Some("opaque+/="), 1, "token")
                .await
                .unwrap()
                .request_reserved_balance,
            "3"
        );
    }
    Mock::given(method("POST")).and(path(format!("/api/v1/tenants/{tenant}/users/{owner}/balance/reservations/{request}/release")))
        .and(body_json(json!({"expected_version":version,"reason":"expired work"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success":true,"message":"done","user_id":owner,"request_id":request,
            "released_amount":"3","reason":"expired work","new_available_balance":"10","new_total_frozen_balance":"0",
            "request_reserved_balance":"0","manually_frozen_balance":"0","released_by":Uuid::new_v4(),"warning":"Late usage may still be charged"}))).expect(1).mount(&server).await;
    assert!(
        api.release(
            owner,
            request,
            &RecoveryCommand {
                expected_version: version,
                reason: "expired work".into()
            },
            "token"
        )
        .await
        .unwrap()
        .success
    );
}
#[tokio::test]
async fn tenant_clients_cannot_construct_money_adjustment_requests_or_nil_targets() {
    let (client, server) = client().await;
    assert!(WalletControlApi::tenant(&client, Uuid::nil()).is_err());
    let api = WalletControlApi::tenant(&client, Uuid::new_v4()).unwrap();
    assert!(
        api.change(
            Uuid::new_v4(),
            WalletAction::Adjust,
            &MoneyCommand {
                amount: "1".into(),
                reason: "not a root client".into()
            },
            "key",
            "token"
        )
        .await
        .is_err()
    );
    assert!(
        api.reservations(Uuid::nil(), None, 10, "token")
            .await
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
