//! 真实数据库集成测试
//!
//! 验证 PostgreSQL 数据库的完整功能：
//! - 数据库连接与迁移
//! - 租户 CRUD 操作
//! - 用户 CRUD 操作
//! - API Key CRUD 操作
//! - UsageLog 写入与查询
//! - 事务处理
//! - 并发操作
//!
//! 运行要求：
//! - 设置环境变量 DATABASE_URL
//! - PostgreSQL 数据库已启动并可访问
//!
//! 运行方式：
//! ```bash
//! DATABASE_URL="postgres://user:pass@localhost/keycompute" cargo test --test e2e_database
//! ```

use bigdecimal::BigDecimal;
use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::common::VerificationChain;
use keycompute_auth::password::{
    PasswordHasher, RegistrationService, RequestRegistrationCodeRequest,
};
use keycompute_db::{
    CreateProduceAiKeyRequest, CreateTenantRequest, CreateUsageLogRequest, CreateUserRequest,
    PendingRegistration, ProduceAiKey, Tenant, UpsertPendingRegistrationRequest, UsageLog, User,
    run_migrations,
};
use keycompute_types::{AssignableUserRole, KeyComputeError, UserRole};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{sync::Arc, time::Duration};
use tokio::sync::Barrier;
use uuid::Uuid;

// ============================================================================
// 测试辅助函数
// ============================================================================

/// 创建测试数据库连接池
///
/// 直接创建连接池，不使用全局 OnceCell（避免多次初始化错误）
async fn create_test_pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@localhost:5432/keycompute".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(900))
        .connect(&database_url)
        .await
        .expect("Failed to connect to database. Set DATABASE_URL environment variable.");

    // 运行迁移
    run_migrations(&pool)
        .await
        .expect("Failed to run database migrations");

    pool
}

/// 生成唯一的测试标识符
/// 每个调用返回一个新的 UUID，用于确保测试数据完全隔离
fn generate_test_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// 清理特定测试运行的数据
async fn cleanup_test_data(pool: &PgPool, run_id: &str) -> Result<(), sqlx::Error> {
    let slug_pattern = format!("test-%-{}", run_id);
    let email_pattern = format!("%{}%", run_id);

    // 按依赖顺序删除 - 只删除当前测试运行的数据
    sqlx::query("DELETE FROM pending_registrations WHERE email LIKE $1")
        .bind(&email_pattern)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM distribution_records WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)")
        .bind(&slug_pattern)
        .execute(pool)
        .await?;
    sqlx::query(
        "DELETE FROM usage_logs WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)",
    )
    .bind(&slug_pattern)
    .execute(pool)
    .await?;
    sqlx::query("DELETE FROM produce_ai_keys WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)")
        .bind(&slug_pattern)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM users WHERE tenant_id IN (SELECT id FROM tenants WHERE slug LIKE $1)")
        .bind(&slug_pattern)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM tenants WHERE slug LIKE $1")
        .bind(&slug_pattern)
        .execute(pool)
        .await?;

    Ok(())
}

