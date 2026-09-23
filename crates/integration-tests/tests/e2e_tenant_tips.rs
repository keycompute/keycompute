//! Real PostgreSQL/router financial boundaries; no external payment or SMTP.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use chrono::Utc;
use integration_tests::{
    common::generate_test_id,
    db::{
        TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
        fixture_dispatch_identity,
    },
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{
    AuditContext, CreateProduceAiKeyRequest, CreateUsageLogRequest, DbRouter, NodeTip,
    NodeTipWithdrawal, Tenant, UsageLog, User,
    models::{
        financial_scope::{FinancialMembership, FinancialScope, FinancialSession},
        node::{CreateNodeRequest, Node},
        node_tip_withdrawal::{WithdrawalFilter, WithdrawalIntent},
    },
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope, TenantRole, TenantScope};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde_json::{Value, json};
use std::{sync::Once, time::Duration as StdDuration};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(
    app: Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value, HeaderMap) {
    let response = tokio::time::timeout(
        StdDuration::from_secs(20),
        app.oneshot(
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
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let data = to_bytes(response.into_body(), 2 << 20).await.unwrap();
    let value = serde_json::from_slice(&data).unwrap_or(Value::Null);
    (status, value, headers)
}
#[track_caller]
fn ok(result: (StatusCode, Value, HeaderMap)) -> Value {
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    result.1
}
fn audit(user: Uuid) -> AuditContext {
    AuditContext {
        actor_user_id: user,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: None,
        request_id: Some(Uuid::new_v4()),
    }
}
struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    a: Tenant,
    b: Tenant,
    user: TenantActor,
    admin: TenantActor,
    root: TenantActor,
    operator: TenantActor,
    key: String,
}
impl Fixture {
    async fn new() -> Self {
        static CRYPTO: Once = Once::new();
        CRYPTO.call_once(|| {
            keycompute_runtime::set_global_crypto(&keycompute_runtime::ApiKeyCrypto::generate_key())
                .unwrap()
        });
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), &run);
        let a = create_test_tenant(&db, "tip-a", &run).await;
        let b = create_test_tenant(&db, "tip-b", &run).await;
        let user = create_test_user(&db, a.id, "tip-owner", &run).await;
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status) VALUES($1,$2,'member','active')",
            [b.id.into(),user.id.into()])).await.unwrap();
        let admin = create_test_user(&db, a.id, "tip-admin", &run).await;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
            [a.id.into(), admin.id.into()],
        ))
        .await
        .unwrap();
        let root = create_test_user(&db, b.id, "tip-root", &run).await;
        let operator = create_test_user(&db, b.id, "tip-operator", &run).await;
        for (user, role) in [(root.id, "root"), (operator.id, "operator")] {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET platform_role=$2 WHERE id=$1",
                [user.into(), role.into()],
            ))
            .await
            .unwrap();
        }
        let key = ProduceAiKeyValidator::generate_key();
        integration_tests::db::create_test_api_key(
            &db,
            &CreateProduceAiKeyRequest {
                tenant_id: a.id,
                user_id: user.id,
                name: "tip-inference".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "sk-tip***".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        let state =
            AppState::try_with_pool_and_config(DbRouter::single(db.clone()), Default::default())
                .await
                .unwrap();
        Self {
            db,
            guard,
            state,
            a,
            b,
            user,
            admin,
            root,
            operator,
            key,
        }
    }
    async fn token(&self, user: Uuid, tenant: Option<Uuid>) -> String {
        let current = User::find_by_id(&self.db, user).await.unwrap().unwrap();
        let token = self
            .state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(user, None, current.token_version, None, None, 3600)
            .unwrap();
        let ctx = self.state.auth.verify_token(&token).await.unwrap();
        self.state
            .auth
            .select_tenant(&ctx, tenant)
            .await
            .unwrap()
            .access_token
    }
    async fn scope(&self, user: Uuid, tenant: Uuid, kind: &str) -> FinancialScope {
        let u = User::find_by_id(&self.db, user).await.unwrap().unwrap();
        if kind == "root" {
            return FinancialScope::platform_tenant(
                PlatformScope::checked(user, PlatformRole::Root).unwrap(),
                FinancialSession {
                    user_id: user,
                    credential_kind: CredentialKind::Jwt,
                    token_version: u.token_version,
                    expires_at: Utc::now().timestamp() + 3600,
                    selected: None,
                },
                tenant,
            )
            .unwrap();
        }
        let t = Tenant::find_by_id(&self.db, tenant).await.unwrap().unwrap();
        let m = keycompute_db::TenantMembership::find(&self.db, tenant, user)
            .await
            .unwrap()
            .unwrap();
        let role = m.tenant_role().unwrap();
        let selected = FinancialMembership {
            tenant_id: tenant,
            tenant_role: role,
            tenant_authz_version: t.authz_version,
            membership_authz_version: m.authz_version,
        };
        let session = FinancialSession {
            user_id: user,
            credential_kind: CredentialKind::Jwt,
            token_version: u.token_version,
            expires_at: Utc::now().timestamp() + 3600,
            selected: Some(selected),
        };
        let scope = TenantScope::checked(tenant, user, role).unwrap();
        if kind == "admin" {
            FinancialScope::tenant_admin(scope, session).unwrap()
        } else {
            FinancialScope::personal(scope, session).unwrap()
        }
    }
    async fn seed(&self, tenant: &Tenant, money: &str, bill: i64) -> NodeTip {
        let consumer = tenant.owner_user_id;
        let node = Node::create(
            &self.db,
            &CreateNodeRequest {
                tenant_id: tenant.id,
                owner_user_id: self.user.id,
                client_instance_id: Uuid::new_v4().to_string(),
                display_name: "financial fixture".into(),
                capabilities_json: json!({}),
            },
        )
        .await
        .unwrap();
        let key = integration_tests::db::create_test_api_key(
            &self.db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant.id,
                user_id: consumer,
                name: "settled fixture".into(),
                produce_ai_key_hash: Uuid::new_v4().simple().to_string(),
                produce_ai_key_preview: "fixture***".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        let now = Utc::now();
        let log = UsageLog::create(
            &self.db,
            &CreateUsageLogRequest {
                request_id: Uuid::new_v4(),
                tenant_id: tenant.id,
                user_id: consumer,
                produce_ai_key_id: key.id,
                model_name: "fixture".into(),
                provider_name: "node".into(),
                account_id: Uuid::new_v4(),
                input_tokens: 7,
                output_tokens: 3,
                input_unit_price_snapshot: 1.into(),
                output_unit_price_snapshot: 1.into(),
                user_amount: bill.into(),
                currency: money.into(),
                usage_source: "upstream".into(),
                status: "success".into(),
                started_at: now,
                finished_at: now,
            },
        )
        .await
        .unwrap();
        self.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO node_tasks(request_id,tenant_id,user_id,model,payload_json,status,assigned_node_id,deadline_at,complete_grace_until) VALUES($1,$2,$3,'fixture',$4,'succeeded',$5,clock_timestamp()+interval '1 minute',clock_timestamp()+interval '2 minutes')",
            [log.request_id.into(),tenant.id.into(),consumer.into(),json!({"dispatch_identity":fixture_dispatch_identity(&self.db,tenant.id,consumer).await}).into(),node.id.into()])).await.unwrap();
        NodeTip::create_from_usage_log(&self.db, tenant.id, log.id)
            .await
            .unwrap()
            .unwrap()
    }
    fn request(&self, kind: &str, id: Uuid) -> Value {
        if kind == "balance" {
            json!({"request_id":id,"withdrawal_type":kind,"currency":"CNY","alipay_account":null,"real_name":null})
        } else {
            json!({"request_id":id,"withdrawal_type":kind,"currency":"CNY","alipay_account":"secret-payee@example.invalid","real_name":"Private Payee"})
        }
    }
    async fn finish(mut self) {
        for tenant in [self.a.id, self.b.id] {
            self.db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM node_tip_withdrawals WHERE tenant_id=$1",
                    [tenant.into()],
                ))
                .await
                .unwrap();
        }
        self.guard.cleanup().await.unwrap();
    }
}
fn balance_intent(id: Uuid) -> WithdrawalIntent {
    WithdrawalIntent {
        request_id: id,
        withdrawal_type: "balance".into(),
        currency: "CNY".into(),
        recipient_fingerprint: NodeTipWithdrawal::recipient_fingerprint("balance", None, None)
            .unwrap(),
        encrypted_alipay_account: None,
        encrypted_real_name: None,
    }
}

