//! 用户管理相关类型

use keycompute_types::{AssignableUserRole, UserRole};
use serde::{Deserialize, Serialize};

use crate::api::common::encode_query_value;

/// 用户查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct UserQueryParams {
    /// 租户 ID 过滤
    pub tenant_id: Option<String>,
    /// 角色过滤
    pub role: Option<UserRole>,
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

    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    pub fn with_role(mut self, role: UserRole) -> Self {
        self.role = Some(role);
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
        if let Some(ref tenant_id) = self.tenant_id {
            params.push(format!("tenant_id={}", encode_query_value(tenant_id)));
        }
        if let Some(ref role) = self.role {
            params.push(format!("role={}", encode_query_value(role.as_str())));
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
    pub role: String,
    pub tenant_id: String,
    /// 租户名称（后端始终返回，默认 "Unknown"）
    pub tenant_name: String,
    /// 用户余额（后端始终返回，默认 0.0）
    pub balance: f64,
    pub created_at: String,
    pub updated_at: Option<String>,
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
    pub name: Option<String>,
    pub role: Option<AssignableUserRole>,
}

impl UpdateUserRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_role(mut self, role: AssignableUserRole) -> Self {
        self.role = Some(role);
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUserResponse {
    pub success: bool,
    pub message: String,
    pub user_id: String,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
}

/// 更新余额请求
///
/// 后端使用 amount 的正负值表示操作：正数为充值，负数为扣减
#[derive(Debug, Clone, Serialize)]
pub struct UpdateBalanceRequest {
    /// 金额（字符串格式避免浮点精度问题）
    /// 正数为充值，负数为扣减
    pub amount: String,
    /// 操作原因（必填）
    pub reason: String,
}

impl UpdateBalanceRequest {
    /// 创建充值请求
    pub fn add(amount: f64, reason: impl Into<String>) -> Self {
        Self {
            amount: format_amount(amount),
            reason: reason.into(),
        }
    }

    /// 创建扣减请求
    pub fn subtract(amount: f64, reason: impl Into<String>) -> Self {
        Self {
            amount: format_amount(-amount), // 负数
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
    /// 操作前余额
    pub balance_before: String,
    /// 操作后余额
    pub new_balance: String,
    /// 操作人 ID
    pub updated_by: String,
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
        let req = UpdateBalanceRequest::add(100.0, "Admin recharge");
        assert_eq!(req.amount, "100");
        assert_eq!(req.reason, "Admin recharge");

        // 扣减
        let req = UpdateBalanceRequest::subtract(50.0, "Admin deduction");
        assert_eq!(req.amount, "-50");
        assert_eq!(req.reason, "Admin deduction");
    }
}
