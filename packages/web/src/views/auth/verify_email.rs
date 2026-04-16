use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::auth_service;

/// 邮箱验证页面
/// 路由：/auth/verify-email/:token
/// 挂载后自动调用验证接口，无需用户操作
#[component]
pub fn VerifyEmail(token: String) -> Element {
    let i18n = use_i18n();
    let nav = use_navigator();

    // 页面挂载后自动发起验证请求
    let verify_result = use_resource(move || {
        let token = token.clone();
        async move { auth_service::verify_email(&token).await }
    });

    rsx! {
        div {
            class: "auth-page",
            div {
                class: "auth-card",
                h1 { class: "auth-title", {i18n.t("verify_email.title")} }

                match verify_result() {
                    None => rsx! {
                        div { class: "verify-loading",
                            p { {i18n.t("verify_email.verifying")} }
                        }
                    },
                    Some(Ok(_)) => rsx! {
                        div { class: "alert alert-success",
                            p { {i18n.t("verify_email.success")} }
                            button {
                                class: "btn btn-primary",
                                onclick: move |_| { nav.push(Route::Login {}); },
                                {i18n.t("verify_email.go_login")}
                            }
                        }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "alert alert-error",
                            p { "{i18n.t(\"verify_email.failed\")}：{e}" }
                            p { class: "text-secondary", {i18n.t("verify_email.expired_hint")} }
                            button {
                                class: "btn btn-secondary",
                                onclick: move |_| { nav.push(Route::Login {}); },
                                {i18n.t("auth.back_to_login")}
                            }
                        }
                    },
                }
            }
        }
    }
}
