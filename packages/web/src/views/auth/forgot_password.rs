use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::user_error_message;
use crate::services::auth_service;

#[component]
pub fn ForgotPassword() -> Element {
    let i18n = use_i18n();
    let mut email = use_signal(String::new);
    let mut loading = use_signal(|| false);
    let mut sent = use_signal(|| false);
    let mut error_msg = use_signal(|| Option::<String>::None);

    let nav = use_navigator();

    // 提前提取 &'static str，避免闭包成为 FnOnce
    let t_enter_email = i18n.t("auth.enter_email");
    let t_send_failed = i18n.t("auth.send_failed");

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        if email().is_empty() {
            error_msg.set(Some(t_enter_email.to_string()));
            return;
        }
        loading.set(true);
        error_msg.set(None);
        let email_val = email();
        spawn(async move {
            match auth_service::forgot_password(&email_val).await {
                Ok(_) => {
                    sent.set(true);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(format!("{t_send_failed}：{}", user_error_message(&e))));
                    loading.set(false);
                }
            }
        });
    };

    rsx! {
        div {
            class: "auth-page",
            div {
                class: "auth-card",
                div {
                    class: "auth-header",
                    h1 { class: "auth-title", {i18n.t("auth.reset_password")} }
                    p { class: "auth-subtitle", {i18n.t("auth.reset_subtitle")} }
                }

                if sent() {
                    div {
                        class: "alert alert-success",
                        {i18n.t("auth.reset_sent")}
                    }
                } else {
                    if let Some(err) = error_msg() {
                        div { class: "alert alert-error", "{err}" }
                    }
                    form {
                        onsubmit: on_submit,
                        div {
                            class: "form-group",
                            label { class: "form-label", {i18n.t("auth.email")} }
                            input {
                                class: "form-input",
                                r#type: "email",
                                placeholder: i18n.t("auth.reset_email_placeholder"),
                                value: "{email}",
                                oninput: move |e| email.set(e.value()),
                            }
                        }
                        button {
                            class: "btn btn-primary btn-full",
                            r#type: "submit",
                            disabled: loading(),
                            if loading() { {i18n.t("auth.sending")} } else { {i18n.t("auth.send_reset_link")} }
                        }
                    }
                }

                div {
                    class: "auth-footer",
                    button {
                        class: "link",
                        r#type: "button",
                        onclick: move |_| { nav.push(Route::Login {}); },
                        {i18n.t("auth.back_to_login")}
                    }
                }
            }
        }
    }
}
