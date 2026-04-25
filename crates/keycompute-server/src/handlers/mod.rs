//! 处理器模块
//
//! 处理各种 HTTP 请求

// 管理功能（拆分为多个模块）
pub mod admin_account;
pub mod admin_pricing;
pub mod admin_settings;
pub mod admin_user;

pub mod auth;
pub mod billing;
pub mod distribution;
pub mod gateway;
pub mod health;
pub mod openai;
pub mod payment;
pub mod pricing;
pub mod routing;
pub mod user;

// 认证相关
pub use auth::{
    complete_registration_handler, forgot_password_handler, login_handler, refresh_token_handler,
    register_handler, reset_password_handler, verify_reset_token_handler,
};

// OpenAI 兼容 API (统一入口)
pub use openai::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ListModelsResponse, Model,
    chat_completions, list_models, retrieve_model,
};

// Distribution 分销管理
pub use distribution::{
    create_distribution_rule, delete_distribution_rule, generate_invite_link,
    get_distribution_stats, get_my_distribution_earnings, get_my_referral_code, get_my_referrals,
    list_distribution_records, list_distribution_rules, update_distribution_rule,
};

// 用户自服务
pub use user::{
    change_password, create_api_key, delete_api_key, get_current_user, get_my_usage,
    get_my_usage_stats, list_my_api_keys, update_profile,
};

// 用户管理（admin_user）
pub use admin_user::{
    AdminUserInfo, UpdateUserRequest, UserListQueryParams, UserListResponse, delete_user,
    get_user_by_id, list_all_api_keys, list_all_users, list_tenants, update_user,
    update_user_balance,
};

// 账号管理（admin_account）
pub use admin_account::{
    AccountInfo, CreateAccountRequest, UpdateAccountRequest, create_account, delete_account,
    get_default_endpoint, list_accounts, refresh_account, test_account, update_account,
};

// 定价管理（admin_pricing）
pub use admin_pricing::{
    CreatePricingAdminRequest, PricingInfo, UpdatePricingAdminRequest, create_pricing,
    delete_pricing, list_pricing, set_default_pricing, update_pricing,
};

// 系统设置（admin_settings）
pub use admin_settings::{
    AdminSystemSettings, get_public_settings, get_system_setting_by_key, get_system_settings,
    update_system_setting_by_key, update_system_settings,
};

// 定价和账单
pub use billing::{calculate_cost, get_billing_stats, list_billing_records};
pub use pricing::{calculate_cost as get_pricing_cost, get_pricing};

// 调试接口
pub use gateway::{check_provider_health, get_execution_stats, get_gateway_status};
pub use routing::{debug_routing, get_provider_health, reset_health, set_account_cooldown};

// 健康检查
pub use health::health_check;

// 支付相关
pub use payment::{
    admin_list_payment_orders, alipay_notify, create_payment_order, get_my_balance,
    get_payment_order, list_my_payment_orders, sync_payment_order,
};

fn normalize_public_base_url(base_url: &str) -> Option<String> {
    let normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub(crate) fn configured_public_base_url(configured_base_url: Option<&str>) -> Option<String> {
    configured_base_url.and_then(normalize_public_base_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_configured_public_base_url_prefers_configured_value() {
        let base_url = configured_public_base_url(Some("https://configured.example.com/"));

        assert_eq!(base_url.as_deref(), Some("https://configured.example.com"));
    }

    #[test]
    fn test_configured_public_base_url_returns_none_when_missing() {
        let base_url = configured_public_base_url(None);

        assert!(base_url.is_none());
    }

    #[test]
    fn test_configured_public_base_url_ignores_blank_values() {
        let base_url = configured_public_base_url(Some("   "));

        assert!(base_url.is_none());
    }
}