/// 创建测试租户
/// 使用 test_id 确保数据隔离，避免并行测试冲突
async fn create_test_tenant(pool: &PgPool, suffix: &str, test_id: &str) -> Tenant {
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
async fn create_test_user(pool: &PgPool, tenant_id: Uuid, suffix: &str, test_id: &str) -> User {
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
async fn create_test_pending_registration(
    pool: &PgPool,
    req: UpsertPendingRegistrationRequest,
) -> PendingRegistration {
    let mut tx = pool.begin().await.expect("transaction should start");
    let pending = PendingRegistration::create_in_tx(&mut tx, &req)
        .await
        .expect("pending registration should be created");

    tx.commit()
        .await
        .expect("pending registration transaction should commit");

    pending
}

async fn delete_user_by_email(pool: &PgPool, email: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM users WHERE email = $1")
        .bind(email)
        .execute(pool)
        .await?;

    Ok(())
}

// ============================================================================
// 数据库连接测试
// ============================================================================

/// 测试数据库连接
#[tokio::test]
async fn test_database_connection() {
    let mut chain = VerificationChain::new();

    // 1. 连接数据库
    let pool = create_test_pool().await;
    chain.add_step(
        "keycompute-db",
        "create_test_pool",
        "Database connection established",
        true,
    );

    // 2. 测试简单查询
    let result: Result<(i32,), sqlx::Error> = sqlx::query_as("SELECT 1").fetch_one(&pool).await;
    chain.add_step(
        "keycompute-db",
        "SELECT 1",
        "Simple query executed",
        result.is_ok(),
    );

    // 3. 验证表存在
    let result: Result<(i64,), sqlx::Error> = sqlx::query_as(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'tenants'",
    )
    .fetch_one(&pool)
    .await;
    chain.add_step(
        "keycompute-db",
        "check_tenants_table",
        "Tenants table exists",
        result.map(|r| r.0 > 0).unwrap_or(false),
    );

    chain.print_report();
    assert!(chain.all_passed(), "Database connection tests failed");
}

/// 测试数据库管理器
#[tokio::test]
async fn test_database_manager() {
    let mut chain = VerificationChain::new();

    // 直接使用 PgPoolOptions 创建连接池
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@localhost:5432/keycompute".to_string());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await;

    chain.add_step(
        "keycompute-db",
        "PgPoolOptions::connect",
        "Database pool created",
        pool.is_ok(),
    );

    let pool = pool.expect("Failed to create database pool");

    // 测试连接
    let test_result: Result<(i32,), sqlx::Error> =
        sqlx::query_as("SELECT 1").fetch_one(&pool).await;
    chain.add_step(
        "keycompute-db",
        "test_connection",
        "Connection test passed",
        test_result.is_ok(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// 租户 CRUD 测试
// ============================================================================

/// 测试租户 CRUD 操作
#[tokio::test]
async fn test_tenant_crud() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_tenant_crud cleanup should succeed");

    // 1. 创建租户
    let tenant = create_test_tenant(&pool, "crud", &test_id).await;
    chain.add_step(
        "keycompute-db",
        "Tenant::create",
        format!("Tenant created: {} ({})", tenant.name, tenant.id),
        !tenant.id.is_nil() && tenant.status == "active",
    );

    // 2. 查找租户 (by ID)
    let found = Tenant::find_by_id(&pool, tenant.id).await;
    chain.add_step(
        "keycompute-db",
        "Tenant::find_by_id",
        "Tenant found by ID",
        found.is_ok() && found.as_ref().unwrap().is_some(),
    );

    // 3. 查找租户 (by slug)
    let found_by_slug = Tenant::find_by_slug(&pool, &tenant.slug).await;
    chain.add_step(
        "keycompute-db",
        "Tenant::find_by_slug",
        format!(
            "Tenant found by slug: {:?}",
            found_by_slug
                .as_ref()
                .unwrap()
                .as_ref()
                .map(|t| t.name.clone())
        ),
        found_by_slug.is_ok() && found_by_slug.as_ref().unwrap().is_some(),
    );

    // 4. 更新租户
    let update_req = keycompute_db::UpdateTenantRequest {
        name: Some("Updated Test Tenant".to_string()),
        description: Some("Updated description".to_string()),
        status: None,
        default_rpm_limit: Some(200),
        default_tpm_limit: Some(100000),
    };
    let updated = tenant.update(&pool, &update_req).await;
    chain.add_step(
        "keycompute-db",
        "Tenant::update",
        format!(
            "Tenant updated: {:?}",
            updated.as_ref().map(|t| t.name.clone())
        ),
        updated.is_ok() && updated.as_ref().unwrap().name == "Updated Test Tenant",
    );

    // 5. 验证更新
    if let Ok(Some(t)) = Tenant::find_by_id(&pool, tenant.id).await {
        chain.add_step(
            "keycompute-db",
            "verify_update",
            format!("RPM: {}, TPM: {}", t.default_rpm_limit, t.default_tpm_limit),
            t.default_rpm_limit == 200 && t.default_tpm_limit == 100000,
        );
    }

    // 6. 查找所有租户
    let all = Tenant::find_all(&pool).await;
    chain.add_step(
        "keycompute-db",
        "Tenant::find_all",
        format!(
            "Found {} tenants",
            all.as_ref().map(|v| v.len()).unwrap_or(0)
        ),
        all.is_ok(),
    );

    // 7. 删除租户
    let delete_result = tenant.delete(&pool).await;
    chain.add_step(
        "keycompute-db",
        "Tenant::delete",
        "Tenant deleted",
        delete_result.is_ok(),
    );

    // 8. 验证删除
    let after_delete = Tenant::find_by_id(&pool, tenant.id).await;
    chain.add_step(
        "keycompute-db",
        "verify_delete",
        "Tenant no longer exists",
        after_delete.map(|t| t.is_none()).unwrap_or(false),
    );

    chain.print_report();
    assert!(chain.all_passed(), "Tenant CRUD tests failed");
}

// ============================================================================
// 用户 CRUD 测试
// ============================================================================

/// 测试用户 CRUD 操作
#[tokio::test]
async fn test_user_crud() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_api_key_crud cleanup should succeed");

    // 1. 创建租户和用户
    let tenant = create_test_tenant(&pool, "user-crud", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "user-crud", &test_id).await;

    chain.add_step(
        "keycompute-db",
        "User::create",
        format!("User created: {} ({})", user.email, user.id),
        !user.id.is_nil() && user.tenant_id == tenant.id,
    );

    // 2. 查找用户 (by ID)
    let found = User::find_by_id(&pool, user.id).await;
    chain.add_step(
        "keycompute-db",
        "User::find_by_id",
        "User found by ID",
        found.is_ok() && found.as_ref().unwrap().is_some(),
    );

    // 3. 查找用户 (by email)
    let found_by_email = User::find_by_email(&pool, &user.email).await;
    chain.add_step(
        "keycompute-db",
        "User::find_by_email",
        format!(
            "User found by email: {:?}",
            found_by_email
                .as_ref()
                .unwrap()
                .as_ref()
                .map(|u| u.email.clone())
        ),
        found_by_email.is_ok() && found_by_email.as_ref().unwrap().is_some(),
    );

    // 4. 查找租户下的用户
    let tenant_users = User::find_by_tenant(&pool, tenant.id).await;
    chain.add_step(
        "keycompute-db",
        "User::find_by_tenant",
        format!(
            "Found {} users in tenant",
            tenant_users.as_ref().map(|v| v.len()).unwrap_or(0)
        ),
        tenant_users.is_ok() && tenant_users.as_ref().unwrap().len() == 1,
    );

    // 5. 更新用户
    let update_req = keycompute_db::UpdateUserRequest {
        name: Some("Updated User Name".to_string()),
        role: Some(AssignableUserRole::Admin),
    };
    let updated = user.update(&pool, &update_req).await;
    chain.add_step(
        "keycompute-db",
        "User::update",
        format!(
            "User updated: {:?}",
            updated.as_ref().map(|u| u.name.clone())
        ),
        updated.is_ok() && updated.as_ref().unwrap().name == Some("Updated User Name".to_string()),
    );

    // 6. 删除用户
    let delete_result = user.delete(&pool).await;
    chain.add_step(
        "keycompute-db",
        "User::delete",
        "User deleted",
        delete_result.is_ok(),
    );

    chain.print_report();
    assert!(chain.all_passed(), "User CRUD tests failed");
}

/// 测试 users.role 数据库约束
#[tokio::test]
async fn test_user_role_constraint_rejects_invalid_role() {
    let pool = create_test_pool().await;
    let run_id = generate_test_id();
    let tenant = create_test_tenant(&pool, "role-constraint", &run_id).await;

    let result = sqlx::query(
        r#"
        INSERT INTO users (tenant_id, email, name, role)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(tenant.id)
    .bind(format!("invalid-role-{}@example.com", run_id))
    .bind("Invalid Role User")
    .bind("tenant_admin")
    .execute(&pool)
    .await;

    cleanup_test_data(&pool, &run_id)
        .await
        .expect("test_user_role_constraint_rejects_invalid_role cleanup should succeed");

    let err = result.expect_err("invalid role insert should be rejected");
    assert!(err.to_string().contains("chk_users_role_allowed"));
}

/// 测试 default_user_role 数据库约束
#[tokio::test]
async fn test_default_user_role_setting_constraint_rejects_invalid_role() {
    let pool = create_test_pool().await;

    let result = sqlx::query(
        r#"
        UPDATE system_settings
        SET value = $1
        WHERE key = 'default_user_role'
        "#,
    )
    .bind("tenant_admin")
    .execute(&pool)
    .await;

    let err = result.expect_err("invalid default_user_role should be rejected");
    assert!(
        err.to_string()
            .contains("chk_system_settings_default_user_role")
    );
}

/// 测试 system 角色全局唯一约束
#[tokio::test]
async fn test_system_role_unique_index_rejects_duplicate_system_user() {
    let pool = create_test_pool().await;
    let run_id = generate_test_id();
    let mut tx = pool.begin().await.expect("transaction should start");
    let tenant_a_id = Uuid::new_v4();
    let tenant_b_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)")
        .bind(tenant_a_id)
        .bind("System Unique A")
        .bind(format!("test-tenant-system-unique-a-{}", run_id))
        .bind("System unique A")
        .execute(&mut *tx)
        .await
        .expect("tenant A should be inserted");

    sqlx::query("INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)")
        .bind(tenant_b_id)
        .bind("System Unique B")
        .bind(format!("test-tenant-system-unique-b-{}", run_id))
        .bind("System unique B")
        .execute(&mut *tx)
        .await
        .expect("tenant B should be inserted");

    sqlx::query("INSERT INTO users (tenant_id, email, name, role) VALUES ($1, $2, $3, $4)")
        .bind(tenant_a_id)
        .bind(format!("system-a-{}@example.com", run_id))
        .bind("System A")
        .bind("system")
        .execute(&mut *tx)
        .await
        .expect("first system user should be created");

    let result =
        sqlx::query("INSERT INTO users (tenant_id, email, name, role) VALUES ($1, $2, $3, $4)")
            .bind(tenant_b_id)
            .bind(format!("system-b-{}@example.com", run_id))
            .bind("System B")
            .bind("system")
            .execute(&mut *tx)
            .await;

    let err = result.expect_err("duplicate system user should be rejected");
    assert!(err.to_string().contains("uq_users_single_system_role"));
    tx.rollback()
        .await
        .expect("transaction rollback should succeed");
}

/// 测试禁止将 system 用户降级
#[tokio::test]
async fn test_system_role_change_trigger_rejects_downgrade() {
    let pool = create_test_pool().await;
    let run_id = generate_test_id();
    let mut tx = pool.begin().await.expect("transaction should start");
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)")
        .bind(tenant_id)
        .bind("System Downgrade")
        .bind(format!("test-tenant-system-role-downgrade-{}", run_id))
        .bind("System role downgrade")
        .execute(&mut *tx)
        .await
        .expect("tenant should be inserted");

    sqlx::query("INSERT INTO users (id, tenant_id, email, name, role) VALUES ($1, $2, $3, $4, $5)")
        .bind(user_id)
        .bind(tenant_id)
        .bind(format!("system-downgrade-{}@example.com", run_id))
        .bind("System Downgrade")
        .bind("system")
        .execute(&mut *tx)
        .await
        .expect("system user should be created for downgrade trigger test");

    let result = sqlx::query("UPDATE users SET role = 'user' WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await;

    let err = result.expect_err("system role downgrade should be rejected");
    assert!(
        err.to_string()
            .contains("system user role cannot be changed")
    );
    tx.rollback()
        .await
        .expect("transaction rollback should succeed");
}

/// 测试禁止通过更新将普通用户提升为 system
#[tokio::test]
async fn test_system_role_change_trigger_rejects_promotion() {
    let pool = create_test_pool().await;
    let run_id = generate_test_id();
    let mut tx = pool.begin().await.expect("transaction should start");
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)")
        .bind(tenant_id)
        .bind("System Promotion")
        .bind(format!("test-tenant-system-role-promotion-{}", run_id))
        .bind("System role promotion")
        .execute(&mut *tx)
        .await
        .expect("tenant should be inserted");

    sqlx::query("INSERT INTO users (id, tenant_id, email, name, role) VALUES ($1, $2, $3, $4, $5)")
        .bind(user_id)
        .bind(tenant_id)
        .bind(format!("user-promotion-{}@example.com", run_id))
        .bind("Promotion User")
        .bind("user")
        .execute(&mut *tx)
        .await
        .expect("user should be created for promotion trigger test");

    let result = sqlx::query("UPDATE users SET role = 'system' WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await;

    let err = result.expect_err("promotion to system should be rejected");
    assert!(
        err.to_string()
            .contains("system role cannot be assigned by update")
    );
    tx.rollback()
        .await
        .expect("transaction rollback should succeed");
}

/// 测试 system 用户删除触发器
#[tokio::test]
async fn test_system_user_delete_trigger_rejects_direct_delete() {
    let pool = create_test_pool().await;
    let run_id = generate_test_id();
    let mut tx = pool.begin().await.expect("transaction should start");
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, slug, description) VALUES ($1, $2, $3, $4)")
        .bind(tenant_id)
        .bind("System Delete Guard")
        .bind(format!("test-tenant-system-delete-guard-{}", run_id))
        .bind("System delete guard")
        .execute(&mut *tx)
        .await
        .expect("tenant should be inserted");

    sqlx::query("INSERT INTO users (id, tenant_id, email, name, role) VALUES ($1, $2, $3, $4, $5)")
        .bind(user_id)
        .bind(tenant_id)
        .bind(format!("system-delete-guard-{}@example.com", run_id))
        .bind("System Guard")
        .bind("system")
        .execute(&mut *tx)
        .await
        .expect("system user should be created for trigger test");

    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await;

    let err = result.expect_err("system user delete should be rejected");
    assert!(err.to_string().contains("system user cannot be deleted"));
    tx.rollback()
        .await
        .expect("transaction rollback should succeed");
}

// ============================================================================
// API Key 测试
// ============================================================================

/// 测试 API Key 操作
#[tokio::test]
async fn test_api_key_operations() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_usage_log_crud cleanup should succeed");

    // 1. 创建租户和用户
    let tenant = create_test_tenant(&pool, "apikey", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "apikey", &test_id).await;

    // 2. 创建 API Key
    let key_hash = format!("hash-{}", Uuid::new_v4().simple());
    let api_key = ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: "Test API Key".to_string(),
            produce_ai_key_hash: key_hash.clone(),
            produce_ai_key_preview: "sk-test-****".to_string(),
            expires_at: None,
        },
    )
    .await;

    chain.add_step(
        "keycompute-db",
        "ProduceAiKey::create",
        format!("API Key created: {:?}", api_key.as_ref().map(|k| k.id)),
        api_key.is_ok(),
    );

    let Ok(api_key) = api_key else {
        chain.print_report();
        return;
    };

    // 3. 查找 API Key (by hash)
    let found = ProduceAiKey::find_by_hash(&pool, &key_hash).await;
    chain.add_step(
        "keycompute-db",
        "ProduceAiKey::find_by_hash",
        "API Key found by hash",
        found.is_ok() && found.as_ref().unwrap().is_some(),
    );

    // 4. 验证 API Key 有效
    let found_key = ProduceAiKey::find_by_hash(&pool, &key_hash).await;
    let is_valid = found_key
        .as_ref()
        .map(|k| k.as_ref().map(|k| k.is_valid()).unwrap_or(false))
        .unwrap_or(false);
    chain.add_step(
        "keycompute-db",
        "ProduceAiKey::is_valid",
        format!("API Key is valid: {}", is_valid),
        is_valid,
    );

    // 5. 撤销 API Key
    let revoked = api_key.revoke(&pool).await;
    chain.add_step(
        "keycompute-db",
        "ProduceAiKey::revoke",
        "API Key revoked",
        revoked.is_ok(),
    );

    // 6. 验证撤销后无效
    let revoked_key = ProduceAiKey::find_by_hash(&pool, &key_hash).await;
    let is_valid_after = revoked_key
        .as_ref()
        .map(|k| k.as_ref().map(|k| k.is_valid()).unwrap_or(true))
        .unwrap_or(true);
    chain.add_step(
        "keycompute-db",
        "verify_revoked",
        "Revoked API Key is invalid",
        !is_valid_after,
    );

    chain.print_report();
    assert!(chain.all_passed(), "API Key tests failed");
}

