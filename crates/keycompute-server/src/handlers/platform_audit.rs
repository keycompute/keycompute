//! Immutable platform-wide audit timeline for root and operator identities.

use crate::{
    error::{ApiError, Result},
    extractors::GlobalConsoleAuth,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    response::Response,
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{
    PlatformAuditCursor, PlatformAuditFilter, PlatformAuditRecord, PlatformAuditScope,
    PlatformAuditSession,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const DEFAULT_PAGE_SIZE: i64 = 20;
const MAX_CURSOR_LENGTH: usize = 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformAuditQuery {
    pub tenant_id: Option<Uuid>,
    pub request_id: Option<Uuid>,
    pub cursor: Option<String>,
    pub page_size: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PlatformAuditPage {
    pub items: Vec<PlatformAuditRecord>,
    pub next_cursor: Option<String>,
    pub page_size: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorEnvelope {
    version: u8,
    created_at: chrono::DateTime<chrono::Utc>,
    id: Uuid,
    tenant_id: Option<Uuid>,
    request_id: Option<Uuid>,
    page_size: i64,
}

fn encode_cursor(
    cursor: PlatformAuditCursor,
    filter: PlatformAuditFilter,
    page_size: i64,
) -> String {
    URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&CursorEnvelope {
            version: 1,
            created_at: cursor.created_at,
            id: cursor.id,
            tenant_id: filter.tenant_id,
            request_id: filter.request_id,
            page_size,
        })
        .expect("platform audit cursor serialization"),
    )
}

fn decode_cursor(
    value: &str,
    filter: PlatformAuditFilter,
    page_size: i64,
) -> Result<PlatformAuditCursor> {
    if value.is_empty() || value.len() > MAX_CURSOR_LENGTH {
        return Err(ApiError::BadRequest("Invalid platform audit cursor".into()));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::BadRequest("Invalid platform audit cursor".into()))?;
    let cursor: CursorEnvelope = serde_json::from_slice(&decoded)
        .map_err(|_| ApiError::BadRequest("Invalid platform audit cursor".into()))?;
    if cursor.version != 1
        || cursor.id.is_nil()
        || cursor.tenant_id != filter.tenant_id
        || cursor.request_id != filter.request_id
        || cursor.page_size != page_size
    {
        return Err(ApiError::BadRequest("Invalid platform audit cursor".into()));
    }
    Ok(PlatformAuditCursor {
        created_at: cursor.created_at,
        id: cursor.id,
    })
}

fn normalized_query(
    query: PlatformAuditQuery,
) -> Result<(PlatformAuditFilter, i64, Option<PlatformAuditCursor>)> {
    if query.tenant_id.is_some_and(|id| id.is_nil())
        || query.request_id.is_some_and(|id| id.is_nil())
        || query
            .page_size
            .is_some_and(|page_size| !(1..=100).contains(&page_size))
    {
        return Err(ApiError::BadRequest("Invalid platform audit query".into()));
    }
    let filter = PlatformAuditFilter {
        tenant_id: query.tenant_id,
        request_id: query.request_id,
    };
    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    let cursor = query
        .cursor
        .as_deref()
        .map(|value| decode_cursor(value, filter, page_size))
        .transpose()?;
    Ok((filter, page_size, cursor))
}

fn map_error(error: keycompute_db::DbError) -> ApiError {
    match error {
        keycompute_db::DbError::Other(code) if code == "platform_audit_authority_invalid" => {
            ApiError::Forbidden("Current platform audit authority required".into())
        }
        keycompute_db::DbError::Other(code) if code == "platform_audit_query_invalid" => {
            ApiError::BadRequest("Invalid platform audit query".into())
        }
        error => {
            tracing::error!(%error, "platform audit query failed");
            ApiError::ServiceUnavailable("Platform audit log is temporarily unavailable".into())
        }
    }
}

fn scope(auth: &GlobalConsoleAuth) -> Result<PlatformAuditScope> {
    PlatformAuditScope::checked(
        auth.require_platform(AuthorizationAction::ReadPlatformAudit)?,
        PlatformAuditSession {
            credential_kind: auth.credential_kind,
            token_version: auth.token_version,
            expires_at: auth
                .credential_expires_at
                .ok_or_else(|| ApiError::Auth("Expiring console session required".into()))?,
        },
    )
    .map_err(map_error)
}

pub async fn list_platform_audit_events(
    auth: GlobalConsoleAuth,
    State(state): State<AppState>,
    Query(query): Query<PlatformAuditQuery>,
) -> Result<Json<PlatformAuditPage>> {
    let (filter, page_size, cursor) = normalized_query(query)?;
    let db = state
        .pool
        .as_deref()
        .ok_or_else(|| {
            ApiError::ServiceUnavailable("Platform audit log is temporarily unavailable".into())
        })?
        .write_conn();
    let access = scope(&auth)?;
    let (items, next_cursor) = access
        .page(db, filter, page_size, cursor)
        .await
        .map_err(map_error)?;
    Ok(Json(PlatformAuditPage {
        items,
        next_cursor: next_cursor.map(|cursor| encode_cursor(cursor, filter, page_size)),
        page_size,
    }))
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
            "/api/v1/platform/audit-events",
            get(list_platform_audit_events),
        )
        .layer(axum::middleware::map_response(private_response))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_validation_is_exact_and_never_silently_clamps() {
        let tenant_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let (filter, page_size, cursor) = normalized_query(PlatformAuditQuery {
            tenant_id: Some(tenant_id),
            request_id: Some(request_id),
            cursor: None,
            page_size: Some(25),
        })
        .unwrap();
        assert_eq!(filter.tenant_id, Some(tenant_id));
        assert_eq!(filter.request_id, Some(request_id));
        assert_eq!(page_size, 25);
        assert_eq!(cursor, None);

        let expected = PlatformAuditCursor {
            created_at: chrono::Utc::now(),
            id: Uuid::new_v4(),
        };
        let encoded = encode_cursor(expected, filter, page_size);
        let (_, _, decoded) = normalized_query(PlatformAuditQuery {
            tenant_id: Some(tenant_id),
            request_id: Some(request_id),
            cursor: Some(encoded.clone()),
            page_size: Some(page_size),
        })
        .unwrap();
        assert_eq!(decoded, Some(expected));

        for query in [
            PlatformAuditQuery {
                tenant_id: Some(Uuid::nil()),
                ..Default::default()
            },
            PlatformAuditQuery {
                request_id: Some(Uuid::nil()),
                ..Default::default()
            },
            PlatformAuditQuery {
                cursor: Some("not-a-cursor".into()),
                ..Default::default()
            },
            PlatformAuditQuery {
                page_size: Some(101),
                ..Default::default()
            },
        ] {
            assert!(normalized_query(query).is_err());
        }

        assert!(
            normalized_query(PlatformAuditQuery {
                tenant_id: None,
                request_id: Some(request_id),
                cursor: Some(encoded),
                page_size: Some(page_size),
            })
            .is_err()
        );
    }
}
