#![allow(dead_code)]

use client_api::error::Result;
use client_api::{
    SettingsApi,
    api::common::MessageResponse,
    api::settings::{PublicSettings, SettingValue, SystemSettingRecord},
};
use std::collections::HashMap;

use super::api_client::get_client;

pub async fn get_all(token: &str) -> Result<HashMap<String, SettingValue>> {
    let client = get_client();
    SettingsApi::new(&client).get_system_settings(token).await
}

pub async fn update_all(
    settings: HashMap<String, serde_json::Value>,
    token: &str,
) -> Result<MessageResponse> {
    let client = get_client();
    SettingsApi::new(&client)
        .update_system_settings(&settings, token)
        .await
}

/// 更新单个设置项
pub async fn update_by_key(
    key: &str,
    value: &serde_json::Value,
    token: &str,
) -> Result<SystemSettingRecord> {
    let client = get_client();
    SettingsApi::new(&client)
        .update_system_setting_by_key(key, value, token)
        .await
}

/// 获取公开设置（结构化格式）
pub async fn get_public() -> Result<PublicSettings> {
    let client = get_client();
    SettingsApi::new(&client).get_public_settings().await
}

/// 获取公开设置（HashMap 格式，向后兼容）
pub async fn get_public_map() -> Result<HashMap<String, SettingValue>> {
    let client = get_client();
    SettingsApi::new(&client).get_public_settings_map().await
}
