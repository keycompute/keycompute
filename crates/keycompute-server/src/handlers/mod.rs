//! 处理器模块
//
//! 处理各种 HTTP 请求

pub mod admin;
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
    forgot_password_handler, login_handler, refresh_token_handler, register_handler,
    resend_verification_handler, reset_password_handler, verify_email_handler,
    verify_reset_token_handler,
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

// 管理功能
pub use admin::{
    create_account, create_pricing, delete_account, delete_pricing, delete_user,
    get_system_settings, get_user_by_id, list_accounts, list_all_api_keys, list_all_users,
    list_pricing, list_tenants, refresh_account, set_default_pricing, test_account, update_account,
    update_pricing, update_system_settings, update_user, update_user_balance,
};

// 定价和账单
pub use billing::{calculate_cost, get_billing_stats, list_billing_records};
pub use pricing::{calculate_cost as get_pricing_cost, get_pricing};

// 调试接口
pub use gateway::{check_provider_health, get_execution_stats, get_gateway_status};
pub use routing::{debug_routing, get_provider_health};

// 健康检查
pub use health::health_check;

// 支付相关
pub use payment::{
    admin_list_payment_orders, alipay_notify, create_payment_order, get_my_balance,
    get_payment_order, list_my_payment_orders, sync_payment_order,
};
