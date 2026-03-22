//! Auth 模块端到端测试
//!
//! 验证认证流程：API Key 验证、JWT 解析、权限检查

use integration_tests::common::VerificationChain;
use keycompute_auth::{ApiKeyValidator, JwtValidator, Permission, PermissionChecker, UserInfo};
use keycompute_auth::user::TenantInfo;
use uuid::Uuid;

/// 测试 API Key 验证流程
#[tokio::test]
async fn test_auth_api_key_flow() {
    let mut chain = VerificationChain::new();

    // 1. 创建 API Key 验证器
    let validator = ApiKeyValidator::new("test-secret");
    chain.add_step(
        "keycompute-auth",
        "ApiKeyValidator::new",
        "API Key validator created",
        true,
    );

    // 2. 生成 API Key
    let key = ApiKeyValidator::generate_key();
    chain.add_step(
        "keycompute-auth",
        "ApiKeyValidator::generate_key",
        format!("Generated key prefix: {}", &key[..6]),
        key.starts_with("sk-"),
    );

    // 3. 验证 API Key 格式（通过检查前缀）
    let valid_format = key.starts_with("sk-");
    chain.add_step(
        "keycompute-auth",
        "ApiKeyValidator::validate_format",
        format!("Valid format: {}", valid_format),
        valid_format,
    );

    // 4. 测试无效格式（通过验证调用）
    let invalid_result = validator.validate("invalid-key").await;
    chain.add_step(
        "keycompute-auth",
        "ApiKeyValidator::validate_invalid",
        format!("Invalid format rejected: {}", invalid_result.is_err()),
        invalid_result.is_err(),
    );

    // 5. 测试哈希
    let hash = ApiKeyValidator::hash_key(&key);
    chain.add_step(
        "keycompute-auth",
        "ApiKeyValidator::hash_key",
        format!("Hash length: {}", hash.len()),
        hash.len() == 64, // SHA256 hex 长度
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试 JWT 验证流程
#[test]
fn test_auth_jwt_flow() {
    let mut chain = VerificationChain::new();

    // 1. 创建 JWT 验证器
    let validator = JwtValidator::new("test-secret", "keycompute");
    chain.add_step(
        "keycompute-auth",
        "JwtValidator::new",
        "JWT validator created",
        true,
    );

    // 2. 创建 Claims（需要 role 参数）
    let user_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let claims = keycompute_auth::JwtClaims::new(user_id, tenant_id, "user", 1893456000);
    
    chain.add_step(
        "keycompute-auth",
        "JwtClaims::new",
        format!("User ID: {:?}", claims.user_id),
        claims.user_id == user_id,
    );

    // 3. 检查过期
    let expired = claims.is_expired();
    chain.add_step(
        "keycompute-auth",
        "JwtClaims::is_expired",
        format!("Is expired: {}", expired),
        !expired,
    );

    // 4. 生成 token
    let token = validator.generate_token(&claims).unwrap();
    chain.add_step(
        "keycompute-auth",
        "JwtValidator::generate_token",
        format!("Token generated, length: {}", token.len()),
        !token.is_empty(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试权限检查
#[test]
fn test_auth_permission_flow() {
    let mut chain = VerificationChain::new();

    // 1. 创建权限
    let use_api = Permission::UseApi;
    let manage_users = Permission::ManageUsers;
    
    chain.add_step(
        "keycompute-auth",
        "Permission::UseApi",
        format!("UseApi as_str: {}", use_api.as_str()),
        use_api.as_str() == "api:use",
    );
    chain.add_step(
        "keycompute-auth",
        "Permission::ManageUsers",
        format!("ManageUsers as_str: {}", manage_users.as_str()),
        manage_users.as_str() == "users:manage",
    );

    // 2. 从字符串解析
    let parsed_api = Permission::from_str("api:use");
    let parsed_manage = Permission::from_str("users:manage");
    
    chain.add_step(
        "keycompute-auth",
        "Permission::from_str_api",
        "Parsed api:use permission",
        parsed_api == Some(Permission::UseApi),
    );
    chain.add_step(
        "keycompute-auth",
        "Permission::from_str_manage",
        "Parsed users:manage permission",
        parsed_manage == Some(Permission::ManageUsers),
    );

    // 3. 使用 PermissionChecker 静态方法检查权限
    let user_perms = vec![Permission::UseApi, Permission::ViewUsage];
    let has_api = PermissionChecker::check("user", &user_perms, &Permission::UseApi);
    let has_manage = PermissionChecker::check("user", &user_perms, &Permission::ManageUsers);
    
    chain.add_step(
        "keycompute-auth",
        "PermissionChecker::check_api",
        format!("User has UseApi: {}", has_api),
        has_api,
    );
    chain.add_step(
        "keycompute-auth",
        "PermissionChecker::check_manage",
        format!("User has ManageUsers: {}", has_manage),
        !has_manage,
    );

    // 4. 管理员拥有所有权限
    let admin_has_all = PermissionChecker::check("admin", &user_perms, &Permission::ManageUsers);
    chain.add_step(
        "keycompute-auth",
        "PermissionChecker::admin_has_all",
        format!("Admin has all permissions: {}", admin_has_all),
        admin_has_all,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试用户信息
#[test]
fn test_auth_user_info() {
    let mut chain = VerificationChain::new();

    // 1. 创建用户信息（需要 5 个参数）
    let user_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let user = UserInfo::new(user_id, tenant_id, "test@example.com", "test-user", "user");
    
    chain.add_step(
        "keycompute-auth",
        "UserInfo::new",
        format!("User ID: {:?}", user.id),
        user.id == user_id,
    );

    // 2. 检查租户
    chain.add_step(
        "keycompute-auth",
        "UserInfo::tenant_id",
        format!("Tenant ID: {:?}", user.tenant_id),
        user.tenant_id == tenant_id,
    );

    // 3. 检查名称
    chain.add_step(
        "keycompute-auth",
        "UserInfo::name",
        format!("User name: {}", user.name),
        user.name == "test-user",
    );

    // 4. 检查邮箱
    chain.add_step(
        "keycompute-auth",
        "UserInfo::email",
        format!("User email: {}", user.email),
        user.email == "test@example.com",
    );

    // 5. 检查是否是管理员
    chain.add_step(
        "keycompute-auth",
        "UserInfo::is_admin",
        format!("Is admin: {}", user.is_admin()),
        !user.is_admin(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试租户信息
#[test]
fn test_auth_tenant_info() {
    let mut chain = VerificationChain::new();

    // 1. 创建租户信息（需要 3 个参数）
    let tenant_id = Uuid::new_v4();
    let tenant = TenantInfo::new(tenant_id, "Test Tenant", "test-tenant");
    
    chain.add_step(
        "keycompute-auth",
        "TenantInfo::new",
        format!("Tenant ID: {:?}", tenant.id),
        tenant.id == tenant_id,
    );

    // 2. 检查名称
    chain.add_step(
        "keycompute-auth",
        "TenantInfo::name",
        format!("Tenant name: {}", tenant.name),
        tenant.name == "Test Tenant",
    );

    // 3. 检查 slug
    chain.add_step(
        "keycompute-auth",
        "TenantInfo::slug",
        format!("Tenant slug: {}", tenant.slug),
        tenant.slug == "test-tenant",
    );

    // 4. 检查默认激活状态
    chain.add_step(
        "keycompute-auth",
        "TenantInfo::active",
        format!("Tenant active: {}", tenant.active),
        tenant.active,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
