use super::scope::OperationsScope;
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use chrono::{Duration, NaiveDateTime, Utc};
use client_api::api::platform_operations::{PlatformOperationsApi, UsageOperationsQuery};
use dioxus::prelude::*;
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Platform,
    Tenant(Uuid),
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Filter {
    pub target: Target,
    pub from: String,
    pub to: String,
}
impl Filter {
    pub fn parse(target: &str, tenant: &str, from: &str, to: &str) -> Option<Self> {
        let target = match target {
            "platform" => Target::Platform,
            "tenant" => Target::Tenant(
                Uuid::parse_str(tenant.trim())
                    .ok()
                    .filter(|id| !id.is_nil())?,
            ),
            _ => return None,
        };
        let start = NaiveDateTime::parse_from_str(from, "%Y-%m-%dT%H:%M")
            .ok()?
            .and_utc();
        let end = NaiveDateTime::parse_from_str(to, "%Y-%m-%dT%H:%M")
            .ok()?
            .and_utc();
        if start >= end || end.signed_duration_since(start) > Duration::days(31) {
            return None;
        }
        Some(Self {
            target,
            from: start.to_rfc3339(),
            to: end.to_rfc3339(),
        })
    }
}
#[component]
pub(super) fn UsagePanel(scope: OperationsScope) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut target = use_signal(|| "platform".to_string());
    let mut tenant = use_signal(String::new);
    let mut to = use_signal(|| Utc::now().format("%Y-%m-%dT%H:%M").to_string());
    let mut from = use_signal(|| {
        (Utc::now() - Duration::days(1))
            .format("%Y-%m-%dT%H:%M")
            .to_string()
    });
    let mut filter =
        use_signal(|| Filter::parse("platform", "", &from(), &to()).expect("one-day UTC range"));
    let mut error = use_signal(String::new);
    let mut data = use_resource(move || {
        let filter = filter();
        let key = (scope, filter.clone());
        async move {
            let result = scope
                .read(auth, users, move |token| {
                    let filter = filter.clone();
                    async move {
                        let api = PlatformOperationsApi::new(&get_client());
                        let query = UsageOperationsQuery {
                            from: Some(filter.from),
                            to: Some(filter.to),
                        };
                        match filter.target {
                            Target::Platform => api.platform_usage(&query, &token).await,
                            Target::Tenant(id) => api.tenant_usage(id, &query, &token).await,
                        }
                    }
                })
                .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let loaded = current_keyed_value(&(scope, filter()), data.state().cloned(), data());
    let shown_target = match filter().target {
        Target::Platform => i18n.t("operations.platform").to_string(),
        Target::Tenant(id) => id.to_string(),
    };
    rsx! {section {class:"operations-usage",
        div {class:"card operations-filters",
            label {r#for:"operations-target",{i18n.t("operations.target")}}
            select {id:"operations-target",class:"input-field",value:"{target}",onchange:move |e|target.set(e.value()),option {value:"platform",{i18n.t("operations.platform")}} option {value:"tenant",{i18n.t("operations.one_tenant")}}}
            if target()=="tenant"{label {r#for:"operations-tenant-id",{i18n.t("operations.tenant_id")}}
                input {id:"operations-tenant-id",class:"input-field",value:"{tenant}",maxlength:"36",oninput:move |e|tenant.set(e.value())}}
            label {r#for:"operations-from",{i18n.t("operations.from")}}
            input {id:"operations-from",class:"input-field",r#type:"datetime-local",value:"{from}",oninput:move |e|from.set(e.value())}
            label {r#for:"operations-to",{i18n.t("operations.to")}}
            input {id:"operations-to",class:"input-field",r#type:"datetime-local",value:"{to}",oninput:move |e|to.set(e.value())}
            p {class:"text-secondary",{i18n.t("operations.range_hint")}}
            button {class:"btn btn-primary",onclick:move |_| {
                let Some(next)=Filter::parse(&target(),&tenant(),&from(),&to()) else{error.set(i18n.t("operations.invalid_range").into());return;};
                error.set(String::new());if filter()==next{data.restart();}else{filter.set(next);}
            },{i18n.t("operations.apply")}}
        }
        if !error().is_empty(){p {role:"alert",class:"alert alert-error","{error}"}}
        p {class:"operations-target-label",{i18n.t("operations.showing")} " {shown_target}"}
        match loaded {
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p {role:"alert",class:"alert alert-error",{user_error_message(&e)}}},
            Some(Ok(value))=>rsx!{
                p {class:"text-secondary","{value.from} — {value.to}"} p {class:"text-secondary",{i18n.t("operations.as_of")} " {value.as_of}"}
                div {class:"operations-table",table {class:"table",thead {tr {th {{i18n.t("operations.currency")}} th {{i18n.t("operations.requests")}} th {{i18n.t("operations.successful")}} th {"Tokens"} th {{i18n.t("operations.amount")}}}}
                    tbody {for row in &value.currencies {tr {key:"{row.currency}",td {"{row.currency}"} td {"{row.requests}"} td {"{row.successful_requests}"} td {"{row.total_tokens}"} td {"{row.billed_amount}"}}}}
                }}
                if value.currencies.is_empty(){p {{i18n.t("tenant.empty")}}}
            },
        }
    }}
}
