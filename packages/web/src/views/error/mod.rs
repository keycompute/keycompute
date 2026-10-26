use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::stores::auth_store::AuthStore;

#[component]
pub fn NotFound(route: Vec<String>) -> Element {
    let _ = route;
    let i18n = use_i18n();
    let nav = use_navigator();
    let auth = use_context::<AuthStore>();
    rsx! {
        div {
            class: "error-page",
            div {
                class: "error-content",
                h1 { class: "error-code", "404" }
                h2 { class: "error-title", {i18n.t("page.not_found")} }
                p { class: "error-desc", {i18n.t("error.not_found_desc")} }
                button {
                    class: "btn btn-primary",
                    onclick: move |_| {
                        if auth.is_authenticated() {
                            nav.push(Route::Dashboard {});
                        } else {
                            nav.push(Route::Home {});
                        }
                    },
                    {i18n.t("error.back_home")}
                }
            }
        }
    }
}
