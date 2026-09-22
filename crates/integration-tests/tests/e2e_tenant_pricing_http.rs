//! Real route tests for tenant and platform pricing trust boundaries.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{
        TestDataGuard, create_test_api_key, create_test_pool, create_test_tenant, create_test_user,
    },
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{CreateProduceAiKeyRequest, DbRouter, Tenant, TenantMembership, User};
use keycompute_server::{AppState, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    a: Tenant,
    b: Tenant,
    run: String,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "price-http-a", &run).await;
        let b = create_test_tenant(&db, "price-http-b", &run).await;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        Self {
            db,
            guard,
            state,
            a,
            b,
            run,
        }
    }
    async fn token(&self, tenant: Option<&Tenant>, id: Uuid) -> String {
        let user = User::find_by_id(&self.db, id).await.unwrap().unwrap();
        let member = match tenant {
            Some(t) => Some(
                TenantMembership::find(&self.db, t.id, id)
                    .await
                    .unwrap()
                    .unwrap(),
            ),
            None => None,
        };
        self.state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(
                id,
                tenant.map(|t| t.id),
                user.token_version,
                tenant.map(|t| t.authz_version),
                member.map(|m| m.authz_version),
                3600,
            )
            .unwrap()
    }
    fn base(&self, t: &Tenant) -> String {
        format!("/api/v1/tenants/{}/pricing", t.id)
    }
    fn payload(&self, suffix: &str) -> Value {
        json!({"model_name":format!("pricing-test-{}-{suffix}",self.run),
        "billing_dimension":"provideraccount","input_price_per_1k":"0.1","output_price_per_1k":"0.2"})
    }
}
async fn request(
    state: AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = create_router(state)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("x-request-id", "a1111111-1111-4111-8111-111111111111")
                .body(Body::from(if body.is_null() {
                    String::new()
                } else {
                    body.to_string()
                }))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let canonical = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        canonical, "a1111111-1111-4111-8111-111111111111",
        "client correlation must not become internal audit identity"
    );
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let Some(object) = body.as_object_mut() {
        object.insert("__test_request_id".into(), Value::String(canonical));
    }
    (status, body)
}
#[tokio::test]
async fn tenant_pricing_crud_never_widens_the_verified_tenant_or_payload_scope() {
    let mut f = Fixture::new().await;
    let at = f.token(Some(&f.a), f.a.owner_user_id).await;
    let bt = f.token(Some(&f.b), f.b.owner_user_id).await;
    let member = create_test_user(&f.db, f.a.id, "price-member", &f.run).await;
    let mt = f.token(Some(&f.a), member.id).await;
    let a = f.base(&f.a);
    let b = f.base(&f.b);
    let (s, row) = request(f.state.clone(), "POST", &a, &at, f.payload("owned")).await;
    assert_eq!(s, StatusCode::OK, "{row}");
    let id = row["id"].as_str().unwrap();
    let (s, other) = request(f.state.clone(), "POST", &b, &bt, f.payload("foreign")).await;
    assert_eq!(s, StatusCode::OK, "{other}");
    for (method, path, token, body, expected) in [
        (
            "GET",
            a.clone(),
            mt.as_str(),
            Value::Null,
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            b.clone(),
            at.as_str(),
            Value::Null,
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            format!("{a}/{}", other["id"].as_str().unwrap()),
            at.as_str(),
            Value::Null,
            StatusCode::NOT_FOUND,
        ),
        (
            "DELETE",
            format!("{a}/{}", other["id"].as_str().unwrap()),
            at.as_str(),
            Value::Null,
            StatusCode::NOT_FOUND,
        ),
        (
            "PATCH",
            format!("{a}/{id}"),
            at.as_str(),
            json!({"expected_version":1,"platform_role":"root"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "GET",
            format!("{a}?tenant_id={}", f.b.id),
            at.as_str(),
            Value::Null,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (s, v) = request(f.state.clone(), method, &path, token, body).await;
        assert_eq!(s, expected, "{method} {path}: {v}");
    }
    let mut invalid = f.payload("bad-scope");
    invalid["tenant_id"] = json!(f.b.id);
    assert_eq!(
        request(f.state.clone(), "POST", &a, &at, invalid).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (s, page) = request(f.state.clone(), "GET", &a, &at, Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["total"], 1);
    assert_eq!(page["pricing"][0]["tenant_id"], f.a.id.to_string());
    let (s, updated) = request(
        f.state.clone(),
        "PATCH",
        &format!("{a}/{id}"),
        &at,
        json!({"expected_version":row["version"],"input_price_per_1k":"0.4"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{updated}");
    assert_eq!(updated["version"], 2);
    assert_eq!(
        request(
            f.state.clone(),
            "PATCH",
            &format!("{a}/{id}"),
            &at,
            json!({"expected_version":1,"input_price_per_1k":"0.7"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            f.state.clone(),
            "POST",
            &format!("{a}/{id}/make-default"),
            &at,
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let (s, _) = request(
        f.state.clone(),
        "DELETE",
        &format!("{a}/{id}"),
        &at,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        request(
            f.state.clone(),
            "GET",
            &format!("{a}/{id}"),
            &at,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            f.state.clone(),
            "GET",
            &format!("{b}/{}", other["id"].as_str().unwrap()),
            &bt,
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let audit=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT count(*) AS n FROM tenant_audit_events WHERE tenant_id=$1 AND resource_id=$2 AND request_id IS NOT NULL AND request_id<>$3 AND action LIKE 'pricing.%'",
        [f.a.id.into(),id.into(),Uuid::parse_str("a1111111-1111-4111-8111-111111111111").unwrap().into()])).await.unwrap().unwrap();
    assert_eq!(audit.try_get::<i64>("", "n").unwrap(), 4);
    for (action, response) in [("pricing.create", &row), ("pricing.update", &updated)] {
        let canonical = Uuid::parse_str(response["__test_request_id"].as_str().unwrap()).unwrap();
        let audit=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT request_id FROM tenant_audit_events WHERE tenant_id=$1 AND resource_id=$2 AND action=$3",
            [f.a.id.into(),id.into(),action.into()])).await.unwrap().unwrap();
        assert_eq!(audit.try_get::<Uuid>("", "request_id").unwrap(), canonical);
    }
    let prices = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT metadata FROM tenant_audit_events WHERE tenant_id=$1 AND resource_id=$2 AND action='pricing.update'",
        [f.a.id.into(),id.into()])).await.unwrap().unwrap();
    let prices: Value = prices.try_get("", "metadata").unwrap();
    assert!(
        prices["before"]["input_price_per_1k"]
            .as_str()
            .is_some_and(|v| v != "[redacted]")
    );
    assert!(
        prices["after"]["input_price_per_1k"]
            .as_str()
            .is_some_and(|v| v != "[redacted]")
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn platform_and_inference_credentials_do_not_inherit_tenant_pricing_management() {
    let mut f = Fixture::new().await;
    let at = f.token(Some(&f.a), f.a.owner_user_id).await;
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let rt = f.token(None, root.id).await;
    assert_eq!(
        request(
            f.state.clone(),
            "GET",
            "/api/v1/platform/pricing?scope_type=platform",
            &rt,
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    for path in [
        "/api/v1/pricing?scope_type=platform",
        "/api/v1/platform/pricing?scope_type=platform",
    ] {
        assert_eq!(
            request(f.state.clone(), "GET", path, &at, Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(f.state.clone(), "GET", &f.base(&f.a), &rt, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let raw = ProduceAiKeyValidator::generate_key();
    create_test_api_key(
        &f.db,
        &CreateProduceAiKeyRequest {
            tenant_id: f.a.id,
            user_id: f.a.owner_user_id,
            name: "inference only".into(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&raw),
            produce_ai_key_preview: "test-only".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        request(
            f.state.clone(),
            "POST",
            &f.base(&f.a),
            &raw,
            f.payload("key-denied")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn queued_tenant_price_mutation_rechecks_membership_before_any_write() {
    let mut f = Fixture::new().await;
    let delegate = create_test_user(&f.db, f.a.id, "price-delegate", &f.run).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), delegate.id.into()],
    ))
    .await
    .unwrap();
    let dt = f.token(Some(&f.a), delegate.id).await;
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let path = f.base(&f.a);
    let state = f.state.clone();
    let payload = f.payload("blocked");
    let task = tokio::spawn(async move { request(state, "POST", &path, &dt, payload).await });
    tokio::time::timeout(std::time::Duration::from_secs(8),async{
        loop {let r=f.db.query_one(Statement::from_string(DbBackend::Postgres,
            "SELECT count(*) AS n FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query LIKE '%identity_admin_fence%'".to_string())).await.unwrap().unwrap();
            if r.try_get::<i64>("","n").unwrap()>0{break;}
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
    }).await.expect("mutation must actually wait on authority lock");
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='member' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), delegate.id.into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (s, v) = tokio::time::timeout(std::time::Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let r =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM pricing_models WHERE tenant_id=$1",
            [f.a.id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.try_get::<i64>("", "n").unwrap(), 0);
    f.guard.cleanup().await.unwrap();
}
