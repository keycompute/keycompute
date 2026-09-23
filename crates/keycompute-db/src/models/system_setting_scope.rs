//! Root-only platform settings. Global means platform-owned, not default-tenant-owned.
use super::{SystemSetting, setting_keys as keys};
use crate::{AuditContext, DbError, TenantAuditEvent, models::financial_scope::FinancialScope};
use keycompute_types::{AuditResult, AuditScopeType};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use std::collections::{BTreeMap, HashMap};

/// Constructed from server configuration, never from a request payload.
#[derive(Debug, Clone, Copy)]
pub struct SettingsPolicy {
    pub public_base_url_configured: bool,
}
const VISIBLE: &str = "key NOT IN ('default_user_role','allow_registration','registration_mode','email_verification_required')";
fn invalid(message: impl Into<String>) -> DbError {
    DbError::Other(message.into())
}

const SETTING_DEFINITIONS: &[(&str, &str)] = &[
    (keys::SITE_NAME, "string"),
    (keys::SITE_DESCRIPTION, "string"),
    (keys::SITE_LOGO_URL, "string"),
    (keys::SITE_FAVICON_URL, "string"),
    (keys::MAINTENANCE_MESSAGE, "string"),
    (keys::SYSTEM_NOTICE, "string"),
    (keys::FOOTER_CONTENT, "string"),
    (keys::ABOUT_CONTENT, "string"),
    (keys::TERMS_OF_SERVICE_URL, "string"),
    (keys::PRIVACY_POLICY_URL, "string"),
    (keys::DEFAULT_CURRENCY, "string"),
    (keys::MAINTENANCE_MODE, "bool"),
    (keys::DISTRIBUTION_ENABLED, "bool"),
    (keys::ALIPAY_ENABLED, "bool"),
    (keys::WECHATPAY_ENABLED, "bool"),
    (keys::SYSTEM_NOTICE_ENABLED, "bool"),
    (keys::DEFAULT_RPM_LIMIT, "int"),
    (keys::DEFAULT_TPM_LIMIT, "int"),
    (keys::LOGIN_FAILED_LIMIT, "int"),
    (keys::LOGIN_LOCKOUT_MINUTES, "int"),
    (keys::JWT_EXPIRE_HOURS, "int"),
    (keys::DEFAULT_USER_QUOTA, "decimal"),
    (keys::MIN_RECHARGE_AMOUNT, "decimal"),
    (keys::MAX_RECHARGE_AMOUNT, "decimal"),
    (keys::DISTRIBUTION_LEVEL1_DEFAULT_RATIO, "decimal"),
    (keys::DISTRIBUTION_LEVEL2_DEFAULT_RATIO, "decimal"),
    (keys::DISTRIBUTION_MIN_WITHDRAW, "decimal"),
    (keys::NODE_TIP_RATIO, "decimal"),
];
fn value_type(key: &str) -> Option<&'static str> {
    SETTING_DEFINITIONS
        .iter()
        .find(|(known, _)| *known == key)
        .map(|(_, kind)| *kind)
}
fn safe_projection() -> String {
    // Only compile-time keys enter SQL text; request keys remain bind parameters.
    let names = SETTING_DEFINITIONS
        .iter()
        .map(|(key, _)| format!("'{key}'"))
        .collect::<Vec<_>>()
        .join(",");
    let visible = format!("is_sensitive IS FALSE AND key IN ({names})");
    format!(
        "id,key,CASE WHEN {visible} THEN value ELSE '[REDACTED]' END AS value,value_type,CASE WHEN {visible} THEN description ELSE NULL END AS description,NOT ({visible}) AS is_sensitive,created_at,updated_at"
    )
}

