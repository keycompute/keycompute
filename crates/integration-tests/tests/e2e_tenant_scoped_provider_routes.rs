//! Real HTTP coverage for tenant provider and binding routes.
//! Production routers, signed sessions and database constraints remain active.

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{DbRouter, Tenant, TenantMembership, User};
use keycompute_server::{AppState, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    a: Tenant,
    b: Tenant,
    a_token: String,
    b_token: String,
    a_member_token: String,
}

impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "provider-http-a", &run).await;
        let b = create_test_tenant(&db, "provider-http-b", &run).await;
        let a_member = create_test_user(&db, a.id, "provider-http-member", &run).await;
        let a_owner = User::find_by_id(&db, a.owner_user_id)
            .await
            .unwrap()
            .unwrap();
        let b_owner = User::find_by_id(&db, b.owner_user_id)
            .await
            .unwrap()
            .unwrap();
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        let a_token = token(&state, &db, &a, &a_owner).await;
        let b_token = token(&state, &db, &b, &b_owner).await;
        let a_member_token = token(&state, &db, &a, &a_member.user).await;
        Self {
            db,
            guard,
            state,
            a,
            b,
            a_token,
            b_token,
            a_member_token,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: impl Into<String>,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, body, _) = self.request_with_id(method, path, token, body).await;
        (status, body)
    }

    async fn request_with_id(
        &self,
        method: Method,
        path: impl Into<String>,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value, Uuid) {
        let request = Request::builder()
            .method(method)
            .uri(path.into())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                body.map(|value| value.to_string()).unwrap_or_default(),
            ))
            .unwrap();
        let response = create_router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            request_id,
        )
    }
}

async fn token(state: &AppState, db: &DatabaseConnection, tenant: &Tenant, user: &User) -> String {
    let membership = TenantMembership::find(db, tenant.id, user.id)
        .await
        .unwrap()
        .unwrap();
    state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(
            user.id,
            Some(tenant.id),
            user.token_version,
            Some(tenant.authz_version),
            Some(membership.authz_version),
            3600,
        )
        .unwrap()
}

fn account_body(name: &str) -> Value {
    json!({
        "name": name,
        "provider": "openai",
        "api_key": "tenant-http-secret",
        "api_base": "https://provider.example/v1",
        "models": ["gpt-http"],
        "api_capabilities": ["chat_completions"],
        "rpm_limit": 100,
        "tpm_limit": 100000,
        "priority": 1,
        "pool_enabled": true
    })
}

