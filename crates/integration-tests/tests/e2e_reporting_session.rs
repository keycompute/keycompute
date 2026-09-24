//! Reporting is read-only, but an expired/regranted request cannot publish its old result.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    CreatePaymentOrderRequest, CreateUsageLogRequest, DbRouter, PaymentMethod, PaymentOrder,
    Tenant, TenantMembership, UsageLog, User,
};
use keycompute_server::{AppState, handlers::tenant_reporting};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::Value;
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    state: AppState,
    tenant: Tenant,
    actor: Uuid,
    member: Uuid,
    usage: Uuid,
    order: Uuid,
    url: String,
}
impl Fixture {
    async fn new(db: DatabaseConnection, url: String) -> Self {
        let run = generate_test_id();
        let tenant = create_test_tenant(&db, "report-proof", &run).await;
        let actor = create_test_user(&db, tenant.id, "report-proof-admin", &run)
            .await
            .id;
        let member = create_test_user(&db, tenant.id, "report-proof-member", &run)
            .await
            .id;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
            [tenant.id.into(), actor.into()],
        ))
        .await
        .unwrap();
        let now = Utc::now();
        let usage = UsageLog::create(
            &db,
            &CreateUsageLogRequest {
                request_id: Uuid::new_v4(),
                tenant_id: tenant.id,
                user_id: member,
                produce_ai_key_id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                model_name: "private-report-marker".into(),
                provider_name: "openai".into(),
                input_tokens: 1,
                output_tokens: 2,
                input_unit_price_snapshot: 1.into(),
                output_unit_price_snapshot: 1.into(),
                user_amount: 7.into(),
                currency: "CNY".into(),
                usage_source: "gateway_accumulated".into(),
                status: "success".into(),
                started_at: now,
                finished_at: now,
            },
        )
        .await
        .unwrap()
        .id;
        let order = PaymentOrder::create(
            &db,
            &CreatePaymentOrderRequest {
                tenant_id: tenant.id,
                user_id: member,
                amount: 7.into(),
                subject: "private-order".into(),
                body: None,
                payment_method: PaymentMethod::WechatPay,
                payment_scene: "native".into(),
                expired_at: now + ChronoDuration::minutes(30),
            },
            &format!("REPORTSESSION{}", Uuid::new_v4().simple()),
            "https://payment.invalid/not-used",
        )
        .await
        .unwrap()
        .id;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        Self {
            db,
            state,
            tenant,
            actor,
            member,
            usage,
            order,
            url,
        }
    }
    async fn token(&self, ttl: i64) -> (String, i64) {
        let user = User::find_by_id(&self.db, self.actor)
            .await
            .unwrap()
            .unwrap();
        let tenant = Tenant::find_by_id(&self.db, self.tenant.id)
            .await
            .unwrap()
            .unwrap();
        let member = TenantMembership::find(&self.db, tenant.id, user.id)
            .await
            .unwrap()
            .unwrap();
        let validator = self.state.auth.get_jwt_validator().unwrap();
        let raw = validator
            .generate_identity_token(
                user.id,
                Some(tenant.id),
                user.token_version,
                Some(tenant.authz_version),
                Some(member.authz_version),
                ttl,
            )
            .unwrap();
        let exp = validator.validate_claims(&raw).unwrap().exp;
        (raw, exp)
    }
    fn paths(&self) -> Vec<(String, &'static str)> {
        let prefix = format!("/api/v1/tenants/{}", self.tenant.id);
        vec![
            (format!("{prefix}/billing/records"), "usage_logs"),
            (
                format!("{prefix}/billing/records/{}", self.usage),
                "usage_logs",
            ),
            (format!("{prefix}/billing/stats"), "usage_logs"),
            (format!("{prefix}/payments/orders"), "payment_orders"),
            (
                format!("{prefix}/payments/orders/{}", self.order),
                "payment_orders",
            ),
            (
                format!("{prefix}/balances/{}", self.member),
                "user_balances",
            ),
        ]
    }
    async fn case(&self, path: &str, table: &str, change: &str) {
        assert!(matches!(
            table,
            "usage_logs" | "payment_orders" | "user_balances"
        ));
        let mut options = ConnectOptions::new(self.url.clone());
        options
            .max_connections(1)
            .min_connections(1)
            .sqlx_logging(false);
        let connection = Database::connect(options).await.unwrap();
        let pid = connection
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i32>("", "pid")
            .unwrap();
        let mut state = self.state.clone();
        state.pool = Some(DbRouter::single(connection.clone()));
        let (token, expiry) = self.token(if change == "expiry" { 5 } else { 300 }).await;
        let blocker = self.db.begin().await.unwrap();
        blocker
            .execute_unprepared(&format!("LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"))
            .await
            .unwrap();
        let req = Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let pending = tokio::spawn(async move {
            let response = tenant_reporting::router()
                .with_state(state)
                .oneshot(req)
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        });
        tokio::time::timeout(Duration::from_secs(3),async{
            loop{let row=self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT wait_event_type='Lock' AND query ILIKE $2 AS waiting FROM pg_stat_activity WHERE pid=$1",[pid.into(),format!("%{table}%").into()])).await.unwrap();
                if row.is_some_and(|r|r.try_get::<Option<bool>>("","waiting").unwrap()==Some(true)){break;}tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("the exact financial-query connection must reach the table wait after authentication");
        match change {
            "expiry" => {
                let millis = (expiry * 1000 - Utc::now().timestamp_millis() + 50).max(0);
                tokio::time::sleep(Duration::from_millis(millis as u64)).await;
            }
            "token" => {
                blocker
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "UPDATE users SET token_version=token_version+1 WHERE id=$1",
                        [self.actor.into()],
                    ))
                    .await
                    .unwrap();
            }
            "tenant" => {
                blocker
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "UPDATE tenants SET authz_version=authz_version+1 WHERE id=$1",
                        [self.tenant.id.into()],
                    ))
                    .await
                    .unwrap();
            }
            "role" | "regrant" => {
                let (field, first, last) = if change == "role" {
                    ("tenant_role", "member", "admin")
                } else {
                    ("status", "suspended", "active")
                };
                for value in [first, last] {
                    blocker.execute(Statement::from_sql_and_values(DbBackend::Postgres,format!("UPDATE tenant_memberships SET {field}=$3 WHERE tenant_id=$1 AND user_id=$2"),[self.tenant.id.into(),self.actor.into(),value.into()])).await.unwrap();
                }
            }
            _ => panic!("unknown test case"),
        }
        blocker.commit().await.unwrap();
        let (status, body) = pending.await.unwrap();
        connection.close().await.unwrap();
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{change}: stale financial result was published at {path}"
        );
        for key in ["items", "currencies", "available_balance"] {
            assert!(body.get(key).is_none(), "{key} leaked from denied report");
        }
        assert!(!body.to_string().contains("private-report-marker"));
        let current = self.token(300).await.0;
        let request = Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer {current}"))
            .body(Body::empty())
            .unwrap();
        let current_response = tenant_reporting::router()
            .with_state(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(
            current_response.status(),
            StatusCode::OK,
            "a new valid request must still work"
        );
        assert_eq!(
            current_response.headers()["cache-control"],
            "private, no-store"
        );
    }
}
async fn isolated(expiry: bool) {
    assert!(
        std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
            || std::env::var_os("CI").is_some()
    );
    let raw = integration_tests::common::resolve_database_url();
    let mut endpoint = url::Url::parse(&raw).unwrap();
    assert!(matches!(
        endpoint.host_str(),
        Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
    ));
    let parent = create_test_pool().await;
    let name = format!("kc_report_session_{}", Uuid::new_v4().simple());
    parent
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    endpoint.set_path(&name);
    let url = endpoint.to_string();
    let db = Database::connect(url.as_str()).await.unwrap();
    let owned = db.clone();
    let outcome = tokio::spawn(async move {
        keycompute_db::initialize_schema(&owned).await.unwrap();
        let tx = owned.begin().await.unwrap();
        User::bootstrap_root(&tx, "report-root@fixture.invalid", None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let f = Fixture::new(owned, url).await;
        if expiry {
            for (path, table) in f.paths() {
                f.case(&path, table, "expiry").await;
            }
        } else {
            let (path, table) = f.paths().remove(0);
            for change in ["token", "tenant", "role", "regrant"] {
                f.case(&path, table, change).await;
            }
        }
        let wallets =
            f.db.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*)::BIGINT AS n FROM user_balances WHERE tenant_id=$1",
                [f.tenant.id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            wallets.try_get::<i64>("", "n").unwrap(),
            0,
            "reporting must not initialize wallets"
        );
    })
    .await;
    db.close().await.unwrap();
    parent
        .execute_unprepared(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    outcome.unwrap();
}
#[tokio::test]
async fn queued_financial_reads_reject_changed_and_regranted_original_sessions() {
    isolated(false).await;
}
#[tokio::test]
async fn financial_reads_reject_signed_expiry_after_all_report_waits() {
    isolated(true).await;
}
