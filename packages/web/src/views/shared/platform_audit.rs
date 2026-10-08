use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message, with_auto_refresh},
    stores::{
        auth_store::{AuthState, AuthStore},
        user_store::{UserInfo, UserStore},
    },
    utils::{
        resource::{KeyedResourceValue, current_keyed_value},
        time::format_time,
    },
    views::tenant::audit::{action_label, resource_label, result_label},
};
use client_api::{
    ClientError, Result, UserStatus,
    api::platform_audit::{PlatformAuditApi, PlatformAuditQuery, PlatformAuditRecord},
};
use dioxus::prelude::*;
use std::future::Future;
use ui::{Badge, BadgeVariant, CursorPagination};
use uuid::Uuid;

const PAGE_SIZE: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuditScope {
    epoch: Uuid,
    user_id: Uuid,
}

impl AuditScope {
    fn from_profile(state: &AuthState, loaded: Uuid, user: Option<&UserInfo>) -> Option<Self> {
        if !state.is_authenticated || loaded != state.session_id {
            return None;
        }
        let user = user?;
        if user.status != Some(UserStatus::Active)
            || !user.has_platform_permission("platform:audit_read")
        {
            return None;
        }
        Some(Self {
            epoch: state.session_id,
            user_id: Uuid::parse_str(&user.id).ok().filter(|id| !id.is_nil())?,
        })
    }

    fn current(auth: AuthStore, users: UserStore) -> Option<Self> {
        Self::from_profile(
            &(auth.state)(),
            (users.loaded_session_id)(),
            (users.info)().as_ref(),
        )
    }

    fn is_current(self, auth: AuthStore, users: UserStore) -> bool {
        Self::from_profile(
            &auth.state.peek(),
            *users.loaded_session_id.peek(),
            users.info.peek().as_ref(),
        ) == Some(self)
    }

    async fn read<T, F, Fut>(self, auth: AuthStore, users: UserStore, fetch: F) -> Result<T>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let changed = || ClientError::Other("Platform audit context changed".into());
        if !self.is_current(auth, users) {
            return Err(changed());
        }
        let result = with_auto_refresh(auth, fetch).await;
        if !self.is_current(auth, users) {
            return Err(changed());
        }
        result
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AuditFilter {
    tenant_id: Option<Uuid>,
    request_id: Option<Uuid>,
}

fn optional_uuid(value: &str) -> Option<Option<Uuid>> {
    let value = value.trim();
    if value.is_empty() {
        return Some(None);
    }
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .map(Some)
}

fn result_variant(result: &str) -> BadgeVariant {
    match result {
        "success" => BadgeVariant::Success,
        "denied" => BadgeVariant::Warning,
        "failure" => BadgeVariant::Error,
        _ => BadgeVariant::Neutral,
    }
}

#[component]
pub fn PlatformAudit() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let scope = AuditScope::current(auth, users);
    rsx! {
        if let Some(scope) = scope {
            for key in [format!("{scope:?}")] {
                PlatformAuditPage { key: "{key}", scope }
            }
        } else {
            div { class: "page-container", role: "alert",
                p { class: "alert alert-error", {i18n.t("platform_audit.denied")} }
            }
        }
    }
}

