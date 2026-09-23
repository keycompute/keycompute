use client_api::api::tenant_control::{self as client, InvitationToken, MemberPatch};
use client_api::{ClientError, TenantControlApi};
use keycompute_types::TenantRole;
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, ResponseTemplate};
mod common;

#[test]
fn invitation_secrets_and_role_contracts_have_no_legacy_fallback() {
    let raw = "a".repeat(64);
    let token = InvitationToken::from_fragment(&format!("#token={raw}")).unwrap();
    assert!(!format!("{token:?}").contains(&raw));
    for value in [
        "#token=",
        "?token=x",
        "#token=abc&other=1",
        "#token=../../secret",
    ] {
        assert!(InvitationToken::from_fragment(value).is_err());
    }
    assert!(
        serde_json::from_value::<client_api::api::auth::SelectedTenant>(
            json!({"id":"tenant","role":"admin"})
        )
        .is_err()
    );
    let selected: client_api::api::auth::SelectedTenant =
        serde_json::from_value(json!({"id":"tenant","tenant_role":"admin"})).unwrap();
    assert_eq!(selected.tenant_role, TenantRole::Admin);
}
#[tokio::test]
async fn mutations_are_not_replayed_and_delete_keeps_its_revision_body() {
    let (client, server) = common::create_test_client().await;
    let tenant = Uuid::new_v4();
    let user = Uuid::new_v4();
    let control = TenantControlApi::new(&client, tenant).unwrap();
    let url = format!("/api/v1/tenants/{tenant}/members/{user}");
    Mock::given(method("DELETE"))
        .and(path(&url))
        .and(header("authorization", "Bearer console"))
        .and(body_json(json!({"expected_authz_version":7})))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":"unavailable"})))
        .expect(1)
        .mount(&server)
        .await;
    assert!(control.remove_member(user, 7, "console").await.is_err());
    assert!(control.remove_member(user, 0, "console").await.is_err());
    assert!(
        control
            .patch_member(
                Uuid::nil(),
                &MemberPatch {
                    expected_authz_version: 1,
                    tenant_role: Some(TenantRole::Admin),
                    status: None
                },
                "console"
            )
            .await
            .is_err()
    );
    server.verify().await;
}
#[tokio::test]
async fn acceptance_errors_never_echo_the_secret_and_do_not_retry_uncertain_requests() {
    let (client, server) = common::create_test_client().await;
    let raw = "b".repeat(64);
    let invitation = InvitationToken::parse(&raw).unwrap();
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/invitations/{raw}/accept")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":raw})))
        .expect(1)
        .mount(&server)
        .await;
    let error = client::accept_invitation(&client, &invitation, "console")
        .await
        .unwrap_err();
    assert!(matches!(error, ClientError::ServiceUnavailable(_)));
    assert!(!format!("{error:?} {error}").contains(&raw));
    server.verify().await;
}
#[tokio::test]
async fn tenant_queries_are_fresh_and_paging_rejects_wildcard_inputs() {
    let (client, server) = common::create_test_client().await;
    let tenant = Uuid::new_v4();
    let control = TenantControlApi::new(&client, tenant).unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{tenant}/members")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"items":[],"total":0,"page":1,"page_size":20,"total_pages":0}),
            ),
        )
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        control.members(1, 20, None, "console").await.unwrap();
    }
    assert!(control.members(0, 20, None, "console").await.is_err());
    assert!(control.members(1, 101, None, "console").await.is_err());
    assert!(TenantControlApi::new(&client, Uuid::nil()).is_err());
    server.verify().await;
}

#[tokio::test]
async fn search_text_cannot_inject_a_different_tenant_selector() {
    use wiremock::matchers::query_param;
    let (client, server) = common::create_test_client().await;
    let tenant = Uuid::new_v4();
    let control = TenantControlApi::new(&client, tenant).unwrap();
    let search = format!("a&tenant_id={}&page=500", Uuid::new_v4());
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{tenant}/members")))
        .and(query_param("search", &search))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"items":[],"total":0,"page":1,"page_size":20,"total_pages":0}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    control
        .members(1, 20, Some(&search), "console")
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]
            .url
            .query_pairs()
            .any(|(key, _)| key == "tenant_id")
    );
}
