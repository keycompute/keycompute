//! 节点租赁小费管理
//!
//! 用户端：
//!   - GET  /api/v1/me/tips             → 查看小费汇总
//!   - GET  /api/v1/me/tips/history      → 查看小费历史
//!   - POST /api/v1/me/tips/withdraw     → 发起提现（alipay / balance）
//!   - GET  /api/v1/me/tips/withdrawals  → 查看提现记录
//!
//! 管理端：
//!   - GET  /api/v1/admin/tips/withdrawals/pending  → 待审批提现列表
//!   - POST /api/v1/admin/tips/withdrawals/{id}/approve → 审批提现
//!   - POST /api/v1/admin/tips/withdrawals/{id}/complete → 完成提现（线下打款后）
//!   - PUT  /api/v1/admin/tips/settings/ratio → 设置小费比例

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use keycompute_db::models::{
    node_tip::{NodeTip, NodeTipSummary},
    node_tip_withdrawal::{
        ApproveWithdrawalRequest, CreateTipWithdrawalRequest, NodeTipWithdrawal,
        WITHDRAWAL_TYPE_ALIPAY, WITHDRAWAL_TYPE_BALANCE,
    },
    system_setting::SystemSetting,
    system_setting::setting_keys,
    user_balance::UserBalance,
};
use keycompute_runtime::crypto::{self};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ============================================================================
// 响应类型
// ============================================================================

/// 小费汇总响应
#[derive(Debug, Serialize)]
pub struct TipsSummaryResponse {
    pub pending_amount: String,
    pub withdrawn_amount: String,
    pub total_amount: String,
    pub pending_count: i64,
}

impl From<NodeTipSummary> for TipsSummaryResponse {
    fn from(s: NodeTipSummary) -> Self {
        Self {
            pending_amount: s.pending_amount.to_string(),
            withdrawn_amount: s.withdrawn_amount.to_string(),
            total_amount: s.total_amount.to_string(),
            pending_count: s.pending_count,
        }
    }
}

/// 小费历史条目
#[derive(Debug, Serialize)]
pub struct TipHistoryItem {
    pub id: Uuid,
    pub tip_amount: String,
    pub bill_amount: String,
    pub tip_ratio: String,
    pub created_at: String,
}

/// 小费历史列表响应
#[derive(Debug, Serialize)]
pub struct TipsHistoryResponse {
    pub items: Vec<TipHistoryItem>,
    pub total: i64,
}

/// 提现记录响应
#[derive(Debug, Serialize)]
pub struct WithdrawalItem {
    pub id: Uuid,
    pub withdrawal_type: String,
    pub total_amount: String,
    pub status: String,
    /// 脱敏的支付宝账号（用户视图）
    pub alipay_account_masked: Option<String>,
    /// 脱敏的真实姓名（用户视图）
    pub real_name_masked: Option<String>,
    pub admin_remark: Option<String>,
    pub created_at: String,
}

/// 提现记录列表响应
#[derive(Debug, Serialize)]
pub struct WithdrawalsResponse {
    pub items: Vec<WithdrawalItem>,
}

/// 提现创建响应
#[derive(Debug, Serialize)]
pub struct WithdrawalCreatedResponse {
    pub id: Uuid,
    pub status: String,
    pub message: String,
}

/// 待审批提现条目（管理员视图）
#[derive(Debug, Serialize)]
pub struct PendingWithdrawalItem {
    pub id: Uuid,
    pub user_id: Uuid,
    pub user_email: String,
    pub withdrawal_type: String,
    pub total_amount: String,
    /// 解密的支付宝账号（管理员视图，明文）
    pub alipay_account: Option<String>,
    /// 解密的真实姓名（管理员视图，明文）
    pub real_name: Option<String>,
    pub created_at: String,
}

/// 待审批提现列表响应
#[derive(Debug, Serialize)]
pub struct PendingWithdrawalsResponse {
    pub items: Vec<PendingWithdrawalItem>,
}

/// 管理员操作响应
#[derive(Debug, Serialize)]
pub struct AdminActionResponse {
    pub id: Uuid,
    pub status: String,
    pub message: String,
}

/// 小费历史查询参数
#[derive(Debug, Deserialize)]
pub struct TipsHistoryQuery {
    /// 每页条数（默认 20，最大 100）
    pub limit: Option<i64>,
    /// 偏移量（默认 0）
    pub offset: Option<i64>,
}

