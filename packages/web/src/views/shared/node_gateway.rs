use dioxus::prelude::*;
use ui::{Badge, BadgeVariant, Table, TableHead};

use crate::hooks::use_i18n::use_i18n;
use crate::services::{api_client::with_auto_refresh, node_gateway_service};
use crate::stores::{auth_store::AuthStore, user_store::UserStore};
use crate::views::shared::accounts::NoPermissionView;

#[component]
pub fn NodeGateway() -> Element {
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
        return rsx! { NoPermissionView { resource: i18n.t("page.node_gateway").to_string() } };
    }

    let overview = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            node_gateway_service::overview(&token).await
        })
        .await
    });

    rsx! {
        div { class: "page-container node-gateway-page",
            div { class: "page-header",
                div {
                    h1 { class: "page-title", {i18n.t("page.node_gateway")} }
                    p { class: "page-description", {i18n.t("node_gateway.subtitle")} }
                }
            }

            match overview() {
                None => rsx! { p { class: "text-secondary", {i18n.t("table.loading")} } },
                Some(Err(ref e)) => rsx! {
                    div { class: "alert alert-error",
                        p { "{i18n.t(\"common.load_failed\")}: {e}" }
                    }
                },
                Some(Ok(ref data)) => rsx! {
                    div { class: "section",
                        div { class: "node-gateway-status-row",
                            div {
                                h2 { class: "section-title", {i18n.t("node_gateway.runtime_status")} }
                                p { class: "text-secondary", {i18n.t("node_gateway.runtime_desc")} }
                            }
                            if data.enabled {
                                Badge { variant: BadgeVariant::Success, {i18n.t("node_gateway.enabled")} }
                            } else {
                                Badge { variant: BadgeVariant::Warning, {i18n.t("node_gateway.disabled")} }
                            }
                        }
                        div { class: "stats-grid",
                            StatCard { label: i18n.t("node_gateway.nodes_total").to_string(), value: data.node_stats.total.to_string(), meta: i18n.t("node_gateway.nodes_total_desc").to_string() }
                            StatCard { label: i18n.t("node_gateway.nodes_online").to_string(), value: data.node_stats.online.to_string(), meta: i18n.t("node_gateway.nodes_online_desc").to_string() }
                            StatCard { label: i18n.t("node_gateway.tasks_active").to_string(), value: (data.task_stats.queued + data.task_stats.leased).to_string(), meta: i18n.t("node_gateway.tasks_active_desc").to_string() }
                            StatCard { label: i18n.t("node_gateway.tasks_done").to_string(), value: data.task_stats.succeeded.to_string(), meta: i18n.t("node_gateway.tasks_done_desc").to_string() }
                        }
                    }

                    div { class: "section",
                        h2 { class: "section-title", {i18n.t("node_gateway.protocol_title")} }
                        div { class: "node-gateway-protocol-grid",
                            ProtocolItem { method: "POST".to_string(), path: "/node/v1/register".to_string(), desc: i18n.t("node_gateway.protocol_register").to_string() }
                            ProtocolItem { method: "POST".to_string(), path: "/node/v1/heartbeat".to_string(), desc: i18n.t("node_gateway.protocol_heartbeat").to_string() }
                            ProtocolItem { method: "POST".to_string(), path: "/node/v1/tasks/poll".to_string(), desc: i18n.t("node_gateway.protocol_poll").to_string() }
                            ProtocolItem { method: "POST".to_string(), path: "/node/v1/tasks/{task_id}/complete".to_string(), desc: i18n.t("node_gateway.protocol_complete").to_string() }
                        }
                    }

                    div { class: "section",
                        h2 { class: "section-title", {i18n.t("node_gateway.nodes_title")} }
                        Table {
                            empty: data.nodes.is_empty(),
                            empty_text: i18n.t("node_gateway.no_nodes"),
                            col_count: 6,
                            thead {
                                tr {
                                    TableHead { {i18n.t("node_gateway.node")} }
                                    TableHead { {i18n.t("table.status")} }
                                    TableHead { {i18n.t("node_gateway.models")} }
                                    TableHead { {i18n.t("node_gateway.failures")} }
                                    TableHead { {i18n.t("node_gateway.heartbeat")} }
                                    TableHead { "ID" }
                                }
                            }
                            tbody {
                                for node in data.nodes.iter() {
                                    tr {
                                        td {
                                            div { class: "account-cell-main",
                                                div { class: "account-name-row",
                                                    span { class: "account-name", "{node.display_name}" }
                                                }
                                                div { class: "account-subline", "{node.client_instance_id}" }
                                            }
                                        }
                                        td { NodeStatusBadge { status: node.status.clone() } }
                                        td {
                                            div { class: "account-models",
                                                for model in accepted_models(&node.accepted_models_json).iter() {
                                                    span { class: "account-model-chip", "{model}" }
                                                }
                                                if accepted_models(&node.accepted_models_json).is_empty() {
                                                    span { class: "account-model-chip account-model-chip-muted", {i18n.t("node_gateway.no_models")} }
                                                }
                                            }
                                        }
                                        td { "{node.consecutive_failure_count}/{node.failure_threshold}" }
                                        td { span { class: "account-time-value", {node.last_heartbeat_at.as_deref().unwrap_or("—")} } }
                                        td { span { class: "account-id", "{short_id(&node.id)}" } }
                                    }
                                }
                            }
                        }
                    }

                    div { class: "section",
                        h2 { class: "section-title", {i18n.t("node_gateway.tasks_title")} }
                        Table {
                            empty: data.recent_tasks.is_empty(),
                            empty_text: i18n.t("node_gateway.no_tasks"),
                            col_count: 6,
                            thead {
                                tr {
                                    TableHead { {i18n.t("pricing.model_name")} }
                                    TableHead { {i18n.t("table.status")} }
                                    TableHead { {i18n.t("node_gateway.assigned_node")} }
                                    TableHead { {i18n.t("node_gateway.failures")} }
                                    TableHead { {i18n.t("node_gateway.deadline")} }
                                    TableHead { "ID" }
                                }
                            }
                            tbody {
                                for task in data.recent_tasks.iter() {
                                    tr {
                                        td { "{task.model}" }
                                        td { TaskStatusBadge { status: task.status.clone() } }
                                        td { "{task.assigned_node_id.as_deref().map(short_id).unwrap_or_else(|| \"—\".to_string())}" }
                                        td { "{task.failure_count}/{task.failure_threshold}" }
                                        td { span { class: "account-time-value", "{task.deadline_at}" } }
                                        td { span { class: "account-id", "{short_id(&task.id)}" } }
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn StatCard(label: String, value: String, meta: String) -> Element {
    rsx! {
        div { class: "stat-card",
            p { class: "stat-label", "{label}" }
            p { class: "stat-value", "{value}" }
            p { class: "stat-meta", "{meta}" }
        }
    }
}

#[component]
fn ProtocolItem(method: String, path: String, desc: String) -> Element {
    rsx! {
        div { class: "node-gateway-protocol-item",
            div { class: "node-gateway-protocol-head",
                span { class: "node-gateway-method", "{method}" }
                code { class: "node-gateway-path", "{path}" }
            }
            p { class: "text-secondary", "{desc}" }
        }
    }
}

#[component]
fn NodeStatusBadge(status: String) -> Element {
    let i18n = use_i18n();
    let variant = match status.as_str() {
        "online" => BadgeVariant::Success,
        "excluded" => BadgeVariant::Error,
        "offline" => BadgeVariant::Warning,
        _ => BadgeVariant::Neutral,
    };
    let label = match status.as_str() {
        "online" => i18n.t("node_gateway.status_online"),
        "offline" => i18n.t("node_gateway.status_offline"),
        "excluded" => i18n.t("node_gateway.status_excluded"),
        _ => status.as_str(),
    };
    rsx! { Badge { variant, "{label}" } }
}

#[component]
fn TaskStatusBadge(status: String) -> Element {
    let i18n = use_i18n();
    let variant = match status.as_str() {
        "succeeded" => BadgeVariant::Success,
        "failed" | "expired" => BadgeVariant::Error,
        "leased" => BadgeVariant::Warning,
        "queued" => BadgeVariant::Neutral,
        _ => BadgeVariant::Neutral,
    };
    let label = match status.as_str() {
        "queued" => i18n.t("node_gateway.task_queued"),
        "leased" => i18n.t("node_gateway.task_leased"),
        "succeeded" => i18n.t("node_gateway.task_succeeded"),
        "failed" => i18n.t("node_gateway.task_failed"),
        "expired" => i18n.t("node_gateway.task_expired"),
        _ => status.as_str(),
    };
    rsx! { Badge { variant, "{label}" } }
}

fn accepted_models(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
