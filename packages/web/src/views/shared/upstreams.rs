use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;

/// Shared identity and navigation for the three upstream resources.  Each tab
/// is a real route and embeds its resource view below this header.
#[component]
pub fn UpstreamTabs(active: String, children: Element) -> Element {
    let i = use_i18n();
    let tabs = [
        (
            "accounts",
            i.t("upstreams.accounts"),
            Route::UpstreamAccounts {}.to_string(),
        ),
        (
            "passthrough",
            i.t("upstreams.passthrough"),
            Route::UpstreamPassthrough {}.to_string(),
        ),
        (
            "nodes",
            i.t("upstreams.nodes"),
            Route::UpstreamNodes {}.to_string(),
        ),
    ];
    rsx! {
        div { class: "page-container upstreams-page",
            div { class: "page-header upstreams-header",
                div { class: "page-header-main",
                    h1 { class: "page-title", {i.t("upstreams.title")} }
                    p { class: "page-description", {i.t("upstreams.subtitle")} }
                }
            }
            nav { class: "upstream-tabs", aria_label: i.t("upstreams.title"),
                for (key, label, href) in tabs {
                    Link {
                        to: href,
                        class: if active == key { "upstream-tab active" } else { "upstream-tab" },
                        aria_current: if active == key { "page" } else { "false" },
                        "{label}"
                    }
                }
            }
            {children}
        }
    }
}

#[component]
pub fn UpstreamAccounts() -> Element {
    rsx! { UpstreamTabs { active: "accounts".to_string(), crate::views::shared::accounts::Accounts {} } }
}

#[component]
pub fn UpstreamPassthrough() -> Element {
    rsx! { UpstreamTabs { active: "passthrough".to_string(), crate::views::shared::passthrough_bindings::PassthroughBindings {} } }
}

#[component]
pub fn UpstreamNodes() -> Element {
    rsx! { UpstreamTabs { active: "nodes".to_string(), crate::views::shared::node_gateway::NodeGateway {} } }
}
