//! 系统设置模型
//!
//! 提供全局系统配置的 CRUD 操作

use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

/// 设置值类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SettingValueType {
    String,
    Bool,
    Int,
    Decimal,
    Json,
}

impl From<&str> for SettingValueType {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bool" => SettingValueType::Bool,
            "int" => SettingValueType::Int,
            "decimal" => SettingValueType::Decimal,
            "json" => SettingValueType::Json,
            _ => SettingValueType::String,
        }
    }
}

impl std::fmt::Display for SettingValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingValueType::String => write!(f, "string"),
            SettingValueType::Bool => write!(f, "bool"),
            SettingValueType::Int => write!(f, "int"),
            SettingValueType::Decimal => write!(f, "decimal"),
            SettingValueType::Json => write!(f, "json"),
        }
    }
}

/// 系统设置模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SystemSetting {
    pub id: Uuid,
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub description: Option<String>,
    pub is_sensitive: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 系统设置键名常量
pub mod setting_keys {
    // 站点设置
    pub const SITE_NAME: &str = "site_name";
    pub const SITE_DESCRIPTION: &str = "site_description";
    pub const SITE_LOGO_URL: &str = "site_logo_url";
    pub const SITE_FAVICON_URL: &str = "site_favicon_url";
    /// API 基础 URL（用于生成 API 用法示例）
    pub const API_BASE_URL: &str = "api_base_url";

    // 注册设置
    pub const DEFAULT_USER_QUOTA: &str = "default_user_quota";
    pub const DEFAULT_USER_ROLE: &str = "default_user_role";

    // 限流设置
    pub const DEFAULT_RPM_LIMIT: &str = "default_rpm_limit";
    pub const DEFAULT_TPM_LIMIT: &str = "default_tpm_limit";

    // 系统状态
    pub const MAINTENANCE_MODE: &str = "maintenance_mode";
    pub const MAINTENANCE_MESSAGE: &str = "maintenance_message";

    // 分销设置
    pub const DISTRIBUTION_ENABLED: &str = "distribution_enabled";
    pub const DISTRIBUTION_LEVEL1_DEFAULT_RATIO: &str = "distribution_level1_default_ratio";
    pub const DISTRIBUTION_LEVEL2_DEFAULT_RATIO: &str = "distribution_level2_default_ratio";
    pub const DISTRIBUTION_MIN_WITHDRAW: &str = "distribution_min_withdraw";

    // 支付设置
    pub const ALIPAY_ENABLED: &str = "alipay_enabled";
    pub const WECHATPAY_ENABLED: &str = "wechatpay_enabled";
    pub const MIN_RECHARGE_AMOUNT: &str = "min_recharge_amount";
    pub const MAX_RECHARGE_AMOUNT: &str = "max_recharge_amount";
    pub const DEFAULT_CURRENCY: &str = "default_currency";

    // 安全设置
    pub const LOGIN_FAILED_LIMIT: &str = "login_failed_limit";
    pub const LOGIN_LOCKOUT_MINUTES: &str = "login_lockout_minutes";
    pub const JWT_EXPIRE_HOURS: &str = "jwt_expire_hours";
    // 密码策略使用硬编码，参见 keycompute-auth/src/password/validator.rs

    // 公告设置
    pub const SYSTEM_NOTICE: &str = "system_notice";
    pub const SYSTEM_NOTICE_ENABLED: &str = "system_notice_enabled";

    // 其他设置
    pub const FOOTER_CONTENT: &str = "footer_content";
    pub const ABOUT_CONTENT: &str = "about_content";
    pub const TERMS_OF_SERVICE_URL: &str = "terms_of_service_url";
    pub const PRIVACY_POLICY_URL: &str = "privacy_policy_url";
}

/// 更新系统设置请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSystemSettingRequest {
    pub value: String,
}

/// 批量更新系统设置请求
#[derive(Debug, Clone, Deserialize)]
pub struct BatchUpdateSettingsRequest {
    pub settings: std::collections::HashMap<String, String>,
}

/// 系统设置响应（包含类型信息）
#[derive(Debug, Clone, Serialize)]
pub struct SystemSettingResponse {
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub description: Option<String>,
}

