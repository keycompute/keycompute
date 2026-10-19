//! Test-only real identity fixture. Never used by a production authentication path.
use crate::{AppState, extractors::AuthExtractor};
use keycompute_db::{
    AuditContext, CreateTenantRequest, CreateUserRequest, DbRouter, Tenant, User, initialize_schema,
};
use keycompute_types::{CredentialKind, PlatformRole, TenantRole, UserStatus};
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, TransactionTrait,
};
use uuid::Uuid;

pub(crate) struct TestIdentity {
    pub state: AppState,
    pub auth: Option<AuthExtractor>,
    pub token: String,
    admin: DatabaseConnection,
    database_name: String,
}
impl TestIdentity {
    pub async fn member() -> Self {
        Self::with_roles(PlatformRole::None, TenantRole::Member).await
    }
    pub async fn with_roles(platform: PlatformRole, tenant_role: TenantRole) -> Self {
        assert!(
            std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
                || std::env::var_os("CI").is_some(),
            "real identity regression requires acknowledged isolated PostgreSQL or CI"
        );
        let url = isolated_database_url();
        assert!(
            url.contains("@127.0.0.1:") || url.contains("@localhost:"),
            "local test PostgreSQL required"
        );
        let (prefix, _) = url.rsplit_once('/').expect("database name required");
        let database_name = format!("kc_server_identity_{}", Uuid::new_v4().simple());
        let admin = Database::connect(&url).await.unwrap();
        admin
            .execute_unprepared(&format!("CREATE DATABASE {database_name}"))
            .await
            .unwrap();
        let db = Database::connect(format!("{prefix}/{database_name}"))
            .await
            .unwrap();
        initialize_schema(&db).await.unwrap();
        let tx = db.begin().await.unwrap();
        let root = User::bootstrap_root(&tx, "root@unit.invalid", None)
            .await
            .unwrap();
        let root_actor = AuditContext {
            actor_user_id: root.id,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: PlatformRole::Root,
            actor_tenant_role: None,
            request_id: None,
        };
        let owner = User::create(
            &tx,
            &CreateUserRequest {
                email: "owner@unit.invalid".into(),
                name: None,
            },
        )
        .await
        .unwrap();
        let tenant = Tenant::create_with_admin(
            &tx,
            &CreateTenantRequest {
                name: "Identity fixture".into(),
                slug: "identity-fixture".into(),
                description: None,
                default_rpm_limit: None,
                default_tpm_limit: None,
            },
            owner.id,
            &root_actor,
        )
        .await
        .unwrap();
        let user = match platform {
            PlatformRole::Root => root,
            PlatformRole::Operator => {
                let operator = User::create(
                    &tx,
                    &CreateUserRequest {
                        email: "operator@unit.invalid".into(),
                        name: None,
                    },
                )
                .await
                .unwrap();
                User::set_security(
                    &tx,
                    operator.id,
                    PlatformRole::Operator,
                    UserStatus::Active,
                    &root_actor,
                )
                .await
                .unwrap()
            }
            PlatformRole::None => User::create(
                &tx,
                &CreateUserRequest {
                    email: "member@unit.invalid".into(),
                    name: None,
                },
            )
            .await
            .unwrap(),
        };
        if platform == PlatformRole::None {
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status) VALUES($1,$2,$3,'active')",
                [tenant.id.into(),user.id.into(),tenant_role.as_str().into()],
            )).await.unwrap();
        }
        tx.commit().await.unwrap();
        let state = AppState::with_pool(DbRouter::single(db));
        let selected_tenant = (platform == PlatformRole::None).then_some(tenant.id);
        let token = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(
                user.id,
                selected_tenant,
                user.token_version,
                selected_tenant.map(|_| tenant.authz_version),
                selected_tenant.map(|_| 1),
                3600,
            )
            .unwrap();
        let auth = if selected_tenant.is_some() {
            Some(
                AuthExtractor::from_auth_context(state.auth.verify_token(&token).await.unwrap())
                    .unwrap(),
            )
        } else {
            None
        };
        Self {
            state,
            auth,
            token,
            admin,
            database_name,
        }
    }
    pub async fn finish(self) {
        assert!(self.database_name.starts_with("kc_server_identity_"));
        drop(self.state);
        self.admin
            .execute_unprepared(&format!(
                "DROP DATABASE {} WITH (FORCE)",
                self.database_name
            ))
            .await
            .unwrap();
        self.admin.close().await.unwrap();
    }
}

fn isolated_database_url() -> String {
    std::env::var("DATABASE_URL")
        .or_else(|_| std::env::var("KC__DATABASE__URL"))
        .expect("set DATABASE_URL to an explicitly acknowledged isolated PostgreSQL")
}
