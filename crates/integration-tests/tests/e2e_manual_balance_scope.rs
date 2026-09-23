//! Actual primary-DB authority, savepoint rollback and queued console commands.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::Utc;
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    AuditContext, DbRouter, ManualBalanceCommand, ManualBalanceOperationDecision as Decision,
    ManualBalanceOperationKind as Kind, UserBalance,
    models::financial_scope::{FinancialMembership, FinancialScope, FinancialSession},
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope, TenantRole, TenantScope};
use rust_decimal::Decimal;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{Value, json};
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    tenant: Uuid,
    foreign: Uuid,
    root: Uuid,
    user: Uuid,
    admin: Uuid,
    operator: Uuid,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), &run);
        let tenant = create_test_tenant(&db, "wallet-scope", &run).await.id;
        let foreign = create_test_tenant(&db, "wallet-foreign", &run).await.id;
        let root = create_test_user(&db, tenant, "wallet-root", &run).await.id;
        let user = create_test_user(&db, tenant, "wallet-user", &run).await.id;
        let admin = create_test_user(&db, tenant, "wallet-admin", &run).await.id;
        let operator = create_test_user(&db, tenant, "wallet-operator", &run)
            .await
            .id;
        for (id, role) in [(root, "root"), (operator, "operator")] {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET platform_role=$2 WHERE id=$1",
                [id.into(), role.into()],
            ))
            .await
            .unwrap();
        }
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
            [tenant.into(), admin.into()],
        ))
        .await
        .unwrap();
        UserBalance::get_or_create(&db, tenant, user).await.unwrap();
        UserBalance::recharge(&db, tenant, user, Decimal::from(10), None, None)
            .await
            .unwrap();
        Self {
            db,
            guard,
            tenant,
            foreign,
            root,
            user,
            admin,
            operator,
        }
    }
    async fn session(&self, id: Uuid) -> FinancialSession {
        let u = keycompute_db::User::find_by_id(&self.db, id)
            .await
            .unwrap()
            .unwrap();
        FinancialSession {
            user_id: id,
            credential_kind: CredentialKind::Jwt,
            token_version: u.token_version,
            expires_at: Utc::now().timestamp() + 3600,
            selected: None,
        }
    }
    async fn scope(&self, id: Uuid, target: Uuid) -> FinancialScope {
        // A forged type is deliberately possible here: SQL must independently deny non-root actors.
        FinancialScope::platform_tenant(
            PlatformScope::checked(id, PlatformRole::Root).unwrap(),
            self.session(id).await,
            target,
        )
        .unwrap()
    }
    async fn balance(&self) -> UserBalance {
        UserBalance::find_by_user(&self.db, self.tenant, self.user)
            .await
            .unwrap()
            .unwrap()
    }
    async fn claims(&self) -> i64 {
        self.db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*)::bigint AS n FROM admin_balance_operations WHERE tenant_id=$1",
                [self.tenant.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap()
    }
    async fn token(&self, state: &AppState) -> String {
        let session = self.session(self.root).await;
        state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(self.root, None, session.token_version, None, None, 3600)
            .unwrap()
    }
    async fn finish(mut self) {
        self.guard.cleanup().await.unwrap();
    }
}
fn audit(id: Uuid) -> AuditContext {
    AuditContext {
        actor_user_id: id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        request_id: Some(Uuid::new_v4()),
    }
}
fn command(user_id: Uuid, kind: Kind, key: &str) -> ManualBalanceCommand<'_> {
    ManualBalanceCommand {
        kind,
        user_id,
        amount: Decimal::ONE,
        reason: "isolated wallet operation",
        idempotency_key: key,
    }
}
async fn post(app: Router, tenant: Uuid, user: Uuid, token: &str) -> (StatusCode, Value) {
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/users/{user}/balance"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", Uuid::new_v4().to_string())
                .body(Body::from(
                    json!({"tenant_id":tenant,"amount":"1","reason":"queued authority test"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn manual_wallet_commands_require_current_root_and_matching_console_actor() {
    let f = Fixture::new().await;
    let key = Uuid::new_v4().to_string();
    let cmd = command(f.user, Kind::Recharge, &key);
    for id in [f.user, f.admin, f.operator] {
        assert!(
            UserBalance::apply_admin_manual_operation(
                &f.db,
                f.scope(id, f.tenant).await,
                &audit(id),
                &cmd
            )
            .await
            .is_err()
        );
    }
    let scope = f.scope(f.root, f.tenant).await;
    assert!(
        UserBalance::apply_admin_manual_operation(&f.db, scope, &audit(f.admin), &cmd)
            .await
            .is_err()
    );
    for kind in [
        CredentialKind::ApiKey,
        CredentialKind::Node,
        CredentialKind::System,
    ] {
        let mut wrong = audit(f.root);
        wrong.credential_kind = kind;
        assert!(
            UserBalance::apply_admin_manual_operation(&f.db, scope, &wrong, &cmd)
                .await
                .is_err()
        );
    }
    let t = keycompute_db::Tenant::find_by_id(&f.db, f.tenant)
        .await
        .unwrap()
        .unwrap();
    let m = keycompute_db::TenantMembership::find(&f.db, f.tenant, f.admin)
        .await
        .unwrap()
        .unwrap();
    let tenant_scope = FinancialScope::tenant_admin(
        TenantScope::checked(f.tenant, f.admin, TenantRole::Admin).unwrap(),
        FinancialSession {
            selected: Some(FinancialMembership {
                tenant_id: f.tenant,
                tenant_role: TenantRole::Admin,
                tenant_authz_version: t.authz_version,
                membership_authz_version: m.authz_version,
            }),
            ..f.session(f.admin).await
        },
    )
    .unwrap();
    assert!(
        UserBalance::apply_admin_manual_operation(&f.db, tenant_scope, &audit(f.admin), &cmd)
            .await
            .is_err()
    );
    assert!(
        UserBalance::apply_admin_manual_operation(
            &f.db,
            f.scope(f.root, f.foreign).await,
            &audit(f.root),
            &cmd
        )
        .await
        .is_err()
    );
    assert_eq!(f.claims().await, 0);
    assert_eq!(f.balance().await.available_balance, Decimal::from(10));
    f.finish().await;
}

#[tokio::test]
async fn idempotent_wallet_replay_rechecks_live_identity_before_returning_the_snapshot() {
    let f = Fixture::new().await;
    let scope = f.scope(f.root, f.tenant).await;
    let id = audit(f.root);
    let key = Uuid::new_v4().to_string();
    let cmd = command(f.user, Kind::Recharge, &key);
    let first = UserBalance::apply_admin_manual_operation(&f.db, scope, &id, &cmd)
        .await
        .unwrap();
    assert!(matches!(first, Decision::Completed(_)));
    assert_eq!(
        UserBalance::apply_admin_manual_operation(&f.db, scope, &audit(f.root), &cmd)
            .await
            .unwrap(),
        first
    );
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT request_id,metadata FROM tenant_audit_events WHERE tenant_id=$1 AND action='balance.recharge'",[f.tenant.into()])).await.unwrap().unwrap();
    assert_eq!(
        row.try_get::<Uuid>("", "request_id").unwrap(),
        id.request_id.unwrap()
    );
    assert!(
        !row.try_get::<Value>("", "metadata")
            .unwrap()
            .to_string()
            .contains(&key)
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET token_version=token_version+1 WHERE id=$1",
        [f.root.into()],
    ))
    .await
    .unwrap();
    assert!(
        UserBalance::apply_admin_manual_operation(&f.db, scope, &id, &cmd)
            .await
            .is_err()
    );
    assert_eq!(
        UserBalance::apply_admin_manual_operation(
            &f.db,
            f.scope(f.root, f.tenant).await,
            &audit(f.root),
            &cmd
        )
        .await
        .unwrap(),
        first
    );
    assert_eq!(f.balance().await.available_balance, Decimal::from(11));
    assert_eq!(f.claims().await, 1);
    let count=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE tenant_id=$1 AND action='balance.recharge'",[f.tenant.into()])).await.unwrap().unwrap();
    assert_eq!(count.try_get::<i64>("", "n").unwrap(), 1);
    f.finish().await;
}

#[tokio::test]
async fn every_manual_wallet_kind_rolls_back_on_audit_failure_even_if_outer_commit_succeeds() {
    let f = Fixture::new().await;
    UserBalance::freeze(&f.db, f.tenant, f.user, Decimal::from(3), None)
        .await
        .unwrap();
    let scope = f.scope(f.root, f.tenant).await;
    let before = f.balance().await;
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let name = format!("wallet_audit_{}", Uuid::new_v4().simple());
    tx.execute_unprepared(&format!("CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.action LIKE 'balance.%' THEN RAISE EXCEPTION 'isolated audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {name}();",f.tenant)).await.unwrap();
    for kind in [Kind::Recharge, Kind::Consume, Kind::Freeze, Kind::Unfreeze] {
        let key = Uuid::new_v4().to_string();
        assert!(
            UserBalance::apply_admin_manual_operation(
                &tx,
                scope,
                &audit(f.root),
                &command(f.user, kind, &key)
            )
            .await
            .is_err()
        );
        let row = UserBalance::find_by_user(&tx, f.tenant, f.user)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.available_balance, before.available_balance);
        assert_eq!(row.frozen_balance, before.frozen_balance);
    }
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {name} ON tenant_audit_events; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.claims().await, 0);
    f.finish().await;
}

async fn one_connection() -> (DatabaseConnection, i32) {
    let mut options = ConnectOptions::new(integration_tests::common::resolve_database_url());
    options.min_connections(1).max_connections(1);
    let db = Database::connect(options).await.unwrap();
    let pid = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid() AS pid",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "pid")
        .unwrap();
    (db, pid)
}
async fn wait_locked(db: &DatabaseConnection, pid: i32) {
    tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock') AS waiting",[pid.into()])).await.unwrap().unwrap();
            if row.try_get::<bool>("","waiting").unwrap(){break;}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("the exact command connection must reach its lock wait");
}

#[tokio::test]
async fn queued_http_wallet_command_rejects_root_demotion_before_claiming_idempotency() {
    let f = Fixture::new().await;
    let (db, pid) = one_connection().await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let token = f.token(&state).await;
    let blocker = f.db.begin().await.unwrap();
    blocker
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let app = create_router(state);
    let tenant = f.tenant;
    let user = f.user;
    let task = tokio::spawn(async move { post(app, tenant, user, &token).await });
    wait_locked(&f.db, pid).await;
    blocker
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='operator' WHERE id=$1",
            [f.root.into()],
        ))
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(f.claims().await, 0);
    assert_eq!(f.balance().await.available_balance, Decimal::from(10));
    db.close().await.unwrap();
    f.finish().await;
}

