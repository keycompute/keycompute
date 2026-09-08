use client_api::api::tenant::TenantQueryParams;
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use ui::{Badge, BadgeVariant, PageHeader, Pagination, Table, TableHead};

const PAGE_SIZE: usize = 20;
const SEARCH_DEBOUNCE_MS: u32 = 300;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TenantListQuery {
    search: String,
    page: u32,
}

impl TenantListQuery {
    fn new() -> Self {
        Self {
            search: String::new(),
            page: 1,
        }
    }

    fn commit_search(&mut self, search: String) {
        self.search = search;
        self.page = 1;
    }
}

use crate::hooks::use_i18n::use_i18n;
use crate::services::{api_client::with_auto_refresh, tenant_service};
use crate::stores::auth_store::AuthStore;
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

    use_effect(move || {
        let next_search = search();
        spawn(async move {
            TimeoutFuture::new(SEARCH_DEBOUNCE_MS).await;
            if search() == next_search && query.read().search != next_search {
                query.write().commit_search(next_search);
            }
        });
    });

    let tenants = use_resource(move || {
        let current_query = query();
        async move {
            let request_key = current_query.clone();
            let mut params = TenantQueryParams::new()
                .with_page(current_query.page)
                .with_page_size(PAGE_SIZE as u32);
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
                    col_count: 4,
                    thead {
                        tr {
                            TableHead { {i18n.t("tenants.tenant_id")} }
                            TableHead { {i18n.t("table.name")} }
                            TableHead { {i18n.t("table.status")} }
                            TableHead { {i18n.t("table.created_at")} }
                        }
                    }
                    tbody {
                        for t in paged.iter() {
                            tr {
                                td { code { title: "{t.id}", {short_id(&t.id)} } }
                                td { "{t.name}" }
                                td {
                                    if t.is_active {
                                        Badge { variant: BadgeVariant::Success, {i18n.t("tenants.active")} }
                                    } else {
                                        Badge { variant: BadgeVariant::Neutral, {i18n.t("common.disabled")} }
                                    }
                                }
                                td { { format_time(&t.created_at) } }
                            }
                        }
                    }
                }
                div { class: "pagination",
                    span { class: "pagination-info",
                        "{i18n.t(\"common.total_items\")} {total} {i18n.t(\"pricing.items_suffix\")}"
                    }
                    Pagination {
                        current: current_query.page,
                        total_pages,
                        previous_label: i18n.t("table.previous").to_string(),
                        next_label: i18n.t("table.next").to_string(),
                        on_page_change: move |page| query.write().page = page,
                    }
                }
            }
        }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SEARCH_DEBOUNCE_MS, TenantListQuery};

    #[test]
    fn tenant_search_uses_a_short_debounce() {
        assert!((250..=500).contains(&SEARCH_DEBOUNCE_MS));
    }

    #[test]
    fn tenant_search_resets_the_database_page() {
        let mut query = TenantListQuery {
            search: "old".to_string(),
            page: 4,
        };
        query.commit_search("new".to_string());
        assert_eq!(query.search, "new");
        assert_eq!(query.page, 1);
    }
}
