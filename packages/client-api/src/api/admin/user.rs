//! 用户管理相关类型

use keycompute_types::{PlatformRole, UserStatus};
use serde::{Deserialize, Serialize};

use crate::api::common::encode_query_value;

/// 用户查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct UserQueryParams {
    /// Global platform role filter.
    pub platform_role: Option<PlatformRole>,
    /// 搜索关键词（邮箱或名称）
    pub search: Option<String>,
    /// 页码（从 1 开始）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    /// 每页数量
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i64>,
}

impl UserQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_platform_role(mut self, role: PlatformRole) -> Self {
        self.platform_role = Some(role);
        self
    }

    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn with_page(mut self, page: i64) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_page_size(mut self, page_size: i64) -> Self {
        self.page_size = Some(page_size);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(role) = self.platform_role {
            params.push(format!(
                "platform_role={}",
                encode_query_value(role.as_str())
            ));
        }
        if let Some(ref search) = self.search {
            params.push(format!("search={}", encode_query_value(search)));
        }
        if let Some(page) = self.page {
            params.push(format!("page={}", page));
        }
        if let Some(page_size) = self.page_size {
            params.push(format!("page_size={}", page_size));
        }
        params.join("&")
    }
}

/// 用户详情
#[derive(Debug, Clone, Deserialize)]
pub struct UserDetail {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    #[serde(default)]
    pub platform_role: Option<PlatformRole>,
    #[serde(default)]
    pub status: Option<UserStatus>,
    #[serde(default)]
    pub memberships: Option<Vec<super::super::auth::TenantMembership>>,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
}

/// 用户列表响应（带分页信息）
#[derive(Debug, Clone, Deserialize)]
pub struct UserListResponse {
    pub users: Vec<UserDetail>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// 更新用户请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateUserRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    // Security changes are intentionally a separate server-authorized API;
    // this profile request cannot claim a platform role or tenant.
}

impl UpdateUserRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Global profile returned directly by the server.
pub type UpdateUserResponse = UserDetail;

/// 更新余额请求
///
/// 后端使用 amount 的正负值表示操作：正数为充值，负数为扣减。
/// freeze/unfreeze 始终使用正数金额。
#[derive(Debug, Clone, Serialize)]
pub struct UpdateBalanceRequest {
    pub tenant_id: uuid::Uuid,
    /// 金额（字符串格式避免浮点精度问题）
    /// 正数为充值，负数为扣减
    pub amount: String,
    /// 操作原因（必填）
    pub reason: String,
}

impl UpdateBalanceRequest {
    /// 创建充值请求
    pub fn add(tenant_id: uuid::Uuid, amount: f64, reason: impl Into<String>) -> Self {
        Self {
            tenant_id,
            amount: format_amount(amount),
            reason: reason.into(),
        }
    }

    /// 创建扣减请求
    pub fn subtract(tenant_id: uuid::Uuid, amount: f64, reason: impl Into<String>) -> Self {
        Self {
            tenant_id,
            amount: format_amount(-amount), // 负数
            reason: reason.into(),
        }
    }

    /// 创建通用请求（使用正数金额，适用于 freeze/unfreeze 等场景）
    pub fn new(tenant_id: uuid::Uuid, amount: f64, reason: impl Into<String>) -> Self {
        Self {
            tenant_id,
            amount: format_amount(amount),
            reason: reason.into(),
        }
    }
}

