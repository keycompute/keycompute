use client_api::api::tenant_control::{
    self as client, CreateInvitation, GrantMember, InvitationToken, MemberPatch, TenantAuditQuery,
    TenantPatch,
};
use client_api::{ClientError, TenantControlApi};
use keycompute_types::{MembershipStatus, TenantRole};
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

#[test]
fn tenant_configuration_uses_a_non_authorization_revision() {
    let body = serde_json::to_value(TenantPatch {
        expected_revision: 7,
        name: Some("Workspace".into()),
        description: None,
        default_rpm_limit: None,
        default_tpm_limit: None,
    })
    .unwrap();
    assert_eq!(body["expected_revision"], 7);
    assert!(body.get("expected_authz_version").is_none());
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

#[tokio::test]
async fn member_status_filter_is_typed_and_keeps_the_platform_target() {
    use wiremock::matchers::query_param;
    let (client, server) = common::create_test_client().await;
    let tenant = Uuid::new_v4();
    let control = TenantControlApi::platform_tenant(&client, tenant).unwrap();
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/platform/tenants/{tenant}/control/members"
        )))
        .and(query_param("page", "1"))
        .and(query_param("page_size", "1"))
        .and(query_param("status", "active"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"items":[],"total":7,"page":1,"page_size":1,"total_pages":7}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let page = control
        .members_filtered(1, 1, None, Some(MembershipStatus::Active), "console")
        .await
        .unwrap();

    assert_eq!(page.total, 7);
    server.verify().await;
}

#[tokio::test]
async fn platform_direct_grant_uses_the_control_route() {
    let (client, server) = common::create_test_client().await;
    let tenant = Uuid::new_v4();
    let user = Uuid::new_v4();
    let control = TenantControlApi::platform_tenant(&client, tenant).unwrap();
    let members_path = format!("/api/v1/platform/tenants/{tenant}/control/members");
    Mock::given(method("POST"))
        .and(path(&members_path))
        .and(body_json(json!({
            "user_id": user
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "user_id": user,
            "email": "admin@example.com",
            "name": "Tenant Admin",
            "user_status": "active",
            "tenant_role": "admin",
            "membership_status": "active",
            "authz_version": 1,
            "invited_by": Uuid::new_v4(),
            "joined_at": "2026-10-19T00:00:00Z",
            "removed_at": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let granted = control
        .grant_member(&GrantMember { user_id: user }, "console")
        .await
        .unwrap();
    assert_eq!(granted.user_id, user);

    let tenant_control = TenantControlApi::new(&client, tenant).unwrap();
    assert!(
        tenant_control
            .grant_member(&GrantMember { user_id: user }, "console",)
            .await
            .is_err()
    );
    server.verify().await;
}

#[tokio::test]
async fn platform_target_cannot_use_tenant_invitation_workflows() {
    let (client, server) = common::create_test_client().await;
    let control = TenantControlApi::platform_tenant(&client, Uuid::new_v4()).unwrap();

    assert!(control.invitations(1, 20, "console").await.is_err());
    assert!(
        control
            .create_invitation(
                &CreateInvitation {
                    email: "member@example.com".into(),
                    tenant_role: TenantRole::Member,
                    expires_in_seconds: 3_600,
                },
                "console",
            )
            .await
            .is_err()
    );
    assert!(
        control
            .revoke_invitation(Uuid::new_v4(), "console")
            .await
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn audit_filters_are_exact_encoded_and_keep_the_platform_target() {
    use wiremock::matchers::query_param;

    let (client, server) = common::create_test_client().await;
    let tenant = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let request = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let control = TenantControlApi::platform_tenant(&client, tenant).unwrap();
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/v1/platform/tenants/{tenant}/control/audit-events"
        )))
        .and(query_param("page", "2"))
        .and(query_param("page_size", "20"))
        .and(query_param("actor_user_id", actor.to_string()))
        .and(query_param("request_id", request.to_string()))
        .and(query_param("action", "membership.update"))
        .and(query_param("result", "success"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "id": event_id,
                "scope_type": "platform",
                "tenant_id": tenant,
                "actor_user_id": actor,
                "actor_email": "operator@example.com",
                "actor_name": "Operator",
                "action": "membership.update",
                "resource_type": "tenant_membership",
                "resource_id": null,
                "request_id": request,
                "credential_kind": "jwt",
                "platform_role": "operator",
                "tenant_role": null,
                "metadata": {},
                "result": "success",
                "created_at": "2026-10-18T00:00:00Z"
            }],
            "total": 21,
            "page": 2,
            "page_size": 20,
            "total_pages": 2
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = control
        .audit_filtered(
            &TenantAuditQuery {
                page: 2,
                page_size: 20,
                actor_user_id: Some(actor),
                request_id: Some(request),
                action: Some(" membership.update ".into()),
                result: Some("success".into()),
            },
            "console",
        )
        .await
        .unwrap();
    assert_eq!(page.items[0].scope_type, "platform");
    assert_eq!(
        page.items[0].actor_email.as_deref(),
        Some("operator@example.com")
    );

    for query in [
        TenantAuditQuery {
            page: 1,
            page_size: 20,
            actor_user_id: Some(Uuid::nil()),
            ..Default::default()
        },
        TenantAuditQuery {
            page: 1,
            page_size: 20,
            result: Some("unknown".into()),
            ..Default::default()
        },
    ] {
        assert!(control.audit_filtered(&query, "console").await.is_err());
    }
    server.verify().await;
}
