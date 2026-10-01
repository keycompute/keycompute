use client_api::{
    ApiClient, ClientConfig,
    api::{admin::BalanceReservationInfo, tenant_financial_control::*},
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};
fn client(s: &MockServer) -> ApiClient {
    ApiClient::new(
        ClientConfig::new(s.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap()
}
fn reservation(owner: Uuid, request: Uuid, version: Uuid) -> serde_json::Value {
    json!({"user_id":owner,"available_balance":"7.0000000001","total_frozen_balance":"3","request_reserved_balance":"2","manually_frozen_balance":"1","reservations":[{"request_id":request,"version":version,"amount":"2.0000000001","status":"active","expires_at":"2026-01-01T00:00:00Z","created_at":"2025-12-01T00:00:00Z","updated_at":"2025-12-01T00:00:00Z"}],"next_cursor":null})
}
fn withdrawal(
    t: Uuid,
    id: Uuid,
    owner: Uuid,
    request: Uuid,
    status: &str,
    revision: i64,
) -> serde_json::Value {
    json!({"id":id,"tenant_id":t,"owner_user_id":owner,"request_id":request,"withdrawal_type":"alipay","total_amount":"99.0000000001","currency":"CNY","status":status,"payout_details_present":true,"admin_id":null,"admin_remark":null,"payout_reference":null,"balance_transaction_id":null,"revision":revision,"actioned_at":null,"completed_at":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"})
}
#[tokio::test]
async fn reservation_scope_and_observed_version_are_exact() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let request = Uuid::new_v4();
    let version = Uuid::new_v4();
    let api = TenantFinancialControlApi::new(&client(&s), t).unwrap();
    let base = format!("/api/v1/tenants/{t}/users/{owner}/balance/reservations");
    Mock::given(method("GET"))
        .and(path(&base))
        .and(query_param("limit", "20"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(reservation(owner, request, version)),
        )
        .expect(2)
        .mount(&s)
        .await;
    for _ in 0..2 {
        assert_eq!(
            api.reservations(owner, None, 20, "fixture")
                .await
                .unwrap()
                .reservations[0]
                .version,
            version.to_string()
        );
    }
    s.reset().await;
    Mock::given(method("POST")).and(path(format!("{base}/{request}/release"))).and(body_json(json!({"expected_version":version,"reason":"expired request recovery"}))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"success":true,"message":"released","user_id":owner,"request_id":request,"released_amount":"2.0000000001","reason":"expired request recovery","new_available_balance":"9.0000000002","new_total_frozen_balance":"1","request_reserved_balance":"0","manually_frozen_balance":"1","released_by":Uuid::new_v4(),"warning":"late usage may still debit"}))).expect(1).mount(&s).await;
    let row = BalanceReservationInfo {
        request_id: request.to_string(),
        version: version.to_string(),
        amount: "2.0000000001".into(),
        status: "active".into(),
        expires_at: "past".into(),
        created_at: "past".into(),
        updated_at: "past".into(),
    };
    assert!(
        api.release_expired(owner, &row, "expired request recovery", "fixture")
            .await
            .unwrap()
            .success
    );
    s.verify().await;
}
#[tokio::test]
async fn foreign_reservation_owner_and_bad_versions_fail_closed() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api = TenantFinancialControlApi::new(&client(&s), t).unwrap();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reservation(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        )))
        .mount(&s)
        .await;
    assert!(api.reservations(owner, None, 20, "fixture").await.is_err());
    let row = BalanceReservationInfo {
        request_id: Uuid::new_v4().to_string(),
        version: "not-version".into(),
        amount: "2".into(),
        status: "active".into(),
        expires_at: "past".into(),
        created_at: "past".into(),
        updated_at: "past".into(),
    };
    assert!(
        api.release_expired(owner, &row, "reason", "fixture")
            .await
            .is_err()
    );
}
#[tokio::test]
async fn withdrawal_review_is_tenant_scoped_revision_bound_and_single_dispatch() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let request = Uuid::new_v4();
    let api = TenantFinancialControlApi::new(&client(&s), t).unwrap();
    let base = format!("/api/v1/tenants/{t}/tips/withdrawals");
    Mock::given(method("GET"))
        .and(path(&base))
        .and(query_param("currency", "CNY"))
        .and(query_param("status", "pending"))
        .and(query_param("limit", "20"))
        .and(query_param("offset", "0"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"items":[withdrawal(t,id,owner,request,"pending",1)],"total":1}),
            ),
        )
        .expect(1)
        .mount(&s)
        .await;
    let row = api
        .withdrawals(
            &WithdrawalQuery {
                status: Some("pending".into()),
                ..Default::default()
            },
            "fixture",
        )
        .await
        .unwrap()
        .items
        .remove(0);
    s.reset().await;
    Mock::given(method("POST"))
        .and(path(format!("{base}/{id}/approve")))
        .and(body_json(
            json!({"expected_revision":1,"reason":"tenant approval"}),
        ))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({"error":{"message":"uncertain"}})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.review(
            &row,
            WithdrawalDecision::Approve,
            "tenant approval",
            "fixture"
        )
        .await
        .is_err()
    );
    s.verify().await;
}
#[tokio::test]
async fn withdrawal_secrets_foreign_rows_and_immutable_drift_are_rejected() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let req = Uuid::new_v4();
    let api = TenantFinancialControlApi::new(&client(&s), t).unwrap();
    let mut secret = withdrawal(t, id, owner, req, "pending", 1);
    secret["encrypted_alipay_account"] = "ciphertext".into();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[secret],"total":1})))
        .mount(&s)
        .await;
    assert!(
        api.withdrawals(&Default::default(), "fixture")
            .await
            .is_err()
    );
    s.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"items":[withdrawal(Uuid::new_v4(),id,owner,req,"pending",1)],"total":1}),
        ))
        .mount(&s)
        .await;
    assert!(
        api.withdrawals(&Default::default(), "fixture")
            .await
            .is_err()
    );
}
#[tokio::test]
async fn stale_reservation_and_withdrawal_projection_drift_fail_closed() {
    let s = MockServer::start().await;
    let t = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let request = Uuid::new_v4();
    let version = Uuid::new_v4();
    let api = TenantFinancialControlApi::new(&client(&s), t).unwrap();

    let mut stale = reservation(owner, request, version);
    stale["reservations"][0]["status"] = "released".into();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(stale))
        .expect(1)
        .mount(&s)
        .await;
    assert!(api.reservations(owner, None, 20, "fixture").await.is_err());

    s.reset().await;
    let id = Uuid::new_v4();
    let mut approved = withdrawal(t, id, owner, request, "approved", 2);
    approved["admin_id"] = Uuid::new_v4().to_string().into();
    approved["admin_remark"] = "reviewed".into();
    approved["actioned_at"] = "2026-01-01T00:00:01Z".into();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[approved],"total":1})),
        )
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.withdrawals(
            &WithdrawalQuery {
                status: Some("pending".into()),
                ..Default::default()
            },
            "fixture",
        )
        .await
        .is_err()
    );

    s.reset().await;
    let pending: TenantWithdrawal =
        serde_json::from_value(withdrawal(t, id, owner, request, "pending", 1)).unwrap();
    let mut impossible = withdrawal(t, id, owner, request, "approved", 2);
    impossible["admin_id"] = Uuid::new_v4().to_string().into();
    impossible["admin_remark"] = "tenant approval".into();
    impossible["actioned_at"] = "2026-01-01T00:00:01Z".into();
    impossible["payout_reference"] = "must-only-exist-on-completion".into();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(impossible))
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.review(
            &pending,
            WithdrawalDecision::Approve,
            "tenant approval",
            "fixture"
        )
        .await
        .is_err()
    );
    s.verify().await;
}

#[tokio::test]
async fn reservation_pages_reject_alias_duplicates_and_excess_rows() {
    let s = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let request = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let version = Uuid::new_v4();
    let api = TenantFinancialControlApi::new(&client(&s), tenant).unwrap();
    for duplicate in [true, false] {
        let mut page = reservation(owner, request, version);
        let mut second = page["reservations"][0].clone();
        second["request_id"] = if duplicate {
            request.to_string().to_uppercase().into()
        } else {
            Uuid::new_v4().to_string().into()
        };
        page["reservations"].as_array_mut().unwrap().push(second);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page))
            .expect(1)
            .mount(&s)
            .await;
        let limit = if duplicate { 20 } else { 1 };
        assert!(
            api.reservations(owner, None, limit, "fixture")
                .await
                .is_err()
        );
        s.verify().await;
        s.reset().await;
    }
}