#[tokio::test]
async fn personal_earnings_and_conversion_are_fixed_to_selected_tenant_and_currency() {
    let f = Fixture::new().await;
    let a = f.seed(&f.a, "CNY", 100).await;
    let b = f.seed(&f.b, "CNY", 200).await;
    f.seed(&f.a, "USD", 300).await;
    let ta = f.token(f.user.id, Some(f.a.id)).await;
    let tb = f.token(f.user.id, Some(f.b.id)).await;
    let app = create_router(f.state.clone());
    let sa = ok(call(app.clone(), "GET", "/api/v1/me/tips", &ta, Value::Null).await);
    let sb = ok(call(app.clone(), "GET", "/api/v1/me/tips", &tb, Value::Null).await);
    assert_eq!(
        sa["pending_amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        a.tip_amount
    );
    assert_eq!(
        sb["pending_amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        b.tip_amount
    );
    let request = f.request("balance", Uuid::new_v4());
    let first = ok(call(
        app.clone(),
        "POST",
        "/api/v1/me/tips/withdraw",
        &ta,
        request.clone(),
    )
    .await);
    let repeat = ok(call(
        app.clone(),
        "POST",
        "/api/v1/me/tips/withdraw",
        &ta,
        request.clone(),
    )
    .await);
    assert_eq!(first["id"], repeat["id"]);
    assert_eq!(first["status"], "completed");
    let rows=f.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT tenant_id,user_id,amount FROM balance_transactions WHERE user_id=$1 AND transaction_type='tip_credit'",[f.user.id.into()])).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].try_get::<Uuid>("", "tenant_id").unwrap(), f.a.id);
    assert_eq!(
        rows[0].try_get::<Decimal>("", "amount").unwrap(),
        a.tip_amount
    );
    let sa = ok(call(app.clone(), "GET", "/api/v1/me/tips", &ta, Value::Null).await);
    assert_eq!(
        sa["pending_amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::ZERO
    );
    assert_eq!(
        sa["withdrawn_amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        a.tip_amount
    );
    let sb = ok(call(app.clone(), "GET", "/api/v1/me/tips", &tb, Value::Null).await);
    assert_eq!(
        sb["pending_amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        b.tip_amount
    );
    let bad = call(
        app.clone(),
        "POST",
        "/api/v1/me/tips/withdraw",
        &ta,
        json!({"request_id":Uuid::new_v4(),"currency":"USD","withdrawal_type":"balance"}),
    )
    .await;
    assert_eq!(bad.0, StatusCode::BAD_REQUEST);
    for url in ["/api/v1/me/tips", "/api/v1/me/tips/withdrawals"] {
        let denied = call(app.clone(), "GET", url, &f.key, Value::Null).await;
        assert!(matches!(
            denied.0,
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
        ));
    }
    let mut conflict = request;
    conflict["withdrawal_type"] = "alipay".into();
    conflict["alipay_account"] = "a@fixture.invalid".into();
    conflict["real_name"] = "Other".into();
    assert_eq!(
        call(app, "POST", "/api/v1/me/tips/withdraw", &ta, conflict)
            .await
            .0,
        StatusCode::CONFLICT
    );
    f.finish().await;
}

#[tokio::test]
async fn tenant_review_is_metadata_only_and_root_payout_requires_explicit_audited_access() {
    let f = Fixture::new().await;
    f.seed(&f.a, "CNY", 100).await;
    let app = create_router(f.state.clone());
    let owner = f.token(f.user.id, Some(f.a.id)).await;
    let admin = f.token(f.admin.id, Some(f.a.id)).await;
    let foreign = f.token(f.b.owner_user_id, Some(f.b.id)).await;
    let root = f.token(f.root.id, None).await;
    let operator = f.token(f.operator.id, None).await;
    let created = ok(call(
        app.clone(),
        "POST",
        "/api/v1/me/tips/withdraw",
        &owner,
        f.request("alipay", Uuid::new_v4()),
    )
    .await);
    let id = created["id"].as_str().unwrap();
    let rev = created["revision"].as_i64().unwrap();
    let path = format!("/api/v1/tenants/{}/tips/withdrawals", f.a.id);
    let listed = call(app.clone(), "GET", &path, &admin, Value::Null).await;
    assert_eq!(listed.0, StatusCode::OK, "{}", listed.1);
    assert_eq!(listed.1["total"], 1);
    assert!(
        listed.2["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let raw = listed.1.to_string();
    assert!(
        !raw.contains("secret-payee")
            && !raw.contains("Private Payee")
            && !raw.contains("encrypted_")
            && !raw.contains("fingerprint")
    );
    for token in [&owner, &foreign, &operator, &f.key] {
        assert!(matches!(
            call(app.clone(), "GET", &path, token, Value::Null).await.0,
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
        ));
    }
    let support = format!(
        "/api/v1/platform/tenants/{}/tips/withdrawals/{id}/support-detail?reason=review-fixture",
        f.a.id
    );
    assert_eq!(
        call(app.clone(), "GET", &support, &operator, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(app.clone(), "GET", &support, &admin, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let secret = ok(call(app.clone(), "GET", &support, &root, Value::Null).await);
    assert_eq!(secret["alipay_account"], "secret-payee@example.invalid");
    let audit=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT metadata FROM tenant_audit_events WHERE tenant_id=$1 AND actor_user_id=$2 AND action='withdrawal.payout_access'",[f.a.id.into(),f.root.id.into()])).await.unwrap().unwrap();
    assert!(
        !audit
            .try_get::<Value>("", "metadata")
            .unwrap()
            .to_string()
            .contains("secret-payee")
    );
    let approved = ok(call(
        app.clone(),
        "POST",
        &format!("{path}/{id}/approve"),
        &admin,
        json!({"expected_revision":rev,"reason":"tenant approval"}),
    )
    .await);
    assert_eq!(approved["status"], "approved");
    assert_eq!(
        call(
            app.clone(),
            "POST",
            &format!("{path}/{id}/reject"),
            &admin,
            json!({"expected_revision":rev,"reason":"stale"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let complete = format!(
        "/api/v1/platform/tenants/{}/tips/withdrawals/{id}/complete",
        f.a.id
    );
    let command = json!({"expected_revision":approved["revision"],"reason":"fixture payment attestation","payout_reference":"fixture-transaction-123"});
    assert_eq!(
        call(app.clone(), "POST", &complete, &admin, command.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let done = ok(call(app.clone(), "POST", &complete, &root, command.clone()).await);
    assert_eq!(done["status"], "completed");
    assert_eq!(
        ok(call(app, "POST", &complete, &root, command).await)["id"],
        done["id"]
    );
    f.finish().await;
}

#[tokio::test]
async fn concurrent_withdrawal_retries_never_double_credit_or_consume_future_earnings() {
    let f = Fixture::new().await;
    let tip = f.seed(&f.a, "CNY", 100).await;
    let scope = f.scope(f.user.id, f.a.id, "personal").await;
    let actor = audit(f.user.id);
    let intent = balance_intent(Uuid::new_v4());
    let (one, two) = tokio::join!(
        NodeTipWithdrawal::create(&f.db, scope, &actor, &intent),
        NodeTipWithdrawal::create(&f.db, scope, &actor, &intent)
    );
    let first = one.unwrap();
    let second = two.unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.balance_transaction_id, second.balance_transaction_id);
    assert_eq!(first.total_amount, tip.tip_amount);
    let future = f.seed(&f.a, "CNY", 50).await;
    assert_eq!(
        NodeTipWithdrawal::create(&f.db, scope, &actor, &intent)
            .await
            .unwrap()
            .id,
        first.id
    );
    assert_eq!(
        NodeTip::summary(&f.db, scope, "CNY")
            .await
            .unwrap()
            .pending_amount,
        future.tip_amount
    );
    let next = balance_intent(Uuid::new_v4());
    let other = balance_intent(Uuid::new_v4());
    let (one, two) = tokio::join!(
        NodeTipWithdrawal::create(&f.db, scope, &actor, &next),
        NodeTipWithdrawal::create(&f.db, scope, &actor, &other)
    );
    assert_ne!(
        one.is_ok(),
        two.is_ok(),
        "only one distinct command can consume the current amount"
    );
    let summary = NodeTip::summary(&f.db, scope, "CNY").await.unwrap();
    assert_eq!(summary.pending_amount, Decimal::ZERO);
    assert_eq!(summary.withdrawn_amount, tip.tip_amount + future.tip_amount);
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n,SUM(amount) AS amount FROM balance_transactions WHERE tenant_id=$1 AND user_id=$2 AND transaction_type='tip_credit'",[f.a.id.into(),f.user.id.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 2);
    assert_eq!(
        row.try_get::<Decimal>("", "amount").unwrap(),
        summary.withdrawn_amount
    );
    f.finish().await;
}

#[tokio::test]
async fn financial_commands_reject_forged_credentials_audit_actors_and_current_role_changes() {
    let f = Fixture::new().await;
    f.seed(&f.a, "CNY", 100).await;
    let scope = f.scope(f.user.id, f.a.id, "personal").await;
    let intent = balance_intent(Uuid::new_v4());
    let mut wrong = audit(f.admin.id);
    assert!(
        NodeTipWithdrawal::create(&f.db, scope, &wrong, &intent)
            .await
            .is_err()
    );
    wrong = audit(f.user.id);
    wrong.credential_kind = CredentialKind::ApiKey;
    assert!(
        NodeTipWithdrawal::create(&f.db, scope, &wrong, &intent)
            .await
            .is_err()
    );
    let t = Tenant::find_by_id(&f.db, f.a.id).await.unwrap().unwrap();
    let m = keycompute_db::TenantMembership::find(&f.db, t.id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    let u = User::find_by_id(&f.db, f.user.id).await.unwrap().unwrap();
    let claimed = FinancialSession {
        user_id: u.id,
        credential_kind: CredentialKind::Jwt,
        token_version: u.token_version,
        expires_at: Utc::now().timestamp() + 3600,
        selected: Some(FinancialMembership {
            tenant_id: t.id,
            tenant_role: TenantRole::Admin,
            tenant_authz_version: t.authz_version,
            membership_authz_version: m.authz_version,
        }),
    };
    let fake = FinancialScope::tenant_admin(
        TenantScope::checked(t.id, u.id, TenantRole::Admin).unwrap(),
        claimed,
    )
    .unwrap();
    assert!(
        NodeTipWithdrawal::count_in_scope(&f.db, fake, &WithdrawalFilter::default())
            .await
            .is_err()
    );
    assert!(
        NodeTipWithdrawal::list_in_scope(&f.db, fake, &WithdrawalFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
    for credential in [
        CredentialKind::ApiKey,
        CredentialKind::Node,
        CredentialKind::System,
    ] {
        let selected = FinancialSession {
            credential_kind: credential,
            ..claimed
        };
        assert!(
            FinancialScope::tenant_admin(
                TenantScope::checked(t.id, u.id, TenantRole::Admin).unwrap(),
                selected
            )
            .is_err()
        );
    }
    let forged_root = FinancialScope::platform_tenant(
        PlatformScope::checked(u.id, PlatformRole::Root).unwrap(),
        FinancialSession {
            selected: None,
            ..claimed
        },
        t.id,
    )
    .unwrap();
    assert!(
        NodeTipWithdrawal::count_in_scope(&f.db, forged_root, &WithdrawalFilter::default())
            .await
            .is_err()
    );
    // A legitimate cached scope loses authority when the current identity changes.
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET token_version=token_version+1 WHERE id=$1",
        [u.id.into()],
    ))
    .await
    .unwrap();
    assert!(
        NodeTipWithdrawal::create(&f.db, scope, &audit(u.id), &intent)
            .await
            .is_err()
    );
    let rows =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM node_tip_withdrawals WHERE tenant_id=$1",
            [t.id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.try_get::<i64>("", "n").unwrap(), 0);
    f.finish().await;
}

#[tokio::test]
async fn withdrawal_audit_failure_rolls_back_wallet_and_intent_even_if_outer_transaction_commits() {
    let f = Fixture::new().await;
    f.seed(&f.a, "CNY", 100).await;
    let scope = f.scope(f.user.id, f.a.id, "personal").await;
    let intent = balance_intent(Uuid::new_v4());
    let outer = f.db.begin().await.unwrap();
    outer
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let function = format!("tips_fault_{}", Uuid::new_v4().simple());
    outer.execute_unprepared(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.action='tips.convert' THEN RAISE EXCEPTION 'isolated financial audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {function} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {function}();",f.a.id)).await.unwrap();
    let failed = NodeTipWithdrawal::create(&outer, scope, &audit(f.user.id), &intent).await;
    assert!(failed.is_err());
    let counts=outer.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT (SELECT COUNT(*) FROM node_tip_withdrawals WHERE tenant_id=$1) AS withdrawals,(SELECT COUNT(*) FROM balance_transactions WHERE tenant_id=$1 AND transaction_type='tip_credit') AS credits",
        [f.a.id.into()])).await.unwrap().unwrap();
    assert_eq!(counts.try_get::<i64>("", "withdrawals").unwrap(), 0);
    assert_eq!(counts.try_get::<i64>("", "credits").unwrap(), 0);
    outer
        .execute_unprepared(&format!(
            "DROP TRIGGER {function} ON tenant_audit_events; DROP FUNCTION {function}();"
        ))
        .await
        .unwrap();
    outer.commit().await.unwrap();
    let valid = NodeTipWithdrawal::create(&f.db, scope, &audit(f.user.id), &intent)
        .await
        .unwrap();
    assert_eq!(valid.status, "completed");
    f.finish().await;
}

#[tokio::test]
async fn withdrawal_identity_amount_and_terminal_results_are_database_immutable() {
    let f = Fixture::new().await;
    let tip = f.seed(&f.a, "CNY", 100).await;
    let scope = f.scope(f.user.id, f.a.id, "personal").await;
    let row = NodeTipWithdrawal::create(
        &f.db,
        scope,
        &audit(f.user.id),
        &balance_intent(Uuid::new_v4()),
    )
    .await
    .unwrap();
    for sql in [
        "UPDATE node_tip_withdrawals SET tenant_id=$2 WHERE id=$1",
        "UPDATE node_tip_withdrawals SET owner_user_id=$2 WHERE id=$1",
    ] {
        assert!(
            f.db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                [row.id.into(), f.b.id.into()]
            ))
            .await
            .is_err()
        );
    }
    for sql in [
        "UPDATE node_tip_withdrawals SET total_amount=total_amount+1 WHERE id=$1",
        "UPDATE node_tip_withdrawals SET status='pending',completed_at=NULL,balance_transaction_id=NULL WHERE id=$1",
    ] {
        assert!(
            f.db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                [row.id.into()]
            ))
            .await
            .is_err()
        );
    }
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_tips SET tenant_id=$2 WHERE id=$1",
            [tip.id.into(), f.b.id.into()]
        ))
        .await
        .is_err()
    );
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_tips SET tip_amount=tip_amount+1 WHERE id=$1",
            [tip.id.into()]
        ))
        .await
        .is_err()
    );
    let stored = NodeTipWithdrawal::find_in_scope(&f.db, scope, row.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.total_amount, row.total_amount);
    assert_eq!(stored.revision, row.revision);
    f.finish().await;
}

#[tokio::test]
async fn accepted_tip_accrual_retains_removed_owner_but_removed_member_cannot_withdraw() {
    let f = Fixture::new().await;
    let tip = f.seed(&f.a, "CNY", 100).await;
    let scope = f.scope(f.user.id, f.a.id, "personal").await;
    // Reproduce a delayed post-ledger retry from its original immutable source.
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM node_tips WHERE tenant_id=$1 AND id=$2",
        [f.a.id.into(), tip.id.into()],
    ))
    .await
    .unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='removed' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), f.user.id.into()],
    ))
    .await
    .unwrap();
    let late = NodeTip::create_from_usage_log(&f.db, f.a.id, tip.usage_log_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(late.owner_user_id, tip.owner_user_id);
    assert_eq!(late.consumer_user_id, tip.consumer_user_id);
    assert_eq!(late.currency, tip.currency);
    assert_eq!(late.tenant_id, tip.tenant_id);
    assert!(
        NodeTip::create_from_usage_log(&f.db, f.b.id, tip.usage_log_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        NodeTipWithdrawal::create(
            &f.db,
            scope,
            &audit(f.user.id),
            &balance_intent(Uuid::new_v4())
        )
        .await
        .is_err()
    );
    let admin = f.scope(f.admin.id, f.a.id, "admin").await;
    assert_eq!(
        NodeTip::summary(&f.db, admin, "CNY")
            .await
            .unwrap()
            .total_amount,
        tip.tip_amount
    );
    f.finish().await;
}

#[tokio::test]
async fn failed_review_and_payout_access_audits_withhold_secrets_and_preserve_withdrawal() {
    use keycompute_db::models::node_tip_withdrawal::{ReviewWithdrawal, WithdrawalReview};
    let f = Fixture::new().await;
    f.seed(&f.a, "CNY", 100).await;
    let token = f.token(f.user.id, Some(f.a.id)).await;
    let created = ok(call(
        create_router(f.state.clone()),
        "POST",
        "/api/v1/me/tips/withdraw",
        &token,
        f.request("alipay", Uuid::new_v4()),
    )
    .await);
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let revision = created["revision"].as_i64().unwrap();
    let admin = f.scope(f.admin.id, f.a.id, "admin").await;
    let root = f.scope(f.root.id, f.a.id, "root").await;
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let name = format!("tip_review_fault_{}", Uuid::new_v4().simple());
    tx.execute_unprepared(&format!("CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.action IN ('withdrawal.approve','withdrawal.reject','withdrawal.payout_access') THEN RAISE EXCEPTION 'isolated review audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {name}();",f.a.id)).await.unwrap();
    for action in [WithdrawalReview::Approve, WithdrawalReview::Reject] {
        let result = NodeTipWithdrawal::review(
            &tx,
            admin,
            &audit(f.admin.id),
            &ReviewWithdrawal {
                id,
                expected_revision: revision,
                action,
                reason: "audit failure regression".into(),
            },
        )
        .await;
        assert!(result.is_err(), "an unaudited review must not commit");
        let retained = NodeTipWithdrawal::find_in_scope(&tx, admin, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.status, "pending");
        assert_eq!(retained.revision, revision);
    }
    assert!(
        NodeTipWithdrawal::support_payout(
            &tx,
            root,
            &audit(f.root.id),
            id,
            "audit failure regression"
        )
        .await
        .is_err(),
        "payout ciphertext must not escape a failed audit"
    );
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {name} ON tenant_audit_events; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let retained = NodeTipWithdrawal::find_in_scope(&f.db, admin, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.status, "pending");
    assert_eq!(retained.revision, revision);
    assert!(retained.payout_details_present);
    let count = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE tenant_id=$1 AND action IN ('withdrawal.approve','withdrawal.reject','withdrawal.payout_access')",
        [f.a.id.into()])).await.unwrap().unwrap();
    assert_eq!(count.try_get::<i64>("", "n").unwrap(), 0);
    f.finish().await;
}

