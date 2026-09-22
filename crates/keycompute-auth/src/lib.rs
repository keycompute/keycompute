//! Authentication, identity verification, and scope construction.
pub mod api_key;
pub mod jwt;
pub mod password;
pub mod permission;
pub mod session;
pub mod user;
pub use api_key::{ProduceAiKeyAuth, ProduceAiKeyValidator};
pub use jwt::{JwtClaims, JwtValidator};
use keycompute_db::DbRouter;
use keycompute_types::{
    AuthorizationSubject, CredentialKind, KeyComputeError, PlatformRole, Result, TenantRole,
};
pub use password::{
    CompleteRegistrationRequest, CompleteRegistrationResponse, EmailConfig, EmailService,
    EmailValidator, LoginRequest, LoginResponse, LoginService, PasswordHasher,
    PasswordResetService, PasswordValidator, RegistrationService, RequestPasswordResetRequest,
    RequestRegistrationCodeRequest, RequestRegistrationCodeResponse, ResetPasswordRequest,
};
pub use permission::{
    AuthorizationAction, AuthorizationDecision, Permission, PermissionChecker, ResourceScope,
    authorize, permissions_for,
};
pub use session::{ConsoleSession, SessionTokenResponse};
use std::sync::Arc;
pub use user::{TenantConfig, TenantInfo, UserInfo, UserService};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: Uuid,
    pub selected_tenant_id: Option<Uuid>,
    pub platform_role: PlatformRole,
    pub tenant_role: Option<TenantRole>,
    pub credential_kind: CredentialKind,
    pub produce_ai_key_id: Uuid,
    pub permissions: Vec<Permission>,
    pub token_version: i32,
    pub membership_authz_version: Option<i64>,
    pub authz_version: Option<i64>,
    pub user_info: Option<UserInfo>,
    pub tenant_info: Option<TenantInfo>,
}
impl AuthContext {
    pub fn new(user_id: Uuid, credential_kind: CredentialKind) -> Self {
        Self {
            user_id,
            selected_tenant_id: None,
            platform_role: PlatformRole::None,
            tenant_role: None,
            credential_kind,
            produce_ai_key_id: Uuid::nil(),
            permissions: Vec::new(),
            token_version: 0,
            membership_authz_version: None,
            authz_version: None,
            user_info: None,
            tenant_info: None,
        }
    }
    pub fn global(user_id: Uuid) -> Self {
        Self {
            user_id,
            selected_tenant_id: None,
            platform_role: PlatformRole::None,
            tenant_role: None,
            credential_kind: CredentialKind::Jwt,
            produce_ai_key_id: Uuid::nil(),
            permissions: Vec::new(),
            token_version: 0,
            membership_authz_version: None,
            authz_version: None,
            user_info: None,
            tenant_info: None,
        }
    }
    pub fn selected_tenant(&self) -> Option<Uuid> {
        self.selected_tenant_id
    }
    pub fn with_authorization_subject(mut self, s: AuthorizationSubject) -> Self {
        self.platform_role = s.platform_role;
        self.tenant_role = s.tenant_role;
        self.selected_tenant_id = s.tenant_id;
        self.permissions =
            permissions_for(self.credential_kind, self.platform_role, self.tenant_role);
        self
    }
    pub fn authorization_subject(&self) -> AuthorizationSubject {
        AuthorizationSubject {
            user_id: self.user_id,
            platform_role: self.platform_role,
            tenant_id: self.selected_tenant_id,
            tenant_role: self.tenant_role,
        }
    }
    pub fn with_permissions(mut self, p: Vec<Permission>) -> Self {
        self.permissions = p;
        self
    }
    pub fn with_user_info(mut self, u: UserInfo) -> Self {
        self.user_info = Some(u);
        self
    }
    pub fn with_tenant_info(mut self, t: TenantInfo) -> Self {
        self.tenant_info = Some(t);
        self
    }
    pub fn has_permission(&self, p: &Permission) -> bool {
        PermissionChecker::check(self.credential_kind, &self.permissions, p)
    }
    pub fn user_info(&self) -> Option<&UserInfo> {
        self.user_info.as_ref()
    }
    pub fn tenant_info(&self) -> Option<&TenantInfo> {
        self.tenant_info.as_ref()
    }
    pub fn require_platform(
        &self,
        action: AuthorizationAction,
    ) -> Result<keycompute_types::PlatformScope> {
        if authorize(
            self.credential_kind,
            self.authorization_subject(),
            action,
            ResourceScope::Platform,
        ) == AuthorizationDecision::Allow
        {
            keycompute_types::PlatformScope::checked(self.user_id, self.platform_role)
                .map_err(KeyComputeError::AuthError)
        } else {
            Err(KeyComputeError::PermissionDenied(
                "platform scope required".into(),
            ))
        }
    }
    pub fn require_tenant(
        &self,
        action: AuthorizationAction,
    ) -> Result<keycompute_types::TenantScope> {
        let Some(t) = self.selected_tenant_id else {
            return Err(KeyComputeError::AuthError(
                "active tenant selection required".into(),
            ));
        };
        if authorize(
            self.credential_kind,
            self.authorization_subject(),
            action,
            ResourceScope::Tenant { tenant_id: t },
        ) == AuthorizationDecision::Allow
        {
            keycompute_types::TenantScope::checked(
                t,
                self.user_id,
                self.tenant_role.ok_or_else(|| {
                    KeyComputeError::AuthError("active membership required".into())
                })?,
            )
            .map_err(KeyComputeError::AuthError)
        } else {
            Err(KeyComputeError::PermissionDenied(
                "tenant scope denied".into(),
            ))
        }
    }
    pub fn require_owner(
        &self,
        owner: Uuid,
        action: AuthorizationAction,
    ) -> Result<keycompute_types::TenantScope> {
        let Some(t) = self.selected_tenant_id else {
            return Err(KeyComputeError::AuthError(
                "active tenant selection required".into(),
            ));
        };
        if authorize(
            self.credential_kind,
            self.authorization_subject(),
            action,
            ResourceScope::UserOwned {
                tenant_id: t,
                owner_user_id: owner,
            },
        ) == AuthorizationDecision::Allow
        {
            keycompute_types::TenantScope::checked(
                t,
                self.user_id,
                self.tenant_role.ok_or_else(|| {
                    KeyComputeError::AuthError("active membership required".into())
                })?,
            )
            .map_err(KeyComputeError::AuthError)
        } else {
            Err(KeyComputeError::PermissionDenied(
                "owner scope denied".into(),
            ))
        }
    }
}