/// Shared validation for HTTP and direct DAO callers; no arbitrary setting creation.
pub fn normalize_platform_setting(key: &str, input: &str) -> Result<String, DbError> {
    if key == keys::DEFAULT_USER_ROLE {
        return Err(invalid(
            "Setting default_user_role is fixed and cannot be edited",
        ));
    }
    if matches!(
        key,
        "allow_registration" | "registration_mode" | "email_verification_required"
    ) {
        return Err(invalid(
            "Setting has been removed and can no longer be edited",
        ));
    }
    if key == keys::NODE_TIP_RATIO {
        return Err(invalid(
            "Setting node_tip_ratio requires the versioned platform ratio endpoint",
        ));
    }
    let kind =
        value_type(key).ok_or_else(|| invalid("Unknown or non-editable platform setting"))?;
    if input.len() > 16_384 || input.contains('\0') {
        return Err(invalid(
            "Setting value exceeds the permitted size or encoding",
        ));
    }
    let value = input.trim();
    match kind {
        "bool" => match value.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok("true".into()),
            "false" | "0" | "no" => Ok("false".into()),
            _ => Err(invalid(format!("Setting {key} must be a valid boolean"))),
        },
        "int" => {
            let number = value
                .parse::<i32>()
                .map_err(|_| invalid(format!("Setting {key} must be a valid integer")))?;
            if number <= 0
                || (key == keys::JWT_EXPIRE_HOURS && i64::from(number) > keys::JWT_EXPIRE_HOURS_MAX)
            {
                return Err(invalid(format!(
                    "Setting {key} must be positive and within its supported range"
                )));
            }
            Ok(number.to_string())
        }
        "decimal" => {
            let number = value
                .parse::<Decimal>()
                .map_err(|_| invalid(format!("Setting {key} must be a valid decimal")))?
                .normalize();
            let (max, scale, allow_nonpositive) = match key {
                keys::DEFAULT_USER_QUOTA => (
                    Decimal::from_i128_with_scale(99_999_999_999_999_999_999, 10),
                    10,
                    true,
                ),
                keys::DISTRIBUTION_LEVEL1_DEFAULT_RATIO
                | keys::DISTRIBUTION_LEVEL2_DEFAULT_RATIO => (Decimal::ONE, 4, true),
                _ => (Decimal::new(999_999_999_999, 2), 2, false),
            };
            if number.abs() > max
                || number.scale() > scale
                || (!allow_nonpositive && number <= Decimal::ZERO)
                || (matches!(
                    key,
                    keys::DISTRIBUTION_LEVEL1_DEFAULT_RATIO
                        | keys::DISTRIBUTION_LEVEL2_DEFAULT_RATIO
                ) && number < Decimal::ZERO)
            {
                return Err(invalid(format!(
                    "Setting {key} has unsupported amount, range or precision"
                )));
            }
            Ok(number.to_string())
        }
        _ if key == keys::DEFAULT_CURRENCY => {
            if value != "CNY" {
                return Err(invalid("Setting default_currency is fixed to CNY"));
            }
            Ok(value.into())
        }
        _ => Ok(input.to_owned()),
    }
}

pub fn validate_platform_payment_range(min: Decimal, max: Decimal) -> Result<(), DbError> {
    if min <= Decimal::ZERO || min > max {
        return Err(invalid(
            "Minimum recharge amount must not exceed the positive maximum",
        ));
    }
    Ok(())
}

