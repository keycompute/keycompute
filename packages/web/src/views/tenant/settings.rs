use super::{
    WorkspaceDraftState,
    common::{self, CommandDialog, TechnicalId, WorkspaceContext, WorkspaceLinks, WorkspaceScope},
};
use crate::{
    hooks::use_i18n::use_i18n,
    router::Route,
    services::api_client::user_error_message,
    stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::{
    ClientError,
    api::tenant_control::{TenantContext, TenantPatch},
};
use dioxus::prelude::*;
use uuid::Uuid;

#[derive(Clone, Default, PartialEq)]
struct ConfigDraft {
    name: String,
    description: String,
    rpm: String,
    tpm: String,
    revision: i64,
}

impl ConfigDraft {
    fn from_context(context: &TenantContext) -> Self {
        Self {
            name: context.name.clone(),
            description: context.description.clone().unwrap_or_default(),
            rpm: context.default_rpm_limit.to_string(),
            tpm: context.default_tpm_limit.to_string(),
            revision: context.revision,
        }
    }

    fn patch(&self) -> client_api::Result<TenantPatch> {
        let rpm = self
            .rpm
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|value| *value >= 0);
        let tpm = self
            .tpm
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|value| *value >= 0);
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
            expected_revision: self.revision,
            name: Some(self.name.trim().into()),
            description: Some(self.description.clone()),
            default_rpm_limit: rpm,
            default_tpm_limit: tpm,
        })
    }
}

#[component]
pub fn TenantSettings() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let epoch = (auth.state)().session_id;
    let scope = WorkspaceScope::from_stores(auth, users);
    let key = format!("{epoch}:{scope:?}");
    rsx! {
        if let Some(scope) = scope {
            for identity in [key] {
                TenantSettingsPage {
                    key: "{identity}",
                    scope,
                    embedded: false,
                    on_context_changed: None,
                }
            }
        }
    }
}

#[component]
pub(crate) fn PlatformTenantSettings(
    scope: WorkspaceScope,
    on_context_changed: EventHandler<()>,
) -> Element {
    rsx! { TenantSettingsPage { scope, embedded: true, on_context_changed: Some(on_context_changed) } }
}

