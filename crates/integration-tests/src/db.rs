//! 数据库集成测试公共辅助函数
//!
//! 提供测试数据库连接创建、数据清理等共享函数

use keycompute_db::{
    CreateTenantRequest, CreateUserRequest, PendingRegistration, Tenant,
    UpsertPendingRegistrationRequest, User, initialize_schema,
};
use keycompute_types::TenantRole;
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, TransactionTrait,
};
use std::ops::Deref;
use std::time::Duration;
use uuid::Uuid;

// 一个集成测试可并行运行多个 case，但它们共享同一个测试数据库。
// schema 只需在进程内初始化一次，避免多个 case 同时执行 DDL。
// 历史 system 用户的清理也在这里一次性完成：所有 case 都会等待
// 初始化结束后才开始访问数据库，不会观察到清理过程的中间状态。
static SCHEMA_INITIALIZED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

pub async fn initialize_test_schema(db: &DatabaseConnection) -> Result<(), keycompute_db::DbError> {
    SCHEMA_INITIALIZED
        .get_or_try_init(|| async {
            initialize_schema(db).await?;
            let tx = db.begin().await?;
            tx.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT pg_advisory_xact_lock($1)",
                [981754117i64.into()],
            ))
            .await?;
            if User::count_all(&tx).await? == 0 {
                let root = User::bootstrap_root(
                    &tx,
                    "tenant-test-root@fixture.invalid",
                    Some("Test identity anchor"),
                )
                .await?;
                let actor = keycompute_db::AuditContext {
                    actor_user_id: root.id,
                    credential_kind: keycompute_types::CredentialKind::System,
                    actor_platform_role: keycompute_types::PlatformRole::Root,
                    actor_tenant_role: None,
                    request_id: None,
                };
                Tenant::create_owned(
                    &tx,
                    &CreateTenantRequest {
                        name: "Default".into(),
                        slug: "default".into(),
                        description: None,
                        default_rpm_limit: None,
                        default_tpm_limit: None,
                    },
                    root.id,
                    &actor,
                )
                .await?;
            }
            tx.commit().await?;
            Ok::<(), keycompute_db::DbError>(())
        })
        .await
        .map(|_| ())
}

pub async fn create_test_pool() -> DatabaseConnection {
    let database_url = crate::common::resolve_database_url();

    use sea_orm::ConnectOptions;
    let mut opt = ConnectOptions::new(&database_url);
    opt.max_connections(20)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(900));

    let db = Database::connect(opt)
        .await
        .expect("Failed to connect to database. Set DATABASE_URL environment variable.");

    // 测试环境可能复用数据库；schema 本身保持可重复初始化，业务数据由各测试清理。
    initialize_test_schema(&db)
        .await
        .expect("Failed to initialize database schema");

    db
}

/// 清理特定测试运行的数据
pub async fn cleanup_test_data(
    pool: &DatabaseConnection,
    run_id: &str,
) -> Result<(), sea_orm::DbErr> {
    // Test cleanup is explicit and dependency-ordered: production foreign keys
    // deliberately retain accepted work instead of cascading it away.
    if run_id.len() < 8
        || !run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(sea_orm::DbErr::Custom("invalid test namespace".into()));
    }
    let slug_pattern = format!("test-%-{}", run_id);
    let email_pattern = format!("%{}%", run_id);
    let tx = pool.begin().await?;
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "DELETE FROM node_tips WHERE usage_log_id IN (SELECT u.id FROM usage_logs u JOIN tenants t ON t.id=u.tenant_id WHERE t.slug LIKE $1)",
        [slug_pattern.clone().into()],
    )).await?;
    for table in [
        "scoped_responses",
        "scoped_conversations",
        "response_affinities",
        "responses_idempotency_claims",
        "node_tasks",
        "user_node_gateway_tokens",
        "nodes",
        "gateway_requests",
        "pricing_models",
        "distribution_records",
        "tenant_distribution_rules",
        "balance_reservations",
        "admin_balance_operations",
        "balance_transactions",
        "usage_logs",
        "user_balances",
        "payment_orders",
        "produce_ai_keys",
        "tenant_invitations",
    ] {
        // Table names are internal constants; namespace values are parameters.
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "DELETE FROM {table} WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)"
            ),
            [slug_pattern.clone().into()],
        ))
        .await?;
    }
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "DELETE FROM passthrough_bindings WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1) OR account_id IN (SELECT a.id FROM accounts a JOIN tenants t ON t.id=a.tenant_id WHERE t.slug LIKE $1)",
        [slug_pattern.clone().into()],
    )).await?;
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM accounts WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)",
        [slug_pattern.clone().into()],
    ))
    .await?;
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM tenants WHERE slug LIKE $1",
        [slug_pattern.into()],
    ))
    .await?;
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM pending_registrations WHERE email LIKE $1",
        [email_pattern.clone().into()],
    ))
    .await?;
    // Historical audit snapshots intentionally survive fixture removal, just
    // as they survive real resource deletion. Never disable their protections.
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM users WHERE email LIKE $1 AND email LIKE '%@example.com'",
        [email_pattern.into()],
    ))
    .await?;
    tx.commit().await
}

