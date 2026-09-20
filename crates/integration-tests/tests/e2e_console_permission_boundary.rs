//! In-process authorization regressions. All identities/data belong to the test DB.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use chrono::{Duration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_auth::{Permission, ProduceAiKeyValidator};
use keycompute_db::{
    CreateUserRequest, DbRouter, User,
    models::{
        api_key::{CreateProduceAiKeyRequest, ProduceAiKey},
        usage_log::{CreateUsageLogRequest, UsageLog},
    },
};
use keycompute_server::{AppState, create_router};
use keycompute_types::UserRole;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn request(state: &AppState, method: &str, path: &str, token: &str, body: Value) -> Response {
    create_router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn json_body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

fn jwt(state: &AppState, user: &User) -> String {
    state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_token_with_version(user.id, user.tenant_id, &user.role, user.token_version)
        .unwrap()
}

async fn inference_key(db: &DatabaseConnection, user: &User) -> (ProduceAiKey, String) {
    let key = ProduceAiKeyValidator::generate_key();
    let row = ProduceAiKey::create(
        db,
        &CreateProduceAiKeyRequest {
            tenant_id: user.tenant_id,
            user_id: user.id,
            name: "test inference".into(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&key),
            produce_ai_key_preview: "test-only".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    (row, key)
}

#[tokio::test]
async fn inference_credentials_cannot_access_console_or_mutate_keys() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let tenant = create_test_tenant(&db, "credential-boundary", &run).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    for role in [UserRole::User, UserRole::Admin] {
        let user = User::create(
            &db,
            &CreateUserRequest {
                tenant_id: tenant.id,
                email: format!("boundary-{}-{run}@example.com", role.as_str()),
                name: None,
                role: Some(role),
            },
        )
        .await
        .unwrap();
        let (key, raw_key) = inference_key(&db, &user).await;
        let identity = state.auth.verify_token(&raw_key).await.unwrap();
        assert_eq!(identity.permissions, vec![Permission::UseApi]);
        assert!(!identity.is_admin());
        let before = ProduceAiKey::find_by_user(&db, user.id)
            .await
            .unwrap()
            .len();
        let key_path = format!("/api/v1/keys/{}", key.id);
        let cases = [
            ("GET", "/api/v1/me", Value::Null),
            ("PUT", "/api/v1/me/profile", json!({"name":"not-applied"})),
            (
                "PUT",
                "/api/v1/me/password",
                json!({"current_password":"test-old", "new_password":"test-new"}),
            ),
            ("GET", "/api/v1/keys", Value::Null),
            ("HEAD", "/api/v1/keys", Value::Null),
            (
                "POST",
                "/api/v1/keys",
                json!({"name":"not-created", "never_expires":true}),
            ),
            ("DELETE", key_path.as_str(), Value::Null),
            ("GET", "/api/v1/usage", Value::Null),
            ("GET", "/api/v1/billing/records", Value::Null),
            ("GET", "/api/v1/billing/stats", Value::Null),
            ("GET", "/api/v1/payments/balance", Value::Null),
            ("GET", "/api/v1/me/node-gateway/tokens", Value::Null),
            ("GET", "/api/v1/users", Value::Null),
        ];
        for (method, path, body) in cases {
            let response = request(&state, method, path, &raw_key, body).await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{role}: {method} {path}"
            );
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
        assert_eq!(
            ProduceAiKey::find_by_user(&db, user.id)
                .await
                .unwrap()
                .len(),
            before
        );
        assert!(
            state.auth.verify_token(&raw_key).await.is_ok(),
            "denied mutation must not revoke the test key"
        );
        assert_eq!(
            User::find_by_id(&db, user.id).await.unwrap().unwrap().name,
            None
        );
        // Valid inference credentials still have their intended model-list access.
        for path in ["/v1/models", "/pt/v1/models", "/nt/v1/models"] {
            assert_eq!(
                request(&state, "GET", path, &raw_key, Value::Null)
                    .await
                    .status(),
                StatusCode::OK,
                "{path}"
            );
        }
    }
    guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn ordinary_jwt_retains_self_service_and_public_routes_stay_public() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let tenant = create_test_tenant(&db, "jwt-console", &run).await;
    let user = create_test_user(&db, tenant.id, "jwt-console", &run).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let token = jwt(&state, &user);
    for path in [
        "/api/v1/me",
        "/api/v1/keys",
        "/api/v1/billing/records",
        "/api/v1/billing/stats",
        "/api/v1/payments/balance",
    ] {
        assert_eq!(
            request(&state, "GET", path, &token, Value::Null)
                .await
                .status(),
            StatusCode::OK,
            "{path}"
        );
    }
    let created = request(
        &state,
        "POST",
        "/api/v1/keys",
        &token,
        json!({"name":"jwt-created"}),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let key_id = json_body(created).await["key_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        request(
            &state,
            "DELETE",
            &format!("/api/v1/keys/{key_id}"),
            &token,
            Value::Null
        )
        .await
        .status(),
        StatusCode::OK
    );
    for path in ["/health", "/api/v1/settings/public"] {
        let response = create_router(state.clone())
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    guard.cleanup().await.unwrap();
}

async fn usage(
    db: &DatabaseConnection,
    tenant: Uuid,
    user: Uuid,
    model: &str,
    days_ago: i64,
) -> UsageLog {
    let time = Utc::now() - Duration::days(days_ago) - Duration::hours(1);
    let log = UsageLog::create(
        db,
        &CreateUsageLogRequest {
            request_id: Uuid::new_v4(),
            tenant_id: tenant,
            user_id: user,
            produce_ai_key_id: Uuid::new_v4(),
            model_name: model.into(),
            provider_name: "test".into(),
            account_id: Uuid::new_v4(),
            input_tokens: 10,
            output_tokens: 5,
            input_unit_price_snapshot: 1.into(),
            output_unit_price_snapshot: 1.into(),
            user_amount: 2.into(),
            currency: "CNY".into(),
            usage_source: "provider".into(),
            status: "success".into(),
            started_at: time,
            finished_at: time,
        },
    )
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE usage_logs SET created_at=$2 WHERE id=$1",
        [log.id.into(), time.into()],
    ))
    .await
    .unwrap();
    log
}

#[tokio::test]
async fn personal_billing_scopes_records_counts_totals_and_models_identically() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let tenant = create_test_tenant(&db, "bill-owner", &run).await;
    let other_tenant = create_test_tenant(&db, "bill-other", &run).await;
    let user = create_test_user(&db, tenant.id, "bill-owner", &run).await;
    let peer = create_test_user(&db, tenant.id, "bill-peer", &run).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let own = usage(&db, tenant.id, user.id, "own-model", 0).await;
    usage(&db, tenant.id, user.id, "old-own-model", 70).await;
    usage(&db, tenant.id, peer.id, "peer-private-model", 0).await;
    // Historical ledger row: same global user, different resource tenant.
    usage(&db, other_tenant.id, user.id, "other-tenant-model", 0).await;
    let token = jwt(&state, &user);
    let page = json_body(
        request(
            &state,
            "GET",
            "/api/v1/billing/records?limit=1",
            &token,
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(page["total"], 2);
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    assert_eq!(page["records"][0]["request_id"], own.request_id.to_string());
    let next = json_body(
        request(
            &state,
            "GET",
            "/api/v1/billing/records?limit=1&offset=1",
            &token,
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(next["total"], 2);
    assert_eq!(next["records"][0]["model_name"], "old-own-model");
    let start = (Utc::now() - Duration::days(1)).format("%Y-%m-%dT%H:%M:%SZ");
    let filtered = json_body(
        request(
            &state,
            "GET",
            &format!("/api/v1/billing/records?start_time={start}"),
            &token,
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["records"].as_array().unwrap().len(), 1);
    for role in ["user", "admin"] {
        // The endpoint remains personal even when this identity gains an admin role.
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET role=$2 WHERE id=$1",
            [user.id.into(), role.into()],
        ))
        .await
        .unwrap();
        let current = User::find_by_id(&db, user.id).await.unwrap().unwrap();
        let token = jwt(&state, &current);
        let response = request(&state, "GET", "/api/v1/billing/stats", &token, Value::Null).await;
        assert_eq!(response.status(), StatusCode::OK);
        let stats = json_body(response).await;
        assert_eq!(stats["total_requests"], 1);
        assert_eq!(stats["total_input_tokens"], 10);
        assert_eq!(stats["total_output_tokens"], 5);
        assert_eq!(
            stats["total_amount"]
                .as_str()
                .unwrap()
                .parse::<rust_decimal::Decimal>()
                .unwrap(),
            rust_decimal::Decimal::from(2)
        );
        assert_eq!(stats["by_model"].as_array().unwrap().len(), 1);
        assert_eq!(stats["by_model"][0]["model_name"], "own-model");
        let flat = json_body(
            request(
                &state,
                "GET",
                "/api/v1/billing/stats?group_by_model=false",
                &token,
                Value::Null,
            )
            .await,
        )
        .await;
        assert_eq!(flat["total_requests"], 1);
        assert!(flat["by_model"].as_array().unwrap().is_empty());
        for _ in 0..2 {
            let usage = request(
                &state,
                "GET",
                "/api/v1/usage?page=1&page_size=1",
                &token,
                Value::Null,
            )
            .await;
            assert_eq!(usage.status(), StatusCode::OK);
            let usage = json_body(usage).await;
            assert_eq!(usage["total"], 2);
            assert_eq!(usage["records"].as_array().unwrap().len(), 1);
            assert_eq!(usage["records"][0]["model"], "own-model");
            let stats = request(&state, "GET", "/api/v1/usage/stats", &token, Value::Null).await;
            assert_eq!(stats.status(), StatusCode::OK);
            let stats = json_body(stats).await;
            assert_eq!(stats["total_requests"], 2);
            assert_eq!(stats["total_tokens"], 30);
            assert_eq!(stats["total_cost"].as_f64().unwrap(), 4.0);
            for path in ["/api/v1/usage/trend", "/api/v1/dashboard/overview"] {
                let response = request(&state, "GET", path, &token, Value::Null).await;
                assert_eq!(response.status(), StatusCode::OK, "{path}");
                let value = json_body(response).await;
                let trend = if path.ends_with("overview") {
                    assert_eq!(value["stats"]["total_requests"], 2);
                    assert_eq!(value["recent_usage"].as_array().unwrap().len(), 2);
                    assert!(!value.to_string().contains("peer-private-model"));
                    assert!(!value.to_string().contains("other-tenant-model"));
                    &value["trend"]
                } else {
                    &value
                };
                assert_eq!(
                    trend["buckets"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v["requests"].as_i64().unwrap())
                        .sum::<i64>(),
                    1
                );
            }
        }
        let invalid =
            "/api/v1/billing/records?start_time=2026-02-02T00:00:00Z&end_time=2026-01-01T00:00:00Z";
        assert_eq!(
            request(&state, "GET", invalid, &token, Value::Null)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn denied_platform_commands_do_not_invalidate_display_snapshots() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let tenant = create_test_tenant(&db, "denied-command", &run).await;
    let user = create_test_user(&db, tenant.id, "denied-command", &run).await;
    let state = AppState::with_pool(DbRouter::single(db));
    let token = jwt(&state, &user);
    let response = request(&state, "GET", "/api/v1/usage/stats", &token, Value::Null).await;
    assert_eq!(response.status(), StatusCode::OK);
    let entries = state.display_cache.metrics()["entries"].as_u64().unwrap();
    assert!(entries > 0);
    let denied = request(
        &state,
        "POST",
        "/api/v1/tenants",
        &token,
        json!({"name":"not allowed"}),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(state.display_cache.metrics()["entries"], entries);
    // Successful authorized writes retain both pre/post mutation invalidation.
    let allowed = request(
        &state,
        "POST",
        "/api/v1/keys",
        &token,
        json!({"name":"allowed personal key"}),
    )
    .await;
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(state.display_cache.metrics()["entries"], 0);
    guard.cleanup().await.unwrap();
}