impl From<SystemSetting> for SystemSettingResponse {
    fn from(setting: SystemSetting) -> Self {
        Self {
            key: setting.key,
            value: setting.value,
            value_type: setting.value_type,
            description: setting.description,
        }
    }
}

/// 公开系统设置（只包含前端需要的设置）
#[derive(Debug, Clone, Serialize)]
pub struct PublicSettings {
    pub site_name: String,
    pub site_description: Option<String>,
    pub site_logo_url: Option<String>,
    pub site_favicon_url: Option<String>,
    /// API 基础 URL（用于生成 API 用法示例）
    pub api_base_url: Option<String>,
    pub maintenance_mode: bool,
    pub maintenance_message: Option<String>,
    pub distribution_enabled: bool,
    pub alipay_enabled: bool,
    pub wechatpay_enabled: bool,
    pub system_notice: Option<String>,
    pub system_notice_enabled: bool,
    pub footer_content: Option<String>,
    pub about_content: Option<String>,
    pub terms_of_service_url: Option<String>,
    pub privacy_policy_url: Option<String>,
}

impl Default for PublicSettings {
    fn default() -> Self {
        Self {
            site_name: "KeyCompute".to_string(),
            site_description: Some("AI Gateway Platform".to_string()),
            site_logo_url: None,
            site_favicon_url: None,
            api_base_url: None,
            maintenance_mode: false,
            maintenance_message: None,
            distribution_enabled: true,
            alipay_enabled: false,
            wechatpay_enabled: false,
            system_notice: None,
            system_notice_enabled: false,
            footer_content: None,
            about_content: None,
            terms_of_service_url: None,
            privacy_policy_url: None,
        }
    }
}

impl SystemSetting {
    /// 解析布尔值
    pub fn parse_bool(&self) -> bool {
        self.value.to_lowercase() == "true" || self.value == "1"
    }

    /// 解析整数
    pub fn parse_int(&self) -> Result<i32, std::num::ParseIntError> {
        self.value.parse()
    }

    /// 解析浮点数
    pub fn parse_decimal(&self) -> Result<f64, std::num::ParseFloatError> {
        self.value.parse()
    }

