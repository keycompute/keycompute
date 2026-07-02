//! 数据库集成测试公共辅助函数
//!
//! 提供测试数据库连接创建、数据清理等共享函数

use keycompute_db::{
    CreateTenantRequest, CreateUserRequest, PendingRegistration, Tenant,
    UpsertPendingRegistrationRequest, User, run_migrations,
};
use keycompute_types::UserRole;
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, TransactionTrait,
};
use std::time::Duration;
use uuid::Uuid;

pub async fn create_test_pool() -> DatabaseConnection {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://keycompute:change-me-strong-password@localhost:5432/keycompute".to_string()
    });

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

    // 运行迁移
    run_migrations(&db)
        .await
        .expect("Failed to run database migrations");

    // 清理已存在的 system 用户（`uq_users_single_system_role` 全局唯一索引要求）
    // main.rs 的 initialize_default_admin 或历史测试可能遗留了 system 用户，
    // 导致后续测试无法创建自己的 system 用户
    // 因 `trg_prevent_system_user_delete` + `trg_prevent_system_role_change`
    // 两个触发器保护了 system 用户不可删除/不可改角色，需临时禁用后清理
    db.execute_unprepared("ALTER TABLE users DISABLE TRIGGER trg_prevent_system_user_delete")
        .await
        .ok();
    db.execute_unprepared("ALTER TABLE users DISABLE TRIGGER trg_prevent_system_role_change")
        .await
        .ok();
    db.execute_unprepared("DELETE FROM users WHERE role = 'system'")
        .await
        .ok();
    db.execute_unprepared("ALTER TABLE users ENABLE TRIGGER trg_prevent_system_user_delete")
        .await
        .ok();
    db.execute_unprepared("ALTER TABLE users ENABLE TRIGGER trg_prevent_system_role_change")
        .await
        .ok();

    db
}

/// 清理特定测试运行的数据
pub async fn cleanup_test_data(
    pool: &DatabaseConnection,
    run_id: &str,
) -> Result<(), sea_orm::DbErr> {
    let slug_pattern = format!("test-%-{}", run_id);
    let email_pattern = format!("%{}%", run_id);

    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM pending_registrations WHERE email LIKE $1",
        [email_pattern.into()],
    ))
    .await?;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM distribution_records WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)",
        [slug_pattern.clone().into()],
    )).await?;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM usage_logs WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)",
        [slug_pattern.clone().into()],
    ))
    .await?;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM balance_transactions WHERE user_id IN (SELECT id FROM users WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1))",
        [slug_pattern.clone().into()],
    )).await?;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM user_balances WHERE user_id IN (SELECT id FROM users WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1))",
        [slug_pattern.clone().into()],
    )).await?;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM produce_ai_keys WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)",
        [slug_pattern.clone().into()],
    )).await?;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM users WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)",
        [slug_pattern.clone().into()],
    ))
    .await?;
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM tenants WHERE slug LIKE $1",
        [slug_pattern.into()],
    ))
    .await?;

    Ok(())
}

/// 创建测试租户
pub async fn create_test_tenant(pool: &DatabaseConnection, suffix: &str, test_id: &str) -> Tenant {
    Tenant::create(
        pool,
        &CreateTenantRequest {
            name: format!("Test Tenant {}", suffix),
            slug: format!("test-tenant-{}-{}", suffix, test_id),
            description: Some(format!("Test tenant for {}", suffix)),
            default_rpm_limit: Some(100),
            default_tpm_limit: Some(50000),
        },
    )
    .await
    .expect("Failed to create test tenant")
}

/// 创建测试用户
pub async fn create_test_user(
    pool: &DatabaseConnection,
    tenant_id: Uuid,
    suffix: &str,
    test_id: &str,
) -> User {
    User::create(
        pool,
        &CreateUserRequest {
            tenant_id,
            email: format!("test-{}-{}@example.com", suffix, test_id),
            name: Some(format!("Test User {}", suffix)),
            role: Some(UserRole::User),
        },
    )
    .await
    .expect("Failed to create test user")
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

/// 通过邮箱删除用户
pub async fn delete_user_by_email(
    pool: &DatabaseConnection,
    email: &str,
) -> Result<(), sea_orm::DbErr> {
    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM users WHERE email = $1",
        [email.into()],
    ))
    .await?;
    Ok(())
}
