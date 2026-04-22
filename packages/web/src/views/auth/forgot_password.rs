use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::user_error_message;
use crate::services::auth_service;

const FORGOT_PASSWORD_IDENTITY_ERROR: &str = "邮箱地址或用户名错误，无法发送重置密码链接";
const FORGOT_PASSWORD_ERROR_COOLDOWN_SECS: u32 = 30;

fn start_error_cooldown(mut cooldown_seconds: Signal<u32>) {
    cooldown_seconds.set(FORGOT_PASSWORD_ERROR_COOLDOWN_SECS);
    spawn(async move {
        let mut remaining = FORGOT_PASSWORD_ERROR_COOLDOWN_SECS;
        while remaining > 0 {
            TimeoutFuture::new(1_000).await;
            remaining -= 1;
            cooldown_seconds.set(remaining);
        }
    });
}

#[component]
pub fn ForgotPassword() -> Element {
    let i18n = use_i18n();
    let mut name = use_signal(String::new);
    let mut email = use_signal(String::new);
    let mut loading = use_signal(|| false);
    let cooldown_seconds = use_signal(|| 0u32);
    let mut sent = use_signal(|| false);
    let mut error_msg = use_signal(|| Option::<String>::None);

    let nav = use_navigator();

    // 提前提取 &'static str，避免闭包成为 FnOnce
    let t_enter_name = i18n.t("auth.enter_username");
    let t_enter_email = i18n.t("auth.enter_email");

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        if loading() || cooldown_seconds() > 0 {
            return;
        }
        if name().trim().is_empty() {
            error_msg.set(Some(t_enter_name.to_string()));
            return;
        }
        if email().trim().is_empty() {
            error_msg.set(Some(t_enter_email.to_string()));
            return;
        }
        loading.set(true);
        error_msg.set(None);
        let name_val = name();
        let email_val = email();
        let cooldown_signal = cooldown_seconds;
        spawn(async move {
            match auth_service::forgot_password(&name_val, &email_val).await {
                Ok(_) => {
                    sent.set(true);
                    loading.set(false);
                }
                Err(e) => {
                    let message = user_error_message(&e);
                    if message == FORGOT_PASSWORD_IDENTITY_ERROR {
                        start_error_cooldown(cooldown_signal);
                    }
                    error_msg.set(Some(message));
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
                            label { class: "form-label", {i18n.t("auth.username")} }
                            input {
                                class: "form-input",
                                r#type: "text",
                                placeholder: i18n.t("auth.reset_username_placeholder"),
                                value: "{name}",
                                oninput: move |e| name.set(e.value()),
                            }
                        }
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
                            disabled: loading() || cooldown_seconds() > 0,
                            if loading() {
                                {i18n.t("auth.sending")}
                            } else if cooldown_seconds() > 0 {
                                {format!("{} ({}s)", i18n.t("auth.cooldown_retry"), cooldown_seconds())}
                            } else {
                                {i18n.t("auth.send_reset_link")}
                            }
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
