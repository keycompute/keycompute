//! Actual root/operator platform reads. No raw-resource or business-write grants.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    routing::get,
};
use chrono::{Duration, Utc};
use integration_tests::{
    common::resolve_database_url,
    db::{create_test_api_key, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    CreateProduceAiKeyRequest, CreateUsageLogRequest, DbRouter, UsageLog, User,
    models::platform_operations::{
        OperationsMembership, OperationsSession, OperationsTarget, PlatformOperationsScope,
        TenantHealthQuery,
    },
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope};
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, TransactionTrait,
};
use serde_json::Value;
use std::{future::Future, time::Duration as StdDuration};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    root: Uuid,
    admin: Uuid,
    user: Uuid,
    operator: Uuid,
    a: Uuid,
    b: Uuid,
    key: String,
}
impl Fixture {
    async fn new(db: DatabaseConnection) -> Self {
        keycompute_db::initialize_schema(&db).await.unwrap();
        let tx = db.begin().await.unwrap();
        let root = User::bootstrap_root(&tx, "ops-root@fixture.invalid", None)
            .await
            .unwrap()
            .id;
        tx.commit().await.unwrap();
        let a = create_test_tenant(&db, "operations-a", "isolated").await.id;
        let b = create_test_tenant(&db, "operations-b", "isolated").await.id;
        let admin = create_test_user(&db, a, "ops-admin", "isolated").await.id;
        let user = create_test_user(&db, a, "ops-member", "isolated").await.id;
        let operator = create_test_user(&db, a, "ops-operator", "isolated")
            .await
            .id;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
            [a.into(), admin.into()],
        ))
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='operator' WHERE id=$1",
            [operator.into()],
        ))
        .await
        .unwrap();
        let key = keycompute_auth::ProduceAiKeyValidator::generate_key();
        create_test_api_key(
            &db,
            &CreateProduceAiKeyRequest {
                tenant_id: a,
                user_id: user,
                name: "ops inference fixture".into(),
                produce_ai_key_hash: keycompute_auth::ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "fixture***".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        Self {
            db,
            root,
            admin,
            user,
            operator,
            a,
            b,
            key,
        }
    }
    fn state(&self) -> AppState {
        AppState::with_pool(DbRouter::single(self.db.clone()))
    }
    async fn session(&self, id: Uuid, selected: bool) -> OperationsSession {
        let user = User::find_by_id(&self.db, id).await.unwrap().unwrap();
        let member = if selected {
            let t = keycompute_db::Tenant::find_by_id(&self.db, self.a)
                .await
                .unwrap()
                .unwrap();
            let m = keycompute_db::TenantMembership::find(&self.db, self.a, id)
                .await
                .unwrap()
                .unwrap();
            Some(OperationsMembership {
                tenant_id: t.id,
                tenant_role: m.tenant_role().unwrap(),
                tenant_authz_version: t.authz_version,
                membership_authz_version: m.authz_version,
            })
        } else {
            None
        };
        OperationsSession {
            credential_kind: CredentialKind::Jwt,
            token_version: user.token_version,
            expires_at: Utc::now().timestamp() + 3600,
            selected: member,
        }
    }
    async fn scope(&self, id: Uuid, role: PlatformRole, selected: bool) -> PlatformOperationsScope {
        PlatformOperationsScope::checked(
            PlatformScope::checked(id, role).unwrap(),
            self.session(id, selected).await,
        )
        .unwrap()
    }
    async fn token(&self, state: &AppState, id: Uuid, selected: bool) -> String {
        let session = self.session(id, selected).await;
        state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(
                id,
                session.selected.map(|m| m.tenant_id),
                session.token_version,
                session.selected.map(|m| m.tenant_authz_version),
                session.selected.map(|m| m.membership_authz_version),
                3600,
            )
            .unwrap()
    }
    async fn seed_usage(&self, tenant: Uuid, currency: &str, amount: i32) -> UsageLog {
        let owner = keycompute_db::Tenant::find_by_id(&self.db, tenant)
            .await
            .unwrap()
            .unwrap()
            .owner_user_id;
        let key = create_test_api_key(
            &self.db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant,
                user_id: owner,
                name: "ops source".into(),
                produce_ai_key_hash: Uuid::new_v4().to_string(),
                produce_ai_key_preview: "fixture***".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        UsageLog::create(
            &self.db,
            &CreateUsageLogRequest {
                request_id: Uuid::new_v4(),
                tenant_id: tenant,
                user_id: owner,
                produce_ai_key_id: key.id,
                model_name: "PRIVATE_MODEL_MARKER".into(),
                provider_name: "PRIVATE_PROVIDER".into(),
                account_id: Uuid::new_v4(),
                input_tokens: 7,
                output_tokens: 3,
                input_unit_price_snapshot: 1.into(),
                output_unit_price_snapshot: 1.into(),
                user_amount: amount.into(),
                currency: currency.into(),
                usage_source: "upstream".into(),
                status: "success".into(),
                started_at: Utc::now(),
                finished_at: Utc::now(),
            },
        )
        .await
        .unwrap()
    }
}
async fn isolated<F, Fut>(case: F)
where
    F: FnOnce(Fixture) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    assert!(
        std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
            || std::env::var_os("CI").is_some()
    );
    let url = resolve_database_url();
    assert!(url.contains("@127.0.0.1:") || url.contains("@localhost:"));
    let parent = create_test_pool().await;
    let name = format!("kc_operations_{}", Uuid::new_v4().simple());
    parent
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    let db = Database::connect(format!("{}/{}", url.rsplit_once('/').unwrap().0, name))
        .await
        .unwrap();
    let owned = db.clone();
    let result = tokio::spawn(async move {
        case(Fixture::new(owned).await).await;
    })
    .await;
    db.close().await.unwrap();
    parent
        .execute_unprepared(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    result.unwrap();
}
async fn call(
    app: Router,
    method: &str,
    path: &str,
    token: &str,
) -> (StatusCode, Value, HeaderMap) {
    let response = tokio::time::timeout(
        StdDuration::from_secs(15),
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 2 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}
fn ok(value: (StatusCode, Value, HeaderMap)) -> Value {
    assert_eq!(value.0, StatusCode::OK, "{}", value.1);
    value.1
}

#[tokio::test]
async fn operator_global_identity_reads_explicit_tenant_health_without_membership_bypass() {
    isolated(|f| async move {
        let state = f.state();
        let app = create_router(state.clone());
        let op = f.token(&state, f.operator, false).await;
        let root = f.token(&state, f.root, false).await;
        let admin = f.token(&state, f.admin, true).await;
        let member = f.token(&state, f.user, true).await;
        for target in [
            "/api/v1/platform/operations/tenants".to_owned(),
            format!("/api/v1/platform/operations/tenants/{}", f.b),
            "/api/v1/platform/operations/usage".to_owned(),
            "/api/v1/platform/operations/capacity".to_owned(),
        ] {
            for token in [&op, &root] {
                let r = call(app.clone(), "GET", &target, token).await;
                assert_eq!(r.0, StatusCode::OK, "{target} {}", r.1);
                assert!(r.2["cache-control"].to_str().unwrap().contains("no-store"));
            }
            for token in [&admin, &member, &f.key] {
                assert!(matches!(
                    call(app.clone(), "GET", &target, token).await.0,
                    StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
                ));
            }
        }
        assert!(matches!(
            call(
                app.clone(),
                "GET",
                &format!("/api/v1/tenants/{}/members", f.b),
                &op
            )
            .await
            .0,
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
        ));
        for raw in [
            "/api/v1/platform/settings",
            "/api/v1/admin/monitoring/requests",
        ] {
            assert_eq!(
                call(app.clone(), "GET", raw, &op).await.0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            call(
                app,
                "POST",
                &format!("/api/v1/platform/operations/tenants/{}", f.b),
                &op
            )
            .await
            .0,
            StatusCode::METHOD_NOT_ALLOWED
        );
    })
    .await;
}

#[tokio::test]
async fn bare_operational_handler_still_checks_current_platform_authority() {
    isolated(|f| async move {
        let state = f.state();
        let root = f.token(&state, f.root, false).await;
        let op = f.token(&state, f.operator, false).await;
        let member = f.token(&state, f.user, true).await;
        let app = Router::new()
            .route(
                "/tenants",
                get(keycompute_server::handlers::platform_operations::tenants),
            )
            .with_state(state);
        assert_eq!(
            ok(call(app.clone(), "GET", "/tenants", &root).await)["total"],
            2
        );
        assert_eq!(
            ok(call(app.clone(), "GET", "/tenants", &op).await)["total"],
            2
        );
        assert_eq!(
            call(app, "GET", "/tenants", &member).await.0,
            StatusCode::FORBIDDEN
        );
    })
    .await;
}

#[tokio::test]
async fn operational_paging_counts_and_search_do_not_widen_literal_filters() {
    isolated(|f| async move {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET name='literal%_tenant' WHERE id=$1",
            [f.a.into()],
        ))
        .await
        .unwrap();
        let state = f.state();
        let op = f.token(&state, f.operator, false).await;
        let app = create_router(state);
        let filtered = ok(call(
            app.clone(),
            "GET",
            "/api/v1/platform/operations/tenants?search=%25_",
            &op,
        )
        .await);
        assert_eq!(filtered["total"], 1);
        assert_eq!(filtered["items"][0]["tenant_id"], f.a.to_string());
        let first = ok(call(
            app.clone(),
            "GET",
            "/api/v1/platform/operations/tenants?limit=1",
            &op,
        )
        .await);
        let second = ok(call(
            app.clone(),
            "GET",
            "/api/v1/platform/operations/tenants?limit=1&offset=1",
            &op,
        )
        .await);
        assert_eq!(first["total"], 2);
        assert_eq!(second["total"], 2);
        assert_ne!(
            first["items"][0]["tenant_id"],
            second["items"][0]["tenant_id"]
        );
        let detail = ok(call(
            app.clone(),
            "GET",
            &format!("/api/v1/platform/operations/tenants/{}", f.a),
            &op,
        )
        .await);
        assert_eq!(detail["active_admins"], 2);
        assert_eq!(detail["active_members"], 4);
        assert!(detail.get("owner_user_id").is_none());
        assert!(detail.get("description").is_none());
        for q in [
            "limit=0",
            "limit=101",
            "offset=-1",
            "status=deleted",
            "owner_user_id=x",
            "include_secrets=true",
        ] {
            assert_eq!(
                call(
                    app.clone(),
                    "GET",
                    &format!("/api/v1/platform/operations/tenants?{q}"),
                    &op
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
    })
    .await;
}

#[tokio::test]
async fn operator_usage_is_aggregate_only_and_never_combines_currencies_or_tenant_targets() {
    isolated(|f| async move {
        let first = f.seed_usage(f.a, "CNY", 10).await;
        f.seed_usage(f.a, "USD", 20).await;
        f.seed_usage(f.b, "CNY", 30).await;
        let state = f.state();
        let op = f.token(&state, f.operator, false).await;
        let app = create_router(state);
        let all = ok(call(app.clone(), "GET", "/api/v1/platform/operations/usage", &op).await);
        let specific = ok(call(
            app.clone(),
            "GET",
            &format!("/api/v1/platform/operations/tenants/{}/usage", f.a),
            &op,
        )
        .await);
        let all_rows = all["currencies"].as_array().unwrap();
        assert_eq!(all_rows.len(), 2);
        let cny = all_rows.iter().find(|r| r["currency"] == "CNY").unwrap();
        assert_eq!(cny["requests"], 2);
        assert_eq!(cny["total_tokens"], "20");
        assert_eq!(
            cny["billed_amount"]
                .as_str()
                .unwrap()
                .parse::<rust_decimal::Decimal>()
                .unwrap(),
            40.into()
        );
        let rows = specific["currencies"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let cny = rows.iter().find(|r| r["currency"] == "CNY").unwrap();
        assert_eq!(cny["requests"], 1);
        assert_eq!(
            cny["billed_amount"]
                .as_str()
                .unwrap()
                .parse::<rust_decimal::Decimal>()
                .unwrap(),
            10.into()
        );
        for secret in [
            "PRIVATE_MODEL_MARKER".to_owned(),
            "PRIVATE_PROVIDER".to_owned(),
            first.user_id.to_string(),
            first.request_id.to_string(),
            first.produce_ai_key_id.to_string(),
        ] {
            assert!(!all.to_string().contains(&secret));
        }
        assert_eq!(
            call(
                app.clone(),
                "GET",
                "/api/v1/platform/operations/usage?user_id=someone",
                &op
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                app,
                "GET",
                &format!(
                    "/api/v1/platform/operations/tenants/{}/usage",
                    Uuid::new_v4()
                ),
                &op
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    })
    .await;
}

#[tokio::test]
async fn forged_or_stale_operational_scopes_cannot_authorize_live_reads() {
    isolated(|f| async move {
        let query = TenantHealthQuery {
            limit: 20,
            ..Default::default()
        };
        for id in [f.user, f.admin] {
            let forged = f.scope(id, PlatformRole::Operator, false).await;
            assert!(forged.tenants(&f.db, &query).await.is_err());
            assert!(forged.tenant(&f.db, f.b).await.is_err());
            assert!(
                forged
                    .usage(
                        &f.db,
                        OperationsTarget::Platform,
                        Utc::now() - Duration::days(1),
                        Utc::now()
                    )
                    .await
                    .is_err()
            );
        }
        for kind in [
            CredentialKind::ApiKey,
            CredentialKind::Node,
            CredentialKind::System,
        ] {
            let mut session = f.session(f.operator, false).await;
            session.credential_kind = kind;
            assert!(
                PlatformOperationsScope::checked(
                    PlatformScope::checked(f.operator, PlatformRole::Operator).unwrap(),
                    session
                )
                .is_err()
            );
        }
        let scoped = f.scope(f.operator, PlatformRole::Operator, true).await;
        let global = f.scope(f.operator, PlatformRole::Operator, false).await;
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET status='removed' WHERE tenant_id=$1 AND user_id=$2",
            [f.a.into(), f.operator.into()],
        ))
        .await
        .unwrap();
        assert!(scoped.tenants(&f.db, &query).await.is_err());
        assert!(global.tenants(&f.db, &query).await.is_ok());
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET token_version=token_version+1 WHERE id=$1",
            [f.operator.into()],
        ))
        .await
        .unwrap();
        assert!(global.tenants(&f.db, &query).await.is_err());
        let fresh = f.scope(f.operator, PlatformRole::Operator, false).await;
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='none' WHERE id=$1",
            [f.operator.into()],
        ))
        .await
        .unwrap();
        assert!(fresh.validate_current(&f.db).await.is_err());
    })
    .await;
}

#[tokio::test]
async fn inactive_target_is_visible_operationally_but_invalid_dates_and_targets_are_not_wildcards()
{
    isolated(|f| async move {
        let global = f.scope(f.operator, PlatformRole::Operator, false).await;
        let selected = f.scope(f.operator, PlatformRole::Operator, true).await;
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET status='inactive' WHERE id=$1",
            [f.a.into()],
        ))
        .await
        .unwrap();
        assert_eq!(global.tenant(&f.db, f.a).await.unwrap().status, "inactive");
        assert!(selected.tenant(&f.db, f.b).await.is_err());
        let now = Utc::now();
        assert!(
            global
                .usage(&f.db, OperationsTarget::Platform, now, now)
                .await
                .is_err()
        );
        assert!(
            global
                .usage(
                    &f.db,
                    OperationsTarget::Platform,
                    now - Duration::days(32),
                    now
                )
                .await
                .is_err()
        );
        assert!(
            global
                .usage(
                    &f.db,
                    OperationsTarget::Tenant(Uuid::nil()),
                    now - Duration::hours(1),
                    now
                )
                .await
                .is_err()
        );
        assert!(global.tenant(&f.db, Uuid::nil()).await.is_err());
    })
    .await;
}