// ============================================================================
// UsageLog 测试
// ============================================================================

/// 测试 UsageLog 操作
#[tokio::test]
async fn test_usage_log_operations() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_transaction_handling cleanup should succeed");

    // 1. 创建测试数据
    let tenant = create_test_tenant(&pool, "usage", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "usage", &test_id).await;
    let key_hash = format!("hash-usage-{}", Uuid::new_v4().simple());
    let api_key = ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: "Usage Test Key".to_string(),
            produce_ai_key_hash: key_hash.clone(),
            produce_ai_key_preview: "sk-test-****".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect("Failed to create API key");

    // 2. 创建 UsageLog
    let request_id = Uuid::new_v4();
    let now = Utc::now();
    let usage_log = UsageLog::create(
        &pool,
        &CreateUsageLogRequest {
            request_id,
            tenant_id: tenant.id,
            user_id: user.id,
            produce_ai_key_id: api_key.id,
            model_name: "gpt-4o".to_string(),
            provider_name: "openai".to_string(),
            account_id: Uuid::new_v4(),
            input_tokens: 1000,
            output_tokens: 500,
            input_unit_price_snapshot: BigDecimal::from(1),
            output_unit_price_snapshot: BigDecimal::from(2),
            user_amount: BigDecimal::from(2), // (1000*1 + 500*2) / 1000
            currency: "CNY".to_string(),
            usage_source: "gateway_accumulated".to_string(),
            status: "success".to_string(),
            started_at: now - chrono::Duration::seconds(5),
            finished_at: now,
        },
    )
    .await;

    chain.add_step(
        "keycompute-db",
        "UsageLog::create",
        format!("UsageLog created: {:?}", usage_log.as_ref().map(|l| l.id)),
        usage_log.is_ok(),
    );

    let Ok(usage_log) = usage_log else {
        chain.print_report();
        return;
    };

    // 3. 验证字段
    chain.add_step(
        "keycompute-db",
        "verify_usage_log_fields",
        format!(
            "Input: {}, Output: {}, Total: {}",
            usage_log.input_tokens, usage_log.output_tokens, usage_log.total_tokens
        ),
        usage_log.input_tokens == 1000
            && usage_log.output_tokens == 500
            && usage_log.total_tokens == 1500,
    );

    // 4. 查找 UsageLog (by request_id)
    let found = UsageLog::find_by_request_id(&pool, request_id).await;
    chain.add_step(
        "keycompute-db",
        "UsageLog::find_by_request_id",
        "UsageLog found by request_id",
        found.is_ok() && found.as_ref().unwrap().is_some(),
    );

    // 5. 查找租户的 UsageLog
    let tenant_logs = UsageLog::find_by_tenant(&pool, tenant.id, 100, 0).await;
    chain.add_step(
        "keycompute-db",
        "UsageLog::find_by_tenant",
        format!(
            "Found {} logs for tenant",
            tenant_logs.as_ref().map(|v| v.len()).unwrap_or(0)
        ),
        tenant_logs.is_ok() && tenant_logs.as_ref().unwrap().len() == 1,
    );

    // 6. 获取租户统计
    let stats = UsageLog::get_stats_by_tenant(
        &pool,
        tenant.id,
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    )
    .await;
    chain.add_step(
        "keycompute-db",
        "UsageLog::get_stats_by_tenant",
        format!(
            "Stats: {:?} requests",
            stats.as_ref().map(|s| s.total_requests)
        ),
        stats.is_ok() && stats.as_ref().unwrap().total_requests == 1,
    );

    // 7. 获取用户统计
    let user_stats = UsageLog::get_user_stats(&pool, user.id).await;
    chain.add_step(
        "keycompute-db",
        "UsageLog::get_user_stats",
        format!(
            "User stats: {:?} requests, {:?} tokens",
            user_stats.as_ref().map(|s| s.total_requests),
            user_stats.as_ref().map(|s| s.total_tokens)
        ),
        user_stats.is_ok() && user_stats.as_ref().unwrap().total_requests == 1,
    );

    chain.print_report();
    assert!(chain.all_passed(), "UsageLog tests failed");
}

