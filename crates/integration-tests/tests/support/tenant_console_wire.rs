//! The real Rust SDK speaks to the real router and PostgreSQL, not invented JSON.
use super::*;
use client_api::api::{
    auth::SelectTenantRequest,
    tenant_control::{self as client, CreateInvitation, InvitationToken, MemberPatch, TenantPatch},
};
use client_api::{ApiClient, AuthApi, ClientConfig, TenantControlApi, UserApi};
use keycompute_types::{MembershipStatus, TenantRole};
use uuid::Uuid;

struct Server {
    client: ApiClient,
    handle: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
async fn serve(f: &InvitationFixture) -> Server {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", socket.local_addr().unwrap());
    let app = create_router(f.state.clone());
    let handle = tokio::spawn(async move {
        axum::serve(socket, app).await.unwrap();
    });
    let client = ApiClient::new(ClientConfig::new(endpoint).with_no_proxy(true)).unwrap();
    Server { client, handle }
}
async fn global_token(f: &InvitationFixture, user: Uuid) -> String {
    let u = User::find_by_id(&f.db, user).await.unwrap().unwrap();
    f.state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user, None, u.token_version, None, None, 3600)
        .unwrap()
}
async fn selected(f: &InvitationFixture, api: &ApiClient, user: Uuid) -> String {
    AuthApi::new(api)
        .select_tenant(
            &SelectTenantRequest::new(f.tenant.id.to_string()),
            &global_token(f, user).await,
        )
        .await
        .unwrap()
        .access_token
}
#[tokio::test]
async fn sdk_accepts_actual_session_membership_invitation_and_audit_contracts() {
    let mut f = InvitationFixture::new().await;
    let server = serve(&f).await;
    let api = &server.client;
    let control = TenantControlApi::new(api, f.tenant.id).unwrap();
    let token = selected(&f, api, f.inviter.id).await;
    let profile = UserApi::new(api).get_current_user(&token).await.unwrap();
    assert_eq!(
        profile.selected_tenant.unwrap().tenant_role,
        TenantRole::Admin
    );
    let context = control.context(&token).await.unwrap();
    assert_eq!(context.owner_user_id, f.inviter.id);
    let members = control.members(1, 20, None, &token).await.unwrap();
    assert!(
        members
            .items
            .iter()
            .any(|m| m.user_id == f.inviter.id && m.tenant_role == TenantRole::Admin)
    );
    let command = CreateInvitation {
        email: f.invitee.email.clone(),
        tenant_role: TenantRole::Member,
        expires_in_seconds: 3600,
    };
    let created = control.create_invitation(&command, &token).await.unwrap();
    assert_eq!(created.outcome, client::InvitationOutcome::Created);
    assert_eq!(
        created.notification,
        client::NotificationStatus::Unconfigured
    );
    let link = created.acceptance_link.as_ref().unwrap();
    assert!(link.contains("/invite#token="));
    let secret = link.split('#').nth(1).unwrap();
    let invitation = InvitationToken::from_fragment(&format!("#{secret}")).unwrap();
    assert!(!format!("{created:?} {invitation:?}").contains(secret));
    let duplicate = control.create_invitation(&command, &token).await.unwrap();
    assert_eq!(duplicate.invitation.id, created.invitation.id);
    assert!(duplicate.acceptance_link.is_none());
    let pending = control.invitations(1, 20, &token).await.unwrap();
    assert!(pending.items.iter().any(|i| i.id == created.invitation.id));
    let accepted = client::accept_invitation(api, &invitation, &f.invitee_global_token)
        .await
        .unwrap();
    assert_eq!(accepted.tenant.id, f.tenant.id);
    assert_eq!(accepted.tenant.owner_user_id, f.inviter.id);
    assert_eq!(
        accepted.membership.membership_status,
        MembershipStatus::Active
    );
    let member_token = selected(&f, api, f.invitee.id).await;
    let member_profile = UserApi::new(api)
        .get_current_user(&member_token)
        .await
        .unwrap();
    assert_eq!(
        member_profile.selected_tenant.unwrap().tenant_role,
        TenantRole::Member
    );
    assert!(control.invitations(1, 20, &member_token).await.is_err());
    let used = client::accept_invitation(api, &invitation, &f.invitee_global_token)
        .await
        .unwrap_err();
    assert!(!format!("{used:?}").contains(secret));
    let token = selected(&f, api, f.inviter.id).await;
    let audit = control.audit(1, 100, &token).await.unwrap();
    assert!(
        audit
            .items
            .iter()
            .any(|event| event.action.contains("invitation") && event.request_id.is_some())
    );
    drop(server);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn sdk_member_versions_owner_transfer_and_tenant_selection_are_real_server_boundaries() {
    let mut f = InvitationFixture::new().await;
    let peer = create_test_user(&f.db, f.tenant.id, "console-peer", &f.run).await;
    let other = create_test_tenant(&f.db, "console-foreign", &f.run).await;
    let server = serve(&f).await;
    let api = &server.client;
    let control = TenantControlApi::new(api, f.tenant.id).unwrap();
    let token = selected(&f, api, f.inviter.id).await;
    let peer_before = control.member(peer.id, &token).await.unwrap();
    let promoted = control
        .patch_member(
            peer.id,
            &MemberPatch {
                expected_authz_version: peer_before.authz_version,
                tenant_role: Some(TenantRole::Admin),
                status: None,
            },
            &token,
        )
        .await
        .unwrap();
    assert!(promoted.authz_version > peer_before.authz_version);
    assert!(
        control
            .remove_member(peer.id, peer_before.authz_version, &token)
            .await
            .is_err()
    );
    let owner = control.member(f.inviter.id, &token).await.unwrap();
    assert!(
        control
            .remove_member(owner.user_id, owner.authz_version, &token)
            .await
            .is_err()
    );
    assert!(
        TenantControlApi::new(api, other.id)
            .unwrap()
            .context(&token)
            .await
            .is_err()
    );
    let next = control.transfer_ownership(peer.id, &token).await.unwrap();
    assert_eq!(next.owner_user_id, peer.id);
    assert!(
        control.context(&token).await.is_err(),
        "ownership change invalidates old selected JWT"
    );
    let fresh = selected(&f, api, f.inviter.id).await;
    let before = control.context(&fresh).await.unwrap();
    let after = control
        .patch_context(
            &TenantPatch {
                expected_authz_version: before.authz_version,
                name: Some("Console contract tenant".into()),
                description: Some("Updated using the actual SDK".into()),
                default_rpm_limit: Some(70),
                default_tpm_limit: Some(1000),
            },
            &fresh,
        )
        .await
        .unwrap();
    assert_eq!(after.name, "Console contract tenant");
    assert!(after.authz_version > before.authz_version);
    assert!(control.context(&fresh).await.is_err());
    let global = AuthApi::new(api)
        .select_tenant(
            &SelectTenantRequest::global(),
            &global_token(&f, f.inviter.id).await,
        )
        .await
        .unwrap();
    assert!(global.selected_tenant.is_none());
    assert!(global.capabilities.tenant.is_empty());
    drop(server);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn sdk_revoke_and_member_suspension_preserve_current_role_and_token_boundaries() {
    let mut f = InvitationFixture::new().await;
    let peer = create_test_user(&f.db, f.tenant.id, "console-suspended", &f.run).await;
    let server = serve(&f).await;
    let api = &server.client;
    let control = TenantControlApi::new(api, f.tenant.id).unwrap();
    let token = selected(&f, api, f.inviter.id).await;
    let peer_token = selected(&f, api, peer.id).await;
    let invited = control
        .create_invitation(
            &CreateInvitation {
                email: f.invitee.email.clone(),
                tenant_role: TenantRole::Member,
                expires_in_seconds: 3600,
            },
            &token,
        )
        .await
        .unwrap();
    let link = invited.acceptance_link.unwrap();
    let invitation =
        InvitationToken::from_fragment(&format!("#{}", link.split('#').nth(1).unwrap())).unwrap();
    let revoked = control
        .revoke_invitation(invited.invitation.id, &token)
        .await
        .unwrap();
    assert_eq!(revoked.status, "revoked");
    assert!(revoked.revoked_at.is_some());
    assert!(
        client::accept_invitation(api, &invitation, &f.invitee_global_token)
            .await
            .is_err()
    );
    let before = control.member(peer.id, &token).await.unwrap();
    let suspended = control
        .patch_member(
            peer.id,
            &MemberPatch {
                expected_authz_version: before.authz_version,
                tenant_role: None,
                status: Some(MembershipStatus::Suspended),
            },
            &token,
        )
        .await
        .unwrap();
    assert_eq!(suspended.membership_status, MembershipStatus::Suspended);
    assert!(
        UserApi::new(api)
            .get_current_user(&peer_token)
            .await
            .is_err()
    );
    let restored = control
        .patch_member(
            peer.id,
            &MemberPatch {
                expected_authz_version: suspended.authz_version,
                tenant_role: None,
                status: Some(MembershipStatus::Active),
            },
            &token,
        )
        .await
        .unwrap();
    assert!(restored.authz_version > suspended.authz_version);
    assert!(
        UserApi::new(api)
            .get_current_user(&peer_token)
            .await
            .is_err(),
        "restoring a membership cannot revive its old credential"
    );
    let removed = control
        .remove_member(peer.id, restored.authz_version, &token)
        .await
        .unwrap();
    assert_eq!(removed.membership_status, MembershipStatus::Removed);
    assert!(removed.removed_at.is_some());
    assert!(
        control
            .patch_member(
                peer.id,
                &MemberPatch {
                    expected_authz_version: removed.authz_version,
                    tenant_role: None,
                    status: Some(MembershipStatus::Active)
                },
                &token
            )
            .await
            .is_err(),
        "removed membership must follow a fresh invitation"
    );
    drop(server);
    f.guard.cleanup().await.unwrap();
}
