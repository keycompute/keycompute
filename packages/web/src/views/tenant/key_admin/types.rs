use chrono::{DateTime, Duration, Utc};
use client_api::{
    ClientError, Result,
    api::key_control::{IssuanceIntent, KeyMetadata, KeyPatch, NewIssuance, RotateKey},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Keys,
    Pending,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub tab: Tab,
    pub owner: Option<Uuid>,
    pub page: u32,
    pub revoked: bool,
}
impl Default for Query {
    fn default() -> Self {
        Self {
            tab: Tab::Keys,
            owner: None,
            page: 1,
            revoked: false,
        }
    }
}
#[derive(Clone, PartialEq)]
pub enum Operation {
    Request,
    Edit(KeyMetadata),
    Rotate(KeyMetadata),
    Revoke(KeyMetadata),
    Delete(KeyMetadata),
    Cancel(IssuanceIntent),
}
impl Operation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Request => "tenant_keys.request",
            Self::Edit(_) => "tenant_keys.edit",
            Self::Rotate(_) => "tenant_keys.rotate",
            Self::Revoke(_) => "tenant_keys.revoke",
            Self::Delete(_) => "tenant_keys.delete",
            Self::Cancel(_) => "tenant_keys.cancel_request",
        }
    }
    pub fn key_row(&self) -> Option<&KeyMetadata> {
        match self {
            Self::Edit(r) | Self::Rotate(r) | Self::Revoke(r) | Self::Delete(r) => Some(r),
            _ => None,
        }
    }
    pub fn owner(&self) -> Option<Uuid> {
        self.key_row().map(|r| r.owner_user_id).or(match self {
            Self::Cancel(i) => Some(i.owner_user_id),
            _ => None,
        })
    }
    pub fn key(&self) -> String {
        if let Some(row) = self.key_row() {
            serde_json::json!([
                self.label(),
                row.tenant_id,
                row.owner_user_id,
                row.id,
                row.updated_at
            ])
            .to_string()
        } else if let Self::Cancel(i) = self {
            serde_json::json!([self.label(), i.tenant_id, i.owner_user_id, i.id]).to_string()
        } else {
            self.label().into()
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    Keep,
    Never,
    At,
}
impl Expiry {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Never => "never",
            Self::At => "at",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "keep" => Some(Self::Keep),
            "never" => Some(Self::Never),
            "at" => Some(Self::At),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub owner: String,
    pub name: String,
    pub expiry: Expiry,
    pub date: String,
}
fn invalid(message: &str) -> ClientError {
    ClientError::Config(message.into())
}
pub fn owner_filter(raw: &str) -> Result<Option<Uuid>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(raw)
        .ok()
        .filter(|id| !id.is_nil())
        .map(Some)
        .ok_or_else(|| invalid("Select a real member UUID"))
}
pub fn live_key(row: &KeyMetadata) -> bool {
    !row.revoked
        && row.expires_at.as_ref().is_none_or(|t| {
            DateTime::parse_from_rfc3339(t).is_ok_and(|t| t.with_timezone(&Utc) > Utc::now())
        })
}
impl Draft {
    pub fn for_operation(op: &Operation) -> Self {
        if let Some(row) = op.key_row() {
            Self {
                owner: row.owner_user_id.to_string(),
                name: row.name.clone(),
                expiry: if matches!(op, Operation::Edit(_)) {
                    Expiry::Keep
                } else if row.expires_at.is_some() {
                    Expiry::At
                } else {
                    Expiry::Never
                },
                date: row.expires_at.clone().unwrap_or_default(),
            }
        } else {
            Self {
                owner: op.owner().map(|id| id.to_string()).unwrap_or_default(),
                name: String::new(),
                expiry: Expiry::At,
                date: (Utc::now() + Duration::days(180)).to_rfc3339(),
            }
        }
    }
    fn name(&self) -> Result<String> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 255 || name.chars().any(char::is_control) {
            return Err(invalid(
                "Use a key name of 1–255 characters without control characters",
            ));
        }
        Ok(name.into())
    }
    fn expiration(&self) -> Result<Option<String>> {
        match self.expiry {
            Expiry::Never => Ok(None),
            Expiry::Keep => Err(invalid("Choose the new key expiration explicitly")),
            Expiry::At => {
                let raw = self.date.trim();
                let date = DateTime::parse_from_rfc3339(raw)
                    .map_err(|_| invalid("Use an RFC3339 expiration with a timezone"))?
                    .with_timezone(&Utc);
                let now = Utc::now();
                if date <= now || date > now + Duration::days(3650) {
                    return Err(invalid(
                        "Key expiration must be in the future and within ten years",
                    ));
                }
                Ok(Some(date.to_rfc3339()))
            }
        }
    }
    pub fn request(&self) -> Result<NewIssuance> {
        Ok(NewIssuance {
            owner_user_id: owner_filter(&self.owner)?
                .ok_or_else(|| invalid("A key owner is required"))?,
            name: self.name()?,
            expires_at: self.expiration()?,
        })
    }
    pub fn rotate(&self, row: &KeyMetadata) -> Result<RotateKey> {
        if !live_key(row) || owner_filter(&self.owner)? != Some(row.owner_user_id) {
            return Err(invalid("Reload an active key with its original owner"));
        }
        Ok(RotateKey {
            name: self.name()?,
            expires_at: self.expiration()?,
        })
    }
    pub fn patch(&self, row: &KeyMetadata) -> Result<KeyPatch> {
        if owner_filter(&self.owner)? != Some(row.owner_user_id)
            || DateTime::parse_from_rfc3339(&row.updated_at).is_err()
        {
            return Err(invalid(
                "Reload the original owner and observed key version",
            ));
        }
        let name = self.name()?;
        let name = (name != row.name).then_some(name);
        let expires_at = if self.expiry == Expiry::Keep {
            None
        } else {
            if !live_key(row) {
                return Err(invalid(
                    "An expired or revoked key cannot be extended; request a new key",
                ));
            }
            Some(self.expiration()?)
        };
        if name.is_none() && expires_at.is_none() {
            return Err(invalid("Choose a name or expiration change"));
        }
        Ok(KeyPatch {
            expected_updated_at: row.updated_at.clone(),
            name,
            expires_at,
        })
    }
}
