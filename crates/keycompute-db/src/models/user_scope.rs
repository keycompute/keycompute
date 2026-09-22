//! Explicit control-plane scopes. Global identity lookup is not membership.
use super::*;
use keycompute_types::{MembershipStatus, PlatformScope, TenantRole, TenantScope};

/// Tenant-facing projection deliberately omits global roles and token versions.
#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TenantMemberRecord {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub tenant_role: String,
    pub status: String,
    pub user_status: String,
    pub invited_by: Option<Uuid>,
    pub joined_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    pub authz_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn require_platform_root(scope: PlatformScope) -> Result<(), DbError> {
    if scope.platform_role() != PlatformRole::Root {
        return Err(DbError::Other(
            "root platform identity scope required".into(),
        ));
    }
    Ok(())
}

fn platform_query(
    scope: PlatformScope,
    select: &str,
    role: Option<PlatformRole>,
    search: Option<&str>,
    page: Option<(i64, i64)>,
    id: Option<Uuid>,
) -> Result<Statement, DbError> {
    require_platform_root(scope)?;
    let search = search
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(escape_like_pattern);
    let mut sql = format!("SELECT {select} FROM users u WHERE
        EXISTS (SELECT 1 FROM users actor WHERE actor.id=$1 AND actor.platform_role='root' AND actor.status='active')
        AND ($2::text IS NULL OR u.platform_role=$2)
        AND ($3::text IS NULL OR u.email ILIKE '%'||$3||'%' ESCAPE '\\' OR COALESCE(u.name,'') ILIKE '%'||$3||'%' ESCAPE '\\')
        AND ($4::uuid IS NULL OR u.id=$4)");
    let mut values = vec![
        scope.user_id().into(),
        role.map(|r| r.as_str()).into(),
        search.into(),
        id.into(),
    ];
    if let Some((limit, offset)) = page {
        sql.push_str(" ORDER BY u.created_at DESC,u.id DESC LIMIT $5 OFFSET $6");
        values.extend([limit.clamp(1, 1000).into(), offset.max(0).into()]);
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}

const MEMBER_COLUMNS: &str = "m.tenant_id,m.user_id,u.email,u.name,m.tenant_role,m.status,u.status AS user_status,m.invited_by,m.joined_at,m.removed_at,m.authz_version,m.created_at,m.updated_at";

fn member_query(
    scope: TenantScope,
    select: &str,
    status: Option<MembershipStatus>,
    search: Option<&str>,
    page: Option<(i64, i64)>,
    user_id: Option<Uuid>,
) -> Result<Statement, DbError> {
    if scope.tenant_role() != TenantRole::Admin {
        return Err(DbError::Other("tenant administrator scope required".into()));
    }
    let search = search
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(escape_like_pattern);
    let mut sql = format!("SELECT {select} FROM tenant_memberships m JOIN users u ON u.id=m.user_id
        WHERE m.tenant_id=$1 AND EXISTS (
            SELECT 1 FROM tenant_memberships actor JOIN users au ON au.id=actor.user_id JOIN tenants t ON t.id=actor.tenant_id
            WHERE actor.tenant_id=m.tenant_id AND actor.user_id=$2 AND actor.tenant_role='admin'
              AND actor.status='active' AND au.status='active' AND t.status='active')
        AND ($3::text IS NULL OR m.status=$3)
        AND ($4::text IS NULL OR u.email ILIKE '%'||$4||'%' ESCAPE '\\' OR COALESCE(u.name,'') ILIKE '%'||$4||'%' ESCAPE '\\')
        AND ($5::uuid IS NULL OR m.user_id=$5)");
    let mut values = vec![
        scope.tenant_id().into(),
        scope.user_id().into(),
        status.map(|s| s.as_str()).into(),
        search.into(),
        user_id.into(),
    ];
    if let Some((limit, offset)) = page {
        sql.push_str(" ORDER BY m.created_at DESC,m.user_id DESC LIMIT $6 OFFSET $7");
        values.extend([limit.clamp(1, 1000).into(), offset.max(0).into()]);
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}

impl TenantMemberRecord {
    pub async fn find_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        user_id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(member_query(
            scope,
            MEMBER_COLUMNS,
            None,
            None,
            None,
            Some(user_id),
        )?)
        .one(db)
        .await?)
    }
    pub async fn list_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        status: Option<MembershipStatus>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(member_query(
            scope,
            MEMBER_COLUMNS,
            status,
            search,
            Some((limit, offset)),
            None,
        )?)
        .all(db)
        .await?)
    }
    pub async fn count_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        status: Option<MembershipStatus>,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        let row = db
            .query_one(member_query(scope, "COUNT(*)", status, search, None, None)?)
            .await?
            .ok_or_else(|| DbError::Other("member count returned no row".into()))?;
        Ok(row.try_get_by_index(0)?)
    }
}

