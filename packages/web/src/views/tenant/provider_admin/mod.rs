//! Current-tenant provider accounts and passthrough grants. Server scope remains authoritative.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
use super::common::{Pager, WorkspaceLinks, WorkspaceScope, command, read};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::get_client,
    stores::{auth_store::AuthStore, user_store::UserStore},
};
use client_api::api::tenant_providers::*;
use dioxus::prelude::*;
use uuid::Uuid;
const PAGE: u32 = 20;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Accounts,
    Bindings,
}
fn api(s: WorkspaceScope) -> client_api::Result<TenantProviderApi> {
    TenantProviderApi::new(&get_client(), s.tenant_id)
}
fn models(v: &str) -> Vec<String> {
    let mut o = Vec::new();
    for x in v.split(',').map(str::trim).filter(|x| !x.is_empty()) {
        if !o.iter().any(|v| v == x) {
            o.push(x.to_string())
        }
    }
    o
}
fn capability_mode(provider: &str, values: &[String]) -> &'static str {
    if provider == "anthropic" {
        "messages"
    } else if values.iter().any(|v| v == "chat_completions")
        && values.iter().any(|v| v == "responses")
    {
        "both"
    } else if values.iter().any(|v| v == "responses") {
        "responses"
    } else {
        "chat_completions"
    }
}
fn caps(provider: &str, mode: &str) -> Vec<String> {
    if provider == "anthropic" {
        vec!["messages".into()]
    } else {
        match mode {
            "responses" => vec!["responses".into()],
            "both" => vec!["chat_completions".into(), "responses".into()],
            _ => vec!["chat_completions".into()],
        }
    }
}
#[component]
pub fn TenantProviders() -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let s = WorkspaceScope::from_stores(a, u);
    let ok = u
        .info
        .read()
        .as_ref()
        .is_some_and(|v| v.can_manage_tenant());
    rsx! {if let Some(s)=s.filter(|_|ok){for k in [format!("{s:?}")] {Workspace{key:"{k}",scope:s}}}else{p{role:"alert",{i.t("tenant.admin_required")}}}}
}
#[component]
fn Workspace(scope: WorkspaceScope) -> Element {
    let i = use_i18n();
    let mut k = use_signal(|| Kind::Accounts);
    rsx! {div{class:"page-container tenant-provider-admin",ui::PageHeader{title:i.t("tenant_providers.title").to_string(),description:i.t("tenant_providers.hint").to_string()} WorkspaceLinks{} p{class:"alert alert-info","{scope.tenant_id}"} nav{class:"toolbar",button{class:"btn btn-secondary",aria_pressed:k()==Kind::Accounts,onclick:move |_|k.set(Kind::Accounts),{i.t("tenant_providers.accounts")}} button{class:"btn btn-secondary",aria_pressed:k()==Kind::Bindings,onclick:move |_|k.set(Kind::Bindings),{i.t("tenant_providers.bindings")}}} if k()==Kind::Accounts{Accounts{scope}}else{Bindings{scope}}}}
}
#[component]
fn Accounts(scope: WorkspaceScope) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let mut page = use_signal(|| 1u32);
    let mut q = use_signal(String::new);
    let mut applied = use_signal(String::new);
    let mut tick = use_signal(|| 0u32);
    let mut edit = use_signal(|| None::<Option<TenantAccount>>);
    let key = (page(), applied(), tick());
    let r = use_resource(move || {
        let key = (page(), applied(), tick());
        async move {
            let search = key.1.clone();
            let x = read(a, u, scope, move |t| {
                let q = search.clone();
                async move { api(scope)?.accounts(key.0, PAGE, &q, "", "all", &t).await }
            })
            .await;
            (key, x)
        }
    });
    let d = r().filter(|v| v.0 == key).map(|v| v.1);
    let rows = d
        .as_ref()
        .and_then(|x| x.as_ref().ok())
        .map(|x| x.accounts.clone())
        .unwrap_or_default();
    let totals = d
        .as_ref()
        .and_then(|x| x.as_ref().ok())
        .map(|x| (x.total_pages, x.total))
        .unwrap_or((1, 0));
    rsx! {div{class:"resource-panel",div{class:"toolbar",input{class:"input-field",r#type:"search",value:"{q}",oninput:move|e|q.set(e.value())} button{class:"btn btn-secondary",onclick:move |_|{applied.set(q());page.set(1)},{i.t("tenant_providers.apply")}} button{class:"btn btn-primary",onclick:move |_|edit.set(Some(None)),{i.t("tenant_providers.add_account")}}} match d{None=>rsx!{p{role:"status",{i.t("common.loading")}}},Some(Err(e))=>rsx!{p{role:"alert","{e}"}},Some(Ok(_))if rows.is_empty()=>rsx!{p{{i.t("tenant_providers.empty_accounts")}}},Some(Ok(_))=>rsx!{div{class:"table-container",table{class:"data-table",thead{tr{th{{i.t("tenant_providers.name")}} th{{i.t("tenant_providers.provider")}} th{{i.t("tenant_providers.models")}} th{{i.t("tenant_providers.pool")}} th{{i.t("common.actions")}}}} tbody{for row in rows{AccountRow{key:"{row.id}",scope,row,onedit:move|v|edit.set(Some(Some(v))),changed:move |_|tick+=1}}}}} Pager{page:page(),total_pages:totals.0,total:totals.1,on_page:move|v|page.set(v)}}} if let Some(v)=edit(){AccountEditor{scope,existing:v,onclose:move |_|edit.set(None),saved:move |_|{edit.set(None);tick+=1}}}}}
}
async fn account_control(
    a: AuthStore,
    u: UserStore,
    s: WorkspaceScope,
    id: Uuid,
    op: &'static str,
) -> client_api::Result<()> {
    command(a, u, s, move |t| async move {
        match op {
            "test" => api(s)?.test_account(id, &t).await.map(|_| ()),
            "refresh" => api(s)?.refresh_account(id, &t).await.map(|_| ()),
            _ => api(s)?.delete_account(id, &t).await.map(|_| ()),
        }
    })
    .await
}
async fn binding_control(
    a: AuthStore,
    u: UserStore,
    s: WorkspaceScope,
    id: Uuid,
    revision: i64,
    probe: bool,
) -> client_api::Result<()> {
    command(a, u, s, move |t| async move {
        if probe {
            api(s)?.probe_binding(id, &t).await.map(|_| ())
        } else {
            api(s)?.delete_binding(id, revision, &t).await.map(|_| ())
        }
    })
    .await
}
#[component]
fn AccountRow(
    scope: WorkspaceScope,
    row: TenantAccount,
    onedit: EventHandler<TenantAccount>,
    changed: EventHandler<()>,
) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let busy = use_signal(|| false);
    let err = use_signal(String::new);
    let edit_row = row.clone();
    let bound = row.passthrough_binding_count > 0;
    let action = move |op: &'static str, id: Uuid| {
        let a = a;
        let u = u;
        let mut busy = busy;
        let mut err = err;
        spawn(async move {
            busy.set(true);
            match account_control(a, u, scope, id, op).await {
                Ok(_) => changed.call(()),
                Err(e) => err.set(e.to_string()),
            }
            busy.set(false);
        });
    };
    let id = row.id;
    let test = move |_| action("test", id);
    let id = row.id;
    let refresh = move |_| action("refresh", id);
    let id = row.id;
    let delete = move |_| action("delete", id);
    rsx! {tr{td{strong{"{row.name}"} small{class:"table-meta","{row.api_key_preview}"}} td{"{row.provider}"} td{title:row.models.join(", "),"{row.models.len()}"} td{if row.pool_enabled{{i.t("common.yes")}}else{{i.t("common.no")}}} td{div{class:"table-actions",button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:move |_|onedit.call(edit_row.clone()),{i.t("form.edit")}} button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:test,{i.t("tenant_providers.test")}} button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:refresh,{i.t("common.refresh")}} button{class:"btn btn-danger btn-sm",disabled:busy()||bound,onclick:delete,{i.t("form.delete")}}} if !err().is_empty(){small{class:"text-danger",role:"alert","{err}"}}}}}
}
#[component]
fn AccountEditor(
    scope: WorkspaceScope,
    existing: Option<TenantAccount>,
    onclose: EventHandler<()>,
    saved: EventHandler<()>,
) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let o = existing.clone();
    let mut name = use_signal(move || o.as_ref().map(|v| v.name.clone()).unwrap_or_default());
    let o = existing.clone();
    let mut provider = use_signal(move || {
        o.as_ref()
            .map(|v| v.provider.clone())
            .unwrap_or_else(|| "openai".into())
    });
    let o = existing.clone();
    let mut base = use_signal(move || {
        o.as_ref()
            .and_then(|v| v.api_base.clone())
            .unwrap_or_default()
    });
    let o = existing.clone();
    let mut ms = use_signal(move || o.as_ref().map(|v| v.models.join(", ")).unwrap_or_default());
    let mut secret = use_signal(String::new);
    let o = existing.clone();
    let mut pool = use_signal(move || o.as_ref().is_some_and(|v| v.pool_enabled));
    let o = existing.clone();
    let mut mode = use_signal(move || {
        o.as_ref()
            .map(|v| capability_mode(&v.provider, &v.api_capabilities).to_string())
            .unwrap_or_else(|| "both".into())
    });
    let mut busy = use_signal(|| false);
    let mut err = use_signal(String::new);
    let binding_managed = existing
        .as_ref()
        .is_some_and(|v| v.passthrough_binding_count > 0);
    let submit_existing = existing.clone();
    let submit = move |_| {
        if busy() {
            return;
        }
        let body = (
            name(),
            provider(),
            base(),
            models(&ms()),
            secret(),
            pool(),
            mode(),
        );
        let old = submit_existing.clone();
        busy.set(true);
        spawn(async move {
            let x = command(a, u, scope, move |t| async move {
                let p = api(scope)?;
                if let Some(v) = old {
                    p.update_account(
                        v.id,
                        &UpdateTenantAccount {
                            name: Some(body.0),
                            api_key: (!body.4.is_empty()).then_some(body.4),
                            api_base: Some(body.2),
                            models: Some(body.3),
                            api_capabilities: Some(caps(&body.1, &body.6)),
                            pool_enabled: (v.passthrough_binding_count == 0).then_some(body.5),
                            ..Default::default()
                        },
                        &t,
                    )
                    .await
                } else {
                    p.create_account(
                        &CreateTenantAccount {
                            name: body.0,
                            provider: body.1.clone(),
                            api_key: body.4,
                            api_base: Some(body.2),
                            models: body.3,
                            api_capabilities: Some(caps(&body.1, &body.6)),
                            rpm_limit: Some(60),
                            tpm_limit: Some(100000),
                            priority: Some(0),
                            pool_enabled: Some(body.5),
                        },
                        &t,
                    )
                    .await
                }
            })
            .await;
            busy.set(false);
            match x {
                Ok(_) => saved.call(()),
                Err(e) => err.set(e.to_string()),
            }
        });
    };
    rsx! {div{class:"modal-backdrop",div{class:"modal",role:"dialog",aria_modal:"true",aria_label:i.t("tenant_providers.account_editor"),div{class:"modal-header",h2{{i.t("tenant_providers.account_editor")}}} div{class:"modal-body",label{{i.t("tenant_providers.name")} input{class:"input-field",value:"{name}",oninput:move|e|name.set(e.value())}} label{{i.t("tenant_providers.provider")} select{class:"input-field",disabled:existing.is_some(),value:"{provider}",onchange:move|e|provider.set(e.value()),option{value:"openai","OpenAI-compatible"} option{value:"anthropic","Anthropic-compatible"}}} label{"Base URL" input{class:"input-field",value:"{base}",oninput:move|e|base.set(e.value())}} label{"API Key" input{class:"input-field",r#type:"password",value:"{secret}",oninput:move|e|secret.set(e.value())}} label{{i.t("tenant_providers.models")} input{class:"input-field",value:"{ms}",oninput:move|e|ms.set(e.value())}} if provider()=="openai" { label{{i.t("tenant_providers.capabilities")} select{class:"input-field",value:"{mode}",onchange:move|e|mode.set(e.value()),option{value:"chat_completions","Chat Completions"} option{value:"responses","Responses"} option{value:"both","Both"}}} } label{input{r#type:"checkbox",checked:pool(),disabled:binding_managed,onchange:move|e|pool.set(e.checked())}{i.t("tenant_providers.pool")}} if !err().is_empty(){p{class:"alert alert-error",role:"alert","{err}"}}} div{class:"modal-footer",button{class:"btn btn-secondary",disabled:busy(),onclick:move |_|onclose.call(()),{i.t("form.cancel")}} button{class:"btn btn-primary",disabled:busy(),onclick:submit,{i.t("form.save")}}}}}}
}
#[component]
fn Bindings(scope: WorkspaceScope) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let mut page = use_signal(|| 1u32);
    let mut tick = use_signal(|| 0u32);
    let mut edit = use_signal(|| None::<Option<TenantBinding>>);
    let key = (page(), tick());
    let r = use_resource(move || {
        let key = (page(), tick());
        async move {
            let x = read(a, u, scope, move |t| async move {
                api(scope)?.bindings(key.0, PAGE, "", &t).await
            })
            .await;
            (key, x)
        }
    });
    let d = r().filter(|v| v.0 == key).map(|v| v.1);
    let rows = d
        .as_ref()
        .and_then(|x| x.as_ref().ok())
        .map(|x| x.bindings.clone())
        .unwrap_or_default();
    let totals = d
        .as_ref()
        .and_then(|x| x.as_ref().ok())
        .map(|x| (x.total_pages, x.total))
        .unwrap_or((1, 0));
    rsx! {div{class:"resource-panel",div{class:"toolbar",button{class:"btn btn-primary",onclick:move |_|edit.set(Some(None)),{i.t("tenant_providers.add_binding")}}} match d{None=>rsx!{p{role:"status",{i.t("common.loading")}}},Some(Err(e))=>rsx!{p{role:"alert","{e}"}},Some(Ok(_))if rows.is_empty()=>rsx!{p{{i.t("tenant_providers.empty_bindings")}}},Some(Ok(_))=>rsx!{div{class:"table-container",table{class:"data-table",thead{tr{th{{i.t("tenant_providers.name")}} th{{i.t("tenant_providers.models")}} th{{i.t("tenant_providers.pool")}} th{{i.t("tenant_providers.health")}} th{{i.t("common.actions")}}}} tbody{for row in rows{BindingRow{key:"{row.id}",scope,row,onedit:move|v|edit.set(Some(Some(v))),changed:move |_|tick+=1}}}}} Pager{page:page(),total_pages:totals.0,total:totals.1,on_page:move|v|page.set(v)}}} if let Some(v)=edit(){BindingEditor{scope,existing:v,onclose:move |_|edit.set(None),saved:move |_|{edit.set(None);tick+=1}}}}}
}
#[component]
fn BindingRow(
    scope: WorkspaceScope,
    row: TenantBinding,
    onedit: EventHandler<TenantBinding>,
    changed: EventHandler<()>,
) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let busy = use_signal(|| false);
    let err = use_signal(String::new);
    let edit_row = row.clone();
    let rev = row.revision;
    let action = move |probe: bool, id: Uuid| {
        let a = a;
        let u = u;
        let mut busy = busy;
        let mut err = err;
        spawn(async move {
            busy.set(true);
            match binding_control(a, u, scope, id, rev, probe).await {
                Ok(_) => changed.call(()),
                Err(e) => err.set(e.to_string()),
            }
            busy.set(false);
        });
    };
    let id = row.id;
    let probe = move |_| action(true, id);
    let id = row.id;
    let delete = move |_| action(false, id);
    rsx! {tr{td{strong{"{row.account_name}"} small{class:"table-meta","rev {row.revision}"}} td{title:row.models_supported.join(", "),"{row.models_supported.len()}"} td{if row.pool_enabled{{i.t("common.yes")}}else{{i.t("common.no")}}} td{"{row.health_status}"} td{div{class:"table-actions",button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:move |_|onedit.call(edit_row.clone()),{i.t("form.edit")}} button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:probe,{i.t("tenant_providers.test")}} button{class:"btn btn-danger btn-sm",disabled:busy(),onclick:delete,{i.t("form.delete")}}} if !err().is_empty(){small{class:"text-danger",role:"alert","{err}"}}}}}
}
#[component]
fn BindingEditor(
    scope: WorkspaceScope,
    existing: Option<TenantBinding>,
    onclose: EventHandler<()>,
    saved: EventHandler<()>,
) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let mut pool = use_signal(|| existing.as_ref().is_some_and(|v| v.pool_enabled));
    let mut busy = use_signal(|| false);
    let mut err = use_signal(String::new);
    let options = use_resource(move || async move {
        read(a, u, scope, move |t| async move {
            api(scope)?.binding_options(1, 100, "", &t).await
        })
        .await
    });
    let rows = options()
        .and_then(Result::ok)
        .map(|v| v.accounts)
        .unwrap_or_default();
    let first = existing
        .as_ref()
        .map(|v| v.account_id)
        .or_else(|| rows.first().map(|v| v.id));
    let mut selected = use_signal(move || first.map(|v| v.to_string()).unwrap_or_default());
    let submit = move |_| {
        if busy() {
            return;
        }
        let Ok(account) = Uuid::parse_str(&selected()) else {
            err.set(i.t("tenant_providers.select_account").into());
            return;
        };
        let old = existing.clone();
        let enabled = pool();
        busy.set(true);
        spawn(async move {
            let x = command(a, u, scope, move |t| async move {
                let p = api(scope)?;
                if let Some(v) = old {
                    p.update_binding(
                        v.id,
                        &UpdateTenantBinding {
                            account_id: (account != v.account_id).then_some(account),
                            pool_enabled: Some(enabled),
                            expected_revision: v.revision,
                        },
                        &t,
                    )
                    .await
                } else {
                    p.create_binding(
                        &CreateTenantBinding {
                            account_id: account,
                            pool_enabled: enabled,
                        },
                        &t,
                    )
                    .await
                }
            })
            .await;
            busy.set(false);
            match x {
                Ok(_) => saved.call(()),
                Err(e) => err.set(e.to_string()),
            }
        });
    };
    rsx! {div{class:"modal-backdrop",div{class:"modal",role:"dialog",aria_modal:"true",aria_label:i.t("tenant_providers.binding_editor"),div{class:"modal-header",h2{{i.t("tenant_providers.binding_editor")}}} div{class:"modal-body",label{{i.t("tenant_providers.account")} select{class:"input-field",value:"{selected}",onchange:move|e|selected.set(e.value()),for row in rows{option{value:"{row.id}","{row.name} · {row.provider}"}}}} label{input{r#type:"checkbox",checked:pool(),onchange:move|e|pool.set(e.checked())}{i.t("tenant_providers.pool")}} p{class:"form-hint",{i.t("tenant_providers.binding_hint")}} if !err().is_empty(){p{class:"alert alert-error",role:"alert","{err}"}}} div{class:"modal-footer",button{class:"btn btn-secondary",disabled:busy(),onclick:move |_|onclose.call(()),{i.t("form.cancel")}} button{class:"btn btn-primary",disabled:busy(),onclick:submit,{i.t("form.save")}}}}}}
}
