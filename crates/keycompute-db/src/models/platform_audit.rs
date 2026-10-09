//! Read-only, cross-tenant audit projection for verified platform operators.
//!
//! This scope never grants tenant membership. Every query revalidates the
//! current actor role, token version and session expiry in PostgreSQL before
//! returning immutable audit records.

use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct PlatformAuditSession {
    pub credential_kind: CredentialKind,
    pub token_version: i32,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct PlatformAuditScope {
    platform: PlatformScope,
    session: PlatformAuditSession,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlatformAuditFilter {
    pub tenant_id: Option<Uuid>,
    pub request_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformAuditCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct PlatformAuditRecord {
    pub id: Uuid,
    pub scope_type: String,
    pub tenant_id: Option<Uuid>,
    pub tenant_name: Option<String>,
    pub tenant_slug: Option<String>,
    pub actor_user_id: Uuid,
    pub actor_email: Option<String>,
    pub actor_name: Option<String>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub request_id: Option<Uuid>,
    pub request_event_count: i64,
    pub credential_kind: String,
    pub platform_role: String,
    pub tenant_role: Option<String>,
    pub metadata: serde_json::Value,
    pub result: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct PageRow {
    items: serde_json::Value,
}

fn denied() -> DbError {
    DbError::Other("platform_audit_authority_invalid".into())
}

fn invalid() -> DbError {
    DbError::Other("platform_audit_query_invalid".into())
}

const AUTHORITY: &str = "EXISTS(SELECT 1 FROM users audit_actor WHERE audit_actor.id=$1 AND audit_actor.platform_role=$2 AND audit_actor.platform_role IN ('root','operator') AND audit_actor.status='active' AND audit_actor.token_version=$3 AND clock_timestamp()<to_timestamp($4::double precision))";

impl PlatformAuditScope {
    pub fn checked(
        platform: PlatformScope,
        session: PlatformAuditSession,
    ) -> Result<Self, DbError> {
        if platform.user_id().is_nil()
            || !matches!(
                platform.platform_role(),
                PlatformRole::Root | PlatformRole::Operator
            )
            || session.credential_kind != CredentialKind::Jwt
            || session.token_version < 0
            || session.expires_at <= Utc::now().timestamp()
        {
            return Err(denied());
        }
        Ok(Self { platform, session })
    }

    fn authority_values(self) -> Vec<Value> {
        vec![
            self.platform.user_id().into(),
            self.platform.platform_role().as_str().into(),
            self.session.token_version.into(),
            self.session.expires_at.into(),
        ]
    }

    fn query_values(
        self,
        filter: PlatformAuditFilter,
        cursor: Option<PlatformAuditCursor>,
        limit: i64,
    ) -> Result<Vec<Value>, DbError> {
        if filter.tenant_id.is_some_and(|id| id.is_nil())
            || filter.request_id.is_some_and(|id| id.is_nil())
            || cursor.is_some_and(|value| value.id.is_nil())
            || !(1..=100).contains(&limit)
        {
            return Err(invalid());
        }
        let mut values = self.authority_values();
        values.extend([
            filter.tenant_id.into(),
            filter.request_id.into(),
            cursor.map(|value| value.created_at).into(),
            cursor.map(|value| value.id).into(),
            (limit + 1).into(),
        ]);
        Ok(values)
    }

    /// Read a stable keyset page while revalidating the platform authority in
    /// the same PostgreSQL statement. An absent row means that authority was
    /// revoked; an authorized empty result is represented by an empty JSON
    /// array, so those states cannot be confused.
    pub async fn page(
        self,
        db: &impl ConnectionTrait,
        filter: PlatformAuditFilter,
        limit: i64,
        cursor: Option<PlatformAuditCursor>,
    ) -> Result<(Vec<PlatformAuditRecord>, Option<PlatformAuditCursor>), DbError> {
        let sql = format!(
            "WITH authority AS MATERIALIZED (SELECT 1 AS granted WHERE {AUTHORITY})
             SELECT COALESCE(
                 jsonb_agg(to_jsonb(entry) ORDER BY entry.created_at DESC,entry.id DESC)
                     FILTER (WHERE entry.id IS NOT NULL),
                 '[]'::jsonb
             ) AS items
             FROM authority
             LEFT JOIN LATERAL (
                 SELECT a.id,a.scope_type,a.tenant_id,t.name AS tenant_name,t.slug AS tenant_slug,
                        a.actor_user_id,u.email AS actor_email,u.name AS actor_name,
                        a.action,a.resource_type,a.resource_id,a.request_id,
                        CASE WHEN a.request_id IS NULL THEN 0 ELSE
                            (SELECT COUNT(*)::bigint FROM tenant_audit_events related
                             WHERE related.request_id=a.request_id)
                        END AS request_event_count,
                        a.credential_kind,
                        a.platform_role,a.tenant_role,a.metadata,a.result,a.created_at
                 FROM tenant_audit_events a
                 LEFT JOIN tenants t ON t.id=a.tenant_id
                 LEFT JOIN users u ON u.id=a.actor_user_id
                 WHERE ($5::uuid IS NULL OR a.tenant_id=$5)
                   AND ($6::uuid IS NULL OR a.request_id=$6)
                   AND ($7::timestamptz IS NULL OR (a.created_at,a.id)<($7::timestamptz,$8::uuid))
                 ORDER BY a.created_at DESC,a.id DESC
                 LIMIT $9
             ) entry ON TRUE
             GROUP BY authority.granted"
        );
        let row = PageRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            self.query_values(filter, cursor, limit)?,
        ))
        .one(db)
        .await?
        .ok_or_else(denied)?;
        let mut items: Vec<PlatformAuditRecord> =
            serde_json::from_value(row.items).map_err(|error| DbError::Other(error.to_string()))?;
        let has_more = items.len() > limit as usize;
        if has_more {
            items.truncate(limit as usize);
        }
        let next_cursor = has_more.then(|| {
            let last = items
                .last()
                .expect("nonempty truncated platform audit page");
            PlatformAuditCursor {
                created_at: last.created_at,
                id: last.id,
            }
        });
        Ok((items, next_cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_requires_live_jwt_platform_identity_and_real_filters() {
        let actor = Uuid::new_v4();
        let session = PlatformAuditSession {
            credential_kind: CredentialKind::Jwt,
            token_version: 0,
            expires_at: Utc::now().timestamp() + 60,
        };
        for role in [PlatformRole::Root, PlatformRole::Operator] {
            let scope =
                PlatformAuditScope::checked(PlatformScope::checked(actor, role).unwrap(), session)
                    .unwrap();
            assert!(
                scope
                    .query_values(PlatformAuditFilter::default(), None, 20)
                    .is_ok()
            );
            assert!(
                scope
                    .query_values(
                        PlatformAuditFilter {
                            tenant_id: Some(Uuid::nil()),
                            request_id: None,
                        },
                        None,
                        20,
                    )
                    .is_err()
            );
            assert!(
                scope
                    .query_values(
                        PlatformAuditFilter::default(),
                        Some(PlatformAuditCursor {
                            created_at: Utc::now(),
                            id: Uuid::nil(),
                        }),
                        20,
                    )
                    .is_err()
            );
            assert!(
                scope
                    .query_values(PlatformAuditFilter::default(), None, 101)
                    .is_err()
            );
        }
        let mut invalid = session;
        invalid.credential_kind = CredentialKind::ApiKey;
        assert!(
            PlatformAuditScope::checked(
                PlatformScope::checked(actor, PlatformRole::Operator).unwrap(),
                invalid,
            )
            .is_err()
        );
    }
}