// ============================================================================
// 多租户隔离测试
// ============================================================================

/// 测试多租户数据隔离
#[tokio::test]
async fn test_multi_tenant_isolation() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_concurrent_operations cleanup should succeed");

    // 1. 创建两个租户
    let tenant1 = create_test_tenant(&pool, "isolation-1", &test_id).await;
    let tenant2 = create_test_tenant(&pool, "isolation-2", &test_id).await;

    chain.add_step(
        "keycompute-db",
        "create_two_tenants",
        format!("Created tenants: {} and {}", tenant1.name, tenant2.name),
        tenant1.id != tenant2.id,
    );

    // 2. 每个租户创建用户
    let user1 = create_test_user(&pool, tenant1.id, "isolation-1", &test_id).await;
    let user2 = create_test_user(&pool, tenant2.id, "isolation-2", &test_id).await;

    chain.add_step(
        "keycompute-db",
        "create_users_in_tenants",
        format!(
            "User1 in tenant1: {}, User2 in tenant2: {}",
            user1.tenant_id, user2.tenant_id
        ),
        user1.tenant_id == tenant1.id && user2.tenant_id == tenant2.id,
    );

    // 3. 验证租户用户隔离
    let tenant1_users = User::find_by_tenant(&pool, tenant1.id).await;
    let tenant2_users = User::find_by_tenant(&pool, tenant2.id).await;

    chain.add_step(
        "keycompute-db",
        "verify_tenant_isolation",
        format!(
            "Tenant1: {} users, Tenant2: {} users",
            tenant1_users.as_ref().map(|v| v.len()).unwrap_or(0),
            tenant2_users.as_ref().map(|v| v.len()).unwrap_or(0)
        ),
        tenant1_users
            .as_ref()
            .map(|v| v.len() == 1)
            .unwrap_or(false)
            && tenant2_users
                .as_ref()
                .map(|v| v.len() == 1)
                .unwrap_or(false),
    );

    // 4. 验证跨租户访问被阻止
    // 用户1不应该出现在租户2的用户列表中
    let tenant2_has_user1 = tenant2_users
        .map(|users| users.iter().any(|u| u.id == user1.id))
        .unwrap_or(false);

    chain.add_step(
        "keycompute-db",
        "verify_cross_tenant_blocked",
        "Cross-tenant access blocked",
        !tenant2_has_user1,
    );

    chain.print_report();
    assert!(chain.all_passed(), "Multi-tenant isolation tests failed");
}

