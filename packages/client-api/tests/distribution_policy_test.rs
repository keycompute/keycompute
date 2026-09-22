use client_api::api::distribution_policy::{DistributionPolicyApi, PolicyPatch};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{body_json, method, path},
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
