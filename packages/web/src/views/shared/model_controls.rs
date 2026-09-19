//! Shared language and tenant context for the three-mode administration flow.
use crate::services::api_client::user_error_message;
use crate::{
    hooks::use_i18n::use_i18n,
    i18n::I18n,
    services::{api_client::with_auto_refresh, tenant_service},
    stores::auth_store::AuthStore,
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::api::admin::ModelAccessMode;
use dioxus::prelude::*;

pub fn mode_label(i: I18n, m: ModelAccessMode) -> &'static str {
    i.t(match m {
        ModelAccessMode::AccountPool => "models.pool",
        ModelAccessMode::Passthrough => "models.passthrough",
        ModelAccessMode::NodeDispatch => "models.node",
    })
}
pub fn mode_description(i: I18n, m: ModelAccessMode) -> &'static str {
    i.t(match m {
        ModelAccessMode::AccountPool => "models.pool_desc",
        ModelAccessMode::Passthrough => "models.pass_desc",
        ModelAccessMode::NodeDispatch => "models.node_desc",
    })
}

/// The list is searchable and paginated, with current selection retained by
/// name when it falls outside the currently displayed search page.
#[component]
pub fn TenantPicker(
    mut selected: Signal<String>,
    mut label: Signal<String>,
    onchange: EventHandler<()>,
    #[props(default = false)] disabled: bool,
) -> Element {
    let i = use_i18n();
    let auth = use_context::<AuthStore>();
    let mut search = use_signal(String::new);
    let mut page = use_signal(|| 1u32);
    let request = use_resource(move || {
        let key = (search(), page(), (auth.state)().session_id);
        async move {
            let query = client_api::api::tenant::TenantQueryParams::new()
                .with_search(key.0.clone())
                .with_page(key.1)
                .with_page_size(20);
            let result = with_auto_refresh(auth, move |token| {
                let query = query.clone();
                async move { tenant_service::list_page(query, &token).await }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let key = (search(), page(), (auth.state)().session_id);
    use_effect(move || {
        let current = (search(), page(), (auth.state)().session_id);
        if let Some(Ok(data)) = current_keyed_value(&current, request.state().cloned(), request())
            && let Some(t) = data.tenants.iter().find(|v| v.id == selected())
            && label() != t.name
        {
            label.set(t.name.clone());
        }
    });
    let result = current_keyed_value(&key, request.state().cloned(), request());
    let active_rows = result
        .as_ref()
        .and_then(|v| v.as_ref().ok())
        .map(|v| v.tenants.clone())
        .unwrap_or_default();
    let pages = result
        .as_ref()
        .and_then(|v| v.as_ref().ok())
        .map_or(0, |v| v.total_pages);
    let selected_in_page = active_rows
        .iter()
        .find(|v| v.id == selected())
        .map(|v| v.name.clone());
    let displayed_name = selected_in_page.clone().unwrap_or_else(&*label);
    rsx! {
        div {class:"kc-tenant-context",
            label {class:"form-label", {i.t("models.consumer_tenant")} }
            strong {class:"kc-context-name", "{displayed_name}" }
            div {class:"kc-picker-controls",
                input {class:"input-field",r#type:"search",aria_label:i.t("models.search_tenant"),placeholder:i.t("models.search_tenant"),value:"{search}",disabled,oninput:move|e|{search.set(e.value());page.set(1);}}
                select {class:"input-field",aria_label:i.t("models.choose_tenant"),value:"{selected}",disabled,onchange:move|e|{
                    if let Some(t)=active_rows.iter().find(|v|v.id==e.value()) {selected.set(t.id.clone());label.set(t.name.clone());onchange.call(());}
                },
                    if selected_in_page.is_none() && !selected().is_empty() {option {value:"{selected}","{displayed_name}"}}
                    if selected().is_empty() {option {value:"",{i.t("models.choose_tenant")}}}
                    if let Some(Ok(data))=&result {for tenant in &data.tenants {option {value:"{tenant.id}","{tenant.name}"}}}
                }
                button {class:"btn btn-ghost btn-sm",disabled:disabled||page()<=1,r#type:"button",onclick:move|_|{page-=1;},{i.t("table.previous")}}
                span {"{page} / {pages.max(1)}"}
                button {class:"btn btn-ghost btn-sm",disabled:disabled||page()>=pages,r#type:"button",onclick:move|_|{page+=1;},{i.t("table.next")}}
            }
            match result {
                Some(Err(e))=>rsx!{div{class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
                None=>rsx!{small{role:"status",{i.t("common.loading")}}},
                _=>rsx!{}
            }
        }
    }
}
