//! 用户管理处理器
//!
//! 处理需要 Admin 权限的用户管理请求

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use keycompute_db::models::api_key::ProduceAiKey;
use keycompute_db::models::tenant::Tenant;
use keycompute_db::models::user::User;
use keycompute_types::{AssignableUserRole, UserRole};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ==================== 用户管理 ====================

/// 用户信息（Admin 视图）
#[derive(Debug, Serialize)]
pub struct AdminUserInfo {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub balance: f64,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

/// 用户列表查询参数
#[derive(Debug, Deserialize)]
pub struct UserListQueryParams {
    /// 租户 ID 过滤（可选）
    pub tenant_id: Option<Uuid>,
    /// 角色过滤（可选）
    pub role: Option<String>,
    /// 搜索关键词（邮箱或名称）
    pub search: Option<String>,
    /// 页码（从 1 开始）
    #[serde(default = "default_page")]
    pub page: i64,
    /// 每页数量
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page() -> i64 {
    1
}

fn default_page_size() -> i64 {
    20
}

fn validate_role_change_request(
    auth: &AuthExtractor,
    target_user_id: Uuid,
    target_user: &User,
    requested_role: &Option<AssignableUserRole>,
) -> Result<()> {
    if requested_role.is_none() {
        return Ok(());
    }

    if auth.role != UserRole::System.as_str() {
        return Err(ApiError::Forbidden(
            "Only system can change user roles".to_string(),
        ));
    }

    if auth.user_id == target_user_id {
        return Err(ApiError::BadRequest(
            "System cannot modify its own role".to_string(),
        ));
    }

    if target_user.role == UserRole::System.as_str() {
        return Err(ApiError::BadRequest(
            "System role cannot be modified".to_string(),
        ));
    }

    Ok(())
}

fn validate_user_delete_request(
    auth: &AuthExtractor,
    target_user_id: Uuid,
    target_user: &User,
) -> Result<()> {
    if target_user_id == auth.user_id {
        return Err(ApiError::BadRequest("Cannot delete yourself".to_string()));
    }

    if target_user.role == UserRole::System.as_str() {
        return Err(ApiError::BadRequest(
            "System user cannot be deleted".to_string(),
        ));
    }

    if target_user.role == UserRole::Admin.as_str() && auth.role != UserRole::System.as_str() {
        return Err(ApiError::Forbidden(
            "Only system can delete admin users".to_string(),
        ));
    }

    Ok(())
}

/// 用户列表响应（带分页信息）
#[derive(Debug, Serialize)]
pub struct UserListResponse {
    pub users: Vec<AdminUserInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// 列出所有用户
///
/// GET /api/v1/users
///
/// 支持查询参数：
/// - tenant_id: 租户 ID 过滤
/// - role: 角色过滤
/// - search: 搜索关键词
/// - page: 页码（默认 1）
/// - page_size: 每页数量（默认 20）
///
/// Admin 可以查询所有租户的用户
pub async fn list_all_users(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(params): Query<UserListQueryParams>,
) -> Result<Json<UserListResponse>> {
    // 检查权限
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 计算分页偏移量
    let offset = (params.page - 1) * params.page_size;

    // 查询所有用户（Admin 全局查询）
    let users = User::find_all(pool, params.page_size, offset)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query users: {}", e)))?;

    // 统计用户总数
    let total = User::count_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count users: {}", e)))?;

    // 预加载所有租户到 HashMap（避免 N+1 查询）
    let tenants = Tenant::find_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenants: {}", e)))?;
    let tenant_map: std::collections::HashMap<Uuid, String> =
        tenants.into_iter().map(|t| (t.id, t.name)).collect();

    // 第一遍：应用过滤条件，收集过滤后的用户
    let filtered_users: Vec<_> = users
        .into_iter()
        .filter(|user| {
            // 应用租户过滤
            if let Some(filter_tenant_id) = params.tenant_id
                && user.tenant_id != filter_tenant_id
            {
                return false;
            }
            // 应用角色过滤
            if let Some(ref filter_role) = params.role
                && &user.role != filter_role
            {
                return false;
            }
            // 应用搜索过滤
            if let Some(ref search) = params.search {
                let search_lower = search.to_lowercase();
                let email_match = user.email.to_lowercase().contains(&search_lower);
                let name_match = user
                    .name
                    .as_ref()
                    .map(|n| n.to_lowercase().contains(&search_lower))
                    .unwrap_or(false);
                if !email_match && !name_match {
                    return false;
                }
            }
            true
        })
        .collect();

    // 批量预加载余额（避免 N+1 查询）
    let user_ids: Vec<Uuid> = filtered_users.iter().map(|u| u.id).collect();
    let balance_map = if let Some(bs) = state.billing.balance_service() {
        bs.find_by_users(&user_ids).await.ok().unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };

    // 第二遍：构建用户信息列表
    let result: Vec<AdminUserInfo> = filtered_users
        .into_iter()
        .map(|user| {
            let balance = balance_map.get(&user.id);
            let tenant_name = tenant_map
                .get(&user.tenant_id)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());

            AdminUserInfo {
                id: user.id,
                email: user.email.clone(),
                name: user.name.clone(),
                role: user.role.clone(),
                tenant_id: user.tenant_id,
                tenant_name,
                balance: balance
                    .map(|b| b.available_balance.to_f64().unwrap_or(0.0))
                    .unwrap_or(0.0),
                created_at: user.created_at.to_rfc3339(),
                last_login_at: None,
            }
        })
        .collect();

