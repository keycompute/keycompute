//! Tenant financial commands; personal earnings never become global balances.
use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, GlobalConsoleAuth, RequestId},
    state::AppState,
    tenant_access::TenantAdmin,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::Response,
    routing::{get, post},
};
use keycompute_auth::{AuthContext, AuthorizationAction};
use keycompute_db::{
    AuditContext, NodeTip, NodeTipWithdrawal,
    models::{
        financial_scope::{FinancialMembership, FinancialScope, FinancialSession},
        node_tip_withdrawal::{
            CompleteWithdrawal, ReviewWithdrawal, WithdrawalFilter, WithdrawalIntent,
            WithdrawalReview, WithdrawalView,
        },
    },
};
use keycompute_runtime::crypto;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportQuery {
    currency: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}
impl ReportQuery {
    fn money(&self) -> &str {
        self.currency.as_deref().unwrap_or("CNY")
    }
    fn filter(&self) -> WithdrawalFilter {
        WithdrawalFilter {
            status: self.status.clone(),
            currency: Some(self.money().into()),
            limit: self.limit.unwrap_or(50),
            offset: self.offset.unwrap_or(0),
        }
    }
}
#[derive(Debug, Deserialize)]
pub struct TenantPath {
    tenant_id: Uuid,
}
#[derive(Debug, Deserialize)]
pub struct WithdrawalPath {
    tenant_id: Uuid,
    id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWithdrawalBody {
    request_id: Uuid,
    withdrawal_type: String,
    currency: String,
    alipay_account: Option<String>,
    real_name: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBody {
    expected_revision: i64,
    reason: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteBody {
    expected_revision: i64,
    reason: String,
    payout_reference: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reason {
    reason: String,
}
#[derive(Debug, Serialize)]
pub struct Page<T> {
    items: Vec<T>,
    total: i64,
}
fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Financial storage unavailable".into()))
}
fn map_db(error: keycompute_db::DbError) -> ApiError {
    if error.is_not_found() {
        return ApiError::NotFound("Financial resource not found".into());
    }
    if let keycompute_db::DbError::Other(code) = &error {
        return match code.as_str() {
            "financial_authority_invalid" => {
                ApiError::Forbidden("Current financial authority required".into())
            }
            "withdrawal_idempotency_conflict" | "withdrawal_state_conflict" => {
                ApiError::Conflict(code.clone())
            }
            "withdrawal_request_invalid"
            | "withdrawal_method_invalid"
            | "withdrawal_status_invalid"
            | "withdrawal_revision_invalid"
            | "payout_recipient_invalid"
            | "financial_reason_invalid"
            | "tip_currency_invalid"
            | "tip_page_invalid"
            | "no_withdrawable_tips" => ApiError::BadRequest(code.clone()),
            _ => ApiError::ServiceUnavailable("Financial operation unavailable".into()),
        };
    }
    // Database errors may contain statement values; do not forward their text.
    ApiError::ServiceUnavailable("Financial operation unavailable".into())
}
fn session(ctx: &AuthContext) -> Result<FinancialSession> {
    let selected = ctx
        .selected_tenant_id
        .map(|tenant_id| {
            Ok::<_, ApiError>(FinancialMembership {
                tenant_id,
                tenant_role: ctx
                    .tenant_role
                    .ok_or_else(|| ApiError::Auth("Current membership required".into()))?,
                tenant_authz_version: ctx
                    .authz_version
                    .ok_or_else(|| ApiError::Auth("Current tenant version required".into()))?,
                membership_authz_version: ctx
                    .membership_authz_version
                    .ok_or_else(|| ApiError::Auth("Current membership version required".into()))?,
            })
        })
        .transpose()?;
    Ok(FinancialSession {
        user_id: ctx.user_id,
        credential_kind: ctx.credential_kind,
        token_version: ctx.token_version,
        expires_at: ctx
            .credential_expires_at
            .ok_or_else(|| ApiError::Auth("Expiring console session required".into()))?,
        selected,
    })
}
fn personal(auth: &ConsoleAuth, action: AuthorizationAction) -> Result<FinancialScope> {
    let scope = auth.require_owner(auth.user_id, action)?;
    FinancialScope::personal(scope, session(&auth.authorization_context())?).map_err(map_db)
}
fn tenant(access: &TenantAdmin, id: Uuid) -> Result<FinancialScope> {
    access.require_path_tenant(id)?;
    FinancialScope::tenant_admin(
        access.require(AuthorizationAction::ManageTenantResource)?,
        session(&access.auth().authorization_context())?,
    )
    .map_err(map_db)
}
fn root(auth: &GlobalConsoleAuth, id: Uuid) -> Result<FinancialScope> {
    FinancialScope::platform_tenant(
        auth.require_platform(AuthorizationAction::ManagePlatform)?,
        session(&auth.0)?,
        id,
    )
    .map_err(map_db)
}
fn actor(ctx: &AuthContext, id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: ctx.user_id,
        credential_kind: ctx.credential_kind,
        actor_platform_role: ctx.platform_role,
        actor_tenant_role: ctx.tenant_role,
        request_id: Some(id.0),
    }
}
pub async fn get_my_tips_summary(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>> {
    let scope = personal(&auth, AuthorizationAction::ReadPersonalResource)?;
    let row = NodeTip::summary(pool(&state)?.write_conn(), scope, q.money())
        .await
        .map_err(map_db)?;
    Ok(Json(
        json!({"tenant_id":auth.tenant_id,"currency":q.money().trim().to_ascii_uppercase(),"pending_amount":row.pending_amount.to_string(),"reserved_amount":row.reserved_amount.to_string(),"withdrawn_amount":row.withdrawn_amount.to_string(),"total_amount":row.total_amount.to_string(),"pending_count":row.pending_count}),
    ))
}
pub async fn get_my_tips_history(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Page<NodeTip>>> {
    let scope = personal(&auth, AuthorizationAction::ReadPersonalResource)?;
    let db = pool(&state)?.write_conn();
    let items = NodeTip::list_in_scope(
        db,
        scope,
        q.money(),
        q.limit.unwrap_or(20),
        q.offset.unwrap_or(0),
    )
    .await
    .map_err(map_db)?;
    let total = NodeTip::count_in_scope(db, scope, q.money())
        .await
        .map_err(map_db)?;
    Ok(Json(Page { items, total }))
}
pub async fn get_my_withdrawals(
    auth: ConsoleAuth,
    State(state): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Page<WithdrawalView>>> {
    withdrawals(
        &state,
        personal(&auth, AuthorizationAction::ReadPersonalResource)?,
        q,
    )
    .await
}
async fn withdrawals(
    state: &AppState,
    scope: FinancialScope,
    q: ReportQuery,
) -> Result<Json<Page<WithdrawalView>>> {
    let db = pool(state)?.write_conn();
    let filter = q.filter();
    let items = NodeTipWithdrawal::list_in_scope(db, scope, &filter)
        .await
        .map_err(map_db)?;
    let total = NodeTipWithdrawal::count_in_scope(db, scope, &filter)
        .await
        .map_err(map_db)?;
    Ok(Json(Page { items, total }))
}
pub async fn create_tip_withdrawal(
    auth: ConsoleAuth,
    id: RequestId,
    State(state): State<AppState>,
    Json(body): Json<CreateWithdrawalBody>,
) -> Result<Json<Value>> {
    let scope = personal(&auth, AuthorizationAction::ManagePersonalResource)?;
    let recipient = NodeTipWithdrawal::recipient_fingerprint(
        &body.withdrawal_type,
        body.alipay_account.as_deref(),
        body.real_name.as_deref(),
    )
    .map_err(map_db)?;
    let encrypted = if body.withdrawal_type == "alipay" {
        let crypto = crypto::global_crypto()
            .ok_or_else(|| ApiError::ServiceUnavailable("Payout encryption unavailable".into()))?;
        let encrypt = |value: Option<&str>| -> Result<Option<String>> {
            value
                .map(|value| {
                    crypto
                        .encrypt(value.trim())
                        .map(|v| v.as_str().to_owned())
                        .map_err(|_| {
                            ApiError::ServiceUnavailable("Payout encryption unavailable".into())
                        })
                })
                .transpose()
        };
        (
            encrypt(body.alipay_account.as_deref())?,
            encrypt(body.real_name.as_deref())?,
        )
    } else {
        (None, None)
    };
    let spec = WithdrawalIntent {
        request_id: body.request_id,
        withdrawal_type: body.withdrawal_type,
        currency: body.currency,
        recipient_fingerprint: recipient,
        encrypted_alipay_account: encrypted.0,
        encrypted_real_name: encrypted.1,
    };
    let row = NodeTipWithdrawal::create(
        pool(&state)?.write_conn(),
        scope,
        &actor(&auth.authorization_context(), id),
        &spec,
    )
    .await
    .map_err(map_db)?;
    let mut value = serde_json::to_value(&row)
        .map_err(|_| ApiError::Internal("Financial response encoding unavailable".into()))?;
    value["message"] = if row.status == "completed" {
        "Earnings converted to the original tenant balance"
    } else {
        "Withdrawal recorded for tenant review"
    }
    .into();
    Ok(Json(value))
}
pub async fn tenant_withdrawals(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Page<WithdrawalView>>> {
    withdrawals(&state, tenant(&access, path.tenant_id)?, q).await
}
pub async fn platform_withdrawals(
    auth: GlobalConsoleAuth,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Page<WithdrawalView>>> {
    withdrawals(&state, root(&auth, path.tenant_id)?, q).await
}
async fn review(
    state: &AppState,
    scope: FinancialScope,
    audit: AuditContext,
    path: WithdrawalPath,
    body: ReviewBody,
    action: WithdrawalReview,
) -> Result<Json<WithdrawalView>> {
    NodeTipWithdrawal::review(
        pool(state)?.write_conn(),
        scope,
        &audit,
        &ReviewWithdrawal {
            id: path.id,
            expected_revision: body.expected_revision,
            action,
            reason: body.reason,
        },
    )
    .await
    .map(Json)
    .map_err(map_db)
}
pub async fn tenant_approve(
    access: TenantAdmin,
    id: RequestId,
    Path(path): Path<WithdrawalPath>,
    State(state): State<AppState>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<WithdrawalView>> {
    review(
        &state,
        tenant(&access, path.tenant_id)?,
        access.audit(id),
        path,
        body,
        WithdrawalReview::Approve,
    )
    .await
}
pub async fn tenant_reject(
    access: TenantAdmin,
    id: RequestId,
    Path(path): Path<WithdrawalPath>,
    State(state): State<AppState>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<WithdrawalView>> {
    review(
        &state,
        tenant(&access, path.tenant_id)?,
        access.audit(id),
        path,
        body,
        WithdrawalReview::Reject,
    )
    .await
}
pub async fn platform_approve(
    auth: GlobalConsoleAuth,
    id: RequestId,
    Path(path): Path<WithdrawalPath>,
    State(state): State<AppState>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<WithdrawalView>> {
    review(
        &state,
        root(&auth, path.tenant_id)?,
        actor(&auth.0, id),
        path,
        body,
        WithdrawalReview::Approve,
    )
    .await
}
pub async fn platform_reject(
    auth: GlobalConsoleAuth,
    id: RequestId,
    Path(path): Path<WithdrawalPath>,
    State(state): State<AppState>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<WithdrawalView>> {
    review(
        &state,
        root(&auth, path.tenant_id)?,
        actor(&auth.0, id),
        path,
        body,
        WithdrawalReview::Reject,
    )
    .await
}
pub async fn platform_complete(
    auth: GlobalConsoleAuth,
    id: RequestId,
    Path(path): Path<WithdrawalPath>,
    State(state): State<AppState>,
    Json(body): Json<CompleteBody>,
) -> Result<Json<WithdrawalView>> {
    NodeTipWithdrawal::complete_external(
        pool(&state)?.write_conn(),
        root(&auth, path.tenant_id)?,
        &actor(&auth.0, id),
        &CompleteWithdrawal {
            id: path.id,
            expected_revision: body.expected_revision,
            reason: body.reason,
            payout_reference: body.payout_reference,
        },
    )
    .await
    .map(Json)
    .map_err(map_db)
}
pub async fn platform_payout_detail(
    auth: GlobalConsoleAuth,
    id: RequestId,
    Path(path): Path<WithdrawalPath>,
    State(state): State<AppState>,
    Query(q): Query<Reason>,
) -> Result<Json<Value>> {
    let secret = NodeTipWithdrawal::support_payout(
        pool(&state)?.write_conn(),
        root(&auth, path.tenant_id)?,
        &actor(&auth.0, id),
        path.id,
        &q.reason,
    )
    .await
    .map_err(map_db)?;
    let crypto = crypto::global_crypto()
        .ok_or_else(|| ApiError::ServiceUnavailable("Payout decryption unavailable".into()))?;
    let decrypt = |cipher: String| {
        crypto
            .decrypt(&crypto::EncryptedApiKey::from(cipher))
            .map_err(|_| ApiError::ServiceUnavailable("Payout decryption unavailable".into()))
    };
    Ok(Json(
        json!({"alipay_account":decrypt(secret.alipay_account)?,"real_name":decrypt(secret.real_name)?}),
    ))
}
pub async fn tenant_earnings(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>> {
    let scope = tenant(&access, path.tenant_id)?;
    let row = NodeTip::summary(pool(&state)?.write_conn(), scope, q.money())
        .await
        .map_err(map_db)?;
    Ok(Json(
        json!({"tenant_id":path.tenant_id,"currency":q.money().trim().to_ascii_uppercase(),"summary":row}),
    ))
}
pub async fn tenant_earnings_history(
    access: TenantAdmin,
    Path(path): Path<TenantPath>,
    State(state): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Page<NodeTip>>> {
    let scope = tenant(&access, path.tenant_id)?;
    let db = pool(&state)?.write_conn();
    let items = NodeTip::list_in_scope(
        db,
        scope,
        q.money(),
        q.limit.unwrap_or(20),
        q.offset.unwrap_or(0),
    )
    .await
    .map_err(map_db)?;
    let total = NodeTip::count_in_scope(db, scope, q.money())
        .await
        .map_err(map_db)?;
    Ok(Json(Page { items, total }))
}

async fn private_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        "private, no-store".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert(axum::http::header::PRAGMA, "no-cache".parse().unwrap());
    response
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tenants/{tenant_id}/tips", get(tenant_earnings))
        .route(
            "/api/v1/tenants/{tenant_id}/tips/history",
            get(tenant_earnings_history),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/tips/withdrawals",
            get(tenant_withdrawals),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/tips/withdrawals/{id}/approve",
            post(tenant_approve),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/tips/withdrawals/{id}/reject",
            post(tenant_reject),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tips/withdrawals",
            get(platform_withdrawals),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tips/withdrawals/{id}/approve",
            post(platform_approve),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tips/withdrawals/{id}/reject",
            post(platform_reject),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tips/withdrawals/{id}/complete",
            post(platform_complete),
        )
        .route(
            "/api/v1/platform/tenants/{tenant_id}/tips/withdrawals/{id}/support-detail",
            get(platform_payout_detail),
        )
        .layer(axum::middleware::map_response(private_response))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
}