#[tokio::test]
async fn failed_payout_completion_audit_does_not_record_a_paid_withdrawal() {
    use keycompute_db::models::node_tip_withdrawal::{
        CompleteWithdrawal, ReviewWithdrawal, WithdrawalReview,
    };
    let f = Fixture::new().await;
    f.seed(&f.a, "CNY", 100).await;
    let token = f.token(f.user.id, Some(f.a.id)).await;
    let created = ok(call(
        create_router(f.state.clone()),
        "POST",
        "/api/v1/me/tips/withdraw",
        &token,
        f.request("alipay", Uuid::new_v4()),
    )
    .await);
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let admin = f.scope(f.admin.id, f.a.id, "admin").await;
    let root = f.scope(f.root.id, f.a.id, "root").await;
    let approved = NodeTipWithdrawal::review(
        &f.db,
        admin,
        &audit(f.admin.id),
        &ReviewWithdrawal {
            id,
            expected_revision: created["revision"].as_i64().unwrap(),
            action: WithdrawalReview::Approve,
            reason: "fixture approval".into(),
        },
    )
    .await
    .unwrap();
    let command = CompleteWithdrawal {
        id,
        expected_revision: approved.revision,
        reason: "fixture attestation, no external payment".into(),
        payout_reference: "fixture-payment-1".into(),
    };
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let name = format!("tip_complete_fault_{}", Uuid::new_v4().simple());
    tx.execute_unprepared(&format!("CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.action='withdrawal.complete' THEN RAISE EXCEPTION 'isolated completion audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {name}();",f.a.id)).await.unwrap();
    assert!(
        NodeTipWithdrawal::complete_external(&tx, root, &audit(f.root.id), &command)
            .await
            .is_err()
    );
    let retained = NodeTipWithdrawal::find_in_scope(&tx, root, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.status, "approved");
    assert_eq!(retained.revision, approved.revision);
    assert!(retained.completed_at.is_none() && retained.payout_reference.is_none());
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {name} ON tenant_audit_events; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let paid = NodeTipWithdrawal::complete_external(&f.db, root, &audit(f.root.id), &command)
        .await
        .unwrap();
    assert_eq!(paid.status, "completed");
    let replay = NodeTipWithdrawal::complete_external(&f.db, root, &audit(f.root.id), &command)
        .await
        .unwrap();
    assert_eq!(paid.revision, replay.revision);
    let count = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE tenant_id=$1 AND action='withdrawal.complete'",
        [f.a.id.into()])).await.unwrap().unwrap();
    assert_eq!(count.try_get::<i64>("", "n").unwrap(), 1);
    f.finish().await;
}