    /// 解析 JSON
    pub fn parse_json<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.value)
    }

    /// 根据键名查找设置
    pub async fn find_by_key(
        pool: &sqlx::PgPool,
        key: &str,
    ) -> Result<Option<SystemSetting>, DbError> {
        let setting =
            sqlx::query_as::<_, SystemSetting>("SELECT * FROM system_settings WHERE key = $1")
                .bind(key)
                .fetch_optional(pool)
                .await?;

        Ok(setting)
    }

    /// 获取所有设置
    pub async fn find_all(pool: &sqlx::PgPool) -> Result<Vec<SystemSetting>, DbError> {
        let settings =
            sqlx::query_as::<_, SystemSetting>("SELECT * FROM system_settings ORDER BY key ASC")
                .fetch_all(pool)
                .await?;

        Ok(settings)
    }

    /// 获取所有非敏感设置
    pub async fn find_non_sensitive(pool: &sqlx::PgPool) -> Result<Vec<SystemSetting>, DbError> {
        let settings = sqlx::query_as::<_, SystemSetting>(
            "SELECT * FROM system_settings WHERE is_sensitive = false ORDER BY key ASC",
        )
        .fetch_all(pool)
        .await?;

        Ok(settings)
    }

    /// 更新设置值（如果不存在则创建）
    pub async fn update_value(
        pool: &sqlx::PgPool,
        key: &str,
        value: &str,
    ) -> Result<SystemSetting, DbError> {
        // 使用 INSERT ... ON CONFLICT 实现 UPSERT
        let setting = sqlx::query_as::<_, SystemSetting>(
            r#"
            INSERT INTO system_settings (key, value, value_type, description, created_at, updated_at)
            VALUES ($1, $2, 'string', '', NOW(), NOW())
            ON CONFLICT (key) DO UPDATE SET
                value = EXCLUDED.value,
                updated_at = NOW()
            RETURNING *
            "#,
        )
        .bind(key)
        .bind(value)
        .fetch_one(pool)
        .await?;

        Ok(setting)
    }

    /// 批量更新设置（使用事务）
    ///
    /// 所有更新在同一事务中执行，保证原子性
    pub async fn batch_update(
        pool: &sqlx::PgPool,
        settings: &std::collections::HashMap<String, String>,
    ) -> Result<Vec<SystemSetting>, DbError> {
        let mut tx = pool.begin().await?;
        let updated = Self::batch_update_tx(&mut tx, settings).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// 批量更新设置（在现有事务中执行）
    ///
    /// 用于在调用者已有事务中执行批量更新
    pub async fn batch_update_tx(
        tx: &mut Transaction<'_, Postgres>,
        settings: &std::collections::HashMap<String, String>,
    ) -> Result<Vec<SystemSetting>, DbError> {
        let mut updated = Vec::with_capacity(settings.len());

        for (key, value) in settings {
            let setting = sqlx::query_as::<_, SystemSetting>(
                r#"
                INSERT INTO system_settings (key, value, value_type, description, created_at, updated_at)
                VALUES ($1, $2, 'string', '', NOW(), NOW())
                ON CONFLICT (key) DO UPDATE SET
                    value = EXCLUDED.value,
                    updated_at = NOW()
                RETURNING *
                "#,
            )
            .bind(key)
            .bind(value)
            .fetch_one(&mut **tx)
            .await?;

            updated.push(setting);
        }

        Ok(updated)
    }

    /// 初始化默认设置
    ///
    /// 如果设置不存在，则使用默认值创建
    pub async fn init_default_settings(pool: &sqlx::PgPool) -> Result<(), DbError> {
        let defaults = vec![
            // 站点设置
            (setting_keys::SITE_NAME, "KeyCompute", "string"),
            (setting_keys::SITE_DESCRIPTION, "AI 模型聚合平台", "string"),
            // 注册设置
            (setting_keys::DEFAULT_USER_QUOTA, "10.00", "decimal"),
            (setting_keys::DEFAULT_USER_ROLE, "user", "string"),
            // 限流设置
            (setting_keys::DEFAULT_RPM_LIMIT, "60", "int"),
            (setting_keys::DEFAULT_TPM_LIMIT, "10000", "int"),
            // 系统状态
            (setting_keys::MAINTENANCE_MODE, "false", "bool"),
            (
                setting_keys::MAINTENANCE_MESSAGE,
                "系统维护中，请稍后再试",
                "string",
            ),
            // 支付设置
            (setting_keys::MIN_RECHARGE_AMOUNT, "1.0", "decimal"),
            (setting_keys::MAX_RECHARGE_AMOUNT, "10000.0", "decimal"),
            (setting_keys::DEFAULT_CURRENCY, "CNY", "string"),
            // 安全设置
            (setting_keys::LOGIN_FAILED_LIMIT, "5", "int"),
            (setting_keys::LOGIN_LOCKOUT_MINUTES, "30", "int"),
            (setting_keys::JWT_EXPIRE_HOURS, "72", "int"),
            // 公告设置
            (setting_keys::SYSTEM_NOTICE_ENABLED, "false", "bool"),
            // 分销设置 - 默认分销比例 (与 RuleEngine 硬编码保持一致)
            (setting_keys::DISTRIBUTION_ENABLED, "true", "bool"),
            (
                setting_keys::DISTRIBUTION_LEVEL1_DEFAULT_RATIO,
                "0.03",
                "decimal",
            ), // 一级分销默认 3%
            (
                setting_keys::DISTRIBUTION_LEVEL2_DEFAULT_RATIO,
                "0.02",
                "decimal",
            ), // 二级分销默认 2%
            (setting_keys::DISTRIBUTION_MIN_WITHDRAW, "100.0", "decimal"), // 最小提现金额 100元
        ];

        for (key, value, value_type) in defaults {
            // 检查设置是否已存在
            if Self::find_by_key(pool, key).await?.is_none() {
                // 不存在则创建
                sqlx::query(
                    r#"
                    INSERT INTO system_settings (key, value, value_type, description, created_at, updated_at)
                    VALUES ($1, $2, $3, '', NOW(), NOW())
                    "#,
                )
                .bind(key)
                .bind(value)
                .bind(value_type)
                .execute(pool)
                .await?;
            }
        }

        Ok(())
    }

    /// 获取设置的字符串值，不存在则返回默认值
    pub async fn get_string(pool: &sqlx::PgPool, key: &str, default: &str) -> String {
        match Self::find_by_key(pool, key).await {
            Ok(Some(setting)) => setting.value,
            _ => default.to_string(),
        }
    }

    /// 获取设置的布尔值，不存在则返回默认值
    pub async fn get_bool(pool: &sqlx::PgPool, key: &str, default: bool) -> bool {
        match Self::find_by_key(pool, key).await {
            Ok(Some(setting)) => setting.parse_bool(),
            _ => default,
        }
    }

    /// 获取设置的整数值，不存在则返回默认值
    pub async fn get_int(pool: &sqlx::PgPool, key: &str, default: i32) -> i32 {
        match Self::find_by_key(pool, key).await {
            Ok(Some(setting)) => setting.parse_int().unwrap_or(default),
            _ => default,
        }
    }

    /// 获取设置的浮点数值，不存在则返回默认值
    pub async fn get_decimal(pool: &sqlx::PgPool, key: &str, default: f64) -> f64 {
        match Self::find_by_key(pool, key).await {
            Ok(Some(setting)) => setting.parse_decimal().unwrap_or(default),
            _ => default,
        }
    }

    /// 获取公开设置
    pub async fn get_public_settings(pool: &sqlx::PgPool) -> PublicSettings {
        let settings = Self::find_non_sensitive(pool).await.unwrap_or_default();

        let get_value = |key: &str| -> Option<String> {
            settings
                .iter()
                .find(|s| s.key == key)
                .map(|s| s.value.clone())
        };

        let get_bool_value = |key: &str, default: bool| -> bool {
            settings
                .iter()
                .find(|s| s.key == key)
                .map(|s| s.parse_bool())
                .unwrap_or(default)
        };

        PublicSettings {
            site_name: get_value(setting_keys::SITE_NAME)
                .unwrap_or_else(|| "KeyCompute".to_string()),
            site_description: get_value(setting_keys::SITE_DESCRIPTION),
            site_logo_url: get_value(setting_keys::SITE_LOGO_URL),
            site_favicon_url: get_value(setting_keys::SITE_FAVICON_URL),
            api_base_url: get_value(setting_keys::API_BASE_URL),
            maintenance_mode: get_bool_value(setting_keys::MAINTENANCE_MODE, false),
            maintenance_message: get_value(setting_keys::MAINTENANCE_MESSAGE),
            distribution_enabled: get_bool_value(setting_keys::DISTRIBUTION_ENABLED, true),
            alipay_enabled: get_bool_value(setting_keys::ALIPAY_ENABLED, false),
            wechatpay_enabled: get_bool_value(setting_keys::WECHATPAY_ENABLED, false),
            system_notice: get_value(setting_keys::SYSTEM_NOTICE),
            system_notice_enabled: get_bool_value(setting_keys::SYSTEM_NOTICE_ENABLED, false),
            footer_content: get_value(setting_keys::FOOTER_CONTENT),
            about_content: get_value(setting_keys::ABOUT_CONTENT),
            terms_of_service_url: get_value(setting_keys::TERMS_OF_SERVICE_URL),
            privacy_policy_url: get_value(setting_keys::PRIVACY_POLICY_URL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_type_from_str() {
        assert_eq!(SettingValueType::from("string"), SettingValueType::String);
        assert_eq!(SettingValueType::from("bool"), SettingValueType::Bool);
        assert_eq!(SettingValueType::from("int"), SettingValueType::Int);
        assert_eq!(SettingValueType::from("decimal"), SettingValueType::Decimal);
        assert_eq!(SettingValueType::from("json"), SettingValueType::Json);
    }

    #[test]
    fn test_parse_bool() {
        let setting = SystemSetting {
            id: Uuid::nil(),
            key: "test".to_string(),
            value: "true".to_string(),
            value_type: "bool".to_string(),
            description: None,
            is_sensitive: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(setting.parse_bool());

        let setting = SystemSetting {
            value: "false".to_string(),
            ..setting
        };
        assert!(!setting.parse_bool());
    }
}
