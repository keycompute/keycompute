//! Read-only operator console: no raw business records or arbitrary JSON viewer.
mod capacity;
mod health;
mod scope;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
mod usage;

use crate::{
    hooks::use_i18n::use_i18n,
    stores::{auth_store::AuthStore, user_store::UserStore},
};
use dioxus::prelude::*;
use scope::OperationsScope;

#[component]
pub fn PlatformOperations() -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let scope = OperationsScope::current(auth, users);
    // A dynamic keyed fragment discards private filters/results when identity or
    // capabilities change. AppShell's key alone does not remount router children.
    rsx! {
        if let Some(scope)=scope {
            for key in [format!("{scope:?}")] { OperationsPage { key:"{key}", scope } }
        } else { div {class:"page-container",role:"alert",{i18n.t("operations.denied")}} }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Health,
    Usage,
    Capacity,
}
#[component]
fn OperationsPage(scope: OperationsScope) -> Element {
    let i18n = use_i18n();
    let mut tab = use_signal(|| {
        if scope.health {
            Tab::Health
        } else if scope.usage {
            Tab::Usage
        } else {
            Tab::Capacity
        }
    });
    rsx! { div {class:"page-container operations-console",
        ui::PageHeader {title:i18n.t("operations.title").to_string(), description:i18n.t("operations.hint").to_string()}
        p {class:"alert alert-info",{i18n.t("operations.boundary")}}
        nav {class:"toolbar",aria_label:i18n.t("operations.title"),
            if scope.health {button {class:"btn btn-secondary",aria_pressed:tab()==Tab::Health,onclick:move |_|tab.set(Tab::Health),{i18n.t("operations.health")}}}
            if scope.usage {button {class:"btn btn-secondary",aria_pressed:tab()==Tab::Usage,onclick:move |_|tab.set(Tab::Usage),{i18n.t("operations.usage")}}}
            if scope.capacity {button {class:"btn btn-secondary",aria_pressed:tab()==Tab::Capacity,onclick:move |_|tab.set(Tab::Capacity),{i18n.t("operations.capacity")}}}
        }
        match tab() {
            Tab::Health=>rsx!{health::HealthPanel {scope}},
            Tab::Usage=>rsx!{usage::UsagePanel {scope}},
            Tab::Capacity=>rsx!{capacity::CapacityPanel {scope}},
        }
    }}
}