#[tokio::test]
async fn completed_accrual_retry_does_not_reinterpret_a_later_policy() {
    let f = Fixture::new().await;
    let tip = f.seed(&f.a, "CNY", 100).await;
    let tx = f.db.begin().await.unwrap();
    // This uncommitted fixture value is invisible to other concurrent tests.
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE system_settings SET value=$1 WHERE key='node_tip_ratio'",
        ["invalid-fixture-ratio".into()],
    ))
    .await
    .unwrap();
    assert!(
        NodeTip::create_from_usage_log(&tx, tip.tenant_id, tip.usage_log_id)
            .await
            .unwrap()
            .is_none(),
        "a settled credit must not depend on today's policy"
    );
    let scope = f.scope(f.user.id, f.a.id, "personal").await;
    let saved = NodeTip::list_in_scope(&tx, scope, "CNY", 20, 0)
        .await
        .unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, tip.id);
    assert_eq!(saved[0].tip_amount, tip.tip_amount);
    assert_eq!(saved[0].owner_user_id, tip.owner_user_id);
    assert_eq!(saved[0].currency, tip.currency);
    tx.rollback().await.unwrap();
    f.finish().await;
}

#[tokio::test]
async fn credential_expiry_during_wallet_lock_wait_rolls_back_conversion() {
    use sea_orm::{ConnectOptions, Database};
    let f = Fixture::new().await;
    let tip = f.seed(&f.a, "CNY", 100).await;
    keycompute_db::UserBalance::recharge(
        &f.db,
        f.a.id,
        f.user.id,
        Decimal::ONE,
        None,
        Some("isolated expiry fixture"),
    )
    .await
    .unwrap();
    let mut options = ConnectOptions::new(integration_tests::common::resolve_database_url());
    options.max_connections(1).min_connections(1);
    let worker = Database::connect(options).await.unwrap();
    let pid: i32 = worker
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid() AS pid",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "pid")
        .unwrap();
    let t = Tenant::find_by_id(&f.db, f.a.id).await.unwrap().unwrap();
    let u = User::find_by_id(&f.db, f.user.id).await.unwrap().unwrap();
    let m = keycompute_db::TenantMembership::find(&f.db, t.id, u.id)
        .await
        .unwrap()
        .unwrap();
    let blocker = f.db.begin().await.unwrap();
    blocker
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT user_id FROM user_balances WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE",
            [t.id.into(), u.id.into()],
        ))
        .await
        .unwrap();
    let expires_at = Utc::now().timestamp() + 2;
    let scope = FinancialScope::personal(
        TenantScope::checked(t.id, u.id, m.tenant_role().unwrap()).unwrap(),
        FinancialSession {
            user_id: u.id,
            credential_kind: CredentialKind::Jwt,
            token_version: u.token_version,
            expires_at,
            selected: Some(FinancialMembership {
                tenant_id: t.id,
                tenant_role: m.tenant_role().unwrap(),
                tenant_authz_version: t.authz_version,
                membership_authz_version: m.authz_version,
            }),
        },
    )
    .unwrap();
    let db = worker.clone();
    let pending = tokio::spawn(async move {
        NodeTipWithdrawal::create(&db, scope, &audit(u.id), &balance_intent(Uuid::new_v4())).await
    });
    let reached = tokio::time::timeout(StdDuration::from_secs(1),async {
        loop {
            let row = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock' AND query LIKE '%FROM user_balances%') AS waiting",
                [pid.into()])).await.unwrap().unwrap();
            if row.try_get::<bool>("","waiting").unwrap() {break;}
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }
    }).await;
    if reached.is_ok() {
        while Utc::now().timestamp() < expires_at {
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }
    }
    blocker.rollback().await.unwrap();
    let result = tokio::time::timeout(StdDuration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    worker.close().await.unwrap();
    let counts = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT (SELECT COUNT(*) FROM node_tip_withdrawals WHERE tenant_id=$1) AS withdrawals,(SELECT COUNT(*) FROM balance_transactions WHERE tenant_id=$1 AND transaction_type='tip_credit') AS credits",
        [t.id.into()])).await.unwrap().unwrap();
    let current = f.scope(f.user.id, f.a.id, "personal").await;
    let remaining = NodeTip::summary(&f.db, current, "CNY")
        .await
        .unwrap()
        .pending_amount;
    f.finish().await;
    assert!(
        reached.is_ok(),
        "the exact conversion connection never waited for its wallet row"
    );
    assert!(
        matches!(result,Err(keycompute_db::DbError::Other(ref code)) if code=="financial_authority_invalid"),
        "expiry must fail closed after the wallet wait: {result:?}"
    );
    assert_eq!(counts.try_get::<i64>("", "withdrawals").unwrap(), 0);
    assert_eq!(counts.try_get::<i64>("", "credits").unwrap(), 0);
    assert_eq!(remaining, tip.tip_amount);
}
