use client_api::api::tenant::{
    CreateTenantRequest, TenantInfo, TenantQueryParams, UpdateTenantRequest,
};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use ui::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, ConfirmModal, PageHeader, Pagination,
    Table, TableHead,
};

const PAGE_SIZE: usize = 20;
const SEARCH_DEBOUNCE_MS: u32 = 300;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TenantListQuery {
    search: String,
    page: u32,
    page_size: u32,
}

impl TenantListQuery {
    fn new() -> Self {
        Self {
            search: String::new(),
            page: 1,
            page_size: PAGE_SIZE as u32,
        }
    }

    fn commit_search(&mut self, search: String) {
        self.search = search;
        self.page = 1;
    }

    fn set_page_size(&mut self, page_size: u32) {
        self.page_size = page_size;
        self.page = 1;
    }
}

use crate::hooks::use_i18n::use_i18n;
use crate::services::{
    api_client::{user_error_message, with_auto_refresh},
    tenant_service,
};
use crate::stores::auth_store::AuthStore;
use crate::stores::ui_store::UiStore;
use crate::stores::user_store::UserStore;
use crate::utils::display::short_id;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};
use crate::utils::time::format_time;
use crate::views::shared::accounts::NoPermissionView;

/// 租户管理页面（仅 Admin 可访问）
///
/// - 普通用户：无权限提示
/// - Admin：查看全平台租户列表（调用 TenantApi）
#[component]
pub fn Tenants() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let is_admin = user_store
        .info
        .read()
        .as_ref()
        .map(|u| u.is_admin())
        .unwrap_or(false);

    if !is_admin {
        return rsx! { NoPermissionView { resource: i18n.t("page.tenants").to_string() } };
    }

    let mut search = use_signal(String::new);
    let mut query = use_signal(TenantListQuery::new);
    let mut show_create = use_signal(|| false);
    let mut delete_candidate = use_signal(|| None::<TenantInfo>);
    let mut delete_modal_open = use_signal(|| false);
    let mut operation_error = use_signal(String::new);
    let mut pending_tenant = use_signal(|| None::<String>);
    let mut ui_store = use_context::<UiStore>();

    use_effect(move || {
        let next_search = search();
        spawn(async move {
            TimeoutFuture::new(SEARCH_DEBOUNCE_MS).await;
            if search() == next_search && query.read().search != next_search {
                query.write().commit_search(next_search);
            }
        });
    });

    let mut tenants = use_resource(move || {
        let current_query = query();
        async move {
            let request_key = current_query.clone();
            let mut params = TenantQueryParams::new()
                .with_page(current_query.page)
                .with_page_size(current_query.page_size);
            if !current_query.search.is_empty() {
                params = params.with_search(current_query.search);
            }
            let result = with_auto_refresh(auth_store, move |token| {
                let params = params.clone();
                async move { tenant_service::list_page(params, &token).await }
            })
            .await;
            KeyedResourceValue::new(request_key, result)
        }
    });

    rsx! {
        div { class: "page-container tenants-page",
        PageHeader {
            title: i18n.t("page.tenants").to_string(),
            description: i18n.t("tenants.subtitle").to_string(),
            actions: rsx! {
                Button {
                    variant: ButtonVariant::Primary,
                    onclick: move |_| {
                        operation_error.set(String::new());
                        show_create.set(true);
                    },
                    {i18n.t("tenants.create")}
                }
            },
        }

        div { class: "toolbar",
            div { class: "toolbar-left",
                div { class: "input-wrapper",
                    input {
                        class: "input-field",
                        r#type: "search",
                        placeholder: "{i18n.t(\"tenants.search_placeholder\")}",
                        value: "{search}",
                        oninput: move |e| {
                            *search.write() = e.value();
                        },
                    }
                }
            }
        }

        {
            let current_query = query();
            let result = current_keyed_value(
                &current_query,
                tenants.state().cloned(),
                tenants(),
            );
            let (is_empty, empty_text) = match &result {
                None => (true, i18n.t("table.loading")),
                Some(Err(_)) => (true, i18n.t("common.load_failed")),
                Some(Ok(result)) if result.tenants.is_empty() => (true, i18n.t("tenants.empty")),
                _ => (false, ""),
            };
            let total = result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map(|result| result.total)
                .unwrap_or(0);
            let total_pages = result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map(|result| result.total_pages.max(1))
                .unwrap_or(1);
            let paged = result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map(|result| result.tenants.as_slice())
                .unwrap_or_default();
            rsx! {
                Table {
                    empty: is_empty,
                    empty_text: empty_text.to_string(),
                    col_count: 7,
                    thead {
                        tr {
                            TableHead { {i18n.t("tenants.tenant_id")} }
                            TableHead { {i18n.t("table.name")} }
                            TableHead { {i18n.t("tenants.users")} }
                            TableHead { {i18n.t("tenants.accounts")} }
                            TableHead { {i18n.t("table.status")} }
                            TableHead { {i18n.t("table.created_at")} }
                            TableHead { {i18n.t("table.actions")} }
                        }
                    }
                    tbody {
                        for t in paged.iter() {
                            tr {
                                td { code { title: "{t.id}", {short_id(&t.id)} } }
                                td { "{t.name}" }
                                td { "{t.user_count}" }
                                td { "{t.account_count}" }
                                td {
                                    if t.is_active {
                                        Badge { variant: BadgeVariant::Success, {i18n.t("tenants.active")} }
                                    } else {
                                        Badge { variant: BadgeVariant::Neutral, {i18n.t("common.disabled")} }
                                    }
                                }
                                td { { format_time(&t.created_at) } }
                                td {
                                    div { class: "table-actions",
                                        Button {
                                            variant: if t.is_active { ButtonVariant::Secondary } else { ButtonVariant::Primary },
                                            size: ButtonSize::Small,
                                            disabled: pending_tenant().as_deref() == Some(t.id.as_str())
                                                || (t.slug == "system" && t.is_active),
                                            onclick: {
                                                let id = t.id.clone();
                                                let next_status = if t.is_active { "inactive" } else { "active" }.to_string();
                                                move |_| {
                                                    let id = id.clone();
                                                    pending_tenant.set(Some(id.clone()));
                                                    let request = UpdateTenantRequest::new().with_status(next_status.clone());
                                                    let request_auth = auth_store;
                                                    spawn(async move {
                                                        let result = with_auto_refresh(request_auth, move |token| {
                                                            let id = id.clone();
                                                            let request = request.clone();
                                                            async move { tenant_service::update(&id, request, &token).await }
                                                        }).await;
                                                        match result {
                                                            Ok(_) => tenants.restart(),
                                                            Err(error) => operation_error.set(user_error_message(&error)),
                                                        }
                                                        pending_tenant.set(None);
                                                    });
                                                }
                                            },
                                            {if t.is_active { i18n.t("tenants.disable") } else { i18n.t("tenants.enable") }}
                                        }
                                        Button {
                                            variant: ButtonVariant::Danger,
                                            size: ButtonSize::Small,
                                            disabled: pending_tenant().as_deref() == Some(t.id.as_str())
                                                || t.slug == "system"
                                                || t.user_count > 0
                                                || t.account_count > 0,
                                            onclick: {
                                                let candidate = t.clone();
                                                move |_| {
                                                    delete_candidate.set(Some(candidate.clone()));
                                                    delete_modal_open.set(true);
                                                    operation_error.set(String::new());
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
                Pagination {
                    current: current_query.page,
                    total_pages,
                    total,
                    page_size: current_query.page_size,
                    summary: i18n.t_with_args(
                        "common.pagination_summary",
                        &[
                            ("total", &total.to_string()),
                            ("current", &current_query.page.to_string()),
                            ("total_pages", &total_pages.to_string()),
                        ],
                    ),
                    page_size_label: i18n.t("common.pagination_page_size").to_string(),
                    page_size_suffix: i18n.t("pricing.items_suffix").to_string(),
                    previous_label: i18n.t("table.previous").to_string(),
                    next_label: i18n.t("table.next").to_string(),
                    on_page_change: move |page| query.write().page = page,
                    on_page_size_change: move |size| query.write().set_page_size(size),
                }
            }
        }
        if !operation_error().is_empty() {
            div { class: "alert alert-error", "{operation_error}" }
        }
        if show_create() {
            TenantCreateModal {
                auth_store,
                on_close: move |_| show_create.set(false),
                on_created: move |_| {
                    show_create.set(false);
                    query.write().page = 1;
                    tenants.restart();
                    ui_store.show_success(i18n.t("tenants.created"));
                },
            }
        }
        ConfirmModal {
            open: delete_modal_open,
            title: i18n.t("tenants.delete_title").to_string(),
            message: delete_candidate()
                .map(|tenant| format!("{}: {}", i18n.t("tenants.delete_confirm"), tenant.name))
                .unwrap_or_default(),
            confirm_text: i18n.t("form.delete").to_string(),
            cancel_text: i18n.t("form.cancel").to_string(),
            danger: true,
            oncancel: move |_| {
                delete_modal_open.set(false);
                delete_candidate.set(None);
            },
            onconfirm: move |_| {
                let Some(candidate) = delete_candidate() else { return; };
                let id = candidate.id.clone();
                pending_tenant.set(Some(id.clone()));
                let request_auth = auth_store;
                spawn(async move {
                    let result = with_auto_refresh(request_auth, move |token| {
                        let id = id.clone();
                        async move { tenant_service::delete(&id, &token).await }
                    }).await;
                    match result {
                        Ok(_) => {
                            delete_modal_open.set(false);
                            delete_candidate.set(None);
                            tenants.restart();
                            ui_store.show_success(i18n.t("tenants.deleted"));
                        }
                        Err(error) => {
                            delete_modal_open.set(false);
                            operation_error.set(user_error_message(&error));
                        }
                    }
                    pending_tenant.set(None);
                });
            },
        }
        }
    }
}

#[component]
fn TenantCreateModal(
    auth_store: AuthStore,
    on_close: EventHandler<()>,
    on_created: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let mut name = use_signal(String::new);
    let mut slug = use_signal(String::new);
    let mut saving = use_signal(|| false);
    let mut error = use_signal(String::new);

    let on_submit = move |_| {
        let name_value = name().trim().to_string();
        let slug_value = slug().trim().to_string();
        if name_value.is_empty() {
            error.set(i18n.t("tenants.name_required").to_string());
            return;
        }
        let mut request = CreateTenantRequest::new(name_value);
        if !slug_value.is_empty() {
            request = request.with_slug(slug_value);
        }
        let request_auth = auth_store;
        saving.set(true);
        error.set(String::new());
        let on_created = on_created.clone();
        spawn(async move {
            let result = with_auto_refresh(request_auth, move |token| {
                let request = request.clone();
                async move { tenant_service::create(request, &token).await }
            })
            .await;
            match result {
                Ok(_) => on_created.call(()),
                Err(value) => error.set(user_error_message(&value)),
            }
            saving.set(false);
        });
    };

    rsx! {
        div { class: "modal-backdrop", onclick: move |_| on_close.call(()),
            div { class: "modal", role: "dialog", aria_modal: "true", aria_label: i18n.t("tenants.create_title"), onclick: move |event| event.stop_propagation(),
                div { class: "modal-header",
                    h2 { class: "modal-title", {i18n.t("tenants.create_title")} }
                    button { class: "modal-close btn btn-ghost btn-sm", r#type: "button", aria_label: i18n.t("common.close"), onclick: move |_| on_close.call(()), "✕" }
                }
                div { class: "modal-body",
                    if !error().is_empty() {
                        div { class: "alert alert-error", "{error}" }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("tenants.name")} }
                        input { class: "input-field", r#type: "text", required: true, maxlength: "255", value: "{name}", placeholder: "{i18n.t(\"tenants.name_placeholder\")}", oninput: move |event| name.set(event.value()) }
                    }
                    div { class: "form-group",
                        label { class: "form-label", {i18n.t("tenants.slug")} }
                        input { class: "input-field", r#type: "text", maxlength: "100", value: "{slug}", placeholder: "{i18n.t(\"tenants.slug_placeholder\")}", oninput: move |event| slug.set(event.value()) }
                        small { class: "text-secondary", {i18n.t("tenants.slug_hint")} }
                    }
                }
                div { class: "modal-footer",
                    Button { variant: ButtonVariant::Ghost, onclick: move |_| on_close.call(()), {i18n.t("form.cancel")} }
                    Button { variant: ButtonVariant::Primary, loading: saving(), disabled: name().trim().is_empty(), onclick: on_submit, {i18n.t("form.create")} }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PAGE_SIZE, SEARCH_DEBOUNCE_MS, TenantListQuery};

    #[test]
    fn tenant_search_uses_a_short_debounce() {
        assert!((250..=500).contains(&SEARCH_DEBOUNCE_MS));
    }

    #[test]
    fn tenant_search_resets_the_database_page() {
        let mut query = TenantListQuery {
            search: "old".to_string(),
            page: 4,
            page_size: PAGE_SIZE as u32,
        };
        query.commit_search("new".to_string());
        assert_eq!(query.search, "new");
        assert_eq!(query.page, 1);
    }
}
