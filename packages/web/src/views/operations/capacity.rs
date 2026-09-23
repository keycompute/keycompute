use super::scope::OperationsScope;
use crate::{
    hooks::use_i18n::use_i18n,
    services::api_client::{get_client, user_error_message},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::resource::{KeyedResourceValue, current_keyed_value},
};
use client_api::api::platform_operations::PlatformOperationsApi;
use dioxus::prelude::*;
use serde_json::Value;
// Deliberately display named numeric counters, never arbitrary server JSON.
const COUNTERS: &[(&str, &str)] = &[
    ("managed_payload_bytes", "limit"),
    ("managed_payload_bytes", "used"),
    ("managed_payload_bytes", "peak"),
    ("ingress", "active"),
    ("ingress", "queued"),
    ("generation", "active"),
    ("generation", "queued"),
    ("writer_pool", "connections"),
    ("writer_pool", "idle"),
    ("redis_commands", "connections"),
    ("redis_commands", "available"),
    ("redis_commands", "waiting"),
    ("redis_commands", "limit"),
    ("redis_cache", "connections"),
    ("redis_cache", "available"),
    ("redis_cache", "waiting"),
    ("redis_cache", "limit"),
];
pub(super) fn projection(value: &Value) -> Vec<(String, String)> {
    COUNTERS
        .iter()
        .map(|(group, field)| {
            (
                format!("{group}.{field}"),
                value
                    .get(group)
                    .and_then(|v| v.get(field))
                    .and_then(Value::as_u64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
            )
        })
        .collect()
}
#[component]
pub(super) fn CapacityPanel(scope: OperationsScope) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let mut data = use_resource(move || async move {
        let result = scope
            .read(auth, users, move |token| async move {
                PlatformOperationsApi::new(&get_client())
                    .capacity(&token)
                    .await
            })
            .await
            .map(|v| projection(&v));
        KeyedResourceValue::new(scope, result)
    });
    let loaded = current_keyed_value(&scope, data.state().cloned(), data());
    rsx! {section {class:"operations-capacity",
        p {class:"text-secondary",{i18n.t("operations.process_hint")}}
        button {class:"btn btn-secondary",onclick:move |_|data.restart(),{i18n.t("tenant.reload")}}
        match loaded {
            None=>rsx!{p {role:"status",{i18n.t("common.loading")}}},
            Some(Err(e))=>rsx!{p {role:"alert",class:"alert alert-error",{user_error_message(&e)}}},
            Some(Ok(rows))=>rsx!{div {class:"operations-table",table {class:"table",thead {tr {th {{i18n.t("operations.metric")}} th {{i18n.t("operations.value")}}}} tbody {for (name,value) in rows {tr {key:"{name}",td {code {"{name}"}} td {"{value}"}}}}}}},
        }
    }}
}
