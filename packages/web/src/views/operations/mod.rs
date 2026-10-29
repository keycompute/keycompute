//! Read-only operator console: no raw business records or arbitrary JSON viewer.
mod capacity;
mod health;
mod scope;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
mod usage;

use crate::{
    hooks::use_i18n::use_i18n,
    router::{OperationsQuery, Route},
    stores::{auth_store::AuthStore, user_store::UserStore},
    utils::focus_element,
};
use dioxus::prelude::*;
use scope::OperationsScope;

#[component]
pub fn PlatformOperations(query: OperationsQuery) -> Element {
    let auth = use_context::<AuthStore>();
    let users = use_context::<UserStore>();
    let i18n = use_i18n();
    let scope = OperationsScope::current(auth, users);
    // A dynamic keyed fragment discards private filters/results when identity or
    // capabilities change. AppShell's key alone does not remount router children.
    rsx! {
        if let Some(scope)=scope {
            for key in [format!("{scope:?}:{query:?}")] { OperationsPage { key:"{key}", scope, query: query.clone() } }
        } else { div {class:"page-container",role:"alert",{i18n.t("operations.denied")}} }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Health,
    Usage,
    Capacity,
}

impl Tab {
    fn key(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Usage => "usage",
            Self::Capacity => "capacity",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Health => "operations-tab-health",
            Self::Usage => "operations-tab-usage",
            Self::Capacity => "operations-tab-capacity",
        }
    }
}

fn available_tabs(scope: OperationsScope) -> Vec<Tab> {
    [
        (scope.health, Tab::Health),
        (scope.usage, Tab::Usage),
        (scope.capacity, Tab::Capacity),
    ]
    .into_iter()
    .filter_map(|(available, tab)| available.then_some(tab))
    .collect()
}

fn selected_tab(scope: OperationsScope, query: &OperationsQuery) -> Tab {
    let tabs = available_tabs(scope);
    query
        .tab
        .as_deref()
        .and_then(|value| tabs.iter().copied().find(|tab| tab.key() == value))
        .or_else(|| tabs.first().copied())
        .unwrap_or(Tab::Health)
}

fn adjacent_tab(scope: OperationsScope, current: Tab, forward: bool) -> Option<Tab> {
    let tabs = available_tabs(scope);
    let index = tabs.iter().position(|tab| *tab == current)?;
    if forward {
        tabs.get(index + 1)
            .copied()
            .or_else(|| tabs.first().copied())
    } else {
        index
            .checked_sub(1)
            .and_then(|previous| tabs.get(previous).copied())
            .or_else(|| tabs.last().copied())
    }
}

#[component]
fn OperationsPage(scope: OperationsScope, query: OperationsQuery) -> Element {
    let i18n = use_i18n();
    let nav = use_navigator();
    let tab = selected_tab(scope, &query);
    let navigate = move |next: Tab| {
        nav.replace(Route::PlatformOperations {
            query: OperationsQuery::for_tab(next.key()),
        });
    };
    rsx! { div {class:"page-container operations-console",
        ui::PageHeader {title:i18n.t("operations.title").to_string(), description:i18n.t("operations.hint").to_string()}
        p {class:"alert alert-info",{i18n.t("operations.boundary")}}
        nav {class:"segmented-control operations-tabs",role:"tablist",aria_label:i18n.t("operations.title"),
            if scope.health {button {id:Tab::Health.id(),class:if tab==Tab::Health {"btn btn-primary"} else {"btn btn-secondary"},role:"tab",aria_selected:tab==Tab::Health,aria_controls:"operations-health-panel",aria_pressed:tab==Tab::Health,tabindex:if tab==Tab::Health {"0"} else {"-1"},onclick:move |_|navigate(Tab::Health),onkeydown:move |event| match event.key() {Key::ArrowRight=>{event.prevent_default();if let Some(next)=adjacent_tab(scope,tab,true){navigate(next);focus_element(next.id());}},Key::ArrowLeft=>{event.prevent_default();if let Some(next)=adjacent_tab(scope,tab,false){navigate(next);focus_element(next.id());}},Key::Home=>{event.prevent_default();if let Some(next)=available_tabs(scope).first().copied(){navigate(next);focus_element(next.id());}},Key::End=>{event.prevent_default();if let Some(next)=available_tabs(scope).last().copied(){navigate(next);focus_element(next.id());}},_=>{}}, {i18n.t("operations.health")}}}
            if scope.usage {button {id:Tab::Usage.id(),class:if tab==Tab::Usage {"btn btn-primary"} else {"btn btn-secondary"},role:"tab",aria_selected:tab==Tab::Usage,aria_controls:"operations-usage-panel",aria_pressed:tab==Tab::Usage,tabindex:if tab==Tab::Usage {"0"} else {"-1"},onclick:move |_|navigate(Tab::Usage),onkeydown:move |event| match event.key() {Key::ArrowRight=>{event.prevent_default();if let Some(next)=adjacent_tab(scope,tab,true){navigate(next);focus_element(next.id());}},Key::ArrowLeft=>{event.prevent_default();if let Some(next)=adjacent_tab(scope,tab,false){navigate(next);focus_element(next.id());}},Key::Home=>{event.prevent_default();if let Some(next)=available_tabs(scope).first().copied(){navigate(next);focus_element(next.id());}},Key::End=>{event.prevent_default();if let Some(next)=available_tabs(scope).last().copied(){navigate(next);focus_element(next.id());}},_=>{}}, {i18n.t("operations.usage")}}}
            if scope.capacity {button {id:Tab::Capacity.id(),class:if tab==Tab::Capacity {"btn btn-primary"} else {"btn btn-secondary"},role:"tab",aria_selected:tab==Tab::Capacity,aria_controls:"operations-capacity-panel",aria_pressed:tab==Tab::Capacity,tabindex:if tab==Tab::Capacity {"0"} else {"-1"},onclick:move |_|navigate(Tab::Capacity),onkeydown:move |event| match event.key() {Key::ArrowRight=>{event.prevent_default();if let Some(next)=adjacent_tab(scope,tab,true){navigate(next);focus_element(next.id());}},Key::ArrowLeft=>{event.prevent_default();if let Some(next)=adjacent_tab(scope,tab,false){navigate(next);focus_element(next.id());}},Key::Home=>{event.prevent_default();if let Some(next)=available_tabs(scope).first().copied(){navigate(next);focus_element(next.id());}},Key::End=>{event.prevent_default();if let Some(next)=available_tabs(scope).last().copied(){navigate(next);focus_element(next.id());}},_=>{}}, {i18n.t("operations.capacity")}}}
        }
        match tab {
            Tab::Health=>rsx!{health::HealthPanel {scope}},
            Tab::Usage=>rsx!{usage::UsagePanel {scope}},
            Tab::Capacity=>rsx!{capacity::CapacityPanel {scope}},
        }
    }}
}