/// 小费比例设置请求
#[derive(Debug, Deserialize)]
pub struct UpdateTipRatioRequest {
    /// 比例值，使用字符串以避免浮点精度问题（如 "0.90"）
    pub ratio: String,
}

/// 小费比例响应
#[derive(Debug, Serialize)]
pub struct TipRatioResponse {
    pub ratio: String,
}

// ============================================================================
// 用户端 Handler
// ============================================================================

/// 获取当前用户的小费汇总
///
/// GET /api/v1/me/tips
pub async fn get_my_tips_summary(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<TipsSummaryResponse>> {
    let pool = get_pool(&state)?;
    let summary = NodeTip::get_summary(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get tips summary: {}", e)))?;
    Ok(Json(TipsSummaryResponse::from(summary)))
}

/// 获取当前用户的小费历史记录
///
/// GET /api/v1/me/tips/history?limit=20&offset=0
pub async fn get_my_tips_history(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(query): Query<TipsHistoryQuery>,
) -> Result<Json<TipsHistoryResponse>> {
    let pool = get_pool(&state)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);

    let tips = NodeTip::list_by_user(pool, auth.user_id, limit, offset)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get tips history: {}", e)))?;

    let items: Vec<TipHistoryItem> = tips
        .into_iter()
        .map(|t| TipHistoryItem {
            id: t.id,
            tip_amount: t.tip_amount.to_string(),
            bill_amount: t.bill_amount.to_string(),
            tip_ratio: t.tip_ratio.to_string(),
            created_at: t.created_at.to_rfc3339(),
        })
        .collect();

    let total = NodeTip::count_by_user(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to count tips: {}", e)))?;

    Ok(Json(TipsHistoryResponse { items, total }))
}

/// 发起小费提现
///
/// POST /api/v1/me/tips/withdraw
///
/// 支持两种提现方式：
///   - alipay: 需要提供 alipay_account 和 real_name
///   - balance: 直接转入可用余额
pub async fn create_tip_withdrawal(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<CreateTipWithdrawalRequest>,
) -> Result<Json<WithdrawalCreatedResponse>> {
    let pool = get_pool(&state)?;

    // 校验提现方式
    let withdrawal_type = req.withdrawal_type.as_str();
    if withdrawal_type != WITHDRAWAL_TYPE_ALIPAY && withdrawal_type != WITHDRAWAL_TYPE_BALANCE {
        return Err(ApiError::BadRequest(
            "withdrawal_type must be 'alipay' or 'balance'".to_string(),
        ));
    }

    // alipay 方式需要提供支付宝账户和姓名
    if withdrawal_type == WITHDRAWAL_TYPE_ALIPAY {
        if req
            .alipay_account
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(ApiError::BadRequest(
                "alipay_account is required for alipay withdrawal".to_string(),
            ));
        }
        if req.real_name.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Err(ApiError::BadRequest(
                "real_name is required for alipay withdrawal".to_string(),
            ));
        }
    }

    // 获取用户待提现的小费汇总
    let summary = NodeTip::get_summary(pool, auth.user_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get tips summary: {}", e)))?;

    if summary.pending_amount <= Decimal::ZERO {
        return Err(ApiError::BadRequest(
            "No pending tips available for withdrawal".to_string(),
        ));
    }

    // 开始事务
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin transaction: {}", e)))?;

    // ========================================
    // 账户级并发控制：使用 PostgreSQL advisory lock 锁住用户
    // 确保同一用户在同一时间只有一个提现操作在执行
    //
    // 将 UUID 拆分为两个 i64 作为 advisory lock key，避免依赖 hashtext()
    // 的跨版本哈希稳定性。UUID 的 16 字节确定性拆分，跨 PostgreSQL 版本无兼容性风险。
    // ========================================
    let id_bytes = auth.user_id.as_bytes();
    let key1 = i64::from_be_bytes(id_bytes[..8].try_into().unwrap());
    let key2 = i64::from_be_bytes(id_bytes[8..].try_into().unwrap());
    let lock_acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1, $2)")
        .bind(key1)
        .bind(key2)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to acquire lock: {}", e)))?;

    if !lock_acquired {
        return Err(ApiError::Conflict(
            "Another withdrawal is in progress. Please try again.".to_string(),
        ));
    }

    // 事务内重新计算待提现金额（确保锁内快照一致）
    let actual_amount: Decimal = sqlx::query_scalar(
        r#"
        SELECT
            COALESCE((
                SELECT SUM(nt.tip_amount)
                FROM node_tips nt
                WHERE nt.owner_user_id = $1
            ), 0)
            - COALESCE((
                SELECT SUM(ntw.total_amount)
                FROM node_tip_withdrawals ntw
                WHERE ntw.user_id = $1 AND ntw.status != 'rejected'
            ), 0)
        "#,
    )
    .bind(auth.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to calculate pending amount: {}", e)))?;

    if actual_amount <= Decimal::ZERO {
        return Err(ApiError::BadRequest(
            "No pending tips available for withdrawal".to_string(),
        ));
    }

    // 加密 PII 敏感信息（仅 alipay 方式）
    let (encrypted_alipay, encrypted_name) = if withdrawal_type == WITHDRAWAL_TYPE_ALIPAY {
        let crypto = crypto::global_crypto()
            .ok_or_else(|| ApiError::Internal("Global crypto not initialized".to_string()))?;

        let enc_alipay = req
            .alipay_account
            .as_deref()
            .map(|acc| {
                crypto
                    .encrypt(acc)
                    .map(|k| k.as_str().to_string())
                    .map_err(|e| {
                        ApiError::Internal(format!("Failed to encrypt alipay account: {}", e))
                    })
            })
            .transpose()?;

        let enc_name = req
            .real_name
            .as_deref()
            .map(|name| {
                crypto
                    .encrypt(name)
                    .map(|k| k.as_str().to_string())
                    .map_err(|e| ApiError::Internal(format!("Failed to encrypt real name: {}", e)))
            })
            .transpose()?;

        (enc_alipay, enc_name)
    } else {
        (None, None)
    };

    // 创建提现记录（两种方式共享）
    let withdrawal = NodeTipWithdrawal::create(
        &mut tx,
        auth.user_id,
        withdrawal_type,
        actual_amount,
        encrypted_alipay.as_deref(),
        encrypted_name.as_deref(),
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to create withdrawal record: {}", e)))?;

    // 根据提现方式执行不同的后续操作
    if withdrawal_type == WITHDRAWAL_TYPE_BALANCE {
        // Balance 方式：标记提现记录为 completed（用户自助提现，无 admin 操作）
        let _ = NodeTipWithdrawal::mark_completed(
            &mut tx,
            withdrawal.id,
            None, // no admin — user self-service
            None, // preserve admin_remark
            None, // total_amount already set at creation
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to complete withdrawal: {}", e)))?;

        // 转入用户可用余额
        UserBalance::credit_tips(
            &mut tx,
            auth.user_id,
            auth.tenant_id,
            actual_amount,
            Some(&format!("Tips conversion: {} CNY", actual_amount)),
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to credit tips to balance: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to commit transaction: {}", e)))?;

        return Ok(Json(WithdrawalCreatedResponse {
            id: withdrawal.id,
            status: "completed".to_string(),
            message: format!(
                "Successfully converted {} CNY tips to your balance",
                actual_amount
            ),
        }));
    }

    // Alipay 方式：提交事务，等待管理员审批
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit transaction: {}", e)))?;

    Ok(Json(WithdrawalCreatedResponse {
        id: withdrawal.id,
        status: "pending".to_string(),
        message: "Withdrawal request submitted. Please wait for admin approval.".to_string(),
    }))
}

/// 获取当前用户的提现记录
///
/// GET /api/v1/me/tips/withdrawals
///
/// # PII 脱敏
///
/// 对支付宝账号和真实姓名进行脱敏处理，不返回明文
pub async fn get_my_withdrawals(
    auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<WithdrawalsResponse>> {
    let pool = get_pool(&state)?;
    let withdrawals = NodeTipWithdrawal::list_by_user(pool, auth.user_id, 50, 0)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get withdrawals: {}", e)))?;

    let items: Vec<WithdrawalItem> = withdrawals
        .into_iter()
        .map(|w| {
            // 对加密数据进行脱敏处理（不解密，直接返回固定格式）
            let alipay_masked = w
                .encrypted_alipay_account
                .as_ref()
                .map(|_| "****(encrypted)".to_string());
            let real_name_masked = w
                .encrypted_real_name
                .as_ref()
                .map(|_| "****(encrypted)".to_string());

            WithdrawalItem {
                id: w.id,
                withdrawal_type: w.withdrawal_type,
                total_amount: w.total_amount.to_string(),
                status: w.status,
                alipay_account_masked: alipay_masked,
                real_name_masked,
                admin_remark: w.admin_remark,
                created_at: w.created_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(WithdrawalsResponse { items }))
}

// ============================================================================
// 管理端 Handler
// ============================================================================

/// 获取待审批提现列表
///
/// GET /api/v1/admin/tips/withdrawals/pending
///
/// # PII 解密
///
/// 管理员接口需要返回解密后的明文数据，以便管理员进行线下打款操作
pub async fn admin_list_pending_withdrawals(
    _auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<PendingWithdrawalsResponse>> {
    let pool = get_pool(&state)?;
    let withdrawals = NodeTipWithdrawal::list_pending_with_users(pool, 100, 0)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to list pending withdrawals: {}", e)))?;

    let crypto = crypto::global_crypto()
        .ok_or_else(|| ApiError::Internal("Global crypto not initialized".to_string()))?;

    let items: Vec<PendingWithdrawalItem> = withdrawals
        .into_iter()
        .map(|w| {
            // 解密敏感数据（管理员有权限查看明文）
            let alipay_account = match &w.encrypted_alipay_account {
                Some(enc) => {
                    let encrypted_key = crypto::EncryptedApiKey::from(enc.clone());
                    match crypto.decrypt(&encrypted_key) {
                        Ok(plain) => Some(plain),
                        Err(e) => {
                            tracing::warn!(
                                withdrawal_id = %w.id,
                                error = %e,
                                "Failed to decrypt alipay_account"
                            );
                            Some("****(decrypt failed)".to_string())
                        }
                    }
                }
                None => None,
            };

            let real_name = match &w.encrypted_real_name {
                Some(enc) => {
                    let encrypted_key = crypto::EncryptedApiKey::from(enc.clone());
                    match crypto.decrypt(&encrypted_key) {
                        Ok(plain) => Some(plain),
                        Err(e) => {
                            tracing::warn!(
                                withdrawal_id = %w.id,
                                error = %e,
                                "Failed to decrypt real_name"
                            );
                            Some("****(decrypt failed)".to_string())
                        }
                    }
                }
                None => None,
            };

            PendingWithdrawalItem {
                id: w.id,
                user_id: w.user_id,
                user_email: w.user_email,
                withdrawal_type: w.withdrawal_type,
                total_amount: w.total_amount.to_string(),
                alipay_account,
                real_name,
                created_at: w.created_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(PendingWithdrawalsResponse { items }))
}

/// 审批提现
///
/// POST /api/v1/admin/tips/withdrawals/{id}/approve
///
/// - action=approve: 批准提现申请
/// - action=reject:  拒绝提现申请（tips 恢复为 pending 状态）
pub async fn admin_approve_withdrawal(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ApproveWithdrawalRequest>,
) -> Result<Json<AdminActionResponse>> {
    let pool = get_pool(&state)?;

    let action = req.action.as_str();
    if action != "approve" && action != "reject" {
        return Err(ApiError::BadRequest(
            "action must be 'approve' or 'reject'".to_string(),
        ));
    }

    if action == "reject" {
        // 在事务中 reject withdrawal
        // 无需恢复 tips 状态（tips 不再追踪单笔提现状态）
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to begin transaction: {}", e)))?;

        NodeTipWithdrawal::reject(&mut tx, id, auth.user_id, req.remark.as_deref())
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to reject withdrawal: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to commit transaction: {}", e)))?;

        return Ok(Json(AdminActionResponse {
            id,
            status: "rejected".to_string(),
            message: "Withdrawal rejected.".to_string(),
        }));
    }

    // approve: 批准提现
    //
    // 在事务内查询提现类型并审批，消除 TOCTOU 窗口
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin transaction: {}", e)))?;

    let withdrawal_type: String =
        sqlx::query_scalar("SELECT withdrawal_type FROM node_tip_withdrawals WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to query withdrawal: {}", e)))?
            .ok_or_else(|| ApiError::NotFound("Withdrawal not found".to_string()))?;

    // balance 方式在小费提现创建时已自动完成（自助转账），
    // 无需 admin 审批，直接返回错误提示。
    if withdrawal_type == WITHDRAWAL_TYPE_BALANCE {
        return Err(ApiError::BadRequest(
            "Balance withdrawals are auto-completed at creation time and do not require admin approval."
                .to_string(),
        ));
    }

    // alipay 方式：仅审批，管理员后续线下打款
    // approve 内部通过 WHERE status = 'pending' 做行级原子保护
    NodeTipWithdrawal::approve(&mut tx, id, auth.user_id, req.remark.as_deref())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to approve withdrawal: {}", e)))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit transaction: {}", e)))?;

    Ok(Json(AdminActionResponse {
        id,
        status: "approved".to_string(),
        message: "Withdrawal approved.".to_string(),
    }))
}

/// 完成提现（管理员线下打款后标记）
///
/// POST /api/v1/admin/tips/withdrawals/{id}/complete
///
/// 用于 alipay 方式，管理员线下打款后手动标记为已完成。
/// 同时将关联的 tips 标记为 withdrawn。
pub async fn admin_complete_withdrawal(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminActionResponse>> {
    let pool = get_pool(&state)?;

    let withdrawal = NodeTipWithdrawal::find_by_id(pool, id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to find withdrawal: {}", e)))?
        .ok_or_else(|| ApiError::NotFound("Withdrawal not found".to_string()))?;

    if withdrawal.withdrawal_type != WITHDRAWAL_TYPE_ALIPAY {
        return Err(ApiError::BadRequest(
            "Only alipay withdrawals need manual completion".to_string(),
        ));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to begin transaction: {}", e)))?;

    // 标记提现为 completed（total_amount 在创建时已设定，无需重新计算）
    NodeTipWithdrawal::mark_completed(
        &mut tx,
        id,
        Some(auth.user_id),
        None, // NULL → preserves admin_remark from approval stage
        None, // total_amount already set at creation time
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to complete withdrawal: {}", e)))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to commit transaction: {}", e)))?;

    Ok(Json(AdminActionResponse {
        id,
        status: "completed".to_string(),
        message: "Withdrawal completed. Tips have been marked as withdrawn.".to_string(),
    }))
}

/// 更新小费比例
///
/// PUT /api/v1/admin/tips/settings/ratio
pub async fn admin_update_tip_ratio(
    _auth: AuthExtractor,
    State(state): State<AppState>,
    Json(req): Json<UpdateTipRatioRequest>,
) -> Result<Json<TipRatioResponse>> {
    let pool = get_pool(&state)?;

    // 解析为 Decimal 以避免 f64 精度问题
    let ratio: Decimal = req.ratio.parse().map_err(|_| {
        ApiError::BadRequest(
            "Invalid ratio value, expected a decimal string like '0.90'".to_string(),
        )
    })?;

    if ratio <= Decimal::ZERO || ratio > Decimal::ONE {
        return Err(ApiError::BadRequest(
            "ratio must be between 0 and 1 (exclusive of 0)".to_string(),
        ));
    }

    SystemSetting::update_value(pool, setting_keys::NODE_TIP_RATIO, &ratio.to_string())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update tip ratio: {}", e)))?;

    Ok(Json(TipRatioResponse {
        ratio: ratio.to_string(),
    }))
}

/// 获取当前小费比例
///
/// GET /api/v1/admin/tips/settings/ratio
pub async fn admin_get_tip_ratio(
    _auth: AuthExtractor,
    State(state): State<AppState>,
) -> Result<Json<TipRatioResponse>> {
    let pool = get_pool(&state)?;

    // 使用 get_string + Decimal 解析，保持与写入端一致的精度路径
    let ratio_str = SystemSetting::get_string(pool, setting_keys::NODE_TIP_RATIO, "0.90").await;

    Ok(Json(TipRatioResponse { ratio: ratio_str }))
}

// ============================================================================
// 工具函数
// ============================================================================

fn get_pool(state: &AppState) -> Result<&sqlx::PgPool> {
    state
        .pool
        .as_ref()
        .map(|p| p.as_ref())
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))
}
