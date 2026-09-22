//! Current primary authority for a new physical inference attempt.
use keycompute_db::DbRouter;
use keycompute_types::{
    DispatchAuthorizer, DispatchIdentity, KeyComputeError, RequestContext, Result,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::sync::Arc;

#[derive(Clone)]
pub struct DbDispatchAuthorizer {
    pool: Arc<DbRouter>,
}
impl std::fmt::Debug for DbDispatchAuthorizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbDispatchAuthorizer")
            .finish_non_exhaustive()
    }
}
impl DbDispatchAuthorizer {
    pub fn new(pool: Arc<DbRouter>) -> Self {
        Self { pool }
    }
    pub async fn validate_identity(&self, identity: DispatchIdentity) -> Result<()> {
        identity.validate(
            identity.tenant_id,
            identity.resource_owner_user_id,
            identity.api_key_id.unwrap_or_default(),
        )?;
        let encoded = serde_json::to_value(identity)
            .map_err(|_| KeyComputeError::PermissionDenied("execution_authority_invalid".into()))?;
        let row = self
            .pool
            .write_conn()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 WHERE dispatch_identity_is_active($1,$2,$3)",
                [
                    encoded.into(),
                    identity.tenant_id.into(),
                    identity.resource_owner_user_id.into(),
                ],
            ))
            .await
            .map_err(|_| {
                KeyComputeError::ServiceUnavailable("execution_authority_unavailable".into())
            })?;
        if row.is_none() {
            return Err(KeyComputeError::PermissionDenied(
                "execution_authority_invalid".into(),
            ));
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl DispatchAuthorizer for DbDispatchAuthorizer {
    async fn authorize_dispatch(&self, ctx: &RequestContext) -> Result<()> {
        let identity = ctx.dispatch_identity.ok_or_else(|| {
            KeyComputeError::PermissionDenied("execution_authority_invalid".into())
        })?;
        identity.validate_context(ctx)?;
        self.validate_identity(identity).await
    }
}
