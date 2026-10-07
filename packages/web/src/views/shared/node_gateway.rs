use dioxus::prelude::*;
use uuid::Uuid;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::{
    api_client::{user_error_message, with_auto_refresh},
    tenant_service,
};
use crate::stores::{auth_store::AuthStore, ui_store::UiStore, user_store::UserStore};
use crate::utils::on_copy;
use crate::views::shared::accounts::NoPermissionView;
use crate::views::tenant::node_admin::{NodeAdminScope, NodeResourceConsole};

/// Platform node administration always requires an explicit tenant target.
/// The previous compatibility page mounted tenant-scoped requests without a
/// target and surfaced a raw `missing field tenant_id` transport error.
#[component]
pub fn NodeGateway() -> Element {
    let navigator = use_navigator();
    let current = use_route::<Route>();
    use_effect(move || {
        if current.to_string() == "/admin/node-gateway" {
            navigator.replace(Route::UpstreamNodes {});
        }
    });

    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let can_manage_platform = users
        .info
        .read()
        .as_ref()
        .is_some_and(|user| user.can_manage_platform());

    if !can_manage_platform {
        return rsx! {
            NoPermissionView { resource: i18n.t("page.node_gateway").to_string() }
        };
    }

    let mut selected_tenant = use_signal(String::new);
    let copied = use_signal(|| false);
    let tenants = use_resource(move || async move {
        with_auto_refresh(auth, |token| async move {
            tenant_service::list_active(&token).await
        })
        .await
    });

    let selected_scope = Uuid::parse_str(selected_tenant().trim())
        .ok()
        .filter(|id| !id.is_nil())
        .and_then(|tenant_id| NodeAdminScope::platform(auth, users, tenant_id));

    rsx! {
        div { class: "page-container node-gateway-page platform-node-admin",
            ui::PageHeader {
                title: i18n.t("page.node_gateway").to_string(),
                description: i18n.t("node_gateway.subtitle").to_string(),
            }

            NodeDispatchGuide { copied }

            section { class: "card platform-target-card",
                div { class: "card-header",
                    div {
                        h2 { class: "card-title", {i18n.t("node_gateway.target_title")} }
                        p { class: "text-secondary card-description",
                            {i18n.t("node_gateway.target_hint")}
                        }
                    }
                }
                div { class: "card-body platform-target-body",
                    match tenants() {
                        None => rsx! {
                            div { class: "inline-loading", role: "status",
                                span { class: "spinner", aria_hidden: "true" }
                                span { {i18n.t("common.loading")} }
                            }
                        },
                        Some(Err(ref error)) => rsx! {
                            div { class: "alert alert-error", role: "alert",
                                {user_error_message(i18n, error)}
                            }
                        },
                        Some(Ok(ref items)) if items.is_empty() => rsx! {
                            div { class: "empty-state compact-empty-state",
                                h3 { class: "empty-title", {i18n.t("node_gateway.no_target_title")} }
                                p { class: "empty-description", {i18n.t("node_gateway.no_target_hint")} }
                                Link { class: "btn btn-primary", to: Route::Tenants {},
                                    {i18n.t("page.tenants")}
                                }
                            }
                        },
                        Some(Ok(ref items)) => rsx! {
                            div { class: "form-field platform-target-field",
                                label { class: "form-label", r#for: "node-gateway-tenant",
                                    {i18n.t("node_gateway.target_label")}
                                }
                                select {
                                    id: "node-gateway-tenant",
                                    class: "input-field",
                                    value: "{selected_tenant}",
                                    onchange: move |event| selected_tenant.set(event.value()),
                                    option { value: "", {i18n.t("node_gateway.target_placeholder")} }
                                    for tenant in items.iter() {
                                        option { value: "{tenant.id}", "{tenant.name} · {tenant.slug}" }
                                    }
                                }
                                p { class: "form-hint", {i18n.t("node_gateway.target_safety_hint")} }
                            }
                        },
                    }
                }
            }

            if let Some(scope) = selected_scope {
                div { class: "scope-banner", role: "status",
                    span { class: "scope-banner-label", {i18n.t("node_gateway.current_target")} }
                    code { "{scope.tenant_id()}" }
                }
                for key in [scope.tenant_id().to_string()] {
                    NodeResourceConsole { key: "{key}", scope }
                }
            } else if tenants().as_ref().is_some_and(|result| result.as_ref().is_ok_and(|items| !items.is_empty())) {
                div { class: "empty-state platform-target-empty",
                    h3 { class: "empty-title", {i18n.t("node_gateway.select_target_title")} }
                    p { class: "empty-description", {i18n.t("node_gateway.select_target_hint")} }
                }
            }
        }
    }
}

#[component]
fn NodeDispatchGuide(copied: Signal<bool>) -> Element {
    let ui_store = use_context::<UiStore>();
    let i18n = use_i18n();
    let example = r#"curl "$BASE_URL/nt/v1/chat/completions" \
  -H "Authorization: Bearer $PLATFORM_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"gemma3:270m","messages":[{"role":"user","content":"Hello"}]}'"#;

    rsx! {
        details { class: "card node-gateway-dispatch-guide",
            summary { class: "card-header disclosure-summary",
                div {
                    h2 { class: "card-title", {i18n.t("node_gateway.request_title")} }
                    p { class: "text-secondary card-description",
                        {i18n.t("node_gateway.request_entry_help")}
                    }
                }
                span { class: "disclosure-chevron", aria_hidden: "true", "⌄" }
            }
            div { class: "card-body node-gateway-guide-body",
                div { class: "endpoint-callout",
                    span { "POST" }
                    code { "/nt/v1/chat/completions" }
                }
                ol { class: "node-gateway-request-steps",
                    li { {i18n.t("node_gateway.request_step_key")} }
                    li { {i18n.t("node_gateway.request_step_model")} }
                    li { {i18n.t("node_gateway.request_step_stream")} }
                }
                p { class: "form-hint", {i18n.t("node_gateway.request_auth_note")} }
                div { class: "code-sample",
                    pre { class: "code-sample-content", code { "{example}" } }
                    button {
                        class: "btn btn-secondary btn-sm code-sample-copy",
                        r#type: "button",
                        onclick: on_copy(
                            example.to_string(),
                            i18n.t("common.copy_manual_hint").to_string(),
                            ui_store,
                            copied,
                        ),
                        {i18n.t(if copied() { "api_keys.copied" } else { "api_keys.copy" })}
                    }
                }
            }
        }
    }
}