#[tokio::test]
async fn wallet_lock_wait_past_signed_expiry_rolls_back_funds_claim_and_audit() {
    let f = Fixture::new().await;
    let (db, pid) = one_connection().await;
    let session = FinancialSession {
        expires_at: Utc::now().timestamp() + 2,
        ..f.session(f.root).await
    };
    let expiry = session.expires_at;
    let scope = FinancialScope::platform_tenant(
        PlatformScope::checked(f.root, PlatformRole::Root).unwrap(),
        session,
        f.tenant,
    )
    .unwrap();
    let blocker = f.db.begin().await.unwrap();
    blocker
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT user_id FROM user_balances WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE",
            [f.tenant.into(), f.user.into()],
        ))
        .await
        .unwrap();
    let work = db.clone();
    let root = f.root;
    let user = f.user;
    let task = tokio::spawn(async move {
        let key = Uuid::new_v4().to_string();
        UserBalance::apply_admin_manual_operation(
            &work,
            scope,
            &audit(root),
            &command(user, Kind::Freeze, &key),
        )
        .await
    });
    wait_locked(&f.db, pid).await;
    while Utc::now().timestamp() < expiry {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    blocker.rollback().await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error.to_string().contains("financial_authority_invalid"),
        "{error}"
    );
    assert_eq!(f.claims().await, 0);
    let balance = f.balance().await;
    assert_eq!(balance.available_balance, Decimal::from(10));
    assert_eq!(balance.frozen_balance, Decimal::ZERO);
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE tenant_id=$1 AND action='balance.freeze'",[f.tenant.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
    db.close().await.unwrap();
    f.finish().await;
}

