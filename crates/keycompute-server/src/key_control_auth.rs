//! Original-session guard for personal and tenant Key effects.
//! Action grants and resource scope remain enforced by the existing DAOs.
use crate::{
    console_session_proof::ConsoleSessionProof,
    error::{ApiError, Result},
    extractors::ConsoleAuth,
    state::AppState,
};
use keycompute_auth::AuthContext;
use sea_orm::{ConnectionTrait, DatabaseTransaction};
pub(crate) async fn finish_read(db: &impl ConnectionTrait, auth: &ConsoleAuth) -> Result<()> {
    finish_read_context(db, &auth.authorization_context()).await
}
pub(crate) async fn finish_read_context(
    db: &impl ConnectionTrait,
    auth: &AuthContext,
) -> Result<()> {
    ConsoleSessionProof::from_context(auth)?
        .verify_current(db)
        .await
}
pub(crate) async fn begin(state: &AppState, auth: &ConsoleAuth) -> Result<DatabaseTransaction> {
    begin_context(state, &auth.authorization_context()).await
}
pub(crate) async fn begin_context(
    state: &AppState,
    auth: &AuthContext,
) -> Result<DatabaseTransaction> {
    ConsoleSessionProof::from_context(auth)?.begin(state).await
}
#[derive(Clone, Copy)]
pub(crate) enum KeyChange {
    Credential,
    Intent,
}
pub(crate) async fn commit(
    state: &AppState,
    tx: DatabaseTransaction,
    auth: &ConsoleAuth,
    change: KeyChange,
) -> Result<()> {
    commit_context(state, tx, &auth.authorization_context(), change).await
}
pub(crate) async fn commit_context(
    state: &AppState,
    tx: DatabaseTransaction,
    auth: &AuthContext,
    change: KeyChange,
) -> Result<()> {
    let proof = match ConsoleSessionProof::from_context(auth) {
        Ok(proof) => proof,
        Err(error) => {
            tx.rollback()
                .await
                .map_err(|_| ApiError::ServiceUnavailable("Key rollback is unconfirmed".into()))?;
            return Err(error);
        }
    };
    let tx = proof.prepare_commit(tx).await?;
    let _fence =
        matches!(change, KeyChange::Credential).then(|| state.display_cache.mutation_guard());
    tx.commit()
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Key control commit is unconfirmed".into()))?;
    // A delayed acknowledgement is an uncertain outcome, never a promise of rollback.
    proof.check_deadline()
}
