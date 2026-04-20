//! 数据库表结构定义
//
//! 本模块提供表名和列名的常量定义，用于构建类型安全的查询

/// 表名常量
pub mod tables {
    pub const USERS: &str = "users";
    pub const TENANTS: &str = "tenants";
    pub const PRODUCE_AI_KEYS: &str = "produce_ai_keys";
    pub const ACCOUNTS: &str = "accounts";
    pub const PRICING_MODELS: &str = "pricing_models";
    pub const USAGE_LOGS: &str = "usage_logs";
    pub const DISTRIBUTION_RECORDS: &str = "distribution_records";
    pub const TENANT_DISTRIBUTION_RULES: &str = "tenant_distribution_rules";
    pub const PAYMENT_ORDERS: &str = "payment_orders";
    pub const PENDING_REGISTRATIONS: &str = "pending_registrations";
    pub const USER_BALANCES: &str = "user_balances";
    pub const BALANCE_TRANSACTIONS: &str = "balance_transactions";
    pub const SYSTEM_SETTINGS: &str = "system_settings";
}

/// users 表列名
pub mod users {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const EMAIL: &str = "email";
    pub const NAME: &str = "name";
    pub const ROLE: &str = "role";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// tenants 表列名
pub mod tenants {
    pub const ID: &str = "id";
    pub const NAME: &str = "name";
    pub const SLUG: &str = "slug";
    pub const DESCRIPTION: &str = "description";
    pub const STATUS: &str = "status";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// produce_ai_keys 表列名
pub mod produce_ai_keys {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const USER_ID: &str = "user_id";
    pub const NAME: &str = "name";
    pub const PRODUCE_AI_KEY_HASH: &str = "produce_ai_key_hash";
    pub const PRODUCE_AI_KEY_PREVIEW: &str = "produce_ai_key_preview";
    pub const REVOKED: &str = "revoked";
    pub const REVOKED_AT: &str = "revoked_at";
    pub const EXPIRES_AT: &str = "expires_at";
    pub const LAST_USED_AT: &str = "last_used_at";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// accounts 表列名
pub mod accounts {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const PROVIDER: &str = "provider";
    pub const NAME: &str = "name";
    pub const ENDPOINT: &str = "endpoint";
    pub const UPSTREAM_API_KEY_ENCRYPTED: &str = "upstream_api_key_encrypted";
    pub const UPSTREAM_API_KEY_PREVIEW: &str = "upstream_api_key_preview";
    pub const RPM_LIMIT: &str = "rpm_limit";
    pub const TPM_LIMIT: &str = "tpm_limit";
    pub const PRIORITY: &str = "priority";
    pub const ENABLED: &str = "enabled";
    pub const MODELS_SUPPORTED: &str = "models_supported";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// pricing_models 表列名
pub mod pricing_models {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const MODEL_NAME: &str = "model_name";
    pub const PROVIDER: &str = "provider";
    pub const CURRENCY: &str = "currency";
    pub const INPUT_PRICE_PER_1K: &str = "input_price_per_1k";
    pub const OUTPUT_PRICE_PER_1K: &str = "output_price_per_1k";
    pub const IS_DEFAULT: &str = "is_default";
    pub const EFFECTIVE_FROM: &str = "effective_from";
    pub const EFFECTIVE_UNTIL: &str = "effective_until";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// usage_logs 表列名
pub mod usage_logs {
    pub const ID: &str = "id";
    pub const REQUEST_ID: &str = "request_id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const USER_ID: &str = "user_id";
    pub const PRODUCE_AI_KEY_ID: &str = "produce_ai_key_id";
    pub const MODEL_NAME: &str = "model_name";
    pub const PROVIDER_NAME: &str = "provider_name";
    pub const ACCOUNT_ID: &str = "account_id";
    pub const INPUT_TOKENS: &str = "input_tokens";
    pub const OUTPUT_TOKENS: &str = "output_tokens";
    pub const TOTAL_TOKENS: &str = "total_tokens";
    pub const INPUT_UNIT_PRICE_SNAPSHOT: &str = "input_unit_price_snapshot";
    pub const OUTPUT_UNIT_PRICE_SNAPSHOT: &str = "output_unit_price_snapshot";
    pub const USER_AMOUNT: &str = "user_amount";
    pub const CURRENCY: &str = "currency";
    pub const USAGE_SOURCE: &str = "usage_source";
    pub const STATUS: &str = "status";
    pub const STARTED_AT: &str = "started_at";
    pub const FINISHED_AT: &str = "finished_at";
    pub const CREATED_AT: &str = "created_at";
}

/// distribution_records 表列名
pub mod distribution_records {
    pub const ID: &str = "id";
    pub const USAGE_LOG_ID: &str = "usage_log_id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const BENEFICIARY_ID: &str = "beneficiary_id";
    pub const SHARE_AMOUNT: &str = "share_amount";
    pub const SHARE_RATIO: &str = "share_ratio";
    pub const STATUS: &str = "status";
    pub const SETTLED_AT: &str = "settled_at";
    pub const CREATED_AT: &str = "created_at";
}

/// tenant_distribution_rules 表列名
pub mod tenant_distribution_rules {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const BENEFICIARY_ID: &str = "beneficiary_id";
    pub const SHARE_RATIO: &str = "share_ratio";
    pub const PRIORITY: &str = "priority";
    pub const ENABLED: &str = "enabled";
    pub const EFFECTIVE_FROM: &str = "effective_from";
    pub const EFFECTIVE_UNTIL: &str = "effective_until";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// payment_orders 表列名
pub mod payment_orders {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const USER_ID: &str = "user_id";
    pub const OUT_TRADE_NO: &str = "out_trade_no";
    pub const TRADE_NO: &str = "trade_no";
    pub const AMOUNT: &str = "amount";
    pub const CURRENCY: &str = "currency";
    pub const STATUS: &str = "status";
    pub const PAYMENT_METHOD: &str = "payment_method";
    pub const SUBJECT: &str = "subject";
    pub const BODY: &str = "body";
    pub const PAID_AT: &str = "paid_at";
    pub const CLOSED_AT: &str = "closed_at";
    pub const EXPIRED_AT: &str = "expired_at";
    pub const PAY_URL: &str = "pay_url";
    pub const NOTIFY_DATA: &str = "notify_data";
    pub const REMARKS: &str = "remarks";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// pending_registrations 表列名
pub mod pending_registrations {
    pub const ID: &str = "id";
    pub const EMAIL: &str = "email";
    pub const REFERRAL_CODE: &str = "referral_code";
    pub const VERIFICATION_CODE_HASH: &str = "verification_code_hash";
    pub const EXPIRES_AT: &str = "expires_at";
    pub const VERIFY_ATTEMPTS: &str = "verify_attempts";
    pub const RESEND_COUNT: &str = "resend_count";
    pub const LAST_SENT_AT: &str = "last_sent_at";
    pub const REQUESTED_FROM_IP: &str = "requested_from_ip";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// user_balances 表列名
pub mod user_balances {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const USER_ID: &str = "user_id";
    pub const AVAILABLE_BALANCE: &str = "available_balance";
    pub const FROZEN_BALANCE: &str = "frozen_balance";
    pub const TOTAL_RECHARGED: &str = "total_recharged";
    pub const TOTAL_CONSUMED: &str = "total_consumed";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}

/// balance_transactions 表列名
pub mod balance_transactions {
    pub const ID: &str = "id";
    pub const TENANT_ID: &str = "tenant_id";
    pub const USER_ID: &str = "user_id";
    pub const ORDER_ID: &str = "order_id";
    pub const USAGE_LOG_ID: &str = "usage_log_id";
    pub const TRANSACTION_TYPE: &str = "transaction_type";
    pub const AMOUNT: &str = "amount";
    pub const BALANCE_BEFORE: &str = "balance_before";
    pub const BALANCE_AFTER: &str = "balance_after";
    pub const CURRENCY: &str = "currency";
    pub const DESCRIPTION: &str = "description";
    pub const CREATED_AT: &str = "created_at";
}

/// system_settings 表列名
pub mod system_settings {
    pub const ID: &str = "id";
    pub const KEY: &str = "key";
    pub const VALUE: &str = "value";
    pub const VALUE_TYPE: &str = "value_type";
    pub const DESCRIPTION: &str = "description";
    pub const IS_SENSITIVE: &str = "is_sensitive";
    pub const CREATED_AT: &str = "created_at";
    pub const UPDATED_AT: &str = "updated_at";
}
