//! Compact, permission-checked display endpoints. No financial command uses these snapshots.
use crate::{ApiError, AppState, Result, display_cache::DisplayCache, extractors::AuthExtractor};
use axum::{
    Json,
    extract::{Query, State},
};
use chrono::{DateTime, Duration, Utc};
use keycompute_auth::Permission;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TrendQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub granularity: Option<String>,
}
impl TrendQuery {
    pub fn interval(self) -> Result<(DateTime<Utc>, DateTime<Utc>, String)> {
        let grain = self.granularity.unwrap_or_else(|| "day".into());
        let (step, max_days) = match grain.as_str() {
            "day" => (86400, 365),
            "hour" => (3600, 6),
            _ => {
                return Err(ApiError::BadRequest(
                    "granularity must be day or hour".into(),
                ));
            }
        };
        let midnight = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("valid midnight")
            .and_utc();
        let to = self.to.unwrap_or(midnight + Duration::days(1));
        let from = match self.from {
            Some(from) => from,
            None => to
                .checked_sub_signed(Duration::days(if grain == "hour" { 1 } else { 7 }))
                .ok_or_else(|| ApiError::BadRequest("trend date underflow".into()))?,
        };
        if from >= to
            || to - from > Duration::days(max_days)
            || (to.timestamp() / step - from.timestamp() / step) > 365
        {
            return Err(ApiError::BadRequest(
                "trend interval is empty or exceeds the day/hour range limit".into(),
            ));
        }
        Ok((from, to, grain))
    }
}
fn require(auth: &AuthExtractor, permission: Permission) -> Result<()> {
    if auth.has_permission(&permission) {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "Console display permission required".into(),
        ))
    }
}
fn db_error(error: impl std::fmt::Display) -> ApiError {
    tracing::warn!(%error,"console display query failed");
    ApiError::Internal("Display data could not be loaded".into())
}

pub async fn usage_trend(
    auth: AuthExtractor,
    State(state): State<AppState>,
    Query(query): Query<TrendQuery>,
) -> Result<Json<Value>> {
    require(&auth, Permission::ViewUsage)?;
    let (from, to, grain) = query.interval()?;
    let pool = state
        .pool
        .clone()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let key = DisplayCache::key(&auth, "usage-trend", &format!("{from}/{to}/{grain}"));
    let user = auth.user_id;
    let value = state
        .display_cache
        .read(
            state.cache.clone(),
            state.console_admission.origin.clone(),
            auth.tenant_id,
            key,
            async move {
                keycompute_db::models::console_display::usage_trend(
                    pool.write_conn(),
                    user,
                    from,
                    to,
                    &grain,
                )
                .await
                .map_err(db_error)
            },
        )
        .await?;
    Ok(Json(value))
}

pub async fn dashboard(auth: AuthExtractor, State(state): State<AppState>) -> Result<Json<Value>> {
    require(&auth, Permission::ViewUsage)?;
    require(&auth, Permission::ManageOwnBilling)?;
    let pool = state
        .pool
        .clone()
        .ok_or_else(|| ApiError::Internal("Database not configured".into()))?;
    let (from, to, grain) = TrendQuery::default().interval()?;
    let key = DisplayCache::key(&auth, "dashboard", &format!("{from}/{to}"));
    let user = auth.user_id;
    let tenant = auth.tenant_id;
    let value = state
        .display_cache
        .read(
            state.cache.clone(),
            state.console_admission.origin.clone(),
            tenant,
            key,
            async move {
                let mut value = keycompute_db::models::console_display::dashboard(
                    pool.write_conn(),
                    user,
                    tenant,
                )
                .await
                .map_err(db_error)?;
                value["trend"] = keycompute_db::models::console_display::usage_trend(
                    pool.write_conn(),
                    user,
                    from,
                    to,
                    &grain,
                )
                .await
                .map_err(db_error)?;
                Ok(value)
            },
        )
        .await?;
    Ok(Json(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trend_query_rejects_invalid_ranges_and_unbounded_grains() {
        for text in [
            r#"{"granularity":"minute"}"#,
            r#"{"from":"2026-01-01T00:00:00Z","to":"2025-01-01T00:00:00Z"}"#,
            r#"{"granularity":"hour","from":"2026-01-01T00:00:00Z","to":"2026-02-01T00:00:00Z"}"#,
        ] {
            let query: TrendQuery = serde_json::from_str(text).unwrap();
            assert!(query.interval().is_err());
        }
        let (from, to, grain) = TrendQuery::default().interval().unwrap();
        assert_eq!(grain, "day");
        assert_eq!(to - from, Duration::days(7));
    }
    #[tokio::test]
    async fn role_names_without_console_permissions_do_not_reach_the_database() {
        let state = AppState::new();
        let id = uuid::Uuid::new_v4();
        let auth =
            AuthExtractor::new(id, id, id, "system").with_permissions(vec![Permission::UseApi]);
        assert!(matches!(
            dashboard(auth.clone(), State(state.clone())).await,
            Err(ApiError::Forbidden(_))
        ));
        assert!(matches!(
            usage_trend(auth, State(state), Query(TrendQuery::default())).await,
            Err(ApiError::Forbidden(_))
        ));
    }
}
