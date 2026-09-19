//! Account → tenant passthrough grants.  Models are displayed as a read-only
//! summary of the selected account; they are never part of the grant form.

use client_api::api::admin::{
    CreatePassthroughBindingRequest, PassthroughAccountOption, PassthroughAccountOptionsQuery,
    PassthroughBindingInfo, PassthroughBindingQueryParams, UpdatePassthroughBindingRequest,
};
use dioxus::prelude::*;
use ui::{Button, ButtonVariant};

use crate::hooks::use_i18n::use_i18n;
use crate::services::api_client::user_error_message;
use crate::services::{account_service, api_client::with_auto_refresh, tenant_service};
use crate::stores::auth_store::AuthStore;
use crate::utils::resource::{KeyedResourceValue, current_keyed_value};

const PAGE_SIZE: u32 = 20;

fn runtime_health_key(status: Option<&str>) -> &'static str {
    match status {
        Some("healthy") => "passthrough.health_healthy",
        Some("degraded") => "passthrough.health_degraded",
        Some("unhealthy") => "passthrough.health_unhealthy",
        Some("unavailable") => "passthrough.health_unavailable",
        _ => "passthrough.health_unknown",
    }
}

#[component]
pub fn PassthroughBindings() -> Element {
    let i = use_i18n();
    let auth = use_context::<AuthStore>();
    let mut page = use_signal(|| 1u32);
    let mut query = use_signal(String::new);
    let mut refresh = use_signal(|| 0u32);
    let mut editor = use_signal(|| None::<Option<PassthroughBindingInfo>>);
    let session = (auth.state)().session_id;
    let list_key = (query(), page(), refresh(), session);
    let resource = use_resource(move || {
        let key = (query(), page(), refresh(), (auth.state)().session_id);
        async move {
            let params = PassthroughBindingQueryParams::new()
                .with_page(key.1)
                .with_page_size(PAGE_SIZE)
                .with_search(key.0.clone());
            let result = with_auto_refresh(auth, move |token| {
                let params = params.clone();
                async move { account_service::list_passthrough_bindings(Some(params), &token).await }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let data = current_keyed_value(&list_key, resource.state().cloned(), resource());
    let page_data = data.as_ref().and_then(|r| r.as_ref().ok());
    let rows = page_data.map(|p| p.bindings.clone()).unwrap_or_default();
    let total_pages = page_data.map(|p| p.total_pages.max(1)).unwrap_or(1);
    let editor_key = editor()
        .as_ref()
        .and_then(|v| v.as_ref().map(|b| b.id.clone()))
        .unwrap_or_else(|| "new".to_string());
    let mut open_new = editor.clone();
    rsx! {
        div { class: "resource-panel passthrough-bindings-panel",
            div { class: "page-header",
                div { class: "page-header-main",
                    h2 { class: "page-title", {i.t("upstreams.passthrough")} }
                    p { class: "page-description", {i.t("passthrough.subtitle")} }
                }
                div { class: "page-header-actions",
                    button { class: "btn btn-primary", r#type: "button", onclick: move |_| open_new.set(Some(None)), {i.t("passthrough.add")} }
                }
            }
            div { class: "toolbar",
                input { class: "input-field", r#type: "search", value: "{query}", placeholder: i.t("common.search"), aria_label: i.t("common.search"), oninput: move |e| { query.set(e.value()); page.set(1); } }
                button { class: "btn btn-secondary", r#type: "button", onclick: move |_| refresh += 1, {i.t("common.refresh")} }
            }
            match data {
                None => rsx! { div { class: "loading-state", role: "status", {i.t("common.loading")} } },
                Some(Err(error)) => rsx! { div { class: "alert alert-error", role: "alert", "{user_error_message(&error)}" } },
                Some(Ok(_)) if rows.is_empty() => rsx! { div { class: "empty-state", {i.t("passthrough.empty")} } },
                Some(Ok(_)) => rsx! {
                    div { class: "table-container",
                        table { class: "data-table",
                            thead { tr {
                                th { {i.t("passthrough.account")} } th { {i.t("passthrough.tenant")} }
                                th { {i.t("passthrough.scope")} } th { {i.t("passthrough.models")} }
                                th { {i.t("passthrough.pool")} } th { {i.t("passthrough.health")} } th { {i.t("common.actions")} }
                            } }
                            tbody { for row in rows.iter() { PassthroughRow { key: "{row.id}", row: row.clone(), onchanged: move |_| refresh += 1, onedit: move |v| editor.set(Some(Some(v))) } } }
                        }
                    }
                    div { class: "table-pagination",
                        button { class: "btn btn-ghost btn-sm", disabled: page() <= 1, onclick: move |_| page -= 1, {i.t("table.previous")} }
                        span { "{page} / {total_pages}" }
                        button { class: "btn btn-ghost btn-sm", disabled: page() >= total_pages, onclick: move |_| page += 1, {i.t("table.next")} }
                    }
                },
            }
        }
        if let Some(value) = editor() {
            PassthroughBindingEditor {
                key: "{editor_key}:{session}",
                existing: value,
                onclose: move |_| editor.set(None),
                onsaved: move |_| { editor.set(None); refresh += 1; }
            }
        }
    }
}

#[component]
fn PassthroughRow(
    row: PassthroughBindingInfo,
    onchanged: EventHandler<()>,
    onedit: EventHandler<PassthroughBindingInfo>,
) -> Element {
    let i = use_i18n();
    let auth = use_context::<AuthStore>();
    let mut pending = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut confirm_delete = use_signal(|| false);
    let mut confirm_probe = use_signal(|| false);
    let mut diagnostic = use_signal(String::new);
    let models_title = row.models_supported.join(", ");
    let id = row.id.clone();
    let diagnostic_id = row.id.clone();
    let revision = row.revision;
    let delete = move |_| {
        if pending() {
            return;
        }
        pending.set(true);
        error.set(String::new());
        let id = id.clone();
        spawn(async move {
            let result = with_auto_refresh(auth, move |token| {
                let id = id.clone();
                async move {
                    account_service::delete_passthrough_binding(&id, revision, &token)
                        .await
                        .map(|_| ())
                }
            })
            .await;
            pending.set(false);
            match result {
                Ok(()) => onchanged.call(()),
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! { tr {
        td { strong { "{row.account_name}" } p { class: "table-meta", "{row.provider}" } }
        td { "{row.tenant_name}" }
        td { if row.is_global { {i.t("passthrough.global")} } else { {i.t("passthrough.bound_tenant")} } }
        td { details { summary { "{row.models_supported.len()} ", {i.t("passthrough.view_models")} } p {class:"form-hint", "{models_title}"} } }
        td { if row.pool_enabled { {i.t("common.yes")} } else { {i.t("common.no")} } }
        td { {i.t(runtime_health_key(row.health_status.as_deref()))} }
        td { div { class: "table-actions",
            button { class: "btn btn-ghost btn-sm", disabled: pending(), onclick: move |_| onedit.call(row.clone()), {i.t("form.edit")} }
            button { class: "btn btn-ghost btn-sm", disabled: pending(), onclick: move |_| confirm_delete.set(true), {i.t("form.delete")} }
            button {class:"btn btn-ghost btn-sm",disabled:pending(),onclick:move|_|confirm_probe.set(true),{i.t("passthrough.diagnostic")}}
            if confirm_probe() {div{class:"kc-action-confirm",role:"alert",
              p{{i.t("passthrough.probe_warning")}}
              button { class: "btn btn-secondary btn-sm", disabled: pending(), onclick: move |_| {
                if pending(){return;} confirm_probe.set(false);
                pending.set(true); error.set(String::new());
                let id = diagnostic_id.clone();
                spawn(async move {
                    let result = with_auto_refresh(auth, move |token| {
                        let id = id.clone();
                        async move { account_service::probe_passthrough_binding(&id, Default::default(), &token).await }
                    }).await;
                    pending.set(false);
                    match result {
                        Ok(result) => {
                            diagnostic.set(format!("{}: {}", i.t("passthrough.diagnostic"), i.t(runtime_health_key(Some(&result.status)))));
                            onchanged.call(());
                        },
                        Err(e) => error.set(user_error_message(&e)),
                    }
                });
            }, {i.t("passthrough.confirm_probe")} }
              button{class:"btn btn-ghost btn-sm",onclick:move|_|confirm_probe.set(false),{i.t("form.cancel")}}
            }}
            if confirm_delete() {
                div { class: "kc-action-confirm", role: "alert",
                    p { {i.t("passthrough.delete_warning")} }
                    button { class: "btn btn-danger btn-sm", disabled: pending(), onclick: delete, {i.t("passthrough.confirm_delete")} }
                    button { class: "btn btn-ghost btn-sm", disabled: pending(), onclick: move |_| confirm_delete.set(false), {i.t("form.cancel")} }
                }
            }
            if !diagnostic().is_empty() { small { role: "status", "{diagnostic}" } }
            if !error().is_empty() { small { class: "text-danger", "{error}" } }
        } }
    } }
}

#[component]
fn PassthroughBindingEditor(
    existing: Option<PassthroughBindingInfo>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let i = use_i18n();
    let auth = use_context::<AuthStore>();
    let original = existing.clone();
    let mut account_id = use_signal(move || {
        original
            .as_ref()
            .map(|v| v.account_id.clone())
            .unwrap_or_default()
    });
    let original = existing.clone();
    let mut tenant_id = use_signal(move || {
        original
            .as_ref()
            .map(|v| v.tenant_id.clone())
            .unwrap_or_default()
    });
    let original = existing.clone();
    let mut is_global = use_signal(move || original.as_ref().map(|v| v.is_global).unwrap_or(false));
    let original = existing.clone();
    let mut pool_enabled =
        use_signal(move || original.as_ref().map(|v| v.pool_enabled).unwrap_or(false));
    let mut account_query = use_signal(String::new);
    let mut account_page = use_signal(|| 1u32);
    let mut tenant_query = use_signal(String::new);
    let mut tenant_page = use_signal(|| 1u32);
    let mut pending = use_signal(|| false);
    let mut error = use_signal(String::new);
    let account_key = (account_query(), account_page(), (auth.state)().session_id);
    let accounts = use_resource(move || {
        let key = (account_query(), account_page(), (auth.state)().session_id);
        async move {
            let params = PassthroughAccountOptionsQuery::new()
                .with_search(key.0.clone())
                .with_page(key.1)
                .with_page_size(PAGE_SIZE);
            let result = with_auto_refresh(auth, move |token| {
                let params = params.clone();
                async move { account_service::passthrough_binding_options(params, &token).await }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let tenant_key = (tenant_query(), tenant_page(), (auth.state)().session_id);
    let tenants = use_resource(move || {
        let key = (tenant_query(), tenant_page(), (auth.state)().session_id);
        async move {
            let params = client_api::api::tenant::TenantQueryParams::new()
                .with_search(key.0.clone())
                .with_page(key.1)
                .with_page_size(PAGE_SIZE);
            let result = with_auto_refresh(auth, move |token| {
                let params = params.clone();
                async move { tenant_service::list_page(params, &token).await }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let account_data = current_keyed_value(&account_key, accounts.state().cloned(), accounts());
    let tenant_data = current_keyed_value(&tenant_key, tenants.state().cloned(), tenants());
    let account_choices = account_data
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|v| v.accounts.clone())
        .unwrap_or_default();
    let tenant_choices = tenant_data
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|v| v.tenants.clone())
        .unwrap_or_default();
    let original = existing.clone();
    let mut remembered_account = use_signal(move || {
        original.map(|v| PassthroughAccountOption {
            id: v.account_id,
            name: v.account_name,
            provider: v.provider,
            pool_enabled: v.pool_enabled,
            models: v.models_supported,
        })
    });
    let original = existing.clone();
    let mut remembered_tenant = use_signal(move || original.map(|v| (v.tenant_id, v.tenant_name)));
    let display_account = account_choices
        .iter()
        .find(|a| a.id == account_id())
        .cloned()
        .or_else(|| remembered_account().filter(|a| a.id == account_id()));
    let display_tenant = tenant_choices
        .iter()
        .find(|t| t.id == tenant_id())
        .map(|t| (t.id.clone(), t.name.clone()))
        .or_else(|| remembered_tenant().filter(|t| t.0 == tenant_id()));
    let account_pages = account_data
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map_or(1, |p| p.total_pages.max(1));
    let tenant_pages = tenant_data
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map_or(1, |p| p.total_pages.max(1));
    let account_options_for_change = account_choices.clone();
    let tenant_options_for_change = tenant_choices.clone();
    let original = existing.clone();
    let submit = move |_| {
        if pending() || account_id().is_empty() || tenant_id().is_empty() {
            error.set(i.t("passthrough.required").to_string());
            return;
        }
        pending.set(true);
        error.set(String::new());
        let account = account_id();
        let tenant = tenant_id();
        let global = is_global();
        let pool = pool_enabled();
        let old = original.clone();
        spawn(async move {
            let result = with_auto_refresh(auth, move |token| {
                let old = old.clone();
                let account = account.clone();
                let tenant = tenant.clone();
                async move {
                    if let Some(old) = old {
                        account_service::update_passthrough_binding(
                            &old.id,
                            UpdatePassthroughBindingRequest::new(old.revision)
                                .with_account_id(account)
                                .with_tenant_id(tenant)
                                .with_is_global(global)
                                .with_pool_enabled(pool),
                            &token,
                        )
                        .await
                        .map(|_| ())
                    } else {
                        account_service::create_passthrough_binding(
                            CreatePassthroughBindingRequest::new(account, tenant)
                                .with_is_global(global)
                                .with_pool_enabled(pool),
                            &token,
                        )
                        .await
                        .map(|_| ())
                    }
                }
            })
            .await;
            pending.set(false);
            match result {
                Ok(()) => onsaved.call(()),
                Err(e) => error.set(user_error_message(&e)),
            }
        });
    };
    rsx! {
        div { class: "modal-backdrop", onclick: move |_| if !pending() {onclose.call(())},
            div { class: "modal", role: "dialog", aria_modal: "true", aria_label:i.t("passthrough.form_title"), onclick:move|e|e.stop_propagation(),
                div {class:"modal-header",
                    h2 {class:"modal-title", {if existing.is_some(){i.t("passthrough.edit_title")}else{i.t("passthrough.form_title")}}}
                    button {class:"btn btn-ghost btn-sm",r#type:"button",aria_label:i.t("common.close"),disabled:pending(),onclick:move|_|onclose.call(()),"✕"}
                }
                div {class:"modal-body",
                    if !error().is_empty(){div{class:"alert alert-error",role:"alert","{error}"}}
                    div {class:"form-group",
                        label {class:"form-label",r#for:"pt-account-choice",{i.t("passthrough.account")}}
                        input {class:"input-field",r#type:"search",aria_label:i.t("passthrough.search_account"),placeholder:i.t("passthrough.search_account"),value:"{account_query}",disabled:pending(),oninput:move|e|{account_query.set(e.value());account_page.set(1);}}
                        select {id:"pt-account-choice",class:"input-field",aria_label:i.t("passthrough.choose_account"),value:"{account_id}",disabled:pending(),onchange:move|e|{
                            let value=e.value();
                            if value.is_empty(){account_id.set(String::new());remembered_account.set(None);}
                            else if let Some(option)=account_options_for_change.iter().find(|a|a.id==value){account_id.set(value);remembered_account.set(Some(option.clone()));}
                        },
                            option {value:"",selected:account_id().is_empty(),{i.t("passthrough.choose_account")}}
                            if let Some(selected)=&display_account {if !account_choices.iter().any(|a|a.id==selected.id){option{key:"{selected.id}",value:"{selected.id}",selected:true,"{selected.name}"}}}
                            for option in &account_choices {option{key:"{option.id}",value:"{option.id}",selected:option.id==account_id(),"{option.name}"}}
                        }
                        div {class:"picker-pagination",
                            button{class:"btn btn-ghost btn-sm",r#type:"button",disabled:pending()||account_page()<=1,onclick:move|_|account_page-=1,{i.t("table.previous")}}
                            small {"{account_page} / {account_pages}"}
                            button{class:"btn btn-ghost btn-sm",r#type:"button",disabled:pending()||account_page()>=account_pages,onclick:move|_|account_page+=1,{i.t("table.next")}}
                        }
                        match &account_data {
                            None=>rsx!{small{role:"status",{i.t("common.loading")}}},
                            Some(Err(e))=>rsx!{div{class:"alert alert-error",role:"alert",{user_error_message(e)}}},
                            Some(Ok(v)) if v.accounts.is_empty()=>rsx!{small{{i.t("passthrough.no_accounts")}}},
                            _=>rsx!{},
                        }
                        if let Some(selected)=&display_account {
                            details{class:"passthrough-model-summary",open:true,
                                summary{{i.t("passthrough.declared_models")}," ({selected.models.len()})"}
                                p{class:"form-hint",{selected.models.join(", ")}}
                            }
                        }
                    }
                    div{class:"form-group",
                        label{class:"form-label",r#for:"pt-tenant-choice",{i.t("passthrough.tenant")}}
                        input{class:"input-field",r#type:"search",aria_label:i.t("passthrough.search_tenant"),placeholder:i.t("passthrough.search_tenant"),value:"{tenant_query}",disabled:pending(),oninput:move|e|{tenant_query.set(e.value());tenant_page.set(1);}}
                        select{id:"pt-tenant-choice",class:"input-field",aria_label:i.t("passthrough.choose_tenant"),value:"{tenant_id}",disabled:pending(),onchange:move|e|{
                            let value=e.value();
                            if value.is_empty(){tenant_id.set(String::new());remembered_tenant.set(None);}
                            else if let Some(option)=tenant_options_for_change.iter().find(|t|t.id==value){tenant_id.set(value);remembered_tenant.set(Some((option.id.clone(),option.name.clone())));}
                        },
                            option{value:"",selected:tenant_id().is_empty(),{i.t("passthrough.choose_tenant")}}
                            if let Some((id,name))=&display_tenant {if !tenant_choices.iter().any(|t|t.id==*id){option{key:"{id}",value:"{id}",selected:true,"{name}"}}}
                            for option in &tenant_choices {option{key:"{option.id}",value:"{option.id}",selected:option.id==tenant_id(),"{option.name}"}}
                        }
                        div{class:"picker-pagination",
                            button{class:"btn btn-ghost btn-sm",r#type:"button",disabled:pending()||tenant_page()<=1,onclick:move|_|tenant_page-=1,{i.t("table.previous")}}
                            small{"{tenant_page} / {tenant_pages}"}
                            button{class:"btn btn-ghost btn-sm",r#type:"button",disabled:pending()||tenant_page()>=tenant_pages,onclick:move|_|tenant_page+=1,{i.t("table.next")}}
                        }
                        match &tenant_data {
                            None=>rsx!{small{role:"status",{i.t("common.loading")}}},
                            Some(Err(e))=>rsx!{div{class:"alert alert-error",role:"alert",{user_error_message(e)}}},
                            Some(Ok(v)) if v.tenants.is_empty()=>rsx!{small{{i.t("passthrough.no_tenants")}}},
                            _=>rsx!{},
                        }
                    }
                    div{class:"form-group",
                        label{class:"checkbox-label",input{r#type:"checkbox",checked:is_global(),disabled:pending(),onchange:move|e|is_global.set(e.checked())}{i.t("passthrough.global_flag")}}
                        p{class:"form-hint",{i.t("passthrough.global_help")}}
                    }
                    div{class:"form-group",
                        label{class:"checkbox-label",input{r#type:"checkbox",checked:pool_enabled(),disabled:pending(),onchange:move|e|pool_enabled.set(e.checked())}{i.t("passthrough.pool_flag")}}
                        p{class:"form-hint",{i.t("passthrough.pool_help")}}
                    }
                    if is_global()||pool_enabled(){div{class:"alert alert-warning",{i.t("passthrough.broadening_warning")}}}
                }
                div{class:"modal-footer",
                    Button{variant:ButtonVariant::Ghost,disabled:pending(),onclick:move|_|onclose.call(()),{i.t("form.cancel")}}
                    Button{variant:ButtonVariant::Primary,disabled:pending()||account_id().is_empty()||tenant_id().is_empty(),loading:pending(),onclick:submit,{i.t("form.save")}}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn passthrough_editor_uses_channel_modal_without_model_or_enabled_controls() {
        let source = include_str!("passthrough_bindings.rs");
        let editor = source
            .split("fn PassthroughBindingEditor(")
            .nth(1)
            .unwrap()
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(
            editor.contains("modal-body")
                && editor.contains("modal-footer")
                && !editor.contains("kc-binding-editor")
        );
        assert!(!editor.contains("with_enabled"));
        assert!(!editor.contains("with_model"));
        assert!(editor.contains("account_page") && editor.contains("tenant_page"));
    }
}
