use client_api::api::distribution_policy::{
    BeneficiaryScope, CreatePolicy, DistributionPolicyApi, PolicyPatch, validate_commission_rate,
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};
mod common;
use common::{create_test_client, fixtures};
fn row(t: Uuid, id: Uuid) -> serde_json::Value {
    json!({"id":id,"tenant_id":t,"beneficiary_scope":"everyone","beneficiary_id":null,"name":"policy","description":null,"commission_rate":"0.1250","priority":1,"is_active":true,"effective_from":"2026-01-01T00:00:00Z","effective_until":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"})
}
#[tokio::test]
async fn patch_and_delete_keep_exact_revisions_and_nullable_expiration() {
    let (c, s) = create_test_client().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = DistributionPolicyApi::tenant(&c, t).unwrap();
    let revision = "2026-01-01T00:00:00.123456Z";
    let mut patch = PolicyPatch::new(revision, "adjust future allocation");
    patch.effective_until = Some(None);
    patch.commission_rate = Some("0.1250".into());
    Mock::given(method("PATCH")).and(path(format!("/api/v1/tenants/{t}/distribution/rules/{id}"))).and(body_json(json!({"expected_updated_at":revision,"reason":"adjust future allocation","effective_until":null,"commission_rate":"0.1250"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(row(t,id))).expect(1).mount(&s).await;
    let r = api
        .patch(id, &patch, fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();
    assert_eq!(r.commission_rate, "0.1250");
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/tenants/{t}/distribution/rules/{id}")))
        .and(body_json(
            json!({"expected_updated_at":r.updated_at,"reason":"obsolete"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":id,"deleted":true})))
        .expect(1)
        .mount(&s)
        .await;
    assert!(
        api.delete(id, &r.updated_at, "obsolete", fixtures::TEST_ACCESS_TOKEN)
            .await
            .unwrap()
            .deleted
    );
}
#[tokio::test]
async fn platform_target_is_explicit_and_never_falls_back_to_selected_tenant() {
    let (c, s) = create_test_client().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    assert!(DistributionPolicyApi::tenant(&c, Uuid::nil()).is_err());
    let api = DistributionPolicyApi::platform_tenant(&c, t).unwrap();
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/platform/distribution/tenants/{t}/rules/default"
        )))
        .and(body_json(
            json!({"name":"policy","commission_rate":"0.1250","reason":"explicit root update"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(row(t, id)))
        .expect(1)
        .mount(&s)
        .await;
    assert_eq!(
        api.set_default(
            "policy",
            "0.1250",
            "explicit root update",
            fixtures::TEST_ACCESS_TOKEN
        )
        .await
        .unwrap()
        .tenant_id,
        t
    );
    assert!(api.list(0, 10, fixtures::TEST_ACCESS_TOKEN).await.is_err());
}

#[tokio::test]
async fn fresh_reads_validate_exact_tenant_page_ids_and_beneficiary_shape() {
    let (c, s) = create_test_client().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = DistributionPolicyApi::tenant(&c, t).unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{t}/distribution/rules")))
        .and(query_param("page", "1"))
        .and(query_param("page_size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"rules":[row(t,id)],"total":1,"page":1,"page_size":20,"total_pages":1}),
        ))
        .expect(2)
        .mount(&s)
        .await;
    for _ in 0..2 {
        let p = api.list(1, 20, fixtures::TEST_ACCESS_TOKEN).await.unwrap();
        assert_eq!(p.rules[0].id, id);
    }
    s.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(row(Uuid::new_v4(), id)))
        .expect(1)
        .mount(&s)
        .await;
    assert!(api.get(id, fixtures::TEST_ACCESS_TOKEN).await.is_err());
    s.reset().await;
    let mut bad = row(t, id);
    bad["beneficiary_scope"] = json!("tenant_member");
    bad["beneficiary_id"] = serde_json::Value::Null;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(bad))
        .expect(1)
        .mount(&s)
        .await;
    assert!(api.get(id, fixtures::TEST_ACCESS_TOKEN).await.is_err());
}
#[tokio::test]
async fn create_is_exact_and_invalid_policy_input_never_dispatches() {
    let (c, s) = create_test_client().await;
    let t = Uuid::new_v4();
    let id = Uuid::new_v4();
    let member = Uuid::new_v4();
    let api = DistributionPolicyApi::tenant(&c, t).unwrap();
    let body = CreatePolicy {
        name: "member allocation".into(),
        description: None,
        commission_rate: "0.1250".into(),
        beneficiary_scope: BeneficiaryScope::TenantMember,
        beneficiary_id: Some(member),
        priority: 1,
        effective_from: None,
        effective_until: None,
        reason: "future allocation".into(),
    };
    let mut expected = row(t, id);
    expected["beneficiary_scope"] = json!("tenant_member");
    expected["beneficiary_id"] = json!(member);
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/tenants/{t}/distribution/rules")))
        .respond_with(ResponseTemplate::new(200).set_body_json(expected))
        .expect(1)
        .mount(&s)
        .await;
    assert_eq!(
        api.create(&body, fixtures::TEST_ACCESS_TOKEN)
            .await
            .unwrap()
            .beneficiary_id,
        Some(member)
    );
    for rate in ["-0.1", "1.0001", "0.12345", "1e-1", "NaN", ""] {
        assert!(validate_commission_rate(rate).is_err(), "{rate}");
    }
    assert!(validate_commission_rate("0.1250").is_ok());
    assert!(validate_commission_rate("1.0000").is_ok());
    let bad = CreatePolicy {
        beneficiary_scope: BeneficiaryScope::Everyone,
        beneficiary_id: Some(member),
        ..body
    };
    assert!(api.create(&bad, fixtures::TEST_ACCESS_TOKEN).await.is_err());
    s.verify().await;
}