// ============================================================================
// 并发操作测试
// ============================================================================

/// 测试并发数据库操作
#[tokio::test]
async fn test_concurrent_operations() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_tenant_isolation cleanup should succeed");

    // 1. 创建租户
    let tenant = create_test_tenant(&pool, "concurrent", &test_id).await;
    let pool = Arc::new(pool);
    let tenant_id = tenant.id;

    // 2. 并发创建用户
    let barrier = Arc::new(Barrier::new(10));
    let mut handles = Vec::new();

    for i in 0..10 {
        let pool_clone = Arc::clone(&pool);
        let barrier_clone = Arc::clone(&barrier);

        handles.push(tokio::spawn(async move {
            barrier_clone.wait().await;

            let email = format!("concurrent-{}-{}@example.com", i, Uuid::new_v4().simple());
            User::create(
                &pool_clone,
                &CreateUserRequest {
                    tenant_id,
                    email,
                    name: Some(format!("Concurrent User {}", i)),
                    role: Some(UserRole::User),
                },
            )
            .await
        }));
    }

    // 3. 等待所有操作完成
    let results: Vec<_> = futures::future::join_all(handles).await;

    // 收集错误信息用于调试
    let mut errors = Vec::new();
    for (i, r) in results.iter().enumerate() {
        match r {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => errors.push(format!("Task {} error: {}", i, e)),
            Err(e) => errors.push(format!("Task {} panicked: {}", i, e)),
        }
    }
    if !errors.is_empty() {
        eprintln!("Concurrent user creation errors:\n{}", errors.join("\n"));
    }

    let success_count = results
        .iter()
        .filter(|r| r.is_ok() && r.as_ref().unwrap().is_ok())
        .count();
    chain.add_step(
        "keycompute-db",
        "concurrent_user_creation",
        format!(
            "Created {} users concurrently (10 attempted)",
            success_count
        ),
        success_count == 10,
    );

    // 4. 验证所有用户存在
    let all_users = User::find_by_tenant(&pool, tenant_id).await;
    chain.add_step(
        "keycompute-db",
        "verify_concurrent_users",
        format!(
            "Found {} users in tenant",
            all_users.as_ref().map(|v| v.len()).unwrap_or(0)
        ),
        all_users.map(|v| v.len() == 10).unwrap_or(false),
    );

    chain.print_report();
    assert!(chain.all_passed(), "Concurrent operations tests failed");
}