async fn tenant_admin_scope(f: &Fixture) -> FinancialScope {
    let tenant = keycompute_db::Tenant::find_by_id(&f.db, f.tenant)
        .await
        .unwrap()
        .unwrap();
    let member = keycompute_db::TenantMembership::find(&f.db, f.tenant, f.admin)
        .await
        .unwrap()
        .unwrap();
    FinancialScope::tenant_admin(
        TenantScope::checked(f.tenant, f.admin, TenantRole::Admin).unwrap(),
        FinancialSession {
            selected: Some(FinancialMembership {
                tenant_id: f.tenant,
                tenant_role: TenantRole::Admin,
                tenant_authz_version: tenant.authz_version,
                membership_authz_version: member.authz_version,
            }),
            ..f.session(f.admin).await
        },
    )
    .unwrap()
}
async fn reserve(f: &Fixture, amount: i64) -> keycompute_db::BalanceReservation {
    keycompute_billing::balance::BalanceService::new(DbRouter::single(f.db.clone()))
        .reserve_request(
            f.tenant,
            f.user,
            Uuid::new_v4(),
            Decimal::from(amount),
            Duration::from_secs(60),
        )
        .await
        .unwrap()
}
fn release_command(
    row: &keycompute_db::BalanceReservation,
) -> keycompute_db::ReleaseReservationCommand<'static> {
    keycompute_db::ReleaseReservationCommand {
        user_id: row.user_id,
        request_id: row.request_id,
        expected_owner_token: row.owner_token,
        reason: "verified reservation recovery",
    }
}
#[tokio::test]
async fn reservation_display_is_one_read_only_scoped_snapshot_without_reclamation() {
    let f = Fixture::new().await;
    let first = reserve(&f, 2).await;
    let second = reserve(&f, 3).await;
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE balance_reservations SET expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3",
        [f.tenant.into(),f.user.into(),first.request_id.into()])).await.unwrap();
    let before = f.balance().await;
    let scope = tenant_admin_scope(&f).await;
    let page = UserBalance::find_breakdown_page_in_scope(&f.db, scope, f.user, None, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.breakdown.active_reserved, Decimal::from(5));
    assert_eq!(page.reservations.len(), 1);
    let next = UserBalance::find_breakdown_page_in_scope(&f.db, scope, f.user, page.next_cursor, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.breakdown.active_reserved, Decimal::from(5));
    assert_eq!(next.reservations.len(), 1);
    assert!(next.next_cursor.is_none());
    assert_ne!(page.reservations[0].id, next.reservations[0].id);
    let after = f.balance().await;
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after.frozen_balance, before.frozen_balance);
    assert!(
        page.reservations
            .iter()
            .chain(next.reservations.iter())
            .all(|r| r.status == "active")
    );
    let unknown = Uuid::new_v4();
    assert!(
        UserBalance::find_breakdown_page_in_scope(&f.db, scope, unknown, None, 10)
            .await
            .is_err()
    );
    assert!(
        UserBalance::find_breakdown_page_in_scope(
            &f.db,
            f.scope(f.root, f.foreign).await,
            f.user,
            None,
            10
        )
        .await
        .is_err()
    );
    let forged = f.scope(f.user, f.tenant).await;
    assert!(
        UserBalance::find_breakdown_page_in_scope(&f.db, forged, f.user, None, 10)
            .await
            .is_err()
    );
    assert!(
        UserBalance::find_breakdown_page_in_scope(&f.db, scope, f.admin, None, 10)
            .await
            .unwrap()
            .is_none()
    );
    // Neither the expired entry nor the live reservation was silently released.
    assert_ne!(first.id, second.id);
    f.finish().await;
}
#[tokio::test]
async fn tenant_recovery_is_expired_only_and_stays_with_the_original_owner() {
    let f = Fixture::new().await;
    let row = reserve(&f, 4).await;
    let scope = tenant_admin_scope(&f).await;
    let cmd = release_command(&row);
    assert!(
        keycompute_db::BalanceReservation::admin_release(&f.db, scope, &audit(f.admin), &cmd)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.balance().await.frozen_balance, Decimal::from(4));
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE balance_reservations SET expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3",
        [f.tenant.into(),f.user.into(),row.request_id.into()])).await.unwrap();
    let first =
        keycompute_db::BalanceReservation::admin_release(&f.db, scope, &audit(f.admin), &cmd)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(first.released_reservation.tenant_id, f.tenant);
    assert_eq!(first.released_reservation.user_id, f.user);
    assert_eq!(first.released_reservation.released_by, Some(f.admin));
    let replay =
        keycompute_db::BalanceReservation::admin_release(&f.db, scope, &audit(f.admin), &cmd)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        replay.released_reservation.id,
        first.released_reservation.id
    );
    assert_eq!(f.balance().await.available_balance, Decimal::from(10));
    let count=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE tenant_id=$1 AND action='balance.reservation_release'",[f.tenant.into()])).await.unwrap().unwrap();
    assert_eq!(count.try_get::<i64>("", "n").unwrap(), 1);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='member' WHERE tenant_id=$1 AND user_id=$2",
        [f.tenant.into(), f.admin.into()],
    ))
    .await
    .unwrap();
    assert!(
        keycompute_db::BalanceReservation::admin_release(&f.db, scope, &audit(f.admin), &cmd)
            .await
            .is_err()
    );
    f.finish().await;
}
#[tokio::test]
async fn reservation_recovery_audit_failure_rolls_back_money_and_ownership_events() {
    let f = Fixture::new().await;
    let row = reserve(&f, 4).await;
    let scope = f.scope(f.root, f.tenant).await;
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let name = format!("reservation_audit_{}", Uuid::new_v4().simple());
    tx.execute_unprepared(&format!("CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.action='balance.reservation_release' THEN RAISE EXCEPTION 'isolated recovery audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {name}();",f.tenant)).await.unwrap();
    assert!(
        keycompute_db::BalanceReservation::admin_release(
            &tx,
            scope,
            &audit(f.root),
            &release_command(&row)
        )
        .await
        .is_err()
    );
    let state=tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT status FROM balance_reservations WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3",[f.tenant.into(),f.user.into(),row.request_id.into()])).await.unwrap().unwrap();
    assert_eq!(state.try_get::<String>("", "status").unwrap(), "active");
    let events = keycompute_db::BalanceReservationEvent::find_by_request(&tx, row.request_id)
        .await
        .unwrap();
    assert!(events.iter().all(|e| e.event_type != "released"));
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {name} ON tenant_audit_events; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.balance().await.frozen_balance, Decimal::from(4));
    f.finish().await;
}
#[tokio::test]
async fn unsupported_money_precision_cannot_leave_a_pending_idempotency_claim() {
    let f = Fixture::new().await;
    let scope = f.scope(f.root, f.tenant).await;
    for amount in [Decimal::new(1, 11), Decimal::from(10_000_000_000u64)] {
        let key = Uuid::new_v4().to_string();
        let mut cmd = command(f.user, Kind::Recharge, &key);
        cmd.amount = amount;
        assert!(
            UserBalance::apply_admin_manual_operation(&f.db, scope, &audit(f.root), &cmd)
                .await
                .is_err()
        );
    }
    assert_eq!(f.claims().await, 0);
    assert_eq!(f.balance().await.available_balance, Decimal::from(10));
    f.finish().await;
}