    // 计算总页数
    let total_pages = (total + params.page_size - 1) / params.page_size;

    Ok(Json(UserListResponse {
        users: result,
        total,
        page: params.page,
        page_size: params.page_size,
        total_pages,
    }))
}

/// 获取指定用户信息
///
/// GET /api/v1/users/{id}
pub async fn get_user_by_id(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<AdminUserInfo>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

    // 获取租户名称
    let tenant = Tenant::find_by_id(pool, user.tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenant: {}", e)))?;
    let tenant_name = tenant
        .map(|t| t.name)
        .unwrap_or_else(|| "Unknown".to_string());

    // 获取用户余额
    let balance = if let Some(bs) = state.billing.balance_service() {
        bs.find_by_user(user.id).await.ok().flatten()
    } else {
        None
    };

    Ok(Json(AdminUserInfo {
        id: user.id,
        email: user.email,
        name: user.name,
        role: user.role,
        tenant_id: user.tenant_id,
        tenant_name,
        balance: balance
            .as_ref()
            .map(|b| b.available_balance.to_f64().unwrap_or(0.0))
            .unwrap_or(0.0),
        created_at: user.created_at.to_rfc3339(),
        last_login_at: None,
    }))
}

/// 更新用户请求
#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<AssignableUserRole>,
}

/// 更新用户信息
///
/// PUT /api/v1/users/{id}
pub async fn update_user(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

    validate_role_change_request(&auth, user_id, &user, &req.role)?;

    let update_req = keycompute_db::models::user::UpdateUserRequest {
        name: req.name,
        role: req.role,
    };

    let updated = user
        .update(pool, &update_req)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update user: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "User updated",
        "user_id": updated.id,
        "email": updated.email,
        "name": updated.name,
        "role": updated.role,
    })))
}

/// 删除用户
///
/// DELETE /api/v1/users/{id}
pub async fn delete_user(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let user = User::find_by_id(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find user: {}", e)))?
        .ok_or_else(|| ApiError::NotFound(format!("User not found: {}", user_id)))?;

    validate_user_delete_request(&auth, user_id, &user)?;

    user.delete(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to delete user: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "User deleted",
        "user_id": user_id,
        "deleted_by": auth.user_id,
    })))
}

/// 更新用户余额请求
#[derive(Debug, Deserialize)]
pub struct UpdateBalanceRequest {
    pub amount: String, // 使用字符串避免浮点精度问题
    pub reason: String,
}

/// 更新用户余额
///
/// POST /api/v1/users/{id}/balance
pub async fn update_user_balance(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(req): Json<UpdateBalanceRequest>,
) -> Result<Json<serde_json::Value>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let balance_service = state
        .billing
        .balance_service()
        .ok_or_else(|| ApiError::Internal("Balance service not configured".to_string()))?;

    // 解析金额
    let amount: Decimal = req
        .amount
        .parse()
        .map_err(|_| ApiError::BadRequest("Invalid amount format".to_string()))?;

    if amount == Decimal::ZERO {
        return Err(ApiError::BadRequest("Amount cannot be zero".to_string()));
    }

    // 更新余额
    // 注意：余额检查由 BalanceService 内部通过 FOR UPDATE 锁保证原子性
    // 不在此处预检查，避免 TOCTOU 竞争条件
    let (updated_balance, _transaction) = if amount > Decimal::ZERO {
        balance_service
            .recharge(user_id, auth.tenant_id, amount, None, Some(&req.reason))
            .await
            .map_err(ApiError::from)?
    } else {
        // 负数金额视为消费
        balance_service
            .consume(user_id, -amount, None, Some(&req.reason))
            .await
            .map_err(ApiError::from)?
    };

    // 计算操作前的余额
    // balance_before = new_balance - amount 对两种情况都成立
    // 充值: balance_before = new_balance - positive_amount
    // 消费: balance_before = new_balance - negative_amount = new_balance + |amount|
    let balance_before = updated_balance.available_balance - amount;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Balance updated",
        "user_id": user_id,
        "amount": amount.to_string(),
        "reason": req.reason,
        "balance_before": balance_before.to_string(),
        "new_balance": updated_balance.available_balance.to_string(),
        "updated_by": auth.user_id,
    })))
}