/// 测试首次请求验证码失败时，pending 记录仍会保留并进入冷却
#[tokio::test]
async fn test_registration_request_failure_keeps_pending_and_consumes_cooldown() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("registration failure cleanup should succeed");

    let email = format!("registration-placeholder-{}@example.com", test_id);
    let service = RegistrationService::new(Arc::new(pool.clone()));

    let err = service
        .request_registration_code(
            &RequestRegistrationCodeRequest {
                email: email.clone(),
                referral_code: None,
            },
            Some("127.0.0.1".to_string()),
        )
        .await
        .expect_err("missing email service should fail");

    assert!(matches!(err, KeyComputeError::ServiceUnavailable(_)));

    let pending = PendingRegistration::find_by_email(&pool, &email)
        .await
        .expect("pending query should succeed")
        .expect("pending should be kept after failed send attempt");

    assert!(
        !pending.is_expired(),
        "failed send attempt should still record a fresh verification code window"
    );
    assert_eq!(pending.resend_count, 1);
    assert_eq!(pending.verify_attempts, 0);
    assert_eq!(pending.requested_from_ip.as_deref(), Some("127.0.0.1"));

    let retry_err = service
        .request_registration_code(
            &RequestRegistrationCodeRequest {
                email: email.clone(),
                referral_code: None,
            },
            Some("127.0.0.1".to_string()),
        )
        .await
        .expect_err("second request during cooldown should be rejected");

    assert!(matches!(retry_err, KeyComputeError::RateLimitExceeded(_)));
}

/// 测试已有 pending 时，请求验证码失败会刷新字段并重新进入冷却
#[tokio::test]
async fn test_registration_request_failure_refreshes_existing_pending_fields() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("existing pending cleanup should succeed");

    let email = format!("registration-existing-pending-{}@example.com", test_id);
    let initial_expires_at = Utc::now() + ChronoDuration::minutes(5);
    let initial_last_sent_at = Utc::now() - ChronoDuration::seconds(300);

    let initial_pending = create_test_pending_registration(
        &pool,
        UpsertPendingRegistrationRequest {
            email: email.clone(),
            referral_code: None,
            verification_code_hash: "existing-code-hash".to_string(),
            expires_at: initial_expires_at,
            requested_from_ip: Some("10.0.0.1".to_string()),
            resend_count: 3,
            last_sent_at: initial_last_sent_at,
        },
    )
    .await;

    let service = RegistrationService::new(Arc::new(pool.clone()));
    let err = service
        .request_registration_code(
            &RequestRegistrationCodeRequest {
                email: email.clone(),
                referral_code: None,
            },
            Some("127.0.0.1".to_string()),
        )
        .await
        .expect_err("missing email service should fail");

    assert!(matches!(err, KeyComputeError::ServiceUnavailable(_)));

    let pending = PendingRegistration::find_by_email(&pool, &email)
        .await
        .expect("pending query should succeed")
        .expect("existing pending should remain");

    assert_eq!(pending.id, initial_pending.id);
    assert_ne!(
        pending.verification_code_hash,
        initial_pending.verification_code_hash
    );
    assert!(pending.expires_at > initial_pending.expires_at);
    assert_eq!(pending.verify_attempts, 0);
    assert_eq!(pending.resend_count, initial_pending.resend_count + 1);
    assert!(pending.last_sent_at > initial_pending.last_sent_at);
    assert_eq!(pending.requested_from_ip.as_deref(), Some("127.0.0.1"));
}

/// 测试 default_user_quota 小于等于 0 时，不会赠送初始额度
#[tokio::test]
async fn test_complete_registration_skips_initial_balance_when_default_quota_not_positive() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("registration quota cleanup should succeed");

    let email = format!("registration-no-quota-{}@example.com", test_id);
    delete_user_by_email(&pool, &email)
        .await
        .expect("existing test user should be removed");

    let hasher = PasswordHasher::new();
    let code = "123456";
    let code_hash = hasher.hash(code).expect("code hash should succeed");

    create_test_pending_registration(
        &pool,
        UpsertPendingRegistrationRequest {
            email: email.clone(),
            referral_code: None,
            verification_code_hash: code_hash.clone(),
            expires_at: Utc::now() + ChronoDuration::minutes(10),
            requested_from_ip: Some("127.0.0.1".to_string()),
            resend_count: 1,
            last_sent_at: Utc::now() - ChronoDuration::seconds(300),
        },
    )
    .await;

    let service = RegistrationService::new(Arc::new(pool.clone()));
    let response = service
        .complete_registration(
            &keycompute_auth::CompleteRegistrationRequest {
                email: email.clone(),
                code: code.to_string(),
                password: "StrongPassword123!".to_string(),
                name: Some("No Quota User".to_string()),
            },
            0.0,
        )
        .await
        .expect("registration should succeed without initial quota");

    let balance_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_balances WHERE user_id = $1")
            .bind(response.user_id)
            .fetch_one(&pool)
            .await
            .expect("balance count query should succeed");
    assert_eq!(balance_count, 0);

    let transaction_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM balance_transactions WHERE user_id = $1 AND description = 'Initial quota from system'",
    )
    .bind(response.user_id)
    .fetch_one(&pool)
    .await
    .expect("balance transaction count query should succeed");
    assert_eq!(transaction_count, 0);

    delete_user_by_email(&pool, &email)
        .await
        .expect("created test user should be removed");
}

