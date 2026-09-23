//! Dedicated root policy management, independent of selected tenant membership.
use crate::{
    error::{ApiError, Result},
    extractors::{GlobalConsoleAuth, RequestId},
    state::AppState,
};
use axum::{Json, Router, extract::State, response::Response, routing::get};
use chrono::{DateTime, Utc};
use keycompute_db::models::node_tip_setting::{TipRatioSetting, UpdateTipRatio};
use serde::Deserialize;
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTipRatioRequest {
    pub ratio: String,
    pub expected_updated_at: DateTime<Utc>,
    pub reason: String,
}
fn map_error(error: keycompute_db::DbError) -> ApiError {
    match error {
        keycompute_db::DbError::Other(code) if code == "financial_authority_invalid" => {
            ApiError::Forbidden("Current root policy authority required".into())
        }
        keycompute_db::DbError::Other(code) if code == "tip_ratio_revision_conflict" => {
            ApiError::Conflict(code)
        }
        keycompute_db::DbError::Other(code)
            if matches!(
                code.as_str(),
                "tip_ratio_invalid" | "financial_reason_invalid"
            ) =>
        {
            ApiError::BadRequest(code)
        }
        _ => ApiError::ServiceUnavailable("Node earnings policy unavailable".into()),
    }
}
fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Policy storage unavailable".into()))
}
pub async fn admin_get_tip_ratio(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
) -> Result<Json<TipRatioSetting>> {
    TipRatioSetting::read(
        pool(&state)?.write_conn(),
        crate::financial_auth::global_scope(&auth.0)?,
    )
    .await
    .map(Json)
    .map_err(map_error)
}
pub async fn admin_update_tip_ratio(
    auth: GlobalConsoleAuth,
    id: RequestId,
    State(state): State<AppState>,
    Json(body): Json<UpdateTipRatioRequest>,
) -> Result<Json<TipRatioSetting>> {
    let scope = crate::financial_auth::global_scope(&auth.0)?;
    let _fence = state.display_cache.mutation_guard();
    TipRatioSetting::update(
        pool(&state)?.write_conn(),
        scope,
        &crate::financial_auth::audit(&auth.0, id),
        &UpdateTipRatio {
            ratio: body.ratio,
            expected_updated_at: body.expected_updated_at,
            reason: body.reason,
        },
    )
    .await
    .map(Json)
    .map_err(map_error)
}
async fn private_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        "private, no-store".parse().unwrap(),
    );
    response
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/platform/tips/settings/ratio",
            get(admin_get_tip_ratio).put(admin_update_tip_ratio),
        )
        .layer(axum::middleware::map_response(private_response))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
}
