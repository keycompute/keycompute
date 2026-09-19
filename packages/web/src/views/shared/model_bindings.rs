//! Administration UI for tenant model bindings.
//!
//! A binding is deliberately edited as an account id plus an optimistic
//! revision.  The page never accepts an endpoint or credential and always
//! asks the server to re-check tenant visibility, protocol and model health.

use client_api::api::admin::{
    CreateModelBindingRequest, ModelBindingInfo, ModelBindingProbeRequest, ModelBindingQueryParams,
    UpdateModelBindingRequest,
};
use dioxus::prelude::*;
use ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, ConfirmModal, PageHeader, Table,
    TableHead,
};

use crate::hooks::use_i18n::use_i18n;
use crate::services::{
    account_service,
    api_client::{user_error_message, with_auto_refresh},
};
use crate::stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore};
use crate::utils::display::short_id;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::views::shared::accounts::NoPermissionView;

const PAGE_SIZE: u32 = 20;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct BindingListKey {
    page: u32,
    model: String,
    refresh: u32,
}

/// Admin-only model binding management page.
#[component]
pub fn ModelBindings() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let mut ui_store = use_context::<UiStore>();
    if !user_store.is_admin() {
        return rsx! { NoPermissionView { resource: i18n.t("page.model_bindings").to_string() } };
    }

    let tenant_id = user_store
        .info
        .read()
        .as_ref()
        .map(|user| user.tenant_id.clone())
        .unwrap_or_default();
    let mut page = use_signal(|| 1u32);
    let mut model_filter = use_signal(String::new);
    let mut refresh = use_signal(|| 0u32);
    let mut show_form = use_signal(|| false);
    let mut editing = use_signal(|| None::<ModelBindingInfo>);
    let mut delete_candidate = use_signal(|| None::<ModelBindingInfo>);
    let mut delete_open = use_signal(|| false);
    let mut operation_error = use_signal(String::new);
    let mut form_model = use_signal(String::new);
    let mut form_account_id = use_signal(String::new);
    let mut form_tenant_id = use_signal(|| tenant_id.clone());
    let mut form_enabled = use_signal(|| true);
    let mut saving = use_signal(|| false);
    let mut pending_id = use_signal(|| None::<String>);

    let bindings = use_resource(move || {
        let key = BindingListKey {
            page: page(),
            model: model_filter(),
            refresh: refresh(),
        };
        async move {
            let mut params = ModelBindingQueryParams::new()
                .with_page(key.page)
                .with_page_size(PAGE_SIZE)
                .with_api_capability("chat_completions");
            if !key.model.trim().is_empty() {
                params = params.with_model(key.model.clone());
            }
            let result = with_auto_refresh(auth_store, move |token| {
                let params = params.clone();
                async move { account_service::list_model_bindings(Some(params), &token).await }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });

    let open_create = move |_| {
        if saving() || pending_id().is_some() {
            return;
        }
        editing.set(None);
        form_model.set(String::new());
        form_account_id.set(String::new());
        form_tenant_id.set(tenant_id.clone());
        form_enabled.set(true);
        operation_error.set(String::new());
        show_form.set(true);
    };

    let submit = move |_| {
        if saving() || pending_id().is_some() {
            return;
        }
        let model = form_model().trim().to_string();
        let account_id = form_account_id().trim().to_string();
        let tenant = form_tenant_id().trim().to_string();
        if model.is_empty() || account_id.is_empty() || (editing().is_none() && tenant.is_empty()) {
            operation_error.set(i18n.t("model_bindings.required").to_string());
            return;
        }
        let auth = auth_store;
        let existing = editing();
        let enabled = form_enabled();
        saving.set(true);
        operation_error.set(String::new());
        spawn(async move {
            let result = if let Some(binding) = existing {
                let mut request =
                    UpdateModelBindingRequest::new(binding.revision).with_account_id(account_id);
                request = request.with_enabled(enabled).with_model(model);
                with_auto_refresh(auth, move |token| {
                    let request = request.clone();
                    let id = binding.id.clone();
                    async move { account_service::update_model_binding(&id, request, &token).await }
                })
                .await
                .map(|_| ())
            } else {
                let request =
                    CreateModelBindingRequest::new(tenant, model, account_id).with_enabled(enabled);
                with_auto_refresh(auth, move |token| {
                    let request = request.clone();
                    async move { account_service::create_model_binding(request, &token).await }
                })
                .await
                .map(|_| ())
            };
            match result {
                Ok(()) => {
                    show_form.set(false);
                    refresh += 1;
                    ui_store.show_success(i18n.t("model_bindings.saved").to_string());
                }
                Err(error) => operation_error.set(user_error_message(&error)),
            }
            saving.set(false);
        });
    };

    let current_key = BindingListKey {
        page: page(),
        model: model_filter(),
        refresh: refresh(),
    };
    let result = current_keyed_value(&current_key, bindings.state().cloned(), bindings());
    let page_data = result.as_ref().and_then(|value| value.as_ref().ok());
    let rows = page_data
        .map(|value| value.bindings.as_slice())
        .unwrap_or_default();
    let total = page_data.map(|value| value.total).unwrap_or(0);
    let total_pages = page_data.map(|value| value.total_pages.max(1)).unwrap_or(1);

    rsx! {
        div { class: "page-container model-bindings-page",
            PageHeader {
                title: i18n.t("page.model_bindings").to_string(),
                description: i18n.t("model_bindings.subtitle").to_string(),
                actions: rsx! {
                    Button { variant: ButtonVariant::Primary, disabled: saving() || pending_id().is_some(), onclick: open_create,
                        {i18n.t("model_bindings.create")}
                    }
                },
            }
            if !operation_error().is_empty() {
                div { class: "alert alert-error", "{operation_error}" }
            }
            if let Some(Err(error)) = result.as_ref() {
                div { class: "alert alert-error", {user_error_message(error)} }
            } else if result.is_none() {
                div { class: "text-secondary", {i18n.t("common.loading")} }
            }
            div { class: "toolbar",
                div { class: "toolbar-left",
                    input {
                        class: "input-field",
                        r#type: "search",
                        placeholder: "{i18n.t(\"model_bindings.search_placeholder\")}",
                        value: "{model_filter}",
                        oninput: move |event| {
                            model_filter.set(event.value());
                            page.set(1);
                        },
                    }
                }
            }
            div { class: "table-pagination-panel table-pagination-frame",
                Table { empty: rows.is_empty(), empty_text: i18n.t("model_bindings.empty").to_string(), col_count: 7,
                    thead { tr {
                        TableHead { {i18n.t("model_bindings.model")} }
                        TableHead { {i18n.t("model_bindings.account")} }
                        TableHead { {i18n.t("model_bindings.tenant")} }
                        TableHead { {i18n.t("model_bindings.health")} }
                        TableHead { {i18n.t("table.status")} }
                        TableHead { {i18n.t("model_bindings.revision")} }
                        TableHead { {i18n.t("table.actions")} }
                    }}
                    tbody {
                        for binding in rows.iter() {
                            {
                                let binding_for_edit = binding.clone();
                                let binding_for_delete = binding.clone();
                                let binding_for_probe = binding.clone();
                                rsx! {
                                    tr { key: "{binding.id}",
                                        td { code { "{binding.model}" } }
                                        td {
                                            div { "{binding.account_name.as_deref().unwrap_or(\"—\")}" }
                                            code { title: "{binding.account_id}", {short_id(&binding.account_id)} }
                                        }
                                        td { code { title: "{binding.tenant_id}", {short_id(&binding.tenant_id)} } }
                                        td {
                                            match binding.health_status.as_deref() {
                                                Some("healthy") => rsx! { Badge { variant: BadgeVariant::Success, "healthy" } },
                                                Some("degraded") => rsx! { Badge { variant: BadgeVariant::Warning, "degraded" } },
                                                Some("unhealthy") => rsx! { Badge { variant: BadgeVariant::Error, "unhealthy" } },
                                                Some("stale") => rsx! { Badge { variant: BadgeVariant::Neutral, {i18n.t("model_bindings.health_stale")} } },
                                                _ => rsx! { Badge { variant: BadgeVariant::Neutral, "unknown" } },
                                            }
                                        }
                                        td {
                                            if binding.enabled { Badge { variant: BadgeVariant::Success, {i18n.t("common.enabled")} } }
                                            else { Badge { variant: BadgeVariant::Neutral, {i18n.t("common.disabled")} } }
                                        }
                                        td { "{binding.revision}" }
                                        td { class: "table-actions",
                                            Button {
                                                variant: ButtonVariant::Secondary,
                                                size: ButtonSize::Small,
                                                disabled: saving() || pending_id().is_some(),
                                                onclick: {
                                                    let value = binding_for_edit.clone();
                                                    move |_| {
                                                        editing.set(Some(value.clone()));
                                                        form_model.set(value.model.clone());
                                                        form_account_id.set(value.account_id.clone());
                                                        form_tenant_id.set(value.tenant_id.clone());
                                                        form_enabled.set(value.enabled);
                                                        operation_error.set(String::new());
                                                        show_form.set(true);
                                                    }
                                                },
                                                {i18n.t("form.edit")}
                                            }
                                            Button {
                                                variant: ButtonVariant::Ghost,
                                                size: ButtonSize::Small,
                                                disabled: saving() || pending_id().is_some(),
                                                onclick: {
                                                    let value = binding_for_probe.clone();
                                                    move |_| {
                                                        if saving() || pending_id().is_some() { return; }
                                                        let id = value.id.clone();
                                                        let model = value.model.clone();
                                                        let auth = auth_store;
                                                        let mut ui = ui_store;
                                                        pending_id.set(Some(id.clone()));
                                                        spawn(async move {
                                                            let result = with_auto_refresh(auth, move |token| {
                                                                let request = ModelBindingProbeRequest::new(model.clone());
                                                                let id = id.clone();
                                                                async move { account_service::probe_model_binding(&id, request, &token).await }
                                                            }).await;
                                                            match result {
                                                                Ok(response) => {
                                                                    let message = format!("{}: {}", i18n.t("model_bindings.probe_done"), response.status);
                                                                    if response.status == "healthy" { ui.show_success(message); }
                                                                    else { ui.show_error(message); }
                                                                },
                                                                Err(error) => ui.show_error(user_error_message(&error)),
                                                            }
                                                            pending_id.set(None);
                                                            refresh += 1;
                                                        });
                                                    }
                                                },
                                                {i18n.t("model_bindings.probe")}
                                            }
                                            Button {
                                                variant: ButtonVariant::Danger,
                                                size: ButtonSize::Small,
                                                disabled: saving() || pending_id().is_some(),
                                                onclick: {
                                                    let value = binding_for_delete.clone();
                                                    move |_| {
                                                        delete_candidate.set(Some(value.clone()));
                                                        delete_open.set(true);
                                                    }
                                                },
                                                {i18n.t("form.delete")}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "table-pagination",
                span { class: "text-secondary", "{i18n.t_with_args(\"common.pagination_total\", &[(\"total\", &total.to_string())])}" }
                Button { variant: ButtonVariant::Ghost, size: ButtonSize::Small, disabled: page() <= 1,
                    onclick: move |_| if page() > 1 { page -= 1 }, {i18n.t("table.previous")} }
                span { "{page} / {total_pages}" }
                Button { variant: ButtonVariant::Ghost, size: ButtonSize::Small, disabled: page() >= total_pages,
                    onclick: move |_| if page() < total_pages { page += 1 }, {i18n.t("table.next")} }
            }

            if show_form() {
                ModelBindingForm {
                    editing: editing(),
                    model: form_model,
                    account_id: form_account_id,
                    tenant_id: form_tenant_id,
                    enabled: form_enabled,
                    saving: saving(),
                    error: operation_error(),
                    onclose: move |_| if !saving() { show_form.set(false); },
                    onsubmit: submit,
                }
            }
            ConfirmModal {
                open: delete_open,
                title: i18n.t("model_bindings.delete_title").to_string(),
                message: delete_candidate().map(|v| v.model).unwrap_or_default(),
                confirm_text: i18n.t("form.delete").to_string(),
                cancel_text: i18n.t("form.cancel").to_string(),
                danger: true,
                oncancel: move |_| {
                    delete_open.set(false);
                    delete_candidate.set(None);
                },
                onconfirm: move |_| {
                    if saving() || pending_id().is_some() { return; }
                    let Some(candidate) = delete_candidate() else { return; };
                    let id = candidate.id.clone();
                    let revision = candidate.revision;
                    let auth = auth_store;
                    pending_id.set(Some(id.clone()));
                    spawn(async move {
                        let result = with_auto_refresh(auth, move |token| {
                            let id = id.clone();
                            async move {
                                account_service::delete_model_binding_with_revision(
                                    &id, revision, &token,
                                )
                                .await
                            }
                        }).await;
                        match result {
                            Ok(_) => {
                                delete_open.set(false);
                                delete_candidate.set(None);
                                refresh += 1;
                                ui_store.show_success(i18n.t("model_bindings.deleted").to_string());
                            }
                            Err(error) => operation_error.set(user_error_message(&error)),
                        }
                        pending_id.set(None);
                    });
                },
            }
        }
    }
}

#[component]
fn ModelBindingForm(
    editing: Option<ModelBindingInfo>,
    model: Signal<String>,
    account_id: Signal<String>,
    tenant_id: Signal<String>,
    enabled: Signal<bool>,
    saving: bool,
    error: String,
    onclose: EventHandler<()>,
    onsubmit: EventHandler<MouseEvent>,
) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "modal-backdrop", onclick: move |_| onclose.call(()),
            div { class: "modal", role: "dialog", aria_modal: "true", aria_label: i18n.t("page.model_bindings"), onclick: move |event| event.stop_propagation(),
                div { class: "modal-header",
                    h2 { class: "modal-title", {if editing.is_some() { i18n.t("model_bindings.edit") } else { i18n.t("model_bindings.create") }} }
                    button { class: "modal-close btn btn-ghost btn-sm", r#type: "button", aria_label: i18n.t("common.close"), onclick: move |_| onclose.call(()), "✕" }
                }
                div { class: "modal-body",
                    if !error.is_empty() { div { class: "alert alert-error", "{error}" } }
                    div { class: "form-group", label { class: "form-label", {i18n.t("model_bindings.model")} }
                        input { class: "input-field", r#type: "text", value: "{model}", maxlength: "255", oninput: move |event| model.set(event.value()) }
                    }
                    div { class: "form-group", label { class: "form-label", {i18n.t("model_bindings.account")} }
                        input { class: "input-field", r#type: "text", value: "{account_id}", maxlength: "64", oninput: move |event| account_id.set(event.value()) }
                    }
                    div { class: "form-group", label { class: "form-label", {i18n.t("model_bindings.tenant")} }
                        input { class: "input-field", r#type: "text", value: "{tenant_id}", disabled: editing.is_some(), maxlength: "64", oninput: move |event| tenant_id.set(event.value()) }
                    }
                    label { class: "checkbox-label",
                        input { r#type: "checkbox", checked: enabled(), onchange: move |event| enabled.set(event.checked()) }
                        {i18n.t("model_bindings.enabled")}
                    }
                }
                div { class: "modal-footer",
                    Button { variant: ButtonVariant::Ghost, onclick: move |_| onclose.call(()), {i18n.t("form.cancel")} }
                    Button { variant: ButtonVariant::Primary, loading: saving, disabled: saving, onclick: onsubmit, {i18n.t("form.save")} }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BindingListKey;

    #[test]
    fn binding_list_key_changes_when_filter_or_page_changes() {
        let first = BindingListKey {
            page: 1,
            model: "gpt-mini".into(),
            refresh: 0,
        };
        let second = BindingListKey {
            page: 2,
            ..first.clone()
        };
        assert_ne!(first, second);
    }
}