impl SystemSetting {
    /// One primary SQL snapshot. CASE masks secrets before materialization.
    pub async fn find_all_platform(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
    ) -> Result<Vec<Self>, DbError> {
        scope.require_root_global()?;
        let projection = safe_projection();
        let sql = format!(
            "SELECT (SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY s.key),'[]'::jsonb) FROM (SELECT {projection} FROM system_settings WHERE {VISIBLE}) s) AS settings WHERE {}",
            scope.predicate()
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                scope.values(),
            ))
            .await?
            .ok_or_else(|| invalid("financial_authority_invalid"))?;
        serde_json::from_value(row.try_get("", "settings")?)
            .map_err(|_| invalid("settings_projection_invalid"))
    }
    pub async fn find_platform(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        key: &str,
    ) -> Result<Option<Self>, DbError> {
        scope.require_root_global()?;
        let projection = safe_projection();
        let mut values = scope.values();
        values.push(key.into());
        let sql = format!(
            "SELECT {projection} FROM system_settings WHERE key=$10 AND {VISIBLE} AND {}",
            scope.predicate()
        );
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?)
    }
    /// Configuration, current signed authority and audit use one retained transaction.
    pub async fn update_platform_batch(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        settings: &HashMap<String, String>,
        policy: SettingsPolicy,
    ) -> Result<Vec<Self>, DbError> {
        scope.require_root_global()?;
        let projection = safe_projection();
        if settings.len() > 64
            || settings
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                > 65_536
        {
            return Err(invalid("Platform settings batch is too large"));
        }
        let proposed = settings
            .iter()
            .map(|(k, v)| Ok((k.clone(), normalize_platform_setting(k, v)?)))
            .collect::<Result<BTreeMap<_, _>, DbError>>()?;
        if proposed
            .get(keys::DISTRIBUTION_ENABLED)
            .is_some_and(|v| v == "true")
            && !policy.public_base_url_configured
        {
            return Err(invalid(
                "APP_BASE_URL must be configured before enabling distribution",
            ));
        }
        let tx = db.begin().await?;
        let result = async {
            tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='10s'").await?;
            let actor = scope.lock(&tx,audit).await?;
            let mut locked_keys = proposed.keys().cloned().collect::<Vec<_>>();
            let payment = proposed.contains_key(keys::MIN_RECHARGE_AMOUNT) || proposed.contains_key(keys::MAX_RECHARGE_AMOUNT);
            if payment {locked_keys.extend([keys::MIN_RECHARGE_AMOUNT.into(),keys::MAX_RECHARGE_AMOUNT.into()]);}
            locked_keys.sort(); locked_keys.dedup();
            let sql=format!("SELECT {projection} FROM system_settings WHERE key=ANY($1) ORDER BY key FOR UPDATE");
            let old=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,sql,[locked_keys.into()])).all(&tx).await?;
            if old.iter().any(|s|s.is_sensitive) {return Err(invalid("Sensitive settings require a dedicated credential workflow"));}
            if payment {
                let amount=|key:&str,default:Decimal| -> Result<Decimal,DbError> {
                    proposed.get(key).map(String::as_str).or_else(||old.iter().find(|s|s.key==key).map(|s|s.value.as_str()))
                        .map(|v|v.parse::<Decimal>().map_err(|_|invalid("Stored payment setting is invalid"))).unwrap_or(Ok(default))
                };
                validate_platform_payment_range(amount(keys::MIN_RECHARGE_AMOUNT,Decimal::ONE)?,amount(keys::MAX_RECHARGE_AMOUNT,Decimal::from(100_000))?)?;
            }
            scope.current_actor(&tx).await?;
            let mut updated=Vec::with_capacity(proposed.len());
            for (key,value) in &proposed {
                let kind=value_type(key).ok_or_else(||invalid("Unknown platform setting"))?;
                if let Some(before)=old.iter().find(|s|s.key==*key && s.value==*value && s.value_type==kind) {
                    updated.push(before.clone()); continue;
                }
                let mut values=scope.values(); values.extend([key.as_str().into(),value.as_str().into(),kind.into()]);
                let sql=format!("INSERT INTO system_settings(key,value,value_type) SELECT $10,$11,$12 WHERE {} ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,value_type=EXCLUDED.value_type,updated_at=clock_timestamp() WHERE system_settings.is_sensitive IS FALSE RETURNING {projection}",scope.predicate());
                let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,sql,values)).one(&tx).await?
                    .ok_or_else(||invalid("financial_authority_invalid"))?;
                TenantAuditEvent::append(&tx,AuditScopeType::Platform,None,&actor,"settings.update","system_setting",Some(key),AuditResult::Success,
                    serde_json::json!({"changed":true,"updated_at":row.updated_at,"reason":"root platform configuration update"})).await?;
                updated.push(row);
            }
            scope.current_actor(&tx).await?;
            Ok::<_,DbError>(updated)
        }.await;
        match result {
            Ok(rows) => {
                tx.commit().await?;
                Ok(rows)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }
}