impl User {
    pub async fn find_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(
            Self::find_by_statement(platform_query(scope, "u.*", None, None, None, Some(id))?)
                .one(db)
                .await?,
        )
    }
    pub async fn find_platform_filtered(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        role: Option<PlatformRole>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(platform_query(
            scope,
            "u.*",
            role,
            search,
            Some((limit, offset)),
            None,
        )?)
        .all(db)
        .await?)
    }
    pub async fn count_platform_filtered(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        role: Option<PlatformRole>,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        let row = db
            .query_one(platform_query(scope, "COUNT(*)", role, search, None, None)?)
            .await?
            .ok_or_else(|| DbError::Other("user count returned no row".into()))?;
        Ok(row.try_get_by_index(0)?)
    }
    /// Platform target validation still binds the requested membership in SQL.
    pub async fn find_platform_member(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Self>, DbError> {
        require_platform_root(scope)?;
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT u.* FROM users u JOIN tenant_memberships m ON m.user_id=u.id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.status='active' AND EXISTS (SELECT 1 FROM users actor WHERE actor.id=$3 AND actor.platform_role='root' AND actor.status='active')",
            [tenant_id.into(), user_id.into(), scope.user_id().into()])).one(db).await?)
    }
    pub async fn count_memberships_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        tenant_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, i64>, DbError> {
        require_platform_root(scope)?;
        #[derive(FromQueryResult)]
        struct Count {
            tenant_id: Uuid,
            count: i64,
        }
        if tenant_ids.is_empty() {
            return Ok(Default::default());
        }
        let rows = Count::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT tenant_id,COUNT(*) AS count FROM tenant_memberships WHERE tenant_id=ANY($1) AND status='active' AND EXISTS (SELECT 1 FROM users actor WHERE actor.id=$2 AND actor.platform_role='root' AND actor.status='active') GROUP BY tenant_id",
            [tenant_ids.to_vec().into(), scope.user_id().into()])).all(db).await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.tenant_id, row.count))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_and_count_share_membership_filters_and_bound_values() {
        let scope =
            TenantScope::checked(Uuid::new_v4(), Uuid::new_v4(), TenantRole::Admin).unwrap();
        let list = member_query(
            scope,
            MEMBER_COLUMNS,
            Some(MembershipStatus::Active),
            Some("%_"),
            Some((1, 2)),
            None,
        )
        .unwrap();
        let count = member_query(
            scope,
            "COUNT(*)",
            Some(MembershipStatus::Active),
            Some("%_"),
            None,
            None,
        )
        .unwrap();
        let predicate = |s: &str| {
            s.split_once("WHERE m.tenant_id")
                .unwrap()
                .1
                .split(" ORDER BY")
                .next()
                .unwrap()
                .to_owned()
        };
        assert_eq!(predicate(&list.sql), predicate(&count.sql));
        assert_eq!(list.values.unwrap().0[..5], count.values.unwrap().0);
        assert!(!MEMBER_COLUMNS.contains("platform_role"));
        assert!(!MEMBER_COLUMNS.contains("token_version"));
    }
    #[test]
    fn platform_and_tenant_queries_refuse_insufficient_scope() {
        let id = Uuid::new_v4();
        let operator = PlatformScope::checked(id, PlatformRole::Operator).unwrap();
        assert!(platform_query(operator, "u.*", None, None, None, None).is_err());
        let member = TenantScope::checked(Uuid::new_v4(), id, TenantRole::Member).unwrap();
        assert!(member_query(member, MEMBER_COLUMNS, None, None, None, None).is_err());
    }
}