#[tokio::test]
async fn tenant_provider_routes_are_scoped_and_secret_free() {
    let mut f = Fixture::new().await;
    let account_path = format!("/api/v1/tenants/{}/accounts", f.a.id);
    let (status, created) = f
        .request(
            Method::POST,
            &account_path,
            &f.a_token,
            Some(account_body("tenant-http-account")),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(created.get("api_key").is_none());
    assert!(created.get("upstream_api_key_encrypted").is_none());
    assert!(created.get("api_key_preview").is_some());
    let account_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    let (status, listed) = f
        .request(Method::GET, &account_path, &f.a_token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["accounts"].as_array().unwrap().len(), 1);

    let (status, detail) = f
        .request(
            Method::GET,
            format!("{account_path}/{account_id}"),
            &f.a_token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["id"], account_id.to_string());
    assert!(detail.get("upstream_api_key_encrypted").is_none());

    let (status, _) = f
        .request(
            Method::POST,
            &account_path,
            &f.a_token,
            Some(json!({
                "name": "scope-escalation",
                "provider": "openai",
                "api_key": "secret",
                "models": ["gpt-http"],
                "tenant_id": f.b.id,
                "visibility": "global"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let b_account_path = format!("/api/v1/tenants/{}/accounts", f.b.id);
    let (_, b_account) = f
        .request(
            Method::POST,
            &b_account_path,
            &f.b_token,
            Some(account_body("foreign-http-account")),
        )
        .await;
    let foreign_account_id = b_account["id"].as_str().unwrap();
    let (status, _) = f
        .request(
            Method::GET,
            format!("{account_path}/{foreign_account_id}"),
            &f.a_token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn tenant_binding_routes_enforce_owner_scope_and_reject_global_payloads() {
    let mut f = Fixture::new().await;
    let account_path = format!("/api/v1/tenants/{}/accounts", f.a.id);
    let (_, account) = f
        .request(
            Method::POST,
            &account_path,
            &f.a_token,
            Some(account_body("binding-http-account")),
        )
        .await;
    let account_id = account["id"].as_str().unwrap();
    let binding_path = format!("/api/v1/tenants/{}/passthrough-bindings", f.a.id);
    let (status, binding) = f
        .request(
            Method::POST,
            &binding_path,
            &f.a_token,
            Some(json!({"account_id": account_id, "pool_enabled": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(binding["is_global"], false);
    let binding_id = binding["id"].as_str().unwrap();

    let (status, listed) = f
        .request(Method::GET, &binding_path, &f.a_token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["bindings"].as_array().unwrap().len(), 1);

    let (status, account_detail) = f
        .request(
            Method::GET,
            format!("{account_path}/{account_id}"),
            &f.a_token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        account_detail["passthrough_binding_count"], 1,
        "account detail must report the binding which controls pool participation"
    );
    let (status, account_list) = f
        .request(Method::GET, &account_path, &f.a_token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        account_list["accounts"][0]["passthrough_binding_count"], 1,
        "tenant account list and detail must use the same ownership-bound count"
    );

    let (status, _) = f
        .request(
            Method::GET,
            format!("{binding_path}/{binding_id}"),
            &f.b_token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f
        .request(
            Method::GET,
            format!(
                "/api/v1/tenants/{}/passthrough-bindings/{binding_id}",
                f.b.id
            ),
            &f.b_token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = f
        .request(
            Method::POST,
            &binding_path,
            &f.a_token,
            Some(json!({
                "account_id": account_id,
                "tenant_id": f.b.id,
                "is_global": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = f
        .request(Method::GET, &binding_path, &f.a_member_token, None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn stale_signed_membership_cannot_use_tenant_provider_routes() {
    let mut f = Fixture::new().await;
    let actor = create_test_user(
        &f.db,
        f.a.id,
        "revocable-provider-admin",
        &generate_test_id(),
    )
    .await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), actor.id.into()],
    ))
    .await
    .unwrap();
    let stale = token(&f.state, &f.db, &f.a, &actor.user).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='suspended' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), actor.id.into()],
    ))
    .await
    .unwrap();
    let (status, _) = f
        .request(
            Method::GET,
            format!("/api/v1/tenants/{}/accounts", f.a.id),
            &stale,
            None,
        )
        .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn tenant_provider_mutations_preserve_owner_and_audit_server_request_ids() {
    let mut f = Fixture::new().await;
    let accounts = format!("/api/v1/tenants/{}/accounts", f.a.id);
    let (status, created, create_request) = f
        .request_with_id(
            Method::POST,
            &accounts,
            &f.a_token,
            Some(account_body("audited-owner")),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = created["id"].as_str().unwrap();
    let account = format!("{accounts}/{id}");
    let (status, updated, update_request) = f
        .request_with_id(
            Method::PATCH,
            &account,
            &f.a_token,
            Some(json!({"name":"updated-owner", "rpm_limit":110})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["tenant_id"], f.a.id.to_string());
    assert_eq!(updated["name"], "updated-owner");
    let (status, _, delete_request) = f
        .request_with_id(Method::DELETE, &account, &f.a_token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        f.request(Method::GET, &account, &f.a_token, None).await.0,
        StatusCode::NOT_FOUND
    );
    for (action, request_id) in [
        ("account.create", create_request),
        ("account.update", update_request),
        ("account.delete", delete_request),
    ] {
        let row = f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT actor_user_id,tenant_id,credential_kind,metadata FROM tenant_audit_events WHERE action=$1 AND request_id=$2 AND resource_id=$3",
            [action.into(), request_id.into(), id.into()],
        )).await.unwrap().expect("canonical request audit");
        assert_eq!(row.try_get_by_index::<Uuid>(0).unwrap(), f.a.owner_user_id);
        assert_eq!(row.try_get_by_index::<Uuid>(1).unwrap(), f.a.id);
        assert_eq!(row.try_get_by_index::<String>(2).unwrap(), "jwt");
        let metadata: Value = row.try_get_by_index(3).unwrap();
        assert!(!metadata.to_string().contains("tenant-http-secret"));
    }
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn root_platform_and_inference_boundaries_do_not_depend_on_tenant_selection() {
    let mut f = Fixture::new().await;
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let global = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(root.id, None, root.token_version, None, None, 3600)
        .unwrap();
    for path in [
        "/api/v1/platform/accounts",
        "/api/v1/platform/passthrough-bindings",
        "/api/v1/platform/model-catalog",
    ] {
        assert_eq!(
            f.request(Method::GET, path, &global, None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            f.request(Method::GET, path, &f.a_token, None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        f.request(
            Method::GET,
            format!("/api/v1/tenants/{}/accounts", f.a.id),
            &global,
            None,
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let key = keycompute_auth::ProduceAiKeyValidator::generate_key();
    integration_tests::db::create_test_api_key(
        &f.db,
        &keycompute_db::CreateProduceAiKeyRequest {
            tenant_id: f.a.id,
            user_id: f.a.owner_user_id,
            name: "inference-only".into(),
            produce_ai_key_hash: keycompute_auth::ProduceAiKeyValidator::hash_key(&key),
            produce_ai_key_preview: "test-only".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    for path in [
        format!("/api/v1/tenants/{}/accounts", f.a.id),
        format!("/api/v1/tenants/{}/passthrough-bindings", f.a.id),
        "/api/v1/platform/accounts".into(),
    ] {
        assert_eq!(
            f.request(Method::GET, path, &key, None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    f.guard.cleanup().await.unwrap();
}
