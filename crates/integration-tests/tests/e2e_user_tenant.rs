//! 用户/租户系统端到端测试
//!
//! 验证用户/租户系统的完整业务逻辑：
//! - UserService 数据库集成
//! - AuthService 完整认证流程
//! - 用户-租户关联验证
//! - 租户状态检查
//! - Produce AI Key 验证链路

use integration_tests::common::VerificationChain;
use integration_tests::mocks::database::{
    MockProduceAiKey, MockTenant, MockUser, MockUserTenantDatabase,
};
use keycompute_auth::{
    AuthService, JwtValidator, Permission, ProduceAiKeyValidator,
    user::{TenantConfig, TenantInfo, UserInfo, UserService},
};
use uuid::Uuid;

// ============================================================================
// Mock 数据库测试（不需要真实数据库连接）
// ============================================================================

/// 测试 Mock 数据库基础操作
#[test]
fn test_mock_database_basic_operations() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();

    // 1. 创建租户
    let tenant = db.create_test_tenant();
    chain.add_step(
        "integration-tests",
        "MockUserTenantDatabase::create_test_tenant",
        format!("Tenant created: {} ({})", tenant.name, tenant.id),
        db.get_tenant(tenant.id).is_some(),
    );

    // 2. 创建用户
    let user = db.create_test_user(tenant.id, "user");
    chain.add_step(
        "integration-tests",
        "MockUserTenantDatabase::create_test_user",
        format!("User created: {} ({})", user.email, user.id),
        db.get_user(user.id).is_some(),
    );

    // 3. 创建 Produce AI Key
    let (produce_ai_key, raw_key) = db.create_test_produce_ai_key(user.id, tenant.id);
    chain.add_step(
        "integration-tests",
        "MockUserTenantDatabase::create_test_produce_ai_key",
        format!("Produce AI Key created: {}", produce_ai_key.id),
        db.get_produce_ai_key(produce_ai_key.id).is_some() && raw_key.starts_with("sk-test-"),
    );

    // 4. 验证数据统计
    let stats = db.stats();
    chain.add_step(
        "integration-tests",
        "MockUserTenantDatabase::stats",
        format!(
            "Stats: {} tenants, {} users, {} keys",
            stats.tenant_count, stats.user_count, stats.produce_ai_key_count
        ),
        stats.tenant_count == 1 && stats.user_count == 1 && stats.produce_ai_key_count == 1,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试租户状态管理
#[test]
fn test_tenant_status_management() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();

    // 1. 创建激活租户
    let tenant = db.create_test_tenant();
    chain.add_step(
        "integration-tests",
        "tenant_status::initial",
        format!("Initial status: {}", tenant.status),
        tenant.is_active(),
    );

    // 2. 暂停租户
    db.update_tenant_status(tenant.id, "suspended");
    let suspended = db.get_tenant(tenant.id).unwrap();
    chain.add_step(
        "integration-tests",
        "tenant_status::suspended",
        format!("Suspended status: {}", suspended.status),
        !suspended.is_active(),
    );

    // 3. 恢复租户
    db.update_tenant_status(tenant.id, "active");
    let active = db.get_tenant(tenant.id).unwrap();
    chain.add_step(
        "integration-tests",
        "tenant_status::restored",
        format!("Restored status: {}", active.status),
        active.is_active(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 Produce AI Key 生命周期
#[test]
fn test_produce_ai_key_lifecycle() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();

    let tenant = db.create_test_tenant();
    let user = db.create_test_user(tenant.id, "user");
    let (produce_ai_key, _) = db.create_test_produce_ai_key(user.id, tenant.id);

    // 1. 初始状态有效
    chain.add_step(
        "integration-tests",
        "produce_ai_key::initial_valid",
        "Produce AI Key initially valid",
        produce_ai_key.is_valid(),
    );

    // 2. 撤销后无效
    db.revoke_produce_ai_key(produce_ai_key.id);
    let revoked = db.get_produce_ai_key(produce_ai_key.id).unwrap();
    chain.add_step(
        "integration-tests",
        "produce_ai_key::revoked",
        format!("Revoked: {}", revoked.revoked),
        !revoked.is_valid(),
    );

    // 3. 创建过期 Key
    let expired_key = MockProduceAiKey::new(user.id, tenant.id, "expired-hash")
        .with_expires_at(chrono::Utc::now() - chrono::Duration::hours(1));
    chain.add_step(
        "integration-tests",
        "produce_ai_key::expired",
        "Expired key is invalid",
        !expired_key.is_valid(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// UserService 测试（无数据库模式）
// ============================================================================

/// 测试 UserService 无数据库模式
#[tokio::test]
async fn test_user_service_no_database() {
    let mut chain = VerificationChain::new();
    let service = UserService::new();

    // 1. 加载用户（模拟数据）
    let user_id = Uuid::new_v4();
    let user = service.load_user(user_id).await.unwrap();
    chain.add_step(
        "keycompute-auth",
        "UserService::load_user",
        format!("User loaded: {}", user.email),
        user.id == user_id,
    );

    // 2. 加载租户（模拟数据）
    let tenant_id = Uuid::new_v4();
    let tenant = service.load_tenant(tenant_id).await.unwrap();
    chain.add_step(
        "keycompute-auth",
        "UserService::load_tenant",
        format!("Tenant loaded: {}", tenant.name),
        tenant.id == tenant_id,
    );

    // 3. 加载用户和租户
    let (user, tenant) = service.load_user_and_tenant(user_id).await.unwrap();
    chain.add_step(
        "keycompute-auth",
        "UserService::load_user_and_tenant",
        format!("User {} in tenant {}", user.email, tenant.name),
        user.id == user_id,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// UserInfo / TenantInfo 测试
// ============================================================================

/// 测试 UserInfo 功能
#[test]
fn test_user_info_functionality() {
    let mut chain = VerificationChain::new();

    // 1. 创建普通用户
    let user_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let user = UserInfo::new(user_id, tenant_id, "user@test.com", "Test User", "user");

    chain.add_step(
        "keycompute-auth",
        "UserInfo::new",
        format!("User created: {}", user.email),
        user.email == "user@test.com" && !user.is_admin(),
    );

    // 2. 创建管理员用户
    let admin = UserInfo::new(
        Uuid::new_v4(),
        tenant_id,
        "admin@test.com",
        "Admin",
        "admin",
    );
    chain.add_step(
        "keycompute-auth",
        "UserInfo::is_admin",
        format!("Admin check: {}", admin.is_admin()),
        admin.is_admin(),
    );

    // 3. 创建系统管理员
    let system_admin = UserInfo::new(
        Uuid::new_v4(),
        tenant_id,
        "system@test.com",
        "System Admin",
        "system",
    );
    chain.add_step(
        "keycompute-auth",
        "UserInfo::system_admin",
        format!("System admin check: {}", system_admin.is_admin()),
        system_admin.is_admin(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 TenantInfo 和 TenantConfig
#[test]
fn test_tenant_info_with_config() {
    let mut chain = VerificationChain::new();

    // 1. 创建基础租户
    let tenant_id = Uuid::new_v4();
    let tenant = TenantInfo::new(tenant_id, "Test Corp", "test-corp");

    chain.add_step(
        "keycompute-auth",
        "TenantInfo::new",
        format!("Tenant created: {}", tenant.name),
        tenant.slug == "test-corp" && tenant.is_active(),
    );

    // 2. 配置租户
    let config = TenantConfig {
        default_rpm_limit: 100,
        default_tpm_limit: 50000,
    };
    let tenant_with_config = tenant.with_config(config);

    chain.add_step(
        "keycompute-auth",
        "TenantInfo::with_config",
        format!(
            "RPM: {}, TPM: {}",
            tenant_with_config.config.default_rpm_limit,
            tenant_with_config.config.default_tpm_limit,
        ),
        tenant_with_config.config.default_rpm_limit == 100
            && tenant_with_config.config.default_tpm_limit == 50000,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// JWT 认证测试
// ============================================================================

/// 测试 JWT 完整流程
#[test]
fn test_jwt_full_flow() {
    let mut chain = VerificationChain::new();

    // 1. 创建验证器
    let validator = JwtValidator::new("test-secret-key", "keycompute");
    chain.add_step(
        "keycompute-auth",
        "JwtValidator::new",
        "JWT validator created",
        true,
    );

    // 2. 生成 Token
    let user_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let token = validator
        .generate_token(user_id, tenant_id, "admin")
        .unwrap();

    chain.add_step(
        "keycompute-auth",
        "JwtValidator::generate_token",
        format!("Token length: {}", token.len()),
        !token.is_empty(),
    );

    // 3. 验证 Token
    let ctx = validator.validate(&token).unwrap();
    chain.add_step(
        "keycompute-auth",
        "JwtValidator::validate",
        format!("Validated user: {}, role: {}", ctx.user_id, ctx.role),
        ctx.user_id == user_id && ctx.role == "admin",
    );

    // 4. 检查权限
    chain.add_step(
        "keycompute-auth",
        "AuthContext::has_permission",
        format!(
            "Has SystemAdmin: {}",
            ctx.has_permission(&Permission::SystemAdmin)
        ),
        ctx.has_permission(&Permission::SystemAdmin),
    );

    // 5. 刷新 Token
    let refreshed = validator.refresh_token(&token).unwrap();
    let refreshed_ctx = validator.validate(&refreshed).unwrap();
    chain.add_step(
        "keycompute-auth",
        "JwtValidator::refresh_token",
        format!("Refreshed token for user: {}", refreshed_ctx.user_id),
        refreshed_ctx.user_id == user_id,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// API Key 认证测试
// ============================================================================

/// 测试 Produce AI Key 验证流程（无数据库）
///
/// 注意：无数据库连接时，验证会失败（安全默认行为）
#[tokio::test]
async fn test_produce_ai_key_validation_flow_requires_database() {
    let mut chain = VerificationChain::new();

    // 1. 创建验证器（无数据库）
    let validator = ProduceAiKeyValidator::new();
    chain.add_step(
        "keycompute-auth",
        "ProduceAiKeyValidator::new",
        "Produce AI Key validator created (no database)",
        true,
    );

    // 2. 生成 Key
    let key = ProduceAiKeyValidator::generate_key();
    chain.add_step(
        "keycompute-auth",
        "ProduceAiKeyValidator::generate_key",
        format!("Key prefix: {}", &key[..6]),
        key.starts_with("sk-"),
    );

    // 3. 验证格式
    let invalid_result = validator.validate("invalid-key").await;
    chain.add_step(
        "keycompute-auth",
        "ProduceAiKeyValidator::validate_format",
        format!("Invalid format rejected: {}", invalid_result.is_err()),
        invalid_result.is_err(),
    );

    // 4. 验证有效格式的 Key（无数据库连接时应该失败）
    let result = validator.validate(&key).await;
    chain.add_step(
        "keycompute-auth",
        "ProduceAiKeyValidator::validate (no db)",
        "Valid format key rejected without database connection",
        result.is_err(),
    );

    // 5. 验证错误信息
    if let Err(e) = result {
        let err_msg = e.to_string();
        chain.add_step(
            "keycompute-auth",
            "ProduceAiKeyValidator::error_message",
            format!("Error: {}", err_msg),
            err_msg.contains("not properly configured"),
        );
    }

    // 6. 哈希测试
    let hash = ProduceAiKeyValidator::hash_key(&key);
    chain.add_step(
        "keycompute-auth",
        "ProduceAiKeyValidator::hash_key",
        format!("Hash length: {}", hash.len()),
        hash.len() == 64, // SHA256 hex length
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// AuthService 集成测试
// ============================================================================

/// 测试 AuthService 完整流程
///
/// 注意：无数据库连接时，API Key 验证会失败（安全默认行为）
/// JWT 验证不需要数据库，可以正常工作
#[tokio::test]
async fn test_auth_service_integration() {
    let mut chain = VerificationChain::new();

    // 1. 创建 AuthService（无数据库连接）
    let api_validator = ProduceAiKeyValidator::new();
    let jwt_validator = JwtValidator::new("jwt-secret", "keycompute");
    let auth_service = AuthService::new(api_validator).with_jwt(jwt_validator);

    chain.add_step(
        "keycompute-auth",
        "AuthService::new",
        "AuthService created with Produce AI Key and JWT support (no database)",
        true,
    );

    // 2. 验证 Produce AI Key（无数据库连接时应该失败）
    let api_key = ProduceAiKeyValidator::generate_key();
    let api_result = auth_service.verify_api_key(&api_key).await;
    chain.add_step(
        "keycompute-auth",
        "AuthService::verify_api_key (no db)",
        "API Key rejected without database connection",
        api_result.is_err(),
    );

    // 3. 验证 JWT（不需要数据库）
    let user_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let jwt_validator = JwtValidator::new("jwt-secret", "keycompute");
    let token = jwt_validator
        .generate_token(user_id, tenant_id, "user")
        .unwrap();
    let jwt_ctx = auth_service.verify_jwt(&token).unwrap();

    chain.add_step(
        "keycompute-auth",
        "AuthService::verify_jwt",
        format!("JWT validated, user: {}", jwt_ctx.user_id),
        jwt_ctx.user_id == user_id,
    );

    // 4. 自动检测 Token 类型 - API Key（无数据库时应该失败）
    let detected_result = auth_service.verify_token(&api_key).await;
    chain.add_step(
        "keycompute-auth",
        "AuthService::verify_token (API Key, no db)",
        "API Key auto-detected and rejected without database",
        detected_result.is_err(),
    );

    // 5. 自动检测 Token 类型 - JWT（应该成功）
    let jwt_detected = auth_service.verify_token(&token).await.unwrap();
    chain.add_step(
        "keycompute-auth",
        "AuthService::verify_token (JWT)",
        format!(
            "JWT auto-detected and validated, user: {}",
            jwt_detected.user_id
        ),
        jwt_detected.user_id == user_id,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// 权限系统测试
// ============================================================================

/// 测试权限检查
#[test]
fn test_permission_system() {
    use keycompute_auth::{AuthType, build_permissions};

    let mut chain = VerificationChain::new();

    // 1. 权限字符串转换
    let use_api = Permission::from_str("api:use").unwrap();
    chain.add_step(
        "keycompute-auth",
        "Permission::from_str",
        format!("Parsed: {:?}", use_api),
        use_api == Permission::UseApi,
    );

    // 2. 权限检查
    let user_perms = vec![Permission::UseApi, Permission::ViewUsage];
    let has_api =
        keycompute_auth::PermissionChecker::check("user", &user_perms, &Permission::UseApi);
    let has_manage =
        keycompute_auth::PermissionChecker::check("user", &user_perms, &Permission::ManageUsers);

    chain.add_step(
        "keycompute-auth",
        "PermissionChecker::check",
        format!("User has UseApi: {}, ManageUsers: {}", has_api, has_manage),
        has_api && !has_manage,
    );

    // 3. 管理员权限（基于权限列表而非角色）
    // 权限检查完全基于权限列表，admin 角色如果没有 ManageUsers 权限应该返回 false
    let admin_perms = build_permissions(AuthType::Jwt, "admin");
    let admin_has_manage =
        keycompute_auth::PermissionChecker::check("admin", &admin_perms, &Permission::ManageUsers);
    chain.add_step(
        "keycompute-auth",
        "PermissionChecker::admin",
        format!(
            "Admin (with JWT perms) has ManageUsers: {}",
            admin_has_manage
        ),
        admin_has_manage,
    );

    // 4. 角色权限（使用新的 build_permissions 函数）
    let user_role = build_permissions(AuthType::Jwt, "user");
    let admin_role = build_permissions(AuthType::Jwt, "admin");

    chain.add_step(
        "keycompute-auth",
        "roles::jwt",
        format!(
            "User role: {} perms, Admin: {} perms",
            user_role.len(),
            admin_role.len()
        ),
        user_role.contains(&Permission::UseApi) && admin_role.contains(&Permission::ManageApiKeys),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

// ============================================================================
// 多租户场景测试
// ============================================================================

/// 测试多租户隔离
#[test]
fn test_multi_tenant_isolation() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();

    // 1. 创建两个租户
    let tenant1 = MockTenant::new("Tenant A", "tenant-a");
    let tenant2 = MockTenant::new("Tenant B", "tenant-b");
    db.insert_tenant(tenant1.clone());
    db.insert_tenant(tenant2.clone());

    chain.add_step(
        "integration-tests",
        "multi_tenant::create_tenants",
        format!("Created 2 tenants: {} and {}", tenant1.name, tenant2.name),
        db.stats().tenant_count == 2,
    );

    // 2. 每个租户创建用户
    let user1 = MockUser::new(tenant1.id, "user1@a.com", "user");
    let user2 = MockUser::new(tenant2.id, "user2@b.com", "user");
    db.insert_user(user1.clone());
    db.insert_user(user2.clone());

    chain.add_step(
        "integration-tests",
        "multi_tenant::create_users",
        "Created users in different tenants".to_string(),
        db.stats().user_count == 2,
    );

    // 3. 验证租户隔离
    let tenant1_users = db.get_users_by_tenant(tenant1.id);
    let tenant2_users = db.get_users_by_tenant(tenant2.id);

    chain.add_step(
        "integration-tests",
        "multi_tenant::isolation",
        format!(
            "Tenant A: {} users, Tenant B: {} users",
            tenant1_users.len(),
            tenant2_users.len()
        ),
        tenant1_users.len() == 1 && tenant2_users.len() == 1,
    );

    // 4. 验证用户不能跨租户访问
    let user1_in_tenant2 = tenant2_users.iter().find(|u| u.id == user1.id);
    chain.add_step(
        "integration-tests",
        "multi_tenant::cross_access",
        "Cross-tenant access blocked",
        user1_in_tenant2.is_none(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试用户-租户关联验证
#[test]
fn test_user_tenant_association() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();

    // 1. 创建租户和用户
    let tenant = db.create_test_tenant();
    let user = db.create_test_user(tenant.id, "user");

    chain.add_step(
        "integration-tests",
        "association::create",
        format!("User {} in tenant {}", user.id, tenant.id),
        user.tenant_id == tenant.id,
    );

    // 2. 创建另一个租户
    let other_tenant = MockTenant::new("Other", "other");
    db.insert_tenant(other_tenant.clone());

    // 3. 验证用户不属于其他租户
    chain.add_step(
        "integration-tests",
        "association::validate",
        format!("User tenant_id matches: {}", user.tenant_id == tenant.id),
        user.tenant_id != other_tenant.id,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
