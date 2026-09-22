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
        "INSERT INTO tenant_memberships (tenant_id,user_id,tenant_role,status) VALUES ($1,$2,'member','active')",
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

/// Build price fixtures through the same scope and audit checks as management.
pub async fn create_test_pricing(
    db: &(impl ConnectionTrait + TransactionTrait),
    req: &keycompute_db::CreatePricingRequest,
) -> Result<keycompute_db::PricingModel, keycompute_db::DbError> {
    use keycompute_db::models::pricing_model::{
        PlatformPricingScope, PricingScopeType, PricingTarget, TenantPricingScope,
    };
    use keycompute_types::{CredentialKind, PlatformRole};
    let tx = db.begin().await?;
    let row = match (req.scope_type, req.tenant_id) {
        (PricingScopeType::Tenant, Some(tenant_id)) => {
            let tenant = Tenant::find_by_id(&tx, tenant_id)
                .await?
                .ok_or_else(|| keycompute_db::DbError::not_found("tenant", tenant_id))?;
            let user = User::find_by_id(&tx, tenant.owner_user_id)
                .await?
                .ok_or_else(|| keycompute_db::DbError::not_found("owner", tenant.owner_user_id))?;
            let member = keycompute_db::TenantMembership::find(&tx, tenant_id, user.id)
                .await?
                .ok_or_else(|| keycompute_db::DbError::not_found("membership", user.id))?;
            let scope = TenantPricingScope::checked(
                tenant_id,
                user.id,
                CredentialKind::Jwt,
                user.token_version,
                tenant.authz_version,
                member.authz_version,
            )?;
            let actor = keycompute_db::AuditContext {
                actor_user_id: user.id,
                credential_kind: CredentialKind::Jwt,
                actor_platform_role: user.platform_role()?,
                actor_tenant_role: Some(TenantRole::Admin),
                request_id: Some(uuid::Uuid::new_v4()),
            };
            keycompute_db::PricingModel::create_in_tenant(&tx, scope, req, &actor).await?
        }
        (PricingScopeType::Platform, None) => {
            let root = User::find_by_email(&tx, "tenant-test-root@fixture.invalid")
                .await?
                .ok_or_else(|| keycompute_db::DbError::Other("fixture root is missing".into()))?;
            let scope =
                PlatformPricingScope::checked(root.id, CredentialKind::Jwt, root.token_version)?;
            let actor = keycompute_db::AuditContext {
                actor_user_id: root.id,
                credential_kind: CredentialKind::Jwt,
                actor_platform_role: PlatformRole::Root,
                actor_tenant_role: None,
                request_id: Some(uuid::Uuid::new_v4()),
            };
            keycompute_db::PricingModel::create_platform(
                &tx,
                scope,
                PricingTarget::Platform,
                req,
                &actor,
            )
            .await?
        }
        _ => {
            return Err(keycompute_db::DbError::Other(
                "invalid fixture pricing target".into(),
            ));
        }
    };
    tx.commit().await?;
    Ok(row)
}

pub async fn delete_test_tenant_pricing(
    db: &(impl ConnectionTrait + TransactionTrait),
    price: &keycompute_db::PricingModel,
) -> Result<(), keycompute_db::DbError> {
    use keycompute_db::models::pricing_model::TenantPricingScope;
    use keycompute_types::CredentialKind;
    let tenant_id = price.tenant_id.ok_or_else(|| {
        keycompute_db::DbError::Other("fixture deletion requires tenant price".into())
    })?;
    let tx = db.begin().await?;
    let tenant = Tenant::find_by_id(&tx, tenant_id)
        .await?
        .ok_or_else(|| keycompute_db::DbError::not_found("tenant", tenant_id))?;
    let user = User::find_by_id(&tx, tenant.owner_user_id)
        .await?
        .ok_or_else(|| keycompute_db::DbError::not_found("owner", tenant.owner_user_id))?;
    let member = keycompute_db::TenantMembership::find(&tx, tenant_id, user.id)
        .await?
        .ok_or_else(|| keycompute_db::DbError::not_found("membership", user.id))?;
    let scope = TenantPricingScope::checked(
        tenant_id,
        user.id,
        CredentialKind::Jwt,
        user.token_version,
        tenant.authz_version,
        member.authz_version,
    )?;
    let actor = keycompute_db::AuditContext {
        actor_user_id: user.id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: user.platform_role()?,
        actor_tenant_role: Some(TenantRole::Admin),
        request_id: Some(uuid::Uuid::new_v4()),
    };
    keycompute_db::PricingModel::delete_in_tenant(&tx, scope, price.id, &actor).await?;
    tx.commit().await.map_err(keycompute_db::DbError::from)
}

/// Test-only SQL fixture insertion. This is not a production policy write API.
/// Duplicate-default tests intentionally seed conflicting rows before invoking
/// the real scoped/audited repair operation.
pub async fn seed_distribution_rule(
    db: &impl sea_orm::ConnectionTrait,
    req: &keycompute_db::CreateDistributionRuleRequest,
) -> Result<keycompute_db::TenantDistributionRule, keycompute_db::DbError> {
    use sea_orm::FromQueryResult;
    keycompute_db::TenantDistributionRule::find_by_statement(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "INSERT INTO tenant_distribution_rules(tenant_id,beneficiary_scope,beneficiary_id,name,description,commission_rate,priority,effective_from,effective_until) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *",
        [req.tenant_id.into(),req.beneficiary_scope.as_str().into(),req.beneficiary_id.into(),req.name.clone().into(),req.description.clone().into(),req.commission_rate.clone().into(),req.priority.unwrap_or(0).into(),req.effective_from.unwrap_or_else(chrono::Utc::now).into(),req.effective_until.into()]
    )).one(db).await?.ok_or_else(||keycompute_db::DbError::Other("rule fixture insert returned no row".into()))
}

/// Explicit fixture-only proof from the fixture's actual database versions.
/// Production producers must carry the original authenticated request proof;
/// they must never refresh a queued task using this test convenience function.
pub async fn fixture_dispatch_identity(
    db: &impl sea_orm::ConnectionTrait,
    tenant: Uuid,
    user: Uuid,
) -> keycompute_types::DispatchIdentity {
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT u.token_version,t.authz_version AS tenant_version,m.authz_version AS member_version FROM users u JOIN tenant_memberships m ON m.user_id=u.id JOIN tenants t ON t.id=m.tenant_id WHERE u.id=$1 AND t.id=$2",
        [user.into(),tenant.into()])).await.expect("fixture authority query").expect("fixture membership");
    keycompute_types::DispatchIdentity {
        tenant_id: tenant,
        actor_user_id: user,
        resource_owner_user_id: user,
        credential_kind: keycompute_types::CredentialKind::Jwt,
        api_key_id: None,
        token_version: row.try_get("", "token_version").unwrap(),
        tenant_authz_version: row.try_get("", "tenant_version").unwrap(),
        membership_authz_version: row.try_get("", "member_version").unwrap(),
        credential_expires_at: Some(chrono::Utc::now().timestamp() + 3600),
    }
}
