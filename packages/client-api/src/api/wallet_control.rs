//! Explicit tenant/root wallet controls. Amounts are exact decimal strings.
pub use super::admin::{
    ReleaseBalanceReservationResponse, UpdateBalanceResponse, UserBalanceReservationsResponse,
};
use crate::{
    client::ApiClient,
    error::{ClientError, Result},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Debug, Clone, Copy)]
pub enum WalletAction {
    Adjust,
    Freeze,
    Unfreeze,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoneyCommand {
    pub amount: String,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCommand {
    pub expected_version: Uuid,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct WalletControlApi {
    client: ApiClient,
    base: String,
    platform: bool,
}
impl WalletControlApi {
    pub fn tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::target(client, tenant, false)
    }
    pub fn platform_tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::target(client, tenant, true)
    }
    fn target(client: &ApiClient, tenant: Uuid, platform: bool) -> Result<Self> {
        if tenant.is_nil() {
            return Err(ClientError::Config("An explicit tenant is required".into()));
        }
        let prefix = if platform {
            "/api/v1/platform/tenants"
        } else {
            "/api/v1/tenants"
        };
        Ok(Self {
            client: client.clone(),
            base: format!("{prefix}/{tenant}/users"),
            platform,
        })
    }
    fn wallet(&self, user: Uuid) -> Result<String> {
        if user.is_nil() {
            return Err(ClientError::Config(
                "An explicit resource owner is required".into(),
            ));
        }
        Ok(format!("{}/{user}/balance", self.base))
    }
    pub async fn reservations(
        &self,
        user: Uuid,
        cursor: Option<&str>,
        limit: u64,
        token: &str,
    ) -> Result<UserBalanceReservationsResponse> {
        if !(1..=100).contains(&limit) {
            return Err(ClientError::Config(
                "Reservation limit must be between 1 and 100".into(),
            ));
        }
        let mut path = format!("{}/reservations?limit={limit}", self.wallet(user)?);
        if let Some(cursor) = cursor {
            path.push_str(&format!(
                "&cursor={}",
                super::common::encode_query_value(cursor)
            ));
        }
        self.client.get_json_fresh(&path, Some(token)).await
    }
    pub async fn change(
        &self,
        user: Uuid,
        action: WalletAction,
        command: &MoneyCommand,
        idempotency_key: &str,
        token: &str,
    ) -> Result<UpdateBalanceResponse> {
        if !self.platform {
            return Err(ClientError::Config(
                "Money adjustments require an explicit platform target".into(),
            ));
        }
        if idempotency_key.is_empty()
            || idempotency_key.len() > 256
            || !idempotency_key.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(ClientError::Config(
                "A bounded visible-ASCII idempotency key is required".into(),
            ));
        }
        let suffix = match action {
            WalletAction::Adjust => "",
            WalletAction::Freeze => "/freeze",
            WalletAction::Unfreeze => "/unfreeze",
        };
        self.client
            .post_json_with_idempotency_key(
                &format!("{}{suffix}", self.wallet(user)?),
                command,
                idempotency_key,
                Some(token),
            )
            .await
    }
    /// Tenant control only releases expired reservations. Root force recovery
    /// is explicit; late accepted usage can still debit the original wallet.
    pub async fn release(
        &self,
        user: Uuid,
        request: Uuid,
        command: &RecoveryCommand,
        token: &str,
    ) -> Result<ReleaseBalanceReservationResponse> {
        if request.is_nil() || command.expected_version.is_nil() {
            return Err(ClientError::Config(
                "Request and current reservation version are required".into(),
            ));
        }
        self.client
            .post_json(
                &format!("{}/reservations/{request}/release", self.wallet(user)?),
                command,
                Some(token),
            )
            .await
    }
}
