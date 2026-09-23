use super::common::{self, CommandDialog, WorkspaceLinks, WorkspaceScope};
use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::{get_client, user_error_message},
    stores::{
        auth_store::{AuthState, AuthStore},
        ui_store::UiStore,
        user_store::{UserInfo, UserStore},
    },
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::{
    AuthApi, ClientError,
    api::{
        auth::{AuthResponse, SelectTenantRequest},
        tenant_control::{TenantContext, TenantPatch},
    },
};
use dioxus::prelude::*;

pub(super) fn install_selection(
    mut auth: AuthStore,
    mut users: UserStore,
    observed: &AuthState,
    user: &str,
    target: Option<&str>,
    response: AuthResponse,
) -> bool {
    if !auth.select_session_if_current(observed, user, target, &response) {
        return false;
    }
    users.info.set(Some(UserInfo {
        id: response.user_id,
        email: response.email,
        name: response.name,
        platform_role: response.platform_role,
        status: response.status,
        memberships: response.memberships,
        selected_tenant: response.selected_tenant,
        capabilities: response.capabilities,
    }));
    users.loaded_session_id.set(auth.state.peek().session_id);
    users.load_failed.set(false);
    true
}

#[derive(Clone, Default, PartialEq)]
struct ConfigDraft {
    name: String,
    description: String,
    rpm: String,
    tpm: String,
    revision: i64,
}
impl ConfigDraft {
    fn from_context(c: &TenantContext) -> Self {
        Self {
            name: c.name.clone(),
            description: c.description.clone().unwrap_or_default(),
            rpm: c.default_rpm_limit.to_string(),
            tpm: c.default_tpm_limit.to_string(),
            revision: c.authz_version,
        }
    }
    fn patch(&self) -> client_api::Result<TenantPatch> {
        let rpm = self.rpm.trim().parse::<i32>().ok().filter(|v| *v >= 0);
        let tpm = self.tpm.trim().parse::<i32>().ok().filter(|v| *v >= 0);
        if self.revision <= 0
            || self.name.trim().is_empty()
            || self.name.len() > 255
            || self.description.len() > 16 * 1024
            || rpm.is_none()
            || tpm.is_none()
        {
            return Err(ClientError::Config("Invalid tenant configuration".into()));
        }
        Ok(TenantPatch {
            expected_authz_version: self.revision,
            name: Some(self.name.trim().into()),
            description: Some(self.description.clone()),
            default_rpm_limit: rpm,
            default_tpm_limit: tpm,
        })
    }
}

/// The router may reuse an Outlet VNode across AppShell remounts. A page-local
/// key must discard drafts, confirmation dialogs and one-time secrets as well.
#[component]
pub fn TenantWorkspace() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let epoch = (auth.state)().session_id;
    let scope = WorkspaceScope::from_stores(auth, users);
    let key = format!("{epoch}:{scope:?}");
    // A keyed fragment is required: a key on one static component alone
    // does not engage Dioxus's keyed child reconciliation.
    rsx! { for identity in [key] { TenantWorkspacePage { key: "{identity}" } } }
}

