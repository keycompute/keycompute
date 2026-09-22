//! Identity/version proof carried by new work, never a bearer credential.
use crate::{CredentialKind, KeyComputeError, RequestContext, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DispatchIdentity {
    pub tenant_id: Uuid,
    pub actor_user_id: Uuid,
    pub resource_owner_user_id: Uuid,
    pub credential_kind: CredentialKind,
    pub api_key_id: Option<Uuid>,
    pub token_version: i32,
    pub tenant_authz_version: i64,
    pub membership_authz_version: i64,
    pub credential_expires_at: Option<i64>,
}
impl DispatchIdentity {
    /// No impersonation/delegated generation is currently supported. A future
    /// delegated capability needs a separate explicit ownership contract.
    pub fn validate(&self, tenant: Uuid, owner: Uuid, key: Uuid) -> Result<()> {
        let credential_valid = match self.credential_kind {
            CredentialKind::ApiKey => self.api_key_id == Some(key) && !key.is_nil(),
            CredentialKind::Jwt => {
                self.api_key_id.is_none() && key.is_nil() && self.credential_expires_at.is_some()
            }
            _ => false,
        };
        if tenant.is_nil()
            || owner.is_nil()
            || self.tenant_id != tenant
            || self.actor_user_id != owner
            || self.resource_owner_user_id != owner
            || self.token_version < 0
            || self.tenant_authz_version <= 0
            || self.membership_authz_version <= 0
            || !credential_valid
            || self
                .credential_expires_at
                .is_some_and(|t| t <= chrono::Utc::now().timestamp())
        {
            return Err(KeyComputeError::PermissionDenied(
                "execution_authority_invalid".into(),
            ));
        }
        Ok(())
    }
    pub fn validate_context(&self, ctx: &RequestContext) -> Result<()> {
        self.validate(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id)
    }
}

/// The server installs this dependency unconditionally. Standalone gateway
/// embeddings can supply their own authority source; absence of a proof in a
/// server request is rejected, not interpreted as a privileged system task.
#[async_trait::async_trait]
pub trait DispatchAuthorizer: std::fmt::Debug + Send + Sync {
    async fn authorize_dispatch(&self, ctx: &RequestContext) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn proof() -> DispatchIdentity {
        let owner = Uuid::new_v4();
        DispatchIdentity {
            tenant_id: Uuid::new_v4(),
            actor_user_id: owner,
            resource_owner_user_id: owner,
            credential_kind: CredentialKind::Jwt,
            api_key_id: None,
            token_version: 0,
            tenant_authz_version: 1,
            membership_authz_version: 1,
            credential_expires_at: Some(chrono::Utc::now().timestamp() + 3600),
        }
    }
    #[test]
    fn proof_rejects_owner_changes_untrusted_credentials_and_invalid_versions() {
        let valid = proof();
        assert!(
            valid
                .validate(valid.tenant_id, valid.resource_owner_user_id, Uuid::nil())
                .is_ok()
        );
        for changed in [
            DispatchIdentity {
                actor_user_id: Uuid::new_v4(),
                ..valid
            },
            DispatchIdentity {
                resource_owner_user_id: Uuid::new_v4(),
                ..valid
            },
            DispatchIdentity {
                tenant_id: Uuid::nil(),
                ..valid
            },
            DispatchIdentity {
                credential_kind: CredentialKind::System,
                ..valid
            },
            DispatchIdentity {
                credential_kind: CredentialKind::Node,
                ..valid
            },
            DispatchIdentity {
                credential_expires_at: None,
                ..valid
            },
            DispatchIdentity {
                credential_expires_at: Some(chrono::Utc::now().timestamp() - 1),
                ..valid
            },
            DispatchIdentity {
                token_version: -1,
                ..valid
            },
            DispatchIdentity {
                tenant_authz_version: 0,
                ..valid
            },
            DispatchIdentity {
                membership_authz_version: 0,
                ..valid
            },
        ] {
            assert!(
                changed
                    .validate(valid.tenant_id, valid.resource_owner_user_id, Uuid::nil())
                    .is_err()
            );
        }
        let key = Uuid::new_v4();
        let api = DispatchIdentity {
            credential_kind: CredentialKind::ApiKey,
            api_key_id: Some(key),
            credential_expires_at: None,
            ..valid
        };
        assert!(
            api.validate(valid.tenant_id, valid.resource_owner_user_id, key)
                .is_ok()
        );
        assert!(
            api.validate(
                valid.tenant_id,
                valid.resource_owner_user_id,
                Uuid::new_v4()
            )
            .is_err()
        );
    }
    #[test]
    fn work_proof_has_no_role_or_bearer_fallback_and_survives_settlement_clone() {
        let identity = proof();
        let value = serde_json::to_value(identity).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 9);
        for forbidden in [
            "authorization",
            "bearer_token",
            "password",
            "platform_role",
            "tenant_role",
        ] {
            let mut forged = value.clone();
            forged[forbidden] = serde_json::json!("root");
            assert!(serde_json::from_value::<DispatchIdentity>(forged).is_err());
        }
        let mut ctx = RequestContext::new(
            Uuid::new_v4(),
            identity.resource_owner_user_id,
            identity.tenant_id,
            Uuid::nil(),
            "fixture",
            vec![],
            false,
            crate::PricingSnapshot::default(),
        );
        assert!(ctx.validated_dispatch_identity().is_err());
        ctx.dispatch_identity = Some(identity);
        assert_eq!(
            ctx.clone_without_request_payloads()
                .validated_dispatch_identity()
                .unwrap(),
            identity
        );
        ctx.user_id = Uuid::new_v4();
        assert!(ctx.validated_dispatch_identity().is_err());
    }
}
