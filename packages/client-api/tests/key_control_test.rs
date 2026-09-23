//! Tenant key metadata and personal issuance wire contracts use synthetic credentials only.
use client_api::{ApiClient, ClientConfig, api::key_control::*};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};
const STAMP: &str = "2026-09-01T00:00:00Z";
const FUTURE: &str = "2030-09-01T00:00:00Z";
fn client(server: &MockServer) -> ApiClient {
    ApiClient::new(
        ClientConfig::new(server.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap()
}
fn key(tenant: Uuid, owner: Uuid, id: Uuid) -> Value {
    json!({"id":id,"tenant_id":tenant,"owner_user_id":owner,"name":"owned","key_preview":"sk-test***","revoked":false,"revoked_at":null,"expires_at":null,"last_used_at":null,"created_at":STAMP,"updated_at":STAMP})
}
fn intent(tenant: Uuid, owner: Uuid, id: Uuid) -> Value {
    json!({"id":id,"tenant_id":tenant,"owner_user_id":owner,"requested_by_user_id":Uuid::new_v4(),"replaces_key_id":null,"requested_name":"owned","requested_expires_at":null,"status":"pending","expires_at":FUTURE,"claimed_at":null,"created_key_id":null,"created_at":STAMP})
}
fn page(field: &str, item: Value) -> Value {
    let mut v = json!({"total":1,"page":1,"page_size":20,"total_pages":1});
    v[field] = json!([item]);
    v
}
#[tokio::test]
async fn fresh_metadata_and_personal_lists_keep_fixed_tenant_owner_boundaries() {
    let server = MockServer::start().await;
    let client = client(&server);
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let admin = TenantKeyApi::new(&client, tenant).unwrap();
    let mine = OwnerKeyIssuanceApi::new(&client, tenant, owner).unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/tenants/{tenant}/keys")))
        .and(query_param("owner_user_id", owner.to_string()))
        .and(query_param("include_revoked", "false"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page("keys", key(tenant, owner, id))),
        )
        .expect(2)
        .mount(&server)
        .await;
    let query = KeyQuery {
        owner_user_id: Some(owner),
        ..Default::default()
    };
    for _ in 0..2 {
        assert_eq!(admin.list(&query, "console").await.unwrap().keys[0].id, id);
    }
    Mock::given(method("GET"))
        .and(path("/api/v1/me/key-issuance"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(page("intents", intent(tenant, owner, id))),
        )
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        assert_eq!(
            mine.list(1, 20, "console").await.unwrap().intents[0].owner_user_id,
            owner
        );
    }
    for r in server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/api/v1/me/"))
    {
        assert!(
            r.url
                .query_pairs()
                .all(|(k, _)| k != "tenant_id" && k != "owner_user_id")
        );
    }
    assert!(TenantKeyApi::new(&client, Uuid::nil()).is_err());
    assert!(OwnerKeyIssuanceApi::new(&client, tenant, Uuid::nil()).is_err());
}
#[tokio::test]
async fn foreign_owners_tenants_missing_versions_and_malformed_pages_fail_closed() {
    let server = MockServer::start().await;
    let client = client(&server);
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let admin = TenantKeyApi::new(&client, tenant).unwrap();
    let mut missing = key(tenant, owner, id);
    missing.as_object_mut().unwrap().remove("updated_at");
    for row in [
        key(Uuid::new_v4(), owner, id),
        key(tenant, owner, Uuid::nil()),
        missing,
    ] {
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page("keys", row)))
            .expect(1)
            .mount(&server)
            .await;
        assert!(admin.list(&KeyQuery::default(), "console").await.is_err());
        server.verify().await;
    }
    let mine = OwnerKeyIssuanceApi::new(&client, tenant, owner).unwrap();
    for row in [
        intent(tenant, Uuid::new_v4(), id),
        intent(Uuid::new_v4(), owner, id),
    ] {
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page("intents", row)))
            .expect(1)
            .mount(&server)
            .await;
        assert!(mine.list(1, 20, "console").await.is_err());
        server.verify().await;
    }
    server.reset().await;
    assert!(
        admin
            .list(
                &KeyQuery {
                    page: 0,
                    ..Default::default()
                },
                "console"
            )
            .await
            .is_err()
    );
    assert!(mine.list(1, 101, "console").await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn metadata_patch_keeps_omitted_null_and_explicit_expiration_distinct() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantKeyApi::new(&client(&server), tenant).unwrap();
    for expires in [None, Some(None), Some(Some(FUTURE.to_owned()))] {
        server.reset().await;
        let req = KeyPatch {
            expected_updated_at: STAMP.into(),
            name: Some("owned".into()),
            expires_at: expires.clone(),
        };
        let encoded = serde_json::to_value(&req).unwrap();
        match &expires {
            None => assert!(encoded.get("expires_at").is_none()),
            Some(None) => assert!(encoded["expires_at"].is_null()),
            Some(Some(value)) => assert_eq!(encoded["expires_at"], *value),
        }
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v1/tenants/{tenant}/keys/{id}")))
            .and(body_json(encoded))
            .respond_with(ResponseTemplate::new(200).set_body_json(key(tenant, owner, id)))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            api.patch(id, &req, "console").await.unwrap().owner_user_id,
            owner
        );
        server.verify().await;
    }
}
#[tokio::test]
async fn admin_requests_and_cancels_metadata_intents_without_a_secret_route() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let api = TenantKeyApi::new(&client(&server), tenant).unwrap();
    let i = intent(tenant, owner, id);
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/tenants/{tenant}/keys/issuance")))
        .and(body_json(json!({"owner_user_id":owner,"name":"owned"})))
        .respond_with(ResponseTemplate::new(202).set_body_json(
            json!({"intent":i,"outcome":"created","message":"awaiting_owner_claim"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        api.request(
            &NewIssuance {
                owner_user_id: owner,
                name: "owned".into(),
                expires_at: None
            },
            "console"
        )
        .await
        .unwrap()
        .outcome,
        IssuanceOutcome::Created
    );
    let mut rotated = i.clone();
    rotated["replaces_key_id"] = json!(key_id);
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/tenants/{tenant}/keys/{key_id}/rotate"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"intent":rotated,"outcome":"already_pending","message":"awaiting_owner_claim"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        api.rotate(
            key_id,
            &RotateKey {
                name: "owned".into(),
                expires_at: None
            },
            "console"
        )
        .await
        .unwrap()
        .outcome,
        IssuanceOutcome::AlreadyPending
    );
    let mut cancelled = i;
    cancelled["status"] = json!("cancelled");
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/tenants/{tenant}/key-issuance/{id}/cancel"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"intent":cancelled,"outcome":"cancelled","message":"owner_claim_denied"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        api.cancel(id, "console").await.unwrap().intent.status,
        IntentStatus::Cancelled
    );
    for req in server.received_requests().await.unwrap() {
        assert!(!req.url.path().contains("/claim"));
        assert!(!String::from_utf8_lossy(&req.body).contains("platform_role"));
    }
}
#[tokio::test]
async fn owner_claim_is_single_dispatch_redacted_and_bound_to_the_original_intent() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let api = OwnerKeyIssuanceApi::new(&client(&server), tenant, owner).unwrap();
    let original: IssuanceIntent = serde_json::from_value(intent(tenant, owner, id)).unwrap();
    let secret = "sk-fixture-one-time-only";
    Mock::given(method("POST")).and(path(format!("/api/v1/me/key-issuance/{id}/claim"))).and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"outcome":"claimed","intent_id":id,"key_id":key_id,"name":"owned","expires_at":null,"created_at":STAMP,"key":secret,"secret_returned_once":true}))).expect(1).mount(&server).await;
    let claimed = api.claim(&original, "console").await.unwrap();
    assert_eq!(claimed.key.expose(), secret);
    assert!(!format!("{claimed:?}").contains(secret));
    assert!(!format!("{:?}", claimed.key).contains(secret));
    let foreign: IssuanceIntent =
        serde_json::from_value(intent(tenant, Uuid::new_v4(), id)).unwrap();
    assert!(api.claim(&foreign, "console").await.is_err());
    server.verify().await;
    for status in [401, 503] {
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({"error":{"message":secret}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = api.claim(&original, "console").await.unwrap_err();
        assert!(!format!("{error:?}").contains(secret));
        assert!(!error.to_string().contains(secret));
        server.verify().await;
    }
}
#[tokio::test]
async fn deletion_honors_retained_revoked_history_and_decline_is_owner_scoped() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let api = TenantKeyApi::new(&client(&server), tenant).unwrap();
    let mut revoked = key(tenant, owner, id);
    revoked["revoked"] = json!(true);
    revoked["revoked_at"] = json!(STAMP);
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/tenants/{tenant}/keys/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"success":true,"key_id":id,"key":revoked,"revoked_at":STAMP,"deleted":false}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let retained = api.delete(id, "console").await.unwrap();
    assert!(!retained.deleted);
    assert!(retained.key.unwrap().revoked);
    let mine = OwnerKeyIssuanceApi::new(&client(&server), tenant, owner).unwrap();
    let original: IssuanceIntent = serde_json::from_value(intent(tenant, owner, id)).unwrap();
    let mut cancelled = intent(tenant, owner, id);
    cancelled["status"] = json!("cancelled");
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/me/key-issuance/{id}/decline")))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"intent":cancelled,"outcome":"declined","message":"owner_declined"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        mine.decline(&original, "console")
            .await
            .unwrap()
            .intent
            .status,
        IntentStatus::Cancelled
    );
}

#[tokio::test]
async fn malformed_claims_never_publish_secrets_or_change_rotation_identity() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let old = Uuid::new_v4();
    let new = Uuid::new_v4();
    let api = OwnerKeyIssuanceApi::new(&client(&server), tenant, owner).unwrap();
    let mut requested = intent(tenant, owner, id);
    requested["replaces_key_id"] = json!(old);
    let original: IssuanceIntent = serde_json::from_value(requested).unwrap();
    let base = json!({"outcome":"claimed","intent_id":id,"key_id":new,"name":"owned","expires_at":null,"created_at":STAMP,"key":"sk-fixture-not-to-log","secret_returned_once":true});
    for (field, value) in [
        ("intent_id", json!(Uuid::new_v4())),
        ("key_id", json!(old)),
        ("key_id", json!(Uuid::nil())),
        ("expires_at", json!(FUTURE)),
        ("created_at", json!("")),
        ("secret_returned_once", json!(false)),
        ("key", json!("sk-fixture\u{0000}-bad")),
    ] {
        server.reset().await;
        let mut body = base.clone();
        body[field] = value;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let error = api.claim(&original, "console").await.unwrap_err();
        assert!(!format!("{error:?}").contains("sk-fixture"));
        server.verify().await;
    }
}
