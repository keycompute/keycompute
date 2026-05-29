use client_api::api::node_tips::{
    CreateWithdrawalRequest, NodeTipsApi, TipsHistoryResponse, TipsSummary, WithdrawalRecord,
};
use client_api::error::Result;

use super::api_client::get_client;

/// 获取当前用户的小费汇总
pub async fn get_my_tips_summary(token: &str) -> Result<TipsSummary> {
    NodeTipsApi::new(&get_client())
        .get_my_tips_summary(token)
        .await
}

/// 获取当前用户的小费历史（分页）
pub async fn get_my_tips_history(
    token: &str,
    limit: u32,
    offset: u32,
) -> Result<TipsHistoryResponse> {
    NodeTipsApi::new(&get_client())
        .get_my_tips_history(token, limit, offset)
        .await
}

/// 发起小费提现
pub async fn create_withdrawal(
    token: &str,
    withdrawal_type: &str,
    alipay_account: Option<&str>,
    real_name: Option<&str>,
) -> Result<()> {
    let req = CreateWithdrawalRequest {
        withdrawal_type: withdrawal_type.to_string(),
        alipay_account: alipay_account.map(|s| s.to_string()),
        real_name: real_name.map(|s| s.to_string()),
    };
    NodeTipsApi::new(&get_client())
        .create_withdrawal(token, &req)
        .await?;
    Ok(())
}

/// 获取当前用户的提现记录
pub async fn get_my_withdrawals(token: &str) -> Result<Vec<WithdrawalRecord>> {
    let resp = NodeTipsApi::new(&get_client())
        .get_my_withdrawals(token)
        .await?;
    Ok(resp.items)
}
