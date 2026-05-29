//! 节点租赁小费管理模块
//!
//! 管理用户通过提供 node gateway token 获得的小费，
//! 支持两种提现方式：alipay（线下打款）/ balance（转入余额）。

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

pub use super::common::MessageResponse;

/// 节点小费 API 客户端
#[derive(Debug, Clone)]
pub struct NodeTipsApi {
    client: ApiClient,
}

impl NodeTipsApi {
    /// 创建新的 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取当前用户的小费汇总
    ///
    /// GET /api/v1/me/tips
    pub async fn get_my_tips_summary(&self, auth_token: &str) -> Result<TipsSummary> {
        self.client
            .get_json("/api/v1/me/tips", Some(auth_token))
            .await
    }

    /// 获取当前用户的小费历史（分页）
    ///
    /// GET /api/v1/me/tips/history?limit=20&offset=0
    pub async fn get_my_tips_history(
        &self,
        auth_token: &str,
        limit: u32,
        offset: u32,
    ) -> Result<TipsHistoryResponse> {
        let path = format!("/api/v1/me/tips/history?limit={}&offset={}", limit, offset);
        self.client.get_json(&path, Some(auth_token)).await
    }

    /// 发起小费提现
    ///
    /// POST /api/v1/me/tips/withdraw
    ///
    /// withdrawal_type: "alipay" | "balance"
    /// alipay_account + real_name: 仅 alipay 方式需要
    pub async fn create_withdrawal(
        &self,
        auth_token: &str,
        body: &CreateWithdrawalRequest,
    ) -> Result<WithdrawalResponse> {
        self.client
            .post_json("/api/v1/me/tips/withdraw", body, Some(auth_token))
            .await
    }

    /// 获取当前用户的提现记录列表
    ///
    /// GET /api/v1/me/tips/withdrawals
    pub async fn get_my_withdrawals(&self, auth_token: &str) -> Result<WithdrawalsListResponse> {
        self.client
            .get_json("/api/v1/me/tips/withdrawals", Some(auth_token))
            .await
    }
}

/// 小费汇总响应
#[derive(Debug, Clone, Deserialize)]
pub struct TipsSummary {
    /// 待提现总额
    pub pending_amount: String,
    /// 已提现总额
    pub withdrawn_amount: String,
    /// 累计小费总额
    pub total_amount: String,
    /// 待提现笔数
    pub pending_count: i64,
}

/// 小费历史列表响应（分页）
#[derive(Debug, Clone, Deserialize)]
pub struct TipsHistoryResponse {
    pub items: Vec<TipsHistoryItem>,
    pub total: i64,
}

/// 小费历史明细
#[derive(Debug, Clone, Deserialize)]
pub struct TipsHistoryItem {
    pub id: String,
    pub tip_amount: String,
    pub tip_ratio: String,
    pub bill_amount: String,
    pub created_at: String,
}

/// 发起提现请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWithdrawalRequest {
    /// 提现方式：alipay | balance
    pub withdrawal_type: String,
    /// 支付宝账号（仅 alipay）
    #[serde(default)]
    pub alipay_account: Option<String>,
    /// 真实姓名（仅 alipay）
    #[serde(default)]
    pub real_name: Option<String>,
}

/// 提现响应（创建后返回）
#[derive(Debug, Clone, Deserialize)]
pub struct WithdrawalResponse {
    pub id: String,
    #[serde(default)]
    pub withdrawal_type: Option<String>,
    #[serde(default)]
    pub total_amount: Option<String>,
    pub status: String,
    pub message: String,
}

/// 提现记录列表响应
#[derive(Debug, Clone, Deserialize)]
pub struct WithdrawalsListResponse {
    pub items: Vec<WithdrawalRecord>,
}

/// 提现记录
#[derive(Debug, Clone, Deserialize)]
pub struct WithdrawalRecord {
    pub id: String,
    pub withdrawal_type: String,
    pub total_amount: String,
    pub status: String,
    pub alipay_account: Option<String>,
    pub real_name: Option<String>,
    pub admin_remark: Option<String>,
    pub created_at: String,
    /// 更新时间（服务端暂不返回此字段，使用 default 兜底）
    #[serde(default)]
    pub updated_at: String,
}