/// 格式化金额为字符串
/// 保留必要的小数位，避免浮点精度问题
fn format_amount(amount: f64) -> String {
    // 使用 {:.2} 保证精度，然后去除尾部多余的 0
    // 例如: 100.00 -> "100", 50.50 -> "50.5", -50.00 -> "-50"
    format!("{:.2}", amount)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// 余额更新响应（管理员操作用户余额后返回）
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBalanceResponse {
    pub success: bool,
    pub message: String,
    pub user_id: String,
    /// 操作金额
    pub amount: String,
    /// 操作原因
    pub reason: String,
    /// 操作前可用余额（充值/扣减/冻结场景）
    /// 后端已统一为 "available_balance_before"，保留旧 "balance_before" alias 向后兼容
    #[serde(default, alias = "balance_before")]
    pub available_balance_before: Option<String>,
    /// 操作前冻结余额（解冻场景）
    #[serde(default)]
    pub frozen_balance_before: Option<String>,
    /// 操作后可用余额（充值/扣减/冻结/解冻接口均会返回）
    #[serde(alias = "new_available_balance")]
    pub new_balance: Option<String>,
    /// 操作人 ID
    pub updated_by: String,
    /// 操作后冻结余额（冻结/解冻接口特有）
    #[serde(default)]
    pub new_frozen_balance: Option<String>,
}

/// 管理员查看的单笔活跃请求余额预留。
///
/// `version` 是不透明的并发控制值。释放操作必须回传刚从列表读取的
/// version，避免相同 `request_id` 已被新重试接管后误释放新预留。
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceReservationInfo {
    pub request_id: String,
    pub version: String,
    pub amount: String,
    pub status: String,
    pub expires_at: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 用户余额拆分及一页当前活跃的请求预留。
#[derive(Debug, Clone, Deserialize)]
pub struct UserBalanceReservationsResponse {
    pub user_id: String,
    pub available_balance: String,
    /// 总冻结余额，包括请求预留和管理员手工冻结。
    pub total_frozen_balance: String,
    /// 当前活跃请求持有的冻结余额，不能通过通用解冻接口释放。
    pub request_reserved_balance: String,
    /// 管理员通用解冻接口实际可释放的余额。
    pub manually_frozen_balance: String,
    pub reservations: Vec<BalanceReservationInfo>,
    /// Opaque cursor for the next page. `None` means the traversal is complete.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// 管理员按请求释放余额预留的请求。
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseBalanceReservationRequest {
    pub tenant_id: uuid::Uuid,
    /// 最新余额预留列表返回的不透明版本。
    pub expected_version: String,
    pub reason: String,
}

impl ReleaseBalanceReservationRequest {
    pub fn new(
        tenant_id: uuid::Uuid,
        expected_version: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            tenant_id,
            expected_version: expected_version.into(),
            reason: reason.into(),
        }
    }
}

/// 管理员按请求释放余额预留后的响应。
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseBalanceReservationResponse {
    pub success: bool,
    pub message: String,
    pub user_id: String,
    pub request_id: String,
    pub released_amount: String,
    pub reason: String,
    pub new_available_balance: String,
    pub new_total_frozen_balance: String,
    pub request_reserved_balance: String,
    pub manually_frozen_balance: String,
    pub released_by: String,
    /// 迟到的用量结算仍可能从可用余额扣款，调用方应向管理员明确展示。
    pub warning: String,
}

/// API Key 信息（用于 Admin 查看用户 API Key 列表）
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub key_preview: String,
    pub revoked: bool,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_amount() {
        // 正数
        assert_eq!(format_amount(100.0), "100");
        assert_eq!(format_amount(50.5), "50.5");
        assert_eq!(format_amount(0.01), "0.01");

        // 负数
        assert_eq!(format_amount(-50.0), "-50");
        assert_eq!(format_amount(-100.5), "-100.5");

        // 零
        assert_eq!(format_amount(0.0), "0");
    }

    #[test]
    fn test_update_balance_request() {
        // 充值
        let req = UpdateBalanceRequest::add(uuid::Uuid::from_u128(1), 100.0, "Admin recharge");
        assert_eq!(req.amount, "100");
        assert_eq!(req.reason, "Admin recharge");

        // 扣减
        let req = UpdateBalanceRequest::subtract(uuid::Uuid::from_u128(1), 50.0, "Admin deduction");
        assert_eq!(req.amount, "-50");
        assert_eq!(req.reason, "Admin deduction");
    }

    #[test]
    fn update_user_request_serializes_profile_only() {
        let request = UpdateUserRequest::new().with_name("Alice");
        assert_eq!(
            serde_json::to_value(request).expect("request should serialize"),
            serde_json::json!({"name": "Alice"})
        );

        let unchanged = UpdateUserRequest::new();
        assert_eq!(
            serde_json::to_value(unchanged).expect("request should serialize"),
            serde_json::json!({})
        );
    }

    #[test]
    fn update_user_response_matches_global_server_profile() {
        let response: UpdateUserResponse = serde_json::from_value(serde_json::json!({
            "id": "user-1",
            "email": "user@example.com",
            "name": "Alice",
            "platform_role": "none",
            "status": "active",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "last_login_at": null
        }))
        .expect("global profile response should deserialize");
        assert_eq!(response.id, "user-1");
        assert_eq!(response.platform_role, Some(PlatformRole::None));
        assert_eq!(response.status, Some(UserStatus::Active));
        assert!(response.memberships.is_none());
    }
}