/// 测试同邮箱 pending 记录创建时会拒绝重复邮箱
#[tokio::test]
async fn test_pending_registration_create_rejects_duplicate_email() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("duplicate pending cleanup should succeed");

    let email = format!("registration-duplicate-pending-{}@example.com", test_id);

    let _first_pending = create_test_pending_registration(
        &pool,
        UpsertPendingRegistrationRequest {
            email: email.clone(),
            referral_code: None,
            verification_code_hash: "first-code-hash".to_string(),
            expires_at: Utc::now() + ChronoDuration::minutes(10),
            requested_from_ip: Some("10.0.0.1".to_string()),
            resend_count: 1,
            last_sent_at: Utc::now() - ChronoDuration::seconds(120),
        },
    )
    .await;

    let mut tx = pool.begin().await.expect("transaction should start");
    let err = PendingRegistration::create_in_tx(
        &mut tx,
        &UpsertPendingRegistrationRequest {
            email: email.clone(),
            referral_code: None,
            verification_code_hash: "second-code-hash".to_string(),
            expires_at: Utc::now() + ChronoDuration::minutes(10),
            requested_from_ip: Some("10.0.0.2".to_string()),
            resend_count: 1,
            last_sent_at: Utc::now(),
        },
    )
    .await
    .expect_err("duplicate email should be rejected");

    assert!(matches!(err, keycompute_db::DbError::DuplicateKey { .. }));

    tx.rollback()
        .await
        .expect("duplicate pending rollback should succeed");
}

/// 测试首码锁定后，后续无效推荐码会被直接忽略
#[tokio::test]
async fn test_registration_request_ignores_invalid_referral_after_first_touch_locked() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("locked referral cleanup should succeed");

    let tenant = create_test_tenant(&pool, "locked-referral", &test_id).await;
    let referrer = create_test_user(&pool, tenant.id, "locked-referrer", &test_id).await;
    let email = format!("registration-locked-referral-{}@example.com", test_id);

    let initial_pending = create_test_pending_registration(
        &pool,
        UpsertPendingRegistrationRequest {
            email: email.clone(),
            referral_code: Some(referrer.id),
            verification_code_hash: "existing-code-hash".to_string(),
            expires_at: Utc::now() + ChronoDuration::minutes(5),
            requested_from_ip: Some("10.0.0.1".to_string()),
            resend_count: 3,
            last_sent_at: Utc::now() - ChronoDuration::seconds(300),
        },
    )
    .await;

    let service = RegistrationService::new(Arc::new(pool.clone()));
    let err = service
        .request_registration_code(
            &RequestRegistrationCodeRequest {
                email: email.clone(),
                referral_code: Some("not-a-valid-referral".to_string()),
            },
            Some("127.0.0.1".to_string()),
        )
        .await
        .expect_err("missing email service should still be the only failure");

    assert!(matches!(err, KeyComputeError::ServiceUnavailable(_)));

    let pending = PendingRegistration::find_by_email(&pool, &email)
        .await
        .expect("pending query should succeed")
        .expect("locked referral pending should remain");

    assert_eq!(pending.id, initial_pending.id);
    assert_eq!(pending.referral_code, Some(referrer.id));
}

/// 测试同邮箱并发刷新验证码时，只允许一个事务成功刷新 pending
#[tokio::test]
async fn test_pending_registration_refresh_is_serialized_per_email() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("serialized refresh cleanup should succeed");

    let email = format!("registration-concurrent-{}@example.com", test_id);
    let initial_last_sent_at = Utc::now() - ChronoDuration::seconds(300);

    let initial_pending = create_test_pending_registration(
        &pool,
        UpsertPendingRegistrationRequest {
            email: email.clone(),
            referral_code: None,
            verification_code_hash: "initial-code-hash".to_string(),
            expires_at: Utc::now() + ChronoDuration::minutes(10),
            requested_from_ip: Some("10.0.0.1".to_string()),
            resend_count: 1,
            last_sent_at: initial_last_sent_at,
        },
    )
    .await;

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();

    for worker in 0..2 {
        let pool = pool.clone();
        let barrier = Arc::clone(&barrier);
        let email = email.clone();

        handles.push(tokio::spawn(async move {
            let mut tx = pool.begin().await.expect("transaction should start");
            barrier.wait().await;

            PendingRegistration::lock_email_slot(&mut tx, &email)
                .await
                .expect("email slot lock should succeed");

            let pending = PendingRegistration::find_by_email_for_update(&mut tx, &email)
                .await
                .expect("pending lookup should succeed")
                .expect("pending should exist");

            let elapsed = (Utc::now() - pending.last_sent_at).num_seconds();
            if !pending.is_expired() && elapsed < 60 {
                tx.rollback()
                    .await
                    .expect("cooldown transaction rollback should succeed");
                return false;
            }

            tokio::time::sleep(Duration::from_millis(200)).await;

            pending
                .refresh_code_in_tx(
                    &mut tx,
                    &UpsertPendingRegistrationRequest {
                        email,
                        referral_code: None,
                        verification_code_hash: format!("refreshed-hash-{worker}"),
                        expires_at: Utc::now() + ChronoDuration::minutes(10),
                        requested_from_ip: Some(format!("10.0.0.{}", worker + 2)),
                        resend_count: 1,
                        last_sent_at: Utc::now(),
                    },
                )
                .await
                .expect("pending refresh should succeed");

            tx.commit()
                .await
                .expect("refresh transaction should commit");

            true
        }));
    }

    let mut refresh_count = 0;
    let mut blocked_count = 0;
    for handle in handles {
        if handle.await.expect("task should join") {
            refresh_count += 1;
        } else {
            blocked_count += 1;
        }
    }

    let final_pending = PendingRegistration::find_by_email(&pool, &email)
        .await
        .expect("final pending lookup should succeed")
        .expect("pending should still exist");

    assert_eq!(
        refresh_count, 1,
        "exactly one concurrent request should refresh pending"
    );
    assert_eq!(
        blocked_count, 1,
        "exactly one concurrent request should hit cooldown"
    );
    assert_eq!(final_pending.id, initial_pending.id);
    assert_eq!(
        final_pending.resend_count, 2,
        "resend count should increase only once"
    );
    assert!(
        final_pending.last_sent_at > initial_pending.last_sent_at,
        "last_sent_at should be refreshed exactly once"
    );
    assert!(
        matches!(
            final_pending.verification_code_hash.as_str(),
            "refreshed-hash-0" | "refreshed-hash-1"
        ),
        "final code hash should come from the winning refresh"
    );
    assert!(
        matches!(
            final_pending.requested_from_ip.as_deref(),
            Some("10.0.0.2") | Some("10.0.0.3")
        ),
        "final IP should belong to the winning request"
    );
}