#[derive(Clone)]
pub struct AuthService {
    produce_ai_key_validator: ProduceAiKeyValidator,
    jwt_validator: Option<JwtValidator>,
    user_service: Option<UserService>,
}
impl std::fmt::Debug for AuthService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthService")
            .field("jwt_configured", &self.jwt_validator.is_some())
            .field("user_service", &self.user_service.is_some())
            .finish()
    }
}
impl AuthService {
    pub fn new(v: ProduceAiKeyValidator) -> Self {
        Self {
            produce_ai_key_validator: v,
            jwt_validator: None,
            user_service: None,
        }
    }
    pub fn with_jwt(mut self, v: JwtValidator) -> Self {
        self.jwt_validator = Some(v);
        self
    }
    pub fn with_user_service(mut self, s: UserService) -> Self {
        self.user_service = Some(s);
        self
    }
    pub fn with_pool(pool: Arc<DbRouter>) -> Self {
        Self {
            produce_ai_key_validator: ProduceAiKeyValidator::with_pool(Arc::clone(&pool)),
            jwt_validator: None,
            user_service: Some(UserService::with_pool(pool)),
        }
    }
    pub fn set_jwt_validator(&mut self, v: JwtValidator) {
        self.jwt_validator = Some(v)
    }
    pub fn get_jwt_validator(&self) -> Option<&JwtValidator> {
        self.jwt_validator.as_ref()
    }
    pub async fn verify_api_key(&self, key: &str) -> Result<AuthContext> {
        self.produce_ai_key_validator.validate(key).await
    }
    pub fn verify_jwt(&self, token: &str) -> Result<AuthContext> {
        self.jwt_validator
            .as_ref()
            .ok_or_else(|| KeyComputeError::AuthError("JWT validation not configured".into()))?
            .validate(token)
    }
    pub async fn verify_token(&self, token: &str) -> Result<AuthContext> {
        if token.starts_with("sk-") {
            return self.verify_api_key(token).await;
        }
        let mut ctx = self.verify_jwt(token)?;
        let service = self.user_service.as_ref().ok_or_else(|| {
            KeyComputeError::ServiceUnavailable("authentication storage is not configured".into())
        })?;
        if let Some(tenant_id) = ctx.selected_tenant_id {
            // One writer snapshot provides all authorization-bearing fields.
            // Never combine an old global role with a newer membership read.
            let identity = service
                .load_jwt_identity(ctx.user_id, Some(tenant_id))
                .await?
                .filter(|identity| identity.active)
                .ok_or_else(|| {
                    KeyComputeError::AuthError("active tenant membership required".into())
                })?;
            if identity.token_version != ctx.token_version
                || Some(identity.authz_version) != ctx.authz_version
                || Some(identity.membership_authz_version) != ctx.membership_authz_version
            {
                return Err(KeyComputeError::AuthError(
                    "authorization version is stale".into(),
                ));
            }
            ctx.platform_role = identity
                .platform_role
                .parse()
                .map_err(KeyComputeError::DatabaseError)?;
            ctx.tenant_role = Some(
                identity
                    .tenant_role
                    .parse()
                    .map_err(KeyComputeError::DatabaseError)?,
            );
            ctx.user_info = Some(UserInfo::new(
                identity.user_id,
                identity.email,
                identity.user_name.unwrap_or_default(),
                ctx.platform_role,
                keycompute_types::UserStatus::Active,
                identity.token_version,
            ));
            ctx.tenant_info = Some(TenantInfo {
                id: tenant_id,
                name: identity.tenant_name,
                slug: identity.tenant_slug,
                active: true,
                config: TenantConfig {
                    default_rpm_limit: identity.default_rpm_limit.max(0) as u32,
                    default_tpm_limit: identity.default_tpm_limit.max(0) as u32,
                },
            });
        } else {
            let user = service.load_user(ctx.user_id).await?;
            if user.token_version != ctx.token_version {
                return Err(KeyComputeError::AuthError(
                    "token has been invalidated".into(),
                ));
            }
            ctx.platform_role = user.platform_role;
            ctx.user_info = Some(user);
        }
        ctx.permissions = permissions_for(ctx.credential_kind, ctx.platform_role, ctx.tenant_role);
        Ok(ctx)
    }
    pub async fn load_user_details(&self, ctx: &mut AuthContext) -> Result<()> {
        if let Some(s) = &self.user_service {
            ctx.user_info = Some(s.load_user(ctx.user_id).await?)
        }
        Ok(())
    }
    pub async fn load_tenant_details(&self, ctx: &mut AuthContext) -> Result<()> {
        let Some(t) = ctx.selected_tenant_id else {
            return Ok(());
        };
        if let Some(s) = &self.user_service {
            ctx.tenant_info = Some(s.load_tenant(t).await?)
        }
        Ok(())
    }
    pub async fn load_full_context(&self, ctx: &mut AuthContext) -> Result<()> {
        self.load_user_details(ctx).await?;
        self.load_tenant_details(ctx).await
    }
    pub async fn verify_api_key_with_context(&self, key: &str) -> Result<AuthContext> {
        let mut c = self.verify_api_key(key).await?;
        self.load_full_context(&mut c).await?;
        Ok(c)
    }
    pub async fn validate_user_tenant(&self, user: Uuid, tenant: Uuid) -> Result<()> {
        self.user_service
            .as_ref()
            .ok_or_else(|| {
                KeyComputeError::AuthError("authentication service not configured".into())
            })?
            .load_user_with_tenant_validation(user, tenant)
            .await
            .map(|_| ())
    }
    pub async fn is_tenant_active(&self, tenant: Uuid) -> Result<bool> {
        Ok(self
            .user_service
            .as_ref()
            .ok_or_else(|| {
                KeyComputeError::AuthError("authentication service not configured".into())
            })?
            .load_tenant(tenant)
            .await?
            .is_active())
    }
    pub fn has_pool(&self) -> bool {
        self.produce_ai_key_validator.has_pool()
    }
}