#[component]
fn PlatformAuditPage(scope: AuditScope) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let mut page = use_signal(|| 1u32);
    let mut page_size = use_signal(|| PAGE_SIZE);
    let mut cursor = use_signal(String::new);
    let mut cursor_history = use_signal(Vec::<String>::new);
    let mut tenant_input = use_signal(String::new);
    let mut request_input = use_signal(String::new);
    let mut filter = use_signal(AuditFilter::default);
    let mut filter_error = use_signal(String::new);
    let mut data = use_resource(move || {
        let filter = filter();
        let cursor = cursor();
        let page_size = page_size();
        let key = (scope, filter, cursor.clone(), page_size);
        async move {
            let query = PlatformAuditQuery {
                tenant_id: filter.tenant_id,
                request_id: filter.request_id,
                cursor: (!cursor.is_empty()).then_some(cursor),
                page_size,
            };
            let result = scope
                .read(auth, users, move |token| {
                    let query = query.clone();
                    async move {
                        PlatformAuditApi::new(&get_client())
                            .list(&query, &token)
                            .await
                    }
                })
                .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(
        &(scope, filter(), cursor(), page_size()),
        data.state().cloned(),
        data(),
    );
    let data_pending = loaded.is_none();

    rsx! {
        div { class: "page-container platform-audit",
            ui::PageHeader {
                title: i18n.t("platform_audit.title").to_string(),
                description: i18n.t("platform_audit.hint").to_string(),
            }
            p { class: "alert alert-info", {i18n.t("platform_audit.boundary")} }
            div { class: "card filter-panel",
                div { class: "filter-grid platform-audit-filter-grid",
                    div { class: "form-field",
                        label { class: "form-label", r#for: "platform-audit-tenant",
                            {i18n.t("platform_audit.tenant_filter")}
                        }
                        input {
                            id: "platform-audit-tenant",
                            class: "input-field",
                            r#type: "text",
                            maxlength: "36",
                            value: "{tenant_input}",
                            placeholder: i18n.t("platform_audit.tenant_placeholder"),
                            oninput: move |event| tenant_input.set(event.value()),
                        }
                    }
                    div { class: "form-field",
                        label { class: "form-label", r#for: "platform-audit-request",
                            {i18n.t("platform_audit.request_filter")}
                        }
                        input {
                            id: "platform-audit-request",
                            class: "input-field",
                            r#type: "text",
                            maxlength: "36",
                            value: "{request_input}",
                            placeholder: i18n.t("platform_audit.request_placeholder"),
                            oninput: move |event| request_input.set(event.value()),
                        }
                    }
                    div { class: "filter-actions",
                        button {
                            class: "btn btn-primary",
                            r#type: "button",
                            onclick: move |_| {
                                let Some(tenant_id) = optional_uuid(&tenant_input()) else {
                                    filter_error.set(i18n.t("platform_audit.invalid_filter").into());
                                    return;
                                };
                                let Some(request_id) = optional_uuid(&request_input()) else {
                                    filter_error.set(i18n.t("platform_audit.invalid_filter").into());
                                    return;
                                };
                                filter_error.set(String::new());
                                let next = AuditFilter { tenant_id, request_id };
                                let cursor_changed = !cursor().is_empty();
                                let filter_changed = filter() != next;
                                cursor.set(String::new());
                                cursor_history.write().clear();
                                page.set(1);
                                if filter_changed {
                                    filter.set(next);
                                } else if !cursor_changed {
                                    data.restart();
                                }
                            },
                            {i18n.t("platform_audit.apply")}
                        }
                        button {
                            class: "btn btn-secondary",
                            r#type: "button",
                            onclick: move |_| {
                                tenant_input.set(String::new());
                                request_input.set(String::new());
                                filter_error.set(String::new());
                                let cursor_changed = !cursor().is_empty();
                                cursor.set(String::new());
                                cursor_history.write().clear();
                                page.set(1);
                                if filter() == AuditFilter::default() {
                                    if !cursor_changed {
                                        data.restart();
                                    }
                                } else {
                                    filter.set(AuditFilter::default());
                                }
                            },
                            {i18n.t("platform_audit.clear")}
                        }
                        button {
                            class: "btn btn-secondary",
                            r#type: "button",
                            onclick: move |_| data.restart(),
                            {i18n.t("tenant.reload")}
                        }
                    }
                }
            }
            if !filter_error().is_empty() {
                p { class: "alert alert-error", role: "alert", "{filter_error}" }
            }
            match loaded {
                None => rsx! {
                    div { class: "content-loading", role: "status",
                        span { class: "spinner", aria_hidden: "true" }
                        span { {i18n.t("common.loading")} }
                    }
                },
                Some(Err(error)) => rsx! {
                    p { class: "alert alert-error", role: "alert",
                        {user_error_message(i18n, &error)}
                    }
                },
                Some(Ok(value)) => rsx! {
                    if value.items.is_empty() {
                        div { class: "empty-state bordered-empty-state",
                            h3 { class: "empty-title", {i18n.t("platform_audit.empty")} }
                            p { class: "empty-description", {i18n.t("platform_audit.empty_hint")} }
                        }
                    } else {
                        div { class: "table-pagination-panel table-pagination-frame",
                            div { class: "table-container", tabindex: "0",
                                table { class: "table",
                                    thead { tr {
                                        th { {i18n.t("platform_audit.time")} }
                                        th { {i18n.t("platform_audit.tenant")} }
                                        th { {i18n.t("platform_audit.actor")} }
                                        th { {i18n.t("platform_audit.action")} }
                                        th { {i18n.t("platform_audit.resource")} }
                                        th { {i18n.t("platform_audit.request_id")} }
                                        th { {i18n.t("platform_audit.result")} }
                                    } }
                                    tbody {
                                        for event in &value.items {
                                            PlatformAuditRow { key: "{event.id}", event: event.clone() }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !value.items.is_empty() || value.next_cursor.is_some() || page() > 1 {
                    CursorPagination {
                        current: page(),
                        has_previous: !cursor_history.read().is_empty(),
                        has_next: value.next_cursor.is_some(),
                        page_size: page_size(),
                        class: "platform-audit-pagination".to_string(),
                        disabled: data_pending,
                        page_size_options: vec![10, 20, 50, 100],
                        summary: i18n.t_with_args("platform_audit.page", &[("page", &page().to_string())]),
                        page_size_label: i18n.t("common.pagination_page_size").to_string(),
                        page_size_suffix: i18n.t("common.items_suffix").to_string(),
                        previous_label: i18n.t("table.previous").to_string(),
                        next_label: i18n.t("table.next").to_string(),
                        on_previous: move |_| {
                            if let Some(previous) = cursor_history.write().pop() {
                                cursor.set(previous);
                                page.set(page().saturating_sub(1));
                            }
                        },
                        on_next: {
                            let next = value.next_cursor.clone();
                            move |_| {
                                if let Some(next) = next.clone() {
                                    cursor_history.write().push(cursor());
                                    cursor.set(next);
                                    page += 1;
                                }
                            }
                        },
                        on_page_size_change: move |size: u32| {
                            page_size.set(size.clamp(1, 100));
                            cursor.set(String::new());
                            cursor_history.write().clear();
                            page.set(1);
                        },
                    }
                    }
                },
            }
        }
    }
}

#[component]
fn PlatformAuditRow(event: PlatformAuditRecord) -> Element {
    let i18n = use_i18n();
    let tenant_id = event.tenant_id.map(|id| id.to_string());
    let request_id = event.request_id.map(|id| id.to_string());
    let actor_label = event
        .actor_name
        .as_deref()
        .or(event.actor_email.as_deref())
        .unwrap_or("—");
    let tenant_role = event.tenant_role.as_deref().unwrap_or("—");
    rsx! {
        tr {
            td { "{format_time(&event.created_at)}" }
            td {
                if let Some(tenant_id) = tenant_id.as_deref() {
                    strong { class: "table-primary",
                        {event.tenant_name.as_deref().unwrap_or(i18n.t("platform_audit.unknown_tenant"))}
                    }
                    if let Some(slug) = event.tenant_slug.as_deref() {
                        span { class: "table-secondary", "{slug}" }
                    }
                    code { class: "table-secondary", "{tenant_id}" }
                } else {
                    Badge { variant: BadgeVariant::Info, {i18n.t("platform_audit.platform_scope")} }
                }
            }
            td {
                strong { class: "table-primary", "{actor_label}" }
                if event.actor_email.as_deref().is_some_and(|email| email != actor_label) {
                    span { class: "table-secondary", "{event.actor_email.as_deref().unwrap_or_default()}" }
                }
                code { class: "table-secondary", "{event.actor_user_id}" }
            }
            td { "{action_label(i18n, &event.action)}" }
            td {
                "{resource_label(i18n, &event.resource_type)}"
                if let Some(resource_id) = event.resource_id.as_deref() {
                    code { class: "table-secondary", "{resource_id}" }
                }
            }
            td {
                if let Some(request_id) = request_id.as_deref() {
                    code { "{request_id}" }
                } else {
                    "—"
                }
            }
            td {
                Badge { variant: result_variant(&event.result),
                    {result_label(i18n, &event.result)}
                }
                details {
                    summary { {i18n.t("platform_audit.details")} }
                    dl { class: "audit-detail-list",
                        dt { {i18n.t("platform_audit.scope")} }
                        dd { code { "{event.scope_type}" } }
                        dt { {i18n.t("platform_audit.credential")} }
                        dd { code { "{event.credential_kind}" } }
                        dt { {i18n.t("platform_audit.roles")} }
                        dd {
                            code { "{event.platform_role}" }
                            " / "
                            code { "{tenant_role}" }
                        }
                        dt { {i18n.t("platform_audit.metadata")} }
                        dd { pre { "{event.metadata}" } }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::optional_uuid;
    use uuid::Uuid;

    #[test]
    fn optional_filters_are_exact_real_uuids_or_empty() {
        assert_eq!(optional_uuid("  "), Some(None));
        let id = Uuid::new_v4();
        assert_eq!(optional_uuid(&id.to_string()), Some(Some(id)));
        assert_eq!(optional_uuid(&Uuid::nil().to_string()), None);
        assert_eq!(optional_uuid("tenant-name"), None);
    }
}
