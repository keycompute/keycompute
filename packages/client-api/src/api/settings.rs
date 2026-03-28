//! 系统设置模块
//!
//! 处理系统设置的查询和更新（Admin）以及公开设置查询

use crate::client::ApiClient;
use crate::error::Result;
use serde::Deserialize;
use std::collections::HashMap;

/// 设置 API 客户端
#[derive(Debug, Clone)]
pub struct SettingsApi {
    client: ApiClient,
}

impl SettingsApi {
    /// 创建新的设置 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    // ==================== Admin 接口 ====================

    /// 获取所有系统设置（Admin）
    pub async fn get_system_settings(&self, token: &str) -> Result<HashMap<String, SettingValue>> {
        self.client.get_json("/api/v1/settings", Some(token)).await
    }

    /// 批量更新系统设置（Admin）
    pub async fn update_system_settings(
        &self,
        settings: &HashMap<String, serde_json::Value>,
        token: &str,
    ) -> Result<HashMap<String, SettingValue>> {
        self.client
            .put_json("/api/v1/settings", settings, Some(token))
            .await
    }

    /// 获取指定设置（Admin）
    pub async fn get_system_setting_by_key(&self, key: &str, token: &str) -> Result<SettingValue> {
        self.client
            .get_json(&format!("/api/v1/settings/{}", key), Some(token))
            .await
    }

    /// 更新指定设置（Admin）
    pub async fn update_system_setting_by_key(
        &self,
        key: &str,
        value: &serde_json::Value,
        token: &str,
    ) -> Result<SettingValue> {
        self.client
            .put_json(&format!("/api/v1/settings/{}", key), value, Some(token))
            .await
    }

    // ==================== 公开接口 ====================

    /// 获取公开设置（无需认证）
    pub async fn get_public_settings(&self) -> Result<HashMap<String, SettingValue>> {
        self.client.get_json("/api/v1/settings/public", None).await
    }
}

/// 设置值
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Array(Vec<serde_json::Value>),
    Object(serde_json::Map<String, serde_json::Value>),
}

impl SettingValue {
    /// 获取字符串值
    pub fn as_string(&self) -> Option<&str> {
        match self {
            SettingValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// 获取数字值
    pub fn as_number(&self) -> Option<f64> {
        match self {
            SettingValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// 获取布尔值
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            SettingValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }
}
