//! Primary-backed global identity and selected tenant loading.
use keycompute_db::{DbRouter, Tenant};
use keycompute_types::{KeyComputeError, PlatformRole, Result, UserStatus};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct UserInfo {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub platform_role: PlatformRole,
    pub status: UserStatus,
    pub token_version: i32,
}
impl UserInfo {
    pub fn new(
        id: Uuid,
        email: impl Into<String>,
        name: impl Into<String>,
        platform_role: PlatformRole,
        status: UserStatus,
        token_version: i32,
    ) -> Self {
        Self {
            id,
            email: email.into(),
            name: name.into(),
            platform_role,
            status,
            token_version,
        }
    }
}
#[derive(Debug, Clone)]
pub struct TenantInfo {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub active: bool,
    pub config: TenantConfig,
}
#[derive(Debug, Clone, Default)]
pub struct TenantConfig {
    pub default_rpm_limit: u32,
    pub default_tpm_limit: u32,
}
impl TenantInfo {
    pub fn new(id: Uuid, name: impl Into<String>, slug: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            slug: slug.into(),
            active: true,
            config: TenantConfig::default(),
        }
    }
    pub fn from_db_tenant(t: &Tenant) -> Self {
        Self {
            id: t.id,
            name: t.name.clone(),
            slug: t.slug.clone(),
            active: t.status == "active",
            config: TenantConfig {
                default_rpm_limit: t.default_rpm_limit.max(0) as u32,
                default_tpm_limit: t.default_tpm_limit.max(0) as u32,
            },
        }
    }
    pub fn is_active(&self) -> bool {
        self.active
    }
}

#[derive(Debug, FromQueryResult)]
struct IdentityRow {
    id: Uuid,
    email: String,
    name: Option<String>,
    platform_role: String,
    status: String,
    token_version: i32,
}
#[derive(Debug, FromQueryResult)]
pub struct JwtIdentitySnapshot {
    pub user_id: Uuid,
    pub email: String,
    pub user_name: Option<String>,
    pub tenant_id: Uuid,
    pub membership_version: i64,
    pub authz_version: i64,
    pub token_version: i32,
    pub platform_role: String,
    pub tenant_role: String,
    pub tenant_name: String,
    pub tenant_slug: String,
    pub active: bool,
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,
}
#[derive(Clone)]
pub struct UserService {
    pool: Option<Arc<DbRouter>>,
}
impl std::fmt::Debug for UserService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserService")
            .field("pool", &self.pool.is_some())
            .finish()
    }
}
impl UserService {
    pub(crate) fn primary_pool(&self) -> Result<&Arc<DbRouter>> {
        self.pool.as_ref().ok_or_else(|| {
            KeyComputeError::ServiceUnavailable("identity storage unavailable".into())
        })
    }

    pub fn new() -> Self {
        Self { pool: None }
    }
    pub fn with_pool(pool: Arc<DbRouter>) -> Self {
        Self { pool: Some(pool) }
    }
    pub async fn load_user(&self, user_id: Uuid) -> Result<UserInfo> {
        let Some(pool) = &self.pool else {
            return Err(KeyComputeError::AuthError(
                "authentication service not configured".into(),
            ));
        };
        let row = IdentityRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id,email,name,platform_role,status,token_version FROM users WHERE id=$1",
            [user_id.into()],
        ))
        .one(pool.write_conn())
        .await
        .map_err(|e| KeyComputeError::DatabaseError(e.to_string()))?
        .ok_or_else(|| KeyComputeError::AuthError("User not found".into()))?;
        let role = row
            .platform_role
            .parse()
            .map_err(|e: String| KeyComputeError::DatabaseError(e))?;
        let status = row
            .status
            .parse()
            .map_err(|e: String| KeyComputeError::DatabaseError(e))?;
        if status != UserStatus::Active {
            return Err(KeyComputeError::AuthError("User is suspended".into()));
        }
        Ok(UserInfo::new(
            row.id,
            row.email,
            row.name.unwrap_or_default(),
            role,
            status,
            row.token_version,
        ))
    }
    pub async fn load_token_version(&self, user_id: Uuid) -> Result<Option<i32>> {
        let Some(pool) = &self.pool else {
            return Ok(None);
        };
        let row = pool
            .write_conn()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT token_version FROM users WHERE id=$1 AND status='active'",
                [user_id.into()],
            ))
            .await
            .map_err(|e| KeyComputeError::DatabaseError(e.to_string()))?;
        row.map(|r| r.try_get_by_index(0))
            .transpose()
            .map_err(|e| KeyComputeError::DatabaseError(e.to_string()))
    }
    pub async fn load_jwt_identity(
        &self,
        user_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<JwtIdentitySnapshot>> {
        let Some(tid) = tenant_id else {
            return Ok(None);
        };
        let Some(pool) = &self.pool else {
            return Ok(None);
        };
        let row=JwtIdentitySnapshot::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT u.id AS user_id,u.email,u.name AS user_name,m.tenant_id,m.version AS membership_version,t.authz_version,u.token_version,u.platform_role,m.role AS tenant_role,t.name AS tenant_name,t.slug AS tenant_slug,(u.status='active' AND m.status='active' AND t.status='active') AS active,t.default_rpm_limit,t.default_tpm_limit FROM users u JOIN tenant_memberships m ON m.user_id=u.id AND m.tenant_id=$2 JOIN tenants t ON t.id=m.tenant_id WHERE u.id=$1",[user_id.into(),tid.into()])).one(pool.write_conn()).await.map_err(|e|KeyComputeError::DatabaseError(e.to_string()))?;
        Ok(row)
    }
    pub async fn load_tenant(&self, tenant_id: Uuid) -> Result<TenantInfo> {
        let Some(pool) = &self.pool else {
            return Err(KeyComputeError::AuthError(
                "authentication service not configured".into(),
            ));
        };
        let t = Tenant::find_by_id(pool.write_conn(), tenant_id)
            .await
            .map_err(|e| KeyComputeError::DatabaseError(e.to_string()))?
            .ok_or_else(|| KeyComputeError::AuthError("Tenant not found".into()))?;
        let info = TenantInfo::from_db_tenant(&t);
        if !info.active {
            return Err(KeyComputeError::AuthError("Tenant is not active".into()));
        }
        Ok(info)
    }
    pub async fn load_user_with_tenant_validation(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<UserInfo> {
        let user = self.load_user(user_id).await?;
        let Some(pool) = &self.pool else {
            return Err(KeyComputeError::AuthError(
                "authentication service not configured".into(),
            ));
        };
        let exists=pool.write_conn().query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT 1 FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2 AND status='active'",[tenant_id.into(),user_id.into()])).await.map_err(|e|KeyComputeError::DatabaseError(e.to_string()))?.is_some();
        if !exists {
            return Err(KeyComputeError::AuthError(
                "User is not an active tenant member".into(),
            ));
        }
        Ok(user)
    }
    pub async fn load_user_and_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(UserInfo, TenantInfo)> {
        Ok((
            self.load_user_with_tenant_validation(user_id, tenant_id)
                .await?,
            self.load_tenant(tenant_id).await?,
        ))
    }
}
impl Default for UserService {
    fn default() -> Self {
        Self::new()
    }
}
