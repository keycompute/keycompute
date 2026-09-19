//! Redirect historical catalog-center links to the corresponding upstream tab.
use dioxus::prelude::*;
#[component]
pub fn ModelManagementBase() -> Element {
    let navigator = use_navigator();
    use_effect(move || {
        navigator.replace(crate::router::Route::UpstreamAccounts {});
    });
    rsx! {}
}
#[component]
pub fn ModelManagement(mode: String) -> Element {
    let navigator = use_navigator();
    use_effect(move || {
        let target = match mode.as_str() {
            "passthrough" => crate::router::Route::UpstreamPassthrough {},
            "node_dispatch" => crate::router::Route::UpstreamNodes {},
            _ => crate::router::Route::UpstreamAccounts {},
        };
        navigator.replace(target);
    });
    rsx! {}
}