/// 列出用户的所有 API Keys（Admin 视图）
///
/// GET /api/v1/users/{id}/api-keys
pub async fn list_all_api_keys(
    auth: AuthExtractor,
    Path(user_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let keys = ProduceAiKey::find_by_user(pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to fetch API keys: {}", e)))?;

    let result: Vec<serde_json::Value> = keys
        .into_iter()
        .map(|k| {
            serde_json::json!({
                "id": k.id,
                "user_id": k.user_id,
                "name": k.name,
                "key_preview": k.produce_ai_key_preview,
                "is_active": !k.revoked,
                "revoked": k.revoked,
                "revoked_at": k.revoked_at.map(|t| t.to_rfc3339()),
                "created_at": k.created_at.to_rfc3339(),
                "last_used_at": k.last_used_at.map(|t| t.to_rfc3339()),
            })
        })
        .collect();

    Ok(Json(result))
}

// ==================== 租户管理 ====================

/// 租户信息
#[derive(Debug, Serialize)]
pub struct TenantInfo {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub user_count: i64,
    pub is_active: bool,
    pub created_at: String,
}

/// 列出所有租户
///
/// GET /api/v1/tenants
pub async fn list_tenants(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<Vec<TenantInfo>>> {
    if !auth.is_admin() {
        return Err(ApiError::Auth("Admin permission required".to_string()));
    }

    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    let tenants = Tenant::find_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query tenants: {}", e)))?;

    let mut result = Vec::new();
    for tenant in tenants {
        // 统计租户用户数量
        let users = User::find_by_tenant(pool, tenant.id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to count users: {}", e)))?;

        let is_active = tenant.is_active();
        let description = tenant.description.clone();

        result.push(TenantInfo {
            id: tenant.id,
            name: tenant.name,
            description,
            user_count: users.len() as i64,
            is_active,
            created_at: tenant.created_at.to_rfc3339(),
        });
    }

    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_admin_user_info_serialization() {
        let user = AdminUserInfo {
            id: Uuid::new_v4(),
            email: "admin@example.com".to_string(),
            name: Some("Admin".to_string()),
            role: "admin".to_string(),
            tenant_id: Uuid::new_v4(),
            tenant_name: "Test".to_string(),
            balance: 1000.0,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_login_at: None,
        };

        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("admin@example.com"));
    }

    fn make_test_user(id: Uuid, role: &str) -> User {
        use chrono::Utc;

        User {
            id,
            tenant_id: Uuid::new_v4(),
            email: "target@example.com".to_string(),
            name: Some("Target".to_string()),
            role: role.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn test_validate_role_change_request_requires_system() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(Uuid::new_v4(), "user");
        let err = validate_role_change_request(
            &auth,
            target.id,
            &target,
            &Some(AssignableUserRole::Admin),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(msg) if msg.contains("Only system")));
    }

    #[test]
    fn test_validate_role_change_request_rejects_self_role_change() {
        let user_id = Uuid::new_v4();
        let auth = AuthExtractor::new(user_id, Uuid::new_v4(), Uuid::new_v4(), "system");
        let target = make_test_user(user_id, "system");
        let err = validate_role_change_request(
            &auth,
            target.id,
            &target,
            &Some(AssignableUserRole::Admin),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("own role")));
    }

    #[test]
    fn test_validate_role_change_request_rejects_modifying_system_role() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "system");
        let target = make_test_user(Uuid::new_v4(), "system");
        let err = validate_role_change_request(
            &auth,
            target.id,
            &target,
            &Some(AssignableUserRole::Admin),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("cannot be modified")));
    }

    #[test]
    fn test_validate_user_delete_request_rejects_self_delete() {
        let user_id = Uuid::new_v4();
        let auth = AuthExtractor::new(user_id, Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(user_id, "admin");
        let err = validate_user_delete_request(&auth, target.id, &target).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("yourself")));
    }

    #[test]
    fn test_validate_user_delete_request_rejects_system_user() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(Uuid::new_v4(), "system");
        let err = validate_user_delete_request(&auth, target.id, &target).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(msg) if msg.contains("cannot be deleted")));
    }

    #[test]
    fn test_validate_user_delete_request_requires_system_for_admin_target() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "admin");
        let target = make_test_user(Uuid::new_v4(), "admin");
        let err = validate_user_delete_request(&auth, target.id, &target).unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(msg) if msg.contains("Only system")));
    }

    #[test]
    fn test_validate_user_delete_request_allows_system_to_delete_admin_target() {
        let auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "system");
        let target = make_test_user(Uuid::new_v4(), "admin");
        assert!(validate_user_delete_request(&auth, target.id, &target).is_ok());
    }
}
