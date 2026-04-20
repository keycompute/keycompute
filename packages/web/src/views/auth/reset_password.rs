use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::user_error_message;
use crate::services::auth_service;

/// 重置密码页面
/// 路由：/auth/reset-password/:token
#[component]
pub fn ResetPassword(token: String) -> Element {
    let i18n = use_i18n();
    let nav = use_navigator();

    let mut password = use_signal(String::new);
    let mut confirm = use_signal(String::new);
    let mut submitting = use_signal(|| false);
    let mut error_msg = use_signal(|| None::<String>);
    let mut success = use_signal(|| false);

    let on_submit = {
        let token = token.clone();
        move |evt: Event<FormData>| {
            evt.prevent_default();
            let pwd = password();
            let cfm = confirm();

            if pwd.is_empty() || cfm.is_empty() {
                error_msg.set(Some(i18n.t("auth.fill_required").to_string()));
                return;
            }
            if pwd != cfm {
                error_msg.set(Some(i18n.t("form.password_mismatch").to_string()));
                return;
            }
            if pwd.len() < 8 {
                error_msg.set(Some(i18n.t("form.password_too_short").to_string()));
                return;
            }

            let token = token.clone();
            submitting.set(true);
            error_msg.set(None);

            spawn(async move {
                match auth_service::reset_password(&token, &pwd).await {
                    Ok(_) => {
                        success.set(true);
                    }
                    Err(e) => {
                        error_msg.set(Some(format!(
                            "{}：{}",
                            i18n.t("reset_password.failed"),
                            user_error_message(&e)
                        )));
                    }
                }
                submitting.set(false);
            });
        }
    };

    rsx! {
        div {
            class: "auth-page",
            div {
                class: "auth-card",
                h1 { class: "auth-title", {i18n.t("auth.reset_password")} }

                if success() {
                    div { class: "alert alert-success",
                        p { {i18n.t("reset_password.success")} }
                        button {
                            class: "btn btn-primary",
                            onclick: move |_| { nav.push(Route::Login {}); },
                            {i18n.t("reset_password.go_login")}
                        }
                    }
                } else {
                    if let Some(msg) = error_msg() {
                        div { class: "alert alert-error", "{msg}" }
                    }

                    form {
                        onsubmit: on_submit,
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("account_settings.new_password")} }
                            input {
                                class: "form-input",
                                r#type: "password",
                                placeholder: "{i18n.t(\"account_settings.new_password_placeholder\")}",
                                value: "{password}",
                                oninput: move |e| password.set(e.value()),
                                disabled: submitting(),
                            }
                        }
                        div { class: "form-group",
                            label { class: "form-label", {i18n.t("auth.confirm_password")} }
                            input {
                                class: "form-input",
                                r#type: "password",
                                placeholder: "{i18n.t(\"account_settings.confirm_password_placeholder\")}",
                                value: "{confirm}",
                                oninput: move |e| confirm.set(e.value()),
                                disabled: submitting(),
                            }
                        }
                        button {
                            class: "btn btn-primary btn-full",
                            r#type: "submit",
                            disabled: submitting(),
                            if submitting() { {i18n.t("auth.sending")} } else { {i18n.t("reset_password.submit")} }
                        }
                    }

                    div { class: "auth-footer",
                        button {
                            class: "link-btn",
                            onclick: move |_| { nav.push(Route::Login {}); },
                            {i18n.t("auth.back_to_login")}
                        }
                    }
                }
            }
        }
    }
}