#[tokio::test]
async fn completed_manual_command_ownership_and_result_cannot_be_rewritten() {
    let f = Fixture::new().await;
    let key = Uuid::new_v4().to_string();
    let Decision::Completed(outcome) = UserBalance::apply_admin_manual_operation(
        &f.db,
        f.scope(f.root, f.tenant).await,
        &audit(f.root),
        &command(f.user, Kind::Recharge, &key),
    )
    .await
    .unwrap() else {
        panic!("new command must complete")
    };
    for sql in [
        "UPDATE admin_balance_operations SET amount=amount+1 WHERE id=$1",
        "UPDATE admin_balance_operations SET balance_after=balance_after+1 WHERE id=$1",
        "UPDATE admin_balance_operations SET actor_user_id=user_id WHERE id=$1",
        "UPDATE admin_balance_operations SET completed_at=NULL,balance_transaction_id=NULL,balance_before=NULL,balance_after=NULL,frozen_balance_after=NULL WHERE id=$1",
    ] {
        let error =
            f.db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                [outcome.operation_id.into()],
            ))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("immutable"), "{error}");
    }
    assert_eq!(f.balance().await.available_balance, Decimal::from(11));
    f.finish().await;
}
#[tokio::test]
async fn rejected_manual_unfreeze_keeps_expiry_repair_only_if_its_audit_commits() {
    let f = Fixture::new().await;
    let reservation = reserve(&f, 4).await;
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE balance_reservations SET expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3",
        [f.tenant.into(),f.user.into(),reservation.request_id.into()])).await.unwrap();
    let scope = f.scope(f.root, f.tenant).await;
    let key = Uuid::new_v4().to_string();
    let cmd = command(f.user, Kind::Unfreeze, &key);
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let name = format!("unfreeze_denial_{}", Uuid::new_v4().simple());
    tx.execute_unprepared(&format!("CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.action='balance.unfreeze_denied' THEN RAISE EXCEPTION 'isolated denial audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {name}();",f.tenant)).await.unwrap();
    assert!(
        UserBalance::apply_admin_manual_operation(&tx, scope, &audit(f.root), &cmd)
            .await
            .is_err()
    );
    let balance = UserBalance::find_by_user(&tx, f.tenant, f.user)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(balance.available_balance, Decimal::from(6));
    assert_eq!(balance.frozen_balance, Decimal::from(4));
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {name} ON tenant_audit_events; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let error = UserBalance::apply_admin_manual_operation(&f.db, scope, &audit(f.root), &cmd)
        .await
        .unwrap_err();
    assert!(error.is_insufficient_balance());
    assert_eq!(f.claims().await, 0);
    let after = f.balance().await;
    assert_eq!(after.available_balance, Decimal::from(10));
    assert_eq!(after.frozen_balance, Decimal::ZERO);
    let event=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT result FROM tenant_audit_events WHERE tenant_id=$1 AND action='balance.unfreeze_denied'",[f.tenant.into()])).await.unwrap().unwrap();
    assert_eq!(event.try_get::<String>("", "result").unwrap(), "denied");
    f.finish().await;
}

