//! Selected-tenant financial controls that do not grant platform money authority.
//! Tenant administrators may review tenant withdrawal intents and recover only
//! server-confirmed expired request reservations. Platform balance changes and
//! payout-secret/completion operations deliberately live elsewhere.
use super::{
    admin::{
        BalanceReservationInfo, ReleaseBalanceReservationResponse, UserBalanceReservationsResponse,
    },
    wallet_control::{RecoveryCommand, WalletControlApi},
};
use crate::{ApiClient, ClientError, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawalDecision {
    Approve,
    Reject,
}
impl WithdrawalDecision {
    fn suffix(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }
    fn status(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Reject => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawalQuery {
    pub status: Option<String>,
    pub limit: u32,
    pub offset: u32,
}
impl Default for WithdrawalQuery {
    fn default() -> Self {
        Self {
            status: None,
            limit: 20,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TenantWithdrawalPage {
    pub items: Vec<TenantWithdrawal>,
    pub total: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TenantWithdrawal {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub request_id: Uuid,
    pub withdrawal_type: String,
    pub total_amount: String,
    pub currency: String,
    pub status: String,
    pub payout_details_present: bool,
    pub admin_id: Option<Uuid>,
    pub admin_remark: Option<String>,
    pub payout_reference: Option<String>,
    pub balance_transaction_id: Option<Uuid>,
    pub revision: i64,
    pub actioned_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ReviewCommand<'a> {
    expected_revision: i64,
    reason: &'a str,
}

#[derive(Debug, Clone)]
pub struct TenantFinancialControlApi {
    client: ApiClient,
    tenant: Uuid,
    withdrawals: String,
    wallet: WalletControlApi,
}

fn real(id: Uuid, label: &str) -> Result<()> {
    if id.is_nil() {
        Err(ClientError::Config(format!(
            "A real {label} UUID is required"
        )))
    } else {
        Ok(())
    }
}
fn reason(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.len() > 500 || value.chars().any(char::is_control) {
        Err(ClientError::Config(
            "A bounded visible financial reason is required".into(),
        ))
    } else {
        Ok(value)
    }
}
fn decimal(value: &str) -> bool {
    if value.is_empty() || value.len() > 256 {
        return false;
    }
    let value = value.strip_prefix('-').unwrap_or(value);
    let mut dots = 0;
    let mut digits = 0;
    for b in value.bytes() {
        if b.is_ascii_digit() {
            digits += 1;
        } else if b == b'.' {
            dots += 1;
        } else {
            return false;
        }
    }
    digits > 0 && dots <= 1
}
fn nonnegative_decimal(value: &str) -> bool {
    decimal(value) && !value.starts_with('-')
}
fn positive_decimal(value: &str) -> bool {
    nonnegative_decimal(value) && value.bytes().any(|b| matches!(b, b'1'..=b'9'))
}
fn current_status(value: &str) -> bool {
    matches!(value, "pending" | "approved" | "completed" | "rejected")
}
fn current_kind(value: &str) -> bool {
    matches!(value, "alipay" | "balance")
}
fn validate_reservation_row(row: &BalanceReservationInfo) -> Result<(Uuid, Uuid)> {
    let request = Uuid::parse_str(&row.request_id)
        .ok()
        .filter(|v| !v.is_nil())
        .ok_or_else(|| {
            ClientError::InvalidResponse("Invalid reservation request identity".into())
        })?;
    let version = Uuid::parse_str(&row.version)
        .ok()
        .filter(|v| !v.is_nil())
        .ok_or_else(|| {
            ClientError::InvalidResponse("Invalid reservation ownership version".into())
        })?;
    if row.status != "active"
        || !nonnegative_decimal(&row.amount)
        || row.expires_at.is_empty()
        || row.created_at.is_empty()
        || row.updated_at.is_empty()
    {
        return Err(ClientError::InvalidResponse(
            "Wallet reservation state or amount is invalid".into(),
        ));
    }
    Ok((request, version))
}
fn validate_reservations(owner: Uuid, page: &UserBalanceReservationsResponse) -> Result<()> {
    if Uuid::parse_str(&page.user_id).ok() != Some(owner)
        || !decimal(&page.available_balance)
        || ![
            &page.total_frozen_balance,
            &page.request_reserved_balance,
            &page.manually_frozen_balance,
        ]
        .into_iter()
        .all(|v| nonnegative_decimal(v))
    {
        return Err(ClientError::InvalidResponse(
            "Wallet reservation scope or amount is invalid".into(),
        ));
    }
    if page
        .next_cursor
        .as_deref()
        .is_some_and(|v| v.is_empty() || v.len() > 4096 || v.chars().any(char::is_control))
    {
        return Err(ClientError::InvalidResponse(
            "Invalid reservation continuation cursor".into(),
        ));
    }
    let mut requests = std::collections::HashSet::new();
    for row in &page.reservations {
        let (request, _) = validate_reservation_row(row)?;
        if !requests.insert(request) {
            return Err(ClientError::InvalidResponse(
                "Duplicate wallet reservation resource".into(),
            ));
        }
    }
    Ok(())
}
fn validate_withdrawal(tenant: Uuid, row: &TenantWithdrawal) -> Result<()> {
    let remark_valid = row.admin_remark.as_deref().is_none_or(|value| {
        !value.is_empty() && value.len() <= 500 && !value.chars().any(char::is_control)
    });
    let reference_valid = row.payout_reference.as_deref().is_none_or(|value| {
        !value.is_empty() && value.len() <= 500 && !value.chars().any(char::is_control)
    });
    let state_valid = match (row.withdrawal_type.as_str(), row.status.as_str()) {
        ("balance", "pending") => {
            !row.payout_details_present
                && row.admin_id.is_none()
                && row.admin_remark.is_none()
                && row.payout_reference.is_none()
                && row.balance_transaction_id.is_none()
                && row.actioned_at.is_none()
                && row.completed_at.is_none()
        }
        ("balance", "completed") => {
            !row.payout_details_present
                && row.admin_id.is_none()
                && row.admin_remark.is_none()
                && row.payout_reference.is_none()
                && row.balance_transaction_id.is_some()
                && row.actioned_at.is_some()
                && row.completed_at.is_some()
        }
        ("alipay", "pending") => {
            row.payout_details_present
                && row.admin_id.is_none()
                && row.admin_remark.is_none()
                && row.payout_reference.is_none()
                && row.balance_transaction_id.is_none()
                && row.actioned_at.is_none()
                && row.completed_at.is_none()
        }
        ("alipay", "approved" | "rejected") => {
            row.payout_details_present
                && row.admin_id.is_some()
                && row.admin_remark.is_some()
                && row.payout_reference.is_none()
                && row.balance_transaction_id.is_none()
                && row.actioned_at.is_some()
                && row.completed_at.is_none()
        }
        ("alipay", "completed") => {
            row.payout_details_present
                && row.admin_id.is_some()
                && row.admin_remark.is_some()
                && row.payout_reference.is_some()
                && row.balance_transaction_id.is_none()
                && row.actioned_at.is_some()
                && row.completed_at.is_some()
        }
        _ => false,
    };
    if row.tenant_id != tenant
        || row.id.is_nil()
        || row.owner_user_id.is_nil()
        || row.request_id.is_nil()
        || !current_kind(&row.withdrawal_type)
        || !current_status(&row.status)
        || row.currency != "CNY"
        || row.revision <= 0
        || !positive_decimal(&row.total_amount)
        || row.created_at.is_empty()
        || row.updated_at.is_empty()
        || row.admin_id.is_some_and(|id| id.is_nil())
        || row.balance_transaction_id.is_some_and(|id| id.is_nil())
        || !remark_valid
        || !reference_valid
        || !state_valid
    {
        return Err(ClientError::InvalidResponse(
            "Withdrawal response does not match the requested tenant resource".into(),
        ));
    }
    Ok(())
}

impl TenantFinancialControlApi {
    pub fn new(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        real(tenant, "tenant")?;
        Ok(Self {
            client: client.clone(),
            tenant,
            withdrawals: format!("/api/v1/tenants/{tenant}/tips/withdrawals"),
            wallet: WalletControlApi::tenant(client, tenant)?,
        })
    }
    pub async fn reservations(
        &self,
        owner: Uuid,
        cursor: Option<&str>,
        limit: u64,
        token: &str,
    ) -> Result<UserBalanceReservationsResponse> {
        real(owner, "wallet owner")?;
        if cursor.is_some_and(|v| v.is_empty() || v.len() > 4096 || v.chars().any(char::is_control))
        {
            return Err(ClientError::Config("Invalid reservation cursor".into()));
        }
        let page = self
            .wallet
            .reservations(owner, cursor, limit, token)
            .await?;
        validate_reservations(owner, &page)?;
        if page.reservations.len() > limit as usize {
            return Err(ClientError::InvalidResponse(
                "Reservation page exceeds the requested limit".into(),
            ));
        }
        if cursor.is_some() && page.next_cursor.as_deref() == cursor {
            return Err(ClientError::InvalidResponse(
                "Reservation continuation cursor did not advance".into(),
            ));
        }
        Ok(page)
    }
    pub async fn release_expired(
        &self,
        owner: Uuid,
        row: &BalanceReservationInfo,
        why: &str,
        token: &str,
    ) -> Result<ReleaseBalanceReservationResponse> {
        real(owner, "wallet owner")?;
        let (request, version) = validate_reservation_row(row).map_err(|_| {
            ClientError::Config(
                "A current active reservation row with its observed version is required".into(),
            )
        })?;
        let normalized_reason = reason(why)?.to_owned();
        let out = self
            .wallet
            .release(
                owner,
                request,
                &RecoveryCommand {
                    expected_version: version,
                    reason: normalized_reason.clone(),
                },
                token,
            )
            .await?;
        if !out.success
            || Uuid::parse_str(&out.user_id).ok() != Some(owner)
            || Uuid::parse_str(&out.request_id).ok() != Some(request)
            || Uuid::parse_str(&out.released_by)
                .ok()
                .is_none_or(|id| id.is_nil())
            || out.reason != normalized_reason
            || out.message.is_empty()
            || out.warning.is_empty()
            || !nonnegative_decimal(&out.released_amount)
            || !decimal(&out.new_available_balance)
            || ![
                &out.new_total_frozen_balance,
                &out.request_reserved_balance,
                &out.manually_frozen_balance,
            ]
            .into_iter()
            .all(|v| nonnegative_decimal(v))
        {
            return Err(ClientError::InvalidResponse(
                "Reservation release response does not match the requested resource".into(),
            ));
        }
        Ok(out)
    }
    pub async fn withdrawals(
        &self,
        q: &WithdrawalQuery,
        token: &str,
    ) -> Result<TenantWithdrawalPage> {
        if !(1..=100).contains(&q.limit) || q.offset > 1_000_000 {
            return Err(ClientError::Config("Invalid withdrawal pagination".into()));
        }
        if q.status.as_deref().is_some_and(|v| !current_status(v)) {
            return Err(ClientError::Config(
                "Invalid withdrawal status filter".into(),
            ));
        }
        let mut path = format!(
            "{}?currency=CNY&limit={}&offset={}",
            self.withdrawals, q.limit, q.offset
        );
        if let Some(status) = &q.status {
            path.push_str("&status=");
            path.push_str(&super::common::encode_query_value(status));
        }
        let out: TenantWithdrawalPage = self.client.get_json_fresh(&path, Some(token)).await?;
        if out.total < 0 || out.items.len() > q.limit as usize || out.items.len() as i64 > out.total
        {
            return Err(ClientError::InvalidResponse(
                "Invalid withdrawal page metadata".into(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for row in &out.items {
            validate_withdrawal(self.tenant, row)?;
            if q.status
                .as_deref()
                .is_some_and(|status| row.status != status)
                || !ids.insert(row.id)
            {
                return Err(ClientError::InvalidResponse(
                    "Duplicate withdrawal resource".into(),
                ));
            }
        }
        Ok(out)
    }
    pub async fn review(
        &self,
        row: &TenantWithdrawal,
        decision: WithdrawalDecision,
        why: &str,
        token: &str,
    ) -> Result<TenantWithdrawal> {
        validate_withdrawal(self.tenant, row)?;
        if row.status != "pending" || row.withdrawal_type != "alipay" {
            return Err(ClientError::Config(
                "Only pending Alipay withdrawals can be reviewed by a tenant administrator".into(),
            ));
        }
        let normalized_reason = reason(why)?;
        let body = ReviewCommand {
            expected_revision: row.revision,
            reason: normalized_reason,
        };
        let out: TenantWithdrawal = self
            .client
            .post_json(
                &format!("{}/{}/{}", self.withdrawals, row.id, decision.suffix()),
                &body,
                Some(token),
            )
            .await?;
        validate_withdrawal(self.tenant, &out)?;
        if out.id != row.id
            || out.owner_user_id != row.owner_user_id
            || out.request_id != row.request_id
            || out.withdrawal_type != row.withdrawal_type
            || out.total_amount != row.total_amount
            || out.currency != row.currency
            || out.status != decision.status()
            || out.revision <= row.revision
            || out.admin_id.is_none_or(|id| id.is_nil())
            || out.admin_remark.as_deref() != Some(normalized_reason)
            || out.actioned_at.is_none()
            || out.completed_at.is_some()
            || out.payout_reference.is_some()
            || out.balance_transaction_id.is_some()
        {
            return Err(ClientError::InvalidResponse(
                "Withdrawal review response changed immutable identity or revision".into(),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reason_contract_matches_server_utf8_byte_limit() {
        assert!(reason(&"a".repeat(500)).is_ok());
        assert!(reason(&"a".repeat(501)).is_err());
        assert!(reason(&"中".repeat(166)).is_ok());
        assert!(reason(&"中".repeat(167)).is_err());
        assert!(reason("contains\ncontrol").is_err());
    }

    #[test]
    fn exact_money_is_never_parsed_as_float() {
        for v in ["0", "0.0000000001", "999999999999.123456", "-1.5"] {
            assert!(decimal(v));
        }
        for v in ["", "NaN", "1e3", "1.2.3", "+"] {
            assert!(!decimal(v));
        }
    }
}