#[tokio::test]
async fn operational_capacity_projection_has_no_principal_or_connection_payloads() {
    isolated(|f| async move {
        let state = f.state();
        let op = f.token(&state, f.operator, false).await;
        let app = create_router(state);
        let value = ok(call(app, "GET", "/api/v1/platform/operations/capacity", &op).await);
        assert_eq!(value["scope"], "application_process");
        assert!(value["stages"].is_array());
        for text in [
            "redis://",
            "postgres://",
            "api_key",
            "request_id",
            "owner_user_id",
            "session_token",
        ] {
            assert!(!value.to_string().contains(text), "unexpected field {text}");
        }
        let scope = f.scope(f.operator, PlatformRole::Operator, false).await;
        let before = keycompute_db::Tenant::find_by_id(&f.db, f.a)
            .await
            .unwrap()
            .unwrap();
        scope.tenant(&f.db, f.a).await.unwrap();
        scope
            .tenants(
                &f.db,
                &TenantHealthQuery {
                    limit: 20,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let after = keycompute_db::Tenant::find_by_id(&f.db, f.a)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.updated_at, after.updated_at);
        assert_eq!(before.authz_version, after.authz_version);
    })
    .await;
}

#[tokio::test]
async fn operational_session_expiry_is_rechecked_on_each_query_without_cached_grants() {
    isolated(|f| async move {
        let mut session = f.session(f.operator, false).await;
        session.expires_at = Utc::now().timestamp() + 1;
        let scope = PlatformOperationsScope::checked(
            PlatformScope::checked(f.operator, PlatformRole::Operator).unwrap(),
            session,
        )
        .unwrap();
        scope.validate_current(&f.db).await.unwrap();
        while Utc::now().timestamp() < session.expires_at {
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
        assert!(
            scope
                .tenants(&f.db, &TenantHealthQuery::default())
                .await
                .is_err()
        );
        assert!(scope.tenant(&f.db, f.b).await.is_err());
        assert!(
            scope
                .usage(
                    &f.db,
                    OperationsTarget::Platform,
                    Utc::now() - Duration::hours(1),
                    Utc::now()
                )
                .await
                .is_err()
        );
        assert!(scope.validate_current(&f.db).await.is_err());
    })
    .await;
}