/// Owns the data namespace for one integration-test run and cleans it up on
/// normal completion. The `Drop` fallback schedules best-effort cleanup when a
/// test panics or is cancelled before reaching its explicit cleanup call.
pub struct TestDataGuard {
    pool: DatabaseConnection,
    run_id: String,
    cleaned: bool,
}

impl TestDataGuard {
    pub fn new(pool: DatabaseConnection, run_id: impl Into<String>) -> Self {
        Self {
            pool,
            run_id: run_id.into(),
            cleaned: false,
        }
    }

    pub async fn cleanup(&mut self) -> Result<(), sea_orm::DbErr> {
        if self.cleaned {
            return Ok(());
        }
        cleanup_test_data(&self.pool, &self.run_id).await?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for TestDataGuard {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }

        let pool = self.pool.clone();
        let run_id = self.run_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            eprintln!("warning: no Tokio runtime available for test-data cleanup {run_id}");
            return;
        };
        handle.spawn(async move {
            if let Err(error) = cleanup_test_data(&pool, &run_id).await {
                eprintln!("warning: failed to clean test data {run_id}: {error}");
            }
        });
    }
}

/// 创建测试租户
pub async fn create_test_tenant(pool: &DatabaseConnection, suffix: &str, test_id: &str) -> Tenant {
    // Every tenant fixture has a real global owner and an explicit active
    // admin membership.  Keep both inserts in one transaction so the deferred
    // owner invariant is checked only after the membership exists.
    let owner = User::create(
        pool,
        &CreateUserRequest {
            email: format!("test-owner-{}-{}@example.com", suffix, test_id),
            name: Some(format!("Test Owner {}", suffix)),
        },
    )
    .await
    .expect("failed to create global fixture owner");
    let tx = pool
        .begin()
        .await
        .expect("tenant fixture transaction should start");
    let tenant = Tenant::create_owned(
        &tx,
        &CreateTenantRequest {
            name: format!("Test Tenant {}", suffix),
            slug: format!("test-tenant-{}-{}", suffix, test_id),
            description: Some(format!("Test tenant for {}", suffix)),
            default_rpm_limit: Some(100),
            default_tpm_limit: Some(50000),
        },
        owner.id,
        &keycompute_db::AuditContext {
            actor_user_id: owner.id,
            credential_kind: keycompute_types::CredentialKind::Jwt,
            actor_platform_role: keycompute_types::PlatformRole::None,
            actor_tenant_role: None,
            request_id: None,
        },
    )
    .await
    .expect("failed to create owned test tenant");
    tx.commit()
        .await
        .expect("tenant fixture transaction should commit");
    tenant
}

/// 创建测试用户
pub async fn create_test_user(
    pool: &DatabaseConnection,
    tenant_id: Uuid,
    suffix: &str,
    test_id: &str,
) -> TenantActor {
    let user = User::create(
        pool,
        &CreateUserRequest {
            email: format!("test-{}-{}@example.com", suffix, test_id),
            name: Some(format!("Test User {}", suffix)),
        },
    )
    .await
    .expect("Failed to create test user");
    let tx = pool
        .begin()
        .await
        .expect("membership fixture transaction should start");
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO tenant_memberships (tenant_id,user_id,role,status) VALUES ($1,$2,'member','active')",
        [tenant_id.into(), user.id.into()],
    ))
    .await
    .expect("membership fixture insert should succeed");
    tx.commit()
        .await
        .expect("membership fixture transaction should commit");
    TenantActor {
        user,
        tenant_id,
        tenant_role: TenantRole::Member,
    }
}

/// A test-only scoped identity. Production `User` is intentionally global;
/// tests keep the fixture's known tenant beside it instead of selecting an
/// arbitrary membership from the database.
#[derive(Debug, Clone)]
pub struct TenantActor {
    pub user: User,
    pub tenant_id: Uuid,
    pub tenant_role: TenantRole,
}