#[component]
fn TenantSettingsPage(
    scope: WorkspaceScope,
    embedded: bool,
    on_context_changed: Option<EventHandler<()>>,
) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let mut users = use_context::<UserStore>();
    let mut ui = use_context::<UiStore>();
    let shared_dirty = use_context::<WorkspaceDraftState>();
    let draft_owner = use_hook(Uuid::new_v4);
    let mut saving = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut draft = use_signal(ConfigDraft::default);
    let mut dirty = use_signal(|| false);
    let mut pending = use_signal(|| None::<TenantPatch>);
    let mut reload_pending = use_signal(|| false);
    use_drop(move || shared_dirty.set(draft_owner, false));

    let mut context = use_resource(move || async move {
        let result = common::read(auth, users, scope, move |token| async move {
            scope.api()?.context(&token).await
        })
        .await;
        KeyedResourceValue::new(scope, result)
    });
    let loaded = current_keyed_value(&scope, context.state().cloned(), context());
    let current = loaded
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .cloned();
    use_effect(move || {
        let loaded = current_keyed_value(&scope, context.state().cloned(), context());
        if let Some(Ok(value)) = loaded
            && !dirty()
            && draft.peek().revision != value.revision
        {
            draft.set(ConfigDraft::from_context(&value));
        }
    });
    use_effect(move || shared_dirty.set(draft_owner, dirty()));

    let save = move |_| match draft().patch() {
        Ok(body) => pending.set(Some(body)),
        Err(_) => error.set(i18n.t("tenant.invalid_config").into()),
    };
    let confirm = move |_| {
        if saving() {
            return;
        }
        let Some(body) = pending() else {
            return;
        };
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
                Ok(row) => {
                    if !scope.is_platform_target()
                        && let Some(user) = users.info.write().as_mut()
                    {
                        user.sync_tenant_display(
                            &row.id.to_string(),
                            row.name.as_str(),
                            row.slug.as_str(),
                        );
                    }
                    draft.set(ConfigDraft::from_context(&row));
                    dirty.set(false);
                    shared_dirty.set(draft_owner, false);
                    context.restart();
                    if let Some(handler) = on_context_changed {
                        handler.call(());
                    }
                    ui.show_success(i18n.t("tenant.saved"));
                }
                Err(value) => error.set(user_error_message(i18n, &value)),
            }
        });
    };
    let reload = move |_| {
        if dirty() {
            reload_pending.set(true);
            return;
        }
        pending.set(None);
        error.set(String::new());
        draft.set(ConfigDraft::default());
        context.restart();
        if let Some(handler) = on_context_changed {
            handler.call(());
        }
    };
    let confirm_reload = move |_| {
        reload_pending.set(false);
        pending.set(None);
        error.set(String::new());
        dirty.set(false);
        shared_dirty.set(draft_owner, false);
        draft.set(ConfigDraft::default());
        context.restart();
        if let Some(handler) = on_context_changed {
            handler.call(());
        }
    };

    rsx! {
        div { class: if embedded { "tenant-workspace tenant-settings embedded-panel" } else { "page-container tenant-workspace tenant-settings" },
            if !embedded {
                ui::PageHeader { title: i18n.t("tenant.settings").to_string(), description: i18n.t("tenant.settings_hint").to_string() }
                WorkspaceLinks {}
                WorkspaceContext {}
            } else {
                h2 { class: "section-title", {i18n.t("tenants.settings_title")} }
                p { class: "text-secondary", {i18n.t("tenant.settings_hint")} }
            }
            if !error().is_empty() { div { class: "alert alert-error", role: "alert", "{error}" } }
            if let Some(value) = current {
                section { class: "section", aria_label: i18n.t("tenant.settings"),
                    h2 { class: "section-title", {i18n.t("tenant.general_settings")} }
                    div { class: "section-body tenant-settings-form",
                        dl { class: "detail-grid tenant-settings-metadata",
                            div { dt { {i18n.t("tenants.tenant_id")} } dd { TechnicalId { value: value.id.to_string() } } }
                            div { dt { {i18n.t("tenants.slug")} } dd { code { "{value.slug}" } } }
                            div { dt { {i18n.t("table.status")} } dd { span { class: "badge", "{value.status}" } } }
                        }
                        if embedded { Link { class: "btn btn-secondary btn-sm", to: Route::PlatformTenantAccess { tenant_id: value.id.to_string() }, {i18n.t("tenants.manage_access")} } }
                        div { class: "form-field",
                            label { class: "form-label", r#for: "tenant-name", {i18n.t("tenants.name")} }
                            input { id: "tenant-name", class: "input-field", value: "{draft().name}", maxlength: "255", disabled: saving(), oninput: move |event| { draft.write().name = event.value(); dirty.set(true); } }
                        }
                        div { class: "form-field",
                            label { class: "form-label", r#for: "tenant-description", {i18n.t("tenant.description")} }
                            textarea { id: "tenant-description", class: "input-field", rows: "4", value: "{draft().description}", maxlength: "16384", disabled: saving(), oninput: move |event| { draft.write().description = event.value(); dirty.set(true); } }
                        }
                        div { class: "tenant-settings-limit-heading",
                            h3 { {i18n.t("tenant.default_limits")} }
                            p { class: "text-secondary", {i18n.t("tenant.default_limits_hint")} }
                        }
                        div { class: "tenant-settings-limit-grid",
                            div { class: "form-field",
                                label { class: "form-label", r#for: "tenant-rpm", "RPM" }
                                input { id: "tenant-rpm", class: "input-field", r#type: "number", min: "0", value: "{draft().rpm}", disabled: saving(), oninput: move |event| { draft.write().rpm = event.value(); dirty.set(true); } }
                            }
                            div { class: "form-field",
                                label { class: "form-label", r#for: "tenant-tpm", "TPM" }
                                input { id: "tenant-tpm", class: "input-field", r#type: "number", min: "0", value: "{draft().tpm}", disabled: saving(), oninput: move |event| { draft.write().tpm = event.value(); dirty.set(true); } }
                            }
                        }
                        div { class: "tenant-settings-actions",
                            button { class: "btn btn-primary", disabled: saving() || !dirty(), onclick: save, {i18n.t("tenant.save")} }
                            button { class: "btn btn-secondary", disabled: saving(), onclick: move |_| { pending.set(None); error.set(String::new()); dirty.set(false); shared_dirty.set(draft_owner, false); draft.set(ConfigDraft::from_context(&value)); }, {i18n.t("form.cancel")} }
                            button { class: "btn btn-ghost", disabled: saving(), onclick: reload, {i18n.t("tenant.reload")} }
                        }
                    }
                }
            } else if let Some(Err(value)) = loaded {
                div { class: "alert alert-error", role: "alert",
                    p { {user_error_message(i18n, &value)} }
                    button { class: "btn btn-secondary", onclick: move |_| { context.restart(); if let Some(handler) = on_context_changed { handler.call(()); } }, {i18n.t("tenant.reload")} }
                }
            } else {
                p { role: "status", {i18n.t("common.loading")} }
            }
            if pending().is_some() {
                CommandDialog { title: i18n.t("tenant.save").to_string(), target: i18n.t("tenant.config_relogin").to_string(), busy: saving(), on_cancel: move |_| pending.set(None), on_confirm: confirm }
            }
            if reload_pending() {
                CommandDialog { title: i18n.t("tenant.reload").to_string(), target: i18n.t("tenant.reload_discard_confirm").to_string(), busy: false, on_cancel: move |_| reload_pending.set(false), on_confirm: confirm_reload }
            }
        }
    }
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
    }
}