#[component]
fn TenantWorkspacePage() -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let nav = use_navigator();
    let mut target = use_signal(|| {
        users
            .info
            .peek()
            .as_ref()
            .and_then(|u| u.active_tenant_id())
            .unwrap_or_default()
            .to_owned()
    });
    let mut switching = use_signal(|| false);
    let mut saving = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut draft = use_signal(ConfigDraft::default);
    let mut dirty = use_signal(|| false);
    let mut pending = use_signal(|| None::<TenantPatch>);
    let scope = WorkspaceScope::from_stores(auth, users);
    let user = (users.info)();
    let can_manage = user.as_ref().is_some_and(UserInfo::can_manage_tenant);
    let mut context = use_resource(move || {
        let scope = WorkspaceScope::from_stores(auth, users);
        async move {
            let result = if let Some(scope) = scope {
                common::read(auth, users, scope, move |token| async move {
                    scope.api()?.context(&token).await
                })
                .await
                .map(Some)
            } else {
                Ok(None)
            };
            KeyedResourceValue::new(scope, result)
        }
    });
    let loaded = current_keyed_value(&scope, context.state().cloned(), context());
    let current = loaded
        .as_ref()
        .and_then(|v| v.as_ref().ok())
        .and_then(|v| v.as_ref())
        .cloned();
    use_effect(move || {
        let scope = WorkspaceScope::from_stores(auth, users);
        let loaded = current_keyed_value(&scope, context.state().cloned(), context());
        if let Some(Ok(Some(value))) = loaded
            && !dirty()
            && draft.peek().revision != value.authz_version
        {
            draft.set(ConfigDraft::from_context(&value));
        }
    });
    let switch = move |_| {
        if switching() || saving() {
            return;
        }
        let observed = auth.state.peek().clone();
        let Some(user) = users
            .info
            .peek()
            .clone()
            .filter(|_| *users.loaded_session_id.peek() == observed.session_id)
        else {
            return;
        };
        let selected = target().trim().to_owned();
        let selected = (!selected.is_empty()).then_some(selected);
        if selected.as_deref() == user.active_tenant_id() {
            return;
        }
        switching.set(true);
        error.set(String::new());
        spawn(async move {
            let selection = selected.clone();
            let request = SelectTenantRequest {
                tenant_id: selection,
            };
            let result = AuthApi::new(&get_client())
                .select_tenant(
                    &request,
                    observed.access_token.as_deref().unwrap_or_default(),
                )
                .await;
            if !auth.same_session(&observed) {
                return;
            }
            match result {
                Ok(response) => {
                    if !install_selection(
                        auth,
                        users,
                        &observed,
                        &user.id,
                        selected.as_deref(),
                        response,
                    ) {
                        error.set(i18n.t("tenant.session_changed").into());
                    } else {
                        nav.replace(Route::TenantWorkspace {});
                    }
                }
                Err(e) => error.set(user_error_message(&e)),
            }
            switching.set(false);
        });
    };
    let save = move |_| match draft().patch() {
        Ok(body) => pending.set(Some(body)),
        Err(_) => error.set(i18n.t("tenant.invalid_config").into()),
    };
    let confirm = move |_| {
        if saving() {
            return;
        }
        let (Some(scope), Some(body)) = (WorkspaceScope::from_stores(auth, users), pending())
        else {
            return;
        };
        let expected = body.expected_authz_version;
        saving.set(true);
        error.set(String::new());
        spawn(async move {
            let result = common::command(auth, users, scope, move |token| async move {
                scope.api()?.patch_context(&body, &token).await
            })
            .await;
            if !scope.is_current(auth, users) {
                return;
            }
            saving.set(false);
            pending.set(None);
            match result {
                Ok(row) if row.authz_version != expected => {
                    common::invalidate_own_session(
                        auth,
                        users,
                        scope,
                        ui,
                        i18n.t("tenant.saved_relogin"),
                    );
                    nav.replace(Route::Login {});
                }
                Ok(_) => {
                    dirty.set(false);
                    context.restart();
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {div {class:"page-container tenant-workspace",
        ui::PageHeader {title:i18n.t("tenant.workspace").to_string(),description:i18n.t("tenant.workspace_hint").to_string()}
        WorkspaceLinks {}
        if !error().is_empty(){div {class:"alert alert-error",role:"alert","{error}"}}
        section {class:"section",aria_label:i18n.t("tenant.switch"),
            h2 {class:"section-title",{i18n.t("tenant.current")}}
            if let Some(tenant)=user.as_ref().and_then(|u|u.selected_tenant.as_ref()) {
                p {"{tenant.name.as_deref().unwrap_or(&tenant.id)} · {tenant.id} · {tenant.tenant_role.as_str()}"}
            } else {p {{i18n.t("tenant.global")}}}
            label {class:"form-label",r#for:"workspace-select",{i18n.t("tenant.switch")}}
            select {id:"workspace-select",class:"input-field",value:"{target}",disabled:switching()||saving(),onchange:move |e|target.set(e.value()),
                option {value:"",{i18n.t("tenant.global")}}
                if let Some(user)=user.as_ref(){
                    for membership in user.memberships.iter().filter(|m|m.status.as_deref()==Some("active")) {
                        option {value:"{membership.tenant_id}","{membership.tenant_name.as_deref().unwrap_or(&membership.tenant_id)} · {membership.tenant_role.as_str()}"}
                    }
                }
            }
            button {class:"btn btn-primary",disabled:switching()||saving(),onclick:switch,{i18n.t("tenant.switch")}}
        }
        if let Some(value)=current {
            section {class:"section",aria_label:i18n.t("tenant.config"),
                h2 {class:"section-title",{i18n.t("tenant.config")}}
                p {class:"text-secondary",{i18n.t("tenant.owner")} ": {value.owner_user_id}"}
                label {class:"form-label",r#for:"tenant-name",{i18n.t("tenants.name")}}
                input {id:"tenant-name",class:"input-field",value:"{draft().name}",maxlength:"255",disabled:!can_manage||saving(),oninput:move |e|{draft.write().name=e.value();dirty.set(true);}}
                label {class:"form-label",r#for:"tenant-description",{i18n.t("tenant.description")}}
                textarea {id:"tenant-description",class:"input-field",value:"{draft().description}",maxlength:"16384",disabled:!can_manage||saving(),oninput:move |e|{draft.write().description=e.value();dirty.set(true);}}
                label {class:"form-label",r#for:"tenant-rpm","RPM"}
                input {id:"tenant-rpm",class:"input-field",r#type:"number",min:"0",value:"{draft().rpm}",disabled:!can_manage||saving(),oninput:move |e|{draft.write().rpm=e.value();dirty.set(true);}}
                label {class:"form-label",r#for:"tenant-tpm","TPM"}
                input {id:"tenant-tpm",class:"input-field",r#type:"number",min:"0",value:"{draft().tpm}",disabled:!can_manage||saving(),oninput:move |e|{draft.write().tpm=e.value();dirty.set(true);}}
                if can_manage {button {class:"btn btn-primary",disabled:saving()||switching()||!dirty(),onclick:save,{i18n.t("tenant.save")}}}
                button {class:"btn btn-secondary",disabled:saving()||switching(),onclick:move |_|{pending.set(None);dirty.set(false);draft.set(ConfigDraft::default());context.restart();},{i18n.t("tenant.reload")}}
            }
        } else if scope.is_some() {
            if let Some(Err(e))=loaded {div {class:"alert alert-error",role:"alert",{user_error_message(&e)}}}
            else {p {role:"status",{i18n.t("common.loading")}}}
        }
        if pending().is_some() {CommandDialog {title:i18n.t("tenant.save").to_string(),target:i18n.t("tenant.config_relogin").to_string(),busy:saving(),on_cancel:move |_|pending.set(None),on_confirm:confirm}}
    }}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_validation_does_not_guess_revisions_or_permit_negative_limits() {
        let mut value = ConfigDraft {
            name: "Example".into(),
            description: String::new(),
            rpm: "60".into(),
            tpm: "1000".into(),
            revision: 1,
        };
        assert!(value.patch().is_ok());
        value.rpm = "-1".into();
        assert!(value.patch().is_err());
        value.rpm = "60".into();
        value.revision = 0;
        assert!(value.patch().is_err());
        value.revision = 1;
        value.name = " ".into();
        assert!(value.patch().is_err());
    }
}