impl TenantActor {
    /// Use the tenant and member role recorded by this test fixture.
    pub fn scope(&self) -> keycompute_types::TenantScope {
        keycompute_types::TenantScope::checked(self.tenant_id, self.id, self.tenant_role)
            .expect("fixture has valid tenant and user IDs")
    }
}

impl Deref for TenantActor {
    type Target = User;

    fn deref(&self) -> &Self::Target {
        &self.user
    }
}

/// 创建测试中的待完成注册记录
pub async fn create_test_pending_registration(
    pool: &DatabaseConnection,
    req: UpsertPendingRegistrationRequest,
) -> PendingRegistration {
    let tx = pool.begin().await.expect("transaction should start");
    let pending = PendingRegistration::create_in_tx(&tx, &req)
        .await
        .expect("pending registration should be created");
    tx.commit().await.expect("transaction should commit");
    pending
}

/// Remove a freshly registered test identity and its personal workspace.
/// Foreign keys continue to reject any unexpected retained business activity.
pub async fn delete_user_by_email(
    pool: &DatabaseConnection,
    email: &str,
) -> Result<(), sea_orm::DbErr> {
    let tx = pool.begin().await?;
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await?;
    let Some(row) = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM users WHERE email=$1 AND platform_role='none' FOR UPDATE",
            [email.into()],
        ))
        .await?
    else {
        return tx.rollback().await;
    };
    let user: Uuid = row.try_get_by_index(0)?;
    let slug = format!("personal-{}", user.simple());
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "DELETE FROM balance_transactions WHERE user_id=$1 AND tenant_id IN (SELECT id FROM tenants WHERE owner_user_id=$1 AND slug=$2) AND transaction_type='recharge' AND description='Initial quota from system'",
        [user.into(),slug.clone().into()],
    )).await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "DELETE FROM user_balances WHERE user_id=$1 AND tenant_id IN (SELECT id FROM tenants WHERE owner_user_id=$1 AND slug=$2)",
        [user.into(),slug.clone().into()],
    )).await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "DELETE FROM tenants WHERE owner_user_id=$1 AND slug=$2 AND NOT EXISTS (SELECT 1 FROM tenant_memberships m WHERE m.tenant_id=tenants.id AND m.user_id<>$1)",
        [user.into(),slug.into()],
    )).await?;
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM users WHERE id=$1",
        [user.into()],
    ))
    .await?;
    tx.commit().await
}

/// Inspect fixture members through the same scoped projection as tenant administration.
pub async fn list_test_members(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
) -> Result<Vec<keycompute_db::models::user::TenantMemberRecord>, keycompute_db::DbError> {
    let tenant = Tenant::find_by_id(db, tenant_id)
        .await?
        .ok_or_else(|| keycompute_db::DbError::not_found("tenant", tenant_id))?;
    let scope =
        keycompute_types::TenantScope::checked(tenant.id, tenant.owner_user_id, TenantRole::Admin)
            .map_err(keycompute_db::DbError::Other)?;
    keycompute_db::models::user::TenantMemberRecord::list_in_tenant(
        db,
        scope,
        Some(keycompute_types::MembershipStatus::Active),
        None,
        1000,
        0,
    )
    .await
}

/// Test-only fixture creation through the production scoped key mutation API.
pub async fn create_test_api_key(
    db: &(impl ConnectionTrait + TransactionTrait),
    req: &keycompute_db::CreateProduceAiKeyRequest,
) -> Result<keycompute_db::ProduceAiKey, keycompute_db::DbError> {
    let member = keycompute_db::TenantMembership::find_any(db, req.tenant_id, req.user_id)
        .await?
        .ok_or_else(|| keycompute_db::DbError::not_found("membership", req.user_id))?;
    let role = member.tenant_role()?;
    let scope = keycompute_types::TenantScope::checked(req.tenant_id, req.user_id, role)
        .map_err(keycompute_db::DbError::Other)?;
    let actor = keycompute_db::AuditContext {
        actor_user_id: req.user_id,
        credential_kind: keycompute_types::CredentialKind::Jwt,
        actor_platform_role: keycompute_types::PlatformRole::None,
        actor_tenant_role: Some(role),
        request_id: None,
    };
    keycompute_db::ProduceAiKey::create_owned(db, scope, req, &actor).await
}
