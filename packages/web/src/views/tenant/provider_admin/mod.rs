//! Current-tenant provider accounts and passthrough grants. Server scope remains authoritative.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
use super::common::{Pager, WorkspaceContext, WorkspaceLinks, WorkspaceScope, command, read};
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountAction {
    Test,
    Refresh,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BindingAction {
    Probe,
    Delete,
}

fn health_label(i: crate::i18n::I18n, value: &str) -> String {
    match value {
        "healthy" => i.t("tenant_providers.health_healthy").into(),
        "degraded" => i.t("tenant_providers.health_degraded").into(),
        "unhealthy" => i.t("tenant_providers.health_unhealthy").into(),
        "disabled" | "inactive" => i.t("common.disabled").into(),
        "unknown" | "untested" => i.t("tenant_providers.health_unknown").into(),
        other => other.to_owned(),
    }
}

fn parse_account_limits(rpm: &str, tpm: &str, priority: &str) -> Option<(i32, i32, i32)> {
    let rpm = rpm.trim().parse::<i32>().ok().filter(|value| *value > 0)?;
    let tpm = tpm.trim().parse::<i32>().ok().filter(|value| *value > 0)?;
    let priority = priority
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|value| (0..=10).contains(value))?;
    Some((rpm, tpm, priority))
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
        .is_some_and(|v| v.can_manage_providers());
    rsx! {if let Some(s)=s.filter(|_|ok){for k in [format!("{s:?}")] {Workspace{key:"{k}",scope:s}}}else{p{role:"alert",{i.t("tenant.admin_required")}}}}
}
#[component]
fn Workspace(scope: WorkspaceScope) -> Element {
    let i = use_i18n();
    let mut k = use_signal(|| Kind::Accounts);
    rsx! {div{class:"page-container tenant-provider-admin",ui::PageHeader{title:i.t("tenant_providers.title").to_string(),description:i.t("tenant_providers.hint").to_string()} WorkspaceLinks{} WorkspaceContext{} nav{class:"segmented-control",button{class:"btn btn-secondary",aria_pressed:k()==Kind::Accounts,onclick:move |_|k.set(Kind::Accounts),{i.t("tenant_providers.accounts")}} button{class:"btn btn-secondary",aria_pressed:k()==Kind::Bindings,onclick:move |_|k.set(Kind::Bindings),{i.t("tenant_providers.bindings")}}} if k()==Kind::Accounts{Accounts{scope}}else{Bindings{scope}}}}
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
    let mut deleting = use_signal(|| false);
    let mut delete_candidate = use_signal(|| None::<TenantAccount>);
    let mut command_error = use_signal(String::new);
    super::workspace_switcher::use_workspace_dirty_blocker(move || delete_candidate().is_some());
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
    let delete_open = use_memo(move || delete_candidate().is_some());
    rsx! {div{class:"resource-panel",div{class:"toolbar",input{class:"input-field",r#type:"search",aria_label:i.t("tenant_providers.search_placeholder"),placeholder:i.t("tenant_providers.search_placeholder"),value:"{q}",oninput:move|e|q.set(e.value())} button{class:"btn btn-secondary",onclick:move |_|{applied.set(q());page.set(1)},{i.t("tenant_providers.apply")}} button{class:"btn btn-primary",onclick:move |_|edit.set(Some(None)),{i.t("tenant_providers.add_account")}}} if !command_error().is_empty(){p{class:"alert alert-error",role:"alert","{command_error}"}} match d{None=>rsx!{p{role:"status",{i.t("common.loading")}}},Some(Err(e))=>rsx!{p{class:"alert alert-error",role:"alert",{user_error_message(i, &e)}}},Some(Ok(_))if rows.is_empty()=>rsx!{div{class:"empty-state bordered-empty-state",h3{class:"empty-title",{i.t("tenant_providers.empty_accounts")}}}},Some(Ok(_))=>rsx!{div{class:"table-container",table{class:"data-table",thead{tr{th{{i.t("tenant_providers.name")}} th{{i.t("tenant_providers.provider")}} th{{i.t("tenant_providers.models")}} th{{i.t("common.status")}} th{{i.t("tenant_providers.limits")}} th{{i.t("tenant_providers.priority")}} th{{i.t("tenant_providers.pool")}} th{{i.t("common.actions")}}}} tbody{for row in rows{AccountRow{key:"{row.id}",scope,row,onedit:move|v|edit.set(Some(Some(v))),ondelete:move|v|{command_error.set(String::new());delete_candidate.set(Some(v))},changed:move |_|tick+=1}}}}} Pager{page:page(),total_pages:totals.0,total:totals.1,on_page:move|v|page.set(v)}}} if let Some(v)=edit(){AccountEditor{scope,existing:v,onclose:move |_|edit.set(None),saved:move |_|{edit.set(None);tick+=1}}} ui::ConfirmModal{open:delete_open,title:i.t("tenant_providers.delete_account_title").to_string(),message:delete_candidate().map(|row|i.t_with_args("tenant_providers.delete_account_confirm",&[("name",row.name.as_str())])).unwrap_or_default(),confirm_text:i.t("form.delete").to_string(),cancel_text:i.t("form.cancel").to_string(),danger:true,busy:deleting(),oncancel:move |_|delete_candidate.set(None),onconfirm:move |_|{let Some(row)=delete_candidate()else{return};if deleting(){return} deleting.set(true);spawn(async move{match account_control(a,u,scope,row.id,AccountAction::Delete).await{Ok(_)=>{delete_candidate.set(None);tick+=1},Err(e)=>{delete_candidate.set(None);command_error.set(user_error_message(i,&e));}}deleting.set(false);});}}}}
}
async fn account_control(
    a: AuthStore,
    u: UserStore,
    s: WorkspaceScope,
    id: Uuid,
    op: AccountAction,
) -> client_api::Result<()> {
    command(a, u, s, move |t| async move {
        match op {
            AccountAction::Test => api(s)?.test_account(id, &t).await.map(|_| ()),
            AccountAction::Refresh => api(s)?.refresh_account(id, &t).await.map(|_| ()),
            AccountAction::Delete => api(s)?.delete_account(id, &t).await.map(|_| ()),
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
    op: BindingAction,
) -> client_api::Result<()> {
    command(a, u, s, move |t| async move {
        match op {
            BindingAction::Probe => api(s)?.probe_binding(id, &t).await.map(|_| ()),
            BindingAction::Delete => api(s)?.delete_binding(id, revision, &t).await.map(|_| ()),
        }
    })
    .await
}
#[component]
fn AccountRow(
    scope: WorkspaceScope,
    row: TenantAccount,
    onedit: EventHandler<TenantAccount>,
    ondelete: EventHandler<TenantAccount>,
    changed: EventHandler<()>,
) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let busy = use_signal(|| false);
    let err = use_signal(String::new);
    let edit_row = row.clone();
    let delete_row = row.clone();
    let bound = row.passthrough_binding_count > 0;
    let action = move |op: AccountAction| {
        let mut busy = busy;
        let mut err = err;
        if busy() {
            return;
        }
        busy.set(true);
        spawn(async move {
            match account_control(a, u, scope, row.id, op).await {
                Ok(_) => changed.call(()),
                Err(e) => err.set(user_error_message(i, &e)),
            }
            busy.set(false);
        });
    };
    rsx! {tr{td{strong{"{row.name}"} small{class:"table-meta","{row.api_key_preview}"}} td{"{row.provider}"} td{title:row.models.join(", "),"{row.models.len()}"} td{if row.is_active{span{class:"badge badge-success",{i.t("common.enabled")}}}else{span{class:"badge badge-neutral",{i.t("common.disabled")}}} small{class:"table-meta",{health_label(i,&row.health_status)}}} td{"RPM {row.rpm_limit} · TPM {row.tpm_limit}"} td{"{row.priority}"} td{if row.pool_enabled{{i.t("common.yes")}}else{{i.t("common.no")}}} td{div{class:"table-actions",button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:move |_|onedit.call(edit_row.clone()),{i.t("form.edit")}} button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:move |_|action(AccountAction::Test),{i.t("tenant_providers.test")}} button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:move |_|action(AccountAction::Refresh),{i.t("common.refresh")}} button{class:"btn btn-danger btn-sm",disabled:busy()||bound,onclick:move |_|ondelete.call(delete_row.clone()),{i.t("form.delete")}}} if !err().is_empty(){small{class:"text-danger",role:"alert","{err}"}}}}}
}
#[component]
fn AccountEditor(
    scope: WorkspaceScope,
    existing: Option<TenantAccount>,
    onclose: EventHandler<()>,
    saved: EventHandler<()>,
) -> Element {
    super::workspace_switcher::use_workspace_blocker();
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
    let o = existing.clone();
    let mut rpm = use_signal(move || {
        o.as_ref()
            .map(|v| v.rpm_limit.to_string())
            .unwrap_or_else(|| "60".into())
    });
    let o = existing.clone();
    let mut tpm = use_signal(move || {
        o.as_ref()
            .map(|v| v.tpm_limit.to_string())
            .unwrap_or_else(|| "100000".into())
    });
    let o = existing.clone();
    let mut priority = use_signal(move || {
        o.as_ref()
            .map(|v| v.priority.to_string())
            .unwrap_or_else(|| "0".into())
    });
    let o = existing.clone();
    let mut active = use_signal(move || o.as_ref().is_none_or(|v| v.is_active));
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
        let Some(limits) = parse_account_limits(&rpm(), &tpm(), &priority()) else {
            err.set(i.t("tenant_providers.invalid_limits").into());
            return;
        };
        err.set(String::new());
        let body = (
            name(),
            provider(),
            base(),
            models(&ms()),
            secret(),
            pool(),
            mode(),
            limits,
            active(),
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
                            rpm_limit: Some(body.7.0),
                            tpm_limit: Some(body.7.1),
                            is_active: Some(body.8),
                            priority: Some(body.7.2),
                            pool_enabled: (v.passthrough_binding_count == 0).then_some(body.5),
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
                            rpm_limit: Some(body.7.0),
                            tpm_limit: Some(body.7.1),
                            priority: Some(body.7.2),
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
                Err(e) => err.set(user_error_message(i, &e)),
            }
        });
    };
    rsx! {div{class:"modal-backdrop",div{class:"modal modal-wide",role:"dialog",aria_modal:"true",aria_label:i.t("tenant_providers.account_editor"),div{class:"modal-header",h2{{i.t("tenant_providers.account_editor")}}} div{class:"modal-body",div{class:"modal-form-grid",label{{i.t("tenant_providers.name")} input{class:"input-field",value:"{name}",oninput:move|e|name.set(e.value())}} label{{i.t("tenant_providers.provider")} select{class:"input-field",disabled:existing.is_some(),value:"{provider}",onchange:move|e|provider.set(e.value()),option{value:"openai",{i.t("tenant_providers.openai_compatible")}} option{value:"anthropic",{i.t("tenant_providers.anthropic_compatible")}}}} label{class:"modal-field-wide",{i.t("tenant_providers.base_url")} input{class:"input-field",value:"{base}",oninput:move|e|base.set(e.value())}} label{class:"modal-field-wide",{i.t("tenant_providers.api_key")} input{class:"input-field",r#type:"password",value:"{secret}",oninput:move|e|secret.set(e.value())}} label{class:"modal-field-wide",{i.t("tenant_providers.models")} input{class:"input-field",value:"{ms}",oninput:move|e|ms.set(e.value())}} if provider()=="openai" { label{class:"modal-field-wide",{i.t("tenant_providers.capabilities")} select{class:"input-field",value:"{mode}",onchange:move|e|mode.set(e.value()),option{value:"chat_completions",{i.t("tenant_providers.capability_chat")}} option{value:"responses",{i.t("tenant_providers.capability_responses")}} option{value:"both",{i.t("tenant_providers.capability_both")}}}} } label{{i.t("tenant_providers.rpm_limit")} input{class:"input-field",r#type:"number",min:"1",value:"{rpm}",oninput:move|e|rpm.set(e.value())}} label{{i.t("tenant_providers.tpm_limit")} input{class:"input-field",r#type:"number",min:"1",value:"{tpm}",oninput:move|e|tpm.set(e.value())}} label{{i.t("tenant_providers.priority")} input{class:"input-field",r#type:"number",min:"0",max:"10",value:"{priority}",oninput:move|e|priority.set(e.value())}} if existing.is_some(){label{class:"checkbox-field",input{r#type:"checkbox",checked:active(),onchange:move|e|active.set(e.checked())}{i.t("tenant_providers.active")}}} label{class:"checkbox-field",input{r#type:"checkbox",checked:pool(),disabled:binding_managed,onchange:move|e|pool.set(e.checked())}{i.t("tenant_providers.pool")}}} if !err().is_empty(){p{class:"alert alert-error",role:"alert","{err}"}}} div{class:"modal-footer",button{class:"btn btn-secondary",disabled:busy(),onclick:move |_|onclose.call(()),{i.t("form.cancel")}} button{class:"btn btn-primary",disabled:busy(),onclick:submit,{i.t("form.save")}}}}}}
}
#[component]
fn Bindings(scope: WorkspaceScope) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let mut page = use_signal(|| 1u32);
    let mut tick = use_signal(|| 0u32);
    let mut edit = use_signal(|| None::<Option<TenantBinding>>);
    let mut deleting = use_signal(|| false);
    let mut delete_candidate = use_signal(|| None::<TenantBinding>);
    let mut command_error = use_signal(String::new);
    super::workspace_switcher::use_workspace_dirty_blocker(move || delete_candidate().is_some());
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
    let delete_open = use_memo(move || delete_candidate().is_some());
    rsx! {
        div { class: "resource-panel",
            div { class: "toolbar",
                button { class: "btn btn-primary", onclick: move |_| edit.set(Some(None)),
                    {i.t("tenant_providers.add_binding")}
                }
            }
            if !command_error().is_empty() {
                p { class: "alert alert-error", role: "alert", "{command_error}" }
            }
            match d {
                None => rsx! { p { role: "status", {i.t("common.loading")} } },
                Some(Err(e)) => rsx! { p { class: "alert alert-error", role: "alert", {user_error_message(i, &e)} } },
                Some(Ok(_)) if rows.is_empty() => rsx! { div { class: "empty-state bordered-empty-state", h3 { class: "empty-title", {i.t("tenant_providers.empty_bindings")} } } },
                Some(Ok(_)) => rsx! {
                    div { class: "table-container",
                        table { class: "data-table",
                            thead { tr {
                                th { {i.t("tenant_providers.name")} }
                                th { {i.t("tenant_providers.models")} }
                                th { {i.t("tenant_providers.pool")} }
                                th { {i.t("tenant_providers.health")} }
                                th { {i.t("common.actions")} }
                            } }
                            tbody { for row in rows {
                                BindingRow {
                                    key: "{row.id}", scope, row,
                                    onedit: move |value| edit.set(Some(Some(value))),
                                    ondelete: move |value| {
                                        command_error.set(String::new());
                                        delete_candidate.set(Some(value));
                                    },
                                    changed: move |_| tick += 1,
                                }
                            } }
                        }
                    }
                    Pager { page: page(), total_pages: totals.0, total: totals.1, on_page: move |value| page.set(value) }
                },
            }
            if let Some(value) = edit() {
                BindingEditor { scope, existing: value, onclose: move |_| edit.set(None), saved: move |_| { edit.set(None); tick += 1; } }
            }
            ui::ConfirmModal {
                open: delete_open,
                title: i.t("tenant_providers.delete_binding_title").to_string(),
                message: delete_candidate()
                    .map(|row| i.t_with_args("tenant_providers.delete_binding_confirm", &[("name", row.account_name.as_str())]))
                    .unwrap_or_default(),
                confirm_text: i.t("form.delete").to_string(),
                cancel_text: i.t("form.cancel").to_string(),
                danger: true,
                busy: deleting(),
                oncancel: move |_| delete_candidate.set(None),
                onconfirm: move |_| {
                    let Some(row) = delete_candidate() else { return };
                    if deleting() { return }
                    deleting.set(true);
                    spawn(async move {
                        match binding_control(a, u, scope, row.id, row.revision, BindingAction::Delete).await {
                            Ok(_) => { delete_candidate.set(None); tick += 1; }
                            Err(error) => {
                                delete_candidate.set(None);
                                command_error.set(user_error_message(i, &error));
                            }
                        }
                        deleting.set(false);
                    });
                },
            }
        }
    }
}
#[component]
fn BindingRow(
    scope: WorkspaceScope,
    row: TenantBinding,
    onedit: EventHandler<TenantBinding>,
    ondelete: EventHandler<TenantBinding>,
    changed: EventHandler<()>,
) -> Element {
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let busy = use_signal(|| false);
    let err = use_signal(String::new);
    let edit_row = row.clone();
    let delete_row = row.clone();
    let rev = row.revision;
    let action = move |op: BindingAction, id: Uuid| {
        let a = a;
        let u = u;
        let mut busy = busy;
        let mut err = err;
        if busy() {
            return;
        }
        busy.set(true);
        spawn(async move {
            match binding_control(a, u, scope, id, rev, op).await {
                Ok(_) => changed.call(()),
                Err(e) => err.set(user_error_message(i, &e)),
            }
            busy.set(false);
        });
    };
    let id = row.id;
    let probe = move |_| action(BindingAction::Probe, id);
    rsx! {tr{td{strong{"{row.account_name}"} small{class:"table-meta","rev {row.revision}"}} td{title:row.models_supported.join(", "),"{row.models_supported.len()}"} td{if row.pool_enabled{{i.t("common.yes")}}else{{i.t("common.no")}}} td{{health_label(i,&row.health_status)}} td{div{class:"table-actions",button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:move |_|onedit.call(edit_row.clone()),{i.t("form.edit")}} button{class:"btn btn-ghost btn-sm",disabled:busy(),onclick:probe,{i.t("tenant_providers.test")}} button{class:"btn btn-danger btn-sm",disabled:busy(),onclick:move |_|ondelete.call(delete_row.clone()),{i.t("form.delete")}}} if !err().is_empty(){small{class:"text-danger",role:"alert","{err}"}}}}}
}
#[component]
fn BindingEditor(
    scope: WorkspaceScope,
    existing: Option<TenantBinding>,
    onclose: EventHandler<()>,
    saved: EventHandler<()>,
) -> Element {
    super::workspace_switcher::use_workspace_blocker();
    let a = use_context::<AuthStore>();
    let u = use_context::<UserStore>();
    let i = use_i18n();
    let mut pool = use_signal(|| existing.as_ref().is_some_and(|v| v.pool_enabled));
    let mut busy = use_signal(|| false);
    let mut err = use_signal(String::new);
    let initial_account = existing.as_ref().map(|value| {
        (
            value.account_id.to_string(),
            format!("{} · {}", value.account_name, value.provider),
        )
    });
    let initial_account_id = initial_account
        .as_ref()
        .map(|(id, _)| id.clone())
        .unwrap_or_default();
    let mut selected = use_signal(move || initial_account_id);
    let mut selected_account = use_signal(move || initial_account);
    let mut option_page = use_signal(|| 1u32);
    let mut option_query = use_signal(String::new);
    let mut option_applied = use_signal(String::new);
    let option_key = (option_page(), option_applied());
    let options = use_resource(move || {
        let key = (option_page(), option_applied());
        async move {
            let search = key.1.clone();
            let result = read(a, u, scope, move |t| {
                let search = search.clone();
                async move { api(scope)?.binding_options(key.0, PAGE, &search, &t).await }
            })
            .await;
            (key, result)
        }
    });
    let option_data = options()
        .filter(|value| value.0 == option_key)
        .map(|value| value.1);
    let rows = option_data
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .map(|value| value.accounts.clone())
        .unwrap_or_default();
    let option_totals = option_data
        .as_ref()
        .and_then(|value| value.as_ref().ok())
        .map(|value| (value.total_pages, value.total))
        .unwrap_or((1, 0));
    let selected_is_listed = rows.iter().any(|row| row.id.to_string() == selected());
    let selected_label = selected_account()
        .filter(|(id, _)| id == &selected())
        .map(|(_, label)| label);
    let selectable_accounts = rows.clone();
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
                Err(e) => err.set(user_error_message(i, &e)),
            }
        });
    };
    rsx! {div{class:"modal-backdrop",div{class:"modal modal-wide",role:"dialog",aria_modal:"true",aria_label:i.t("tenant_providers.binding_editor"),div{class:"modal-header",h2{{i.t("tenant_providers.binding_editor")}}} div{class:"modal-body",div{class:"toolbar",input{class:"input-field",r#type:"search",placeholder:i.t("tenant_providers.options_search"),aria_label:i.t("tenant_providers.options_search"),value:"{option_query}",oninput:move|event|option_query.set(event.value())} button{class:"btn btn-secondary",r#type:"button",onclick:move |_|{option_applied.set(option_query());option_page.set(1)},{i.t("tenant_providers.apply")}}} label{{i.t("tenant_providers.account")} select{class:"input-field",value:"{selected}",onchange:move|event|{let value=event.value();selected_account.set(selectable_accounts.iter().find(|row|row.id.to_string()==value).map(|row|(value.clone(),format!("{} · {}",row.name,row.provider))));selected.set(value);},option{value:"",disabled:true,{i.t("tenant_providers.select_account")}} if !selected().is_empty()&&!selected_is_listed{option{value:"{selected}",selected:true,"{selected_label.clone().unwrap_or_else(||selected())}"}} for row in rows.iter(){option{value:"{row.id}","{row.name} · {row.provider}"}}}} Pager{page:option_page(),total_pages:option_totals.0,total:option_totals.1,on_page:move|value|option_page.set(value)} label{{i.t("tenant_providers.selected_account_id")} input{class:"input-field",value:"{selected}",maxlength:"36",oninput:move|event|{let value=event.value();if selected_account.peek().as_ref().is_some_and(|(id,_)|id!=&value){selected_account.set(None);}selected.set(value);}}} label{class:"checkbox-field",input{r#type:"checkbox",checked:pool(),onchange:move|e|pool.set(e.checked())}{i.t("tenant_providers.pool")}} p{class:"form-hint",{i.t("tenant_providers.binding_hint")}} if let Some(Err(error))=option_data{p{class:"alert alert-error",role:"alert",{user_error_message(i,&error)}}} if !err().is_empty(){p{class:"alert alert-error",role:"alert","{err}"}}} div{class:"modal-footer",button{class:"btn btn-secondary",disabled:busy(),onclick:move |_|onclose.call(()),{i.t("form.cancel")}} button{class:"btn btn-primary",disabled:busy(),onclick:submit,{i.t("form.save")}}}}}}
}