// ============================================================================
// 事务测试
// ============================================================================

/// 测试数据库事务
#[tokio::test]
async fn test_database_transaction() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_cascade_delete cleanup should succeed");

    // 1. 测试事务提交
    let tx_slug = format!("test-tx-tenant-{}", test_id);
    let tenant_id = {
        let mut tx = pool.begin().await.expect("Failed to begin transaction");

        let tenant = sqlx::query_as::<_, Tenant>(
            "INSERT INTO tenants (name, slug) VALUES ($1, $2) RETURNING *",
        )
        .bind("Transaction Test Tenant")
        .bind(&tx_slug)
        .fetch_one(&mut *tx)
        .await;

        chain.add_step(
            "keycompute-db",
            "transaction_insert",
            "Insert in transaction",
            tenant.is_ok(),
        );

        // 提交事务
        tx.commit().await.expect("Failed to commit transaction");

        tenant.unwrap().id
    };

    // 验证提交后数据存在
    let found = Tenant::find_by_id(&pool, tenant_id).await;
    chain.add_step(
        "keycompute-db",
        "verify_committed",
        "Data exists after commit",
        found.map(|t| t.is_some()).unwrap_or(false),
    );

    // 2. 测试事务回滚
    let rollback_slug = format!("test-rollback-tenant-{}", test_id);
    {
        let mut tx = pool.begin().await.expect("Failed to begin transaction");

        let _ = sqlx::query("INSERT INTO tenants (name, slug) VALUES ($1, $2)")
            .bind("Rollback Test Tenant")
            .bind(&rollback_slug)
            .execute(&mut *tx)
            .await;

        // 回滚事务
        tx.rollback().await.expect("Failed to rollback transaction");
    }

    // 验证回滚后数据不存在
    let found = Tenant::find_by_slug(&pool, &rollback_slug).await;
    chain.add_step(
        "keycompute-db",
        "verify_rolled_back",
        "Data does not exist after rollback",
        found.map(|t| t.is_none()).unwrap_or(false),
    );

    chain.print_report();
    assert!(chain.all_passed(), "Transaction tests failed");
}

// ============================================================================
// 完整业务链路测试
// ============================================================================

/// 测试完整的业务链路：租户 -> 用户 -> API Key -> UsageLog
#[tokio::test]
async fn test_full_business_chain() {
    let mut chain = VerificationChain::new();

    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("test_batch_operations cleanup should succeed");

    // 1. 创建租户
    let tenant = create_test_tenant(&pool, "full-chain", &test_id).await;
    chain.add_step(
        "keycompute-db",
        "step1_tenant",
        format!("Tenant: {} ({})", tenant.name, tenant.id),
        tenant.is_active(),
    );

    // 2. 创建用户
    let user = create_test_user(&pool, tenant.id, "full-chain", &test_id).await;
    chain.add_step(
        "keycompute-db",
        "step2_user",
        format!("User: {} ({})", user.email, user.id),
        user.tenant_id == tenant.id,
    );

    // 3. 创建 API Key
    let key_hash = format!("hash-full-chain-{}", Uuid::new_v4().simple());
    let api_key = ProduceAiKey::create(
        &pool,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: "Full Chain Test Key".to_string(),
            produce_ai_key_hash: key_hash.clone(),
            produce_ai_key_preview: "sk-fc-****".to_string(),
            expires_at: None,
        },
    )
    .await
    .expect("Failed to create API key");

    chain.add_step(
        "keycompute-db",
        "step3_api_key",
        format!("API Key: {} ({})", api_key.name, api_key.id),
        !api_key.revoked,
    );

    // 4. 创建 UsageLog
    let request_id = Uuid::new_v4();
    let now = Utc::now();
    let usage_log = UsageLog::create(
        &pool,
        &CreateUsageLogRequest {
            request_id,
            tenant_id: tenant.id,
            user_id: user.id,
            produce_ai_key_id: api_key.id,
            model_name: "gpt-4o".to_string(),
            provider_name: "openai".to_string(),
            account_id: Uuid::new_v4(),
            input_tokens: 2000,
            output_tokens: 1000,
            input_unit_price_snapshot: BigDecimal::from(5),
            output_unit_price_snapshot: BigDecimal::from(15),
            user_amount: BigDecimal::from(25), // (2000*5 + 1000*15) / 1000
            currency: "CNY".to_string(),
            usage_source: "provider_reported".to_string(),
            status: "success".to_string(),
            started_at: now - chrono::Duration::seconds(10),
            finished_at: now,
        },
    )
    .await
    .expect("Failed to create usage log");

    chain.add_step(
        "keycompute-db",
        "step4_usage_log",
        format!(
            "UsageLog: {} tokens, {} amount",
            usage_log.total_tokens, usage_log.user_amount
        ),
        usage_log.total_tokens == 3000,
    );

    // 5. 验证完整链路可追溯
    // 通过 request_id 找到 UsageLog -> 找到 User -> 找到 Tenant
    let found_log = UsageLog::find_by_request_id(&pool, request_id)
        .await
        .expect("Failed to find log")
        .expect("Log not found");

    let found_user = User::find_by_id(&pool, found_log.user_id)
        .await
        .expect("Failed to find user")
        .expect("User not found");

    let found_tenant = Tenant::find_by_id(&pool, found_user.tenant_id)
        .await
        .expect("Failed to find tenant")
        .expect("Tenant not found");

    chain.add_step(
        "keycompute-db",
        "step5_traceability",
        format!(
            "Traced: {} -> {} -> {}",
            found_tenant.name, found_user.email, found_log.model_name
        ),
        found_tenant.id == tenant.id && found_user.id == user.id && found_log.id == usage_log.id,
    );

    chain.print_report();
    assert!(chain.all_passed(), "Full business chain tests failed");
}