async fn console_token(
    f: &Fixture,
    state: &AppState,
    user: Uuid,
    selected: Option<Uuid>,
) -> String {
    let session = f.session(user).await;
    let raw = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user, None, session.token_version, None, None, 3600)
        .unwrap();
    if let Some(tenant) = selected {
        let context = state.auth.verify_token(&raw).await.unwrap();
        state
            .auth
            .select_tenant(&context, Some(tenant))
            .await
            .unwrap()
            .access_token
    } else {
        raw
    }
}
async fn http_json(
    app: Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", Uuid::new_v4().to_string())
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}
#[tokio::test]
async fn canonical_wallet_routes_separate_root_money_commands_from_tenant_expired_recovery() {
    let f = Fixture::new().await;
    let state = AppState::with_pool(DbRouter::single(f.db.clone()));
    let root = console_token(&f, &state, f.root, None).await;
    let admin = console_token(&f, &state, f.admin, Some(f.tenant)).await;
    let member = console_token(&f, &state, f.user, Some(f.tenant)).await;
    let operator = console_token(&f, &state, f.operator, None).await;
    let app = create_router(state);
    let platform = format!(
        "/api/v1/platform/tenants/{}/users/{}/balance",
        f.tenant, f.user
    );
    let tenant = format!("/api/v1/tenants/{}/users/{}/balance", f.tenant, f.user);
    for token in [&admin, &member, &operator] {
        let denied = http_json(
            app.clone(),
            "POST",
            &platform,
            token,
            json!({"amount":"1","reason":"must be denied"}),
        )
        .await;
        assert_eq!(denied.0, StatusCode::FORBIDDEN, "{}", denied.1);
    }
    let malformed = http_json(
        app.clone(),
        "POST",
        &platform,
        &root,
        json!({"amount":"1","reason":"reject body ownership","tenant_id":f.foreign}),
    )
    .await;
    assert_eq!(malformed.0, StatusCode::UNPROCESSABLE_ENTITY);
    for (suffix, amount) in [("", "2"), ("/freeze", "1"), ("/unfreeze", "1")] {
        let result = http_json(
            app.clone(),
            "POST",
            &format!("{platform}{suffix}"),
            &root,
            json!({"amount":amount,"reason":"canonical test"}),
        )
        .await;
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
        assert!(
            result.2["cache-control"]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
    }
    assert_eq!(f.balance().await.available_balance, Decimal::from(12));
    let row = reserve(&f, 4).await;
    let get = http_json(
        app.clone(),
        "GET",
        &format!("{tenant}/reservations?limit=1"),
        &admin,
        Value::Null,
    )
    .await;
    assert_eq!(get.0, StatusCode::OK, "{}", get.1);
    assert_eq!(
        get.1["request_reserved_balance"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(4)
    );
    let denied = http_json(
        app.clone(),
        "GET",
        &format!("{tenant}/reservations"),
        &member,
        Value::Null,
    )
    .await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    let path = format!("{tenant}/reservations/{}/release", row.request_id);
    let body = json!({"expected_version":row.owner_token,"reason":"expired recovery"});
    let live = http_json(app.clone(), "POST", &path, &admin, body.clone()).await;
    assert_eq!(live.0, StatusCode::CONFLICT, "{}", live.1);
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE balance_reservations SET expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3",
        [f.tenant.into(),f.user.into(),row.request_id.into()])).await.unwrap();
    let released = http_json(app.clone(), "POST", &path, &admin, body).await;
    assert_eq!(released.0, StatusCode::OK, "{}", released.1);
    assert_eq!(released.1["user_id"], f.user.to_string());
    assert_eq!(released.1["released_by"], f.admin.to_string());
    let row = reserve(&f, 2).await;
    let forced = http_json(
        app.clone(),
        "POST",
        &format!("{platform}/reservations/{}/release", row.request_id),
        &root,
        json!({"expected_version":row.owner_token,"reason":"root explicit recovery"}),
    )
    .await;
    assert_eq!(forced.0, StatusCode::OK, "{}", forced.1);
    assert!(forced.1["warning"].as_str().unwrap().contains("Late usage"));
    let get = http_json(
        app,
        "GET",
        &format!("{platform}/reservations"),
        &root,
        Value::Null,
    )
    .await;
    assert_eq!(get.0, StatusCode::OK, "{}", get.1);
    assert!(get.1["reservations"].as_array().unwrap().is_empty());
    f.finish().await;
}
