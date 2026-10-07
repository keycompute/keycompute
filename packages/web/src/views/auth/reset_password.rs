use dioxus::prelude::*;
use ui::ThemeCtx;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::user_error_message;
use crate::services::auth_service;
use crate::stores::public_settings_store::PublicSettingsStore;

/// 重置密码页面
/// 路由：/auth/reset-password/:token
#[component]
pub fn ResetPassword(token: String) -> Element {
    let i18n = use_i18n();
    let nav = use_navigator();
    let public_settings_store = use_context::<PublicSettingsStore>();
    let ThemeCtx(theme) = try_use_context::<ThemeCtx>()
        .unwrap_or_else(|| ThemeCtx(use_signal(|| "dark".to_string())));
    let is_dark = theme().as_str() == "dark";
    let site_name = use_memo(move || {
        public_settings_store
            .site_name()
            .unwrap_or_else(|| "KeyCompute".to_string())
    });

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
                            user_error_message(i18n, &e)
                        )));
                    }
                }
                submitting.set(false);
            });
        }
    };

    rsx! {
        document::Title { "{site_name}" }
        div { class: "kc-login-page",
            div { class: "kc-login-bg-grid" }
            if is_dark {
                div { class: "kc-login-bg-glow kc-login-glow-one" }
                div { class: "kc-login-bg-glow kc-login-glow-two" }
            }
            div { class: "kc-login-container",
                div { class: "kc-login-brand-panel",
                    div { class: "kc-login-brand-content",
                        div { class: "kc-login-logo",
                            div { class: "kc-login-logo-icon" }
                            div { class: "kc-login-logo-text", "{site_name}" }
                        }
                        h1 { class: "kc-login-tagline",
                            "{i18n.t(\"login.tagline_1\")} "
                            span { "{i18n.t(\"login.tagline_highlight\")}" }
                            " {i18n.t(\"login.tagline_2\")}" br {}
                            "{i18n.t(\"login.tagline_3\")}"
                        }
                        p { class: "kc-login-description", "{i18n.t(\"login.description\")}" }
                        div { class: "kc-login-features",
                            for label in [
                                i18n.t("login.feature_routing"),
                                i18n.t("login.feature_billing"),
                                i18n.t("login.feature_ha"),
                                i18n.t("login.feature_api"),
                            ] {
                                div { class: "kc-login-feature-badge",
                                    div { class: "kc-login-feature-dot" }
                                    "{label}"
                                }
                            }
                        }
                    }
                    div { class: "kc-login-tech-circles",
                        div { class: "kc-login-circle kc-login-circle-one" }
                        div { class: "kc-login-circle kc-login-circle-two" }
                        div { class: "kc-login-circle kc-login-circle-three" }
                    }
                }

                div { class: "kc-login-panel",
                    div { class: "kc-login-card kc-auth-card",
                        div { class: "kc-login-header",
                            h1 { class: "kc-login-title", {i18n.t("auth.reset_password")} }
                            p { class: "kc-login-subtitle", {i18n.t("auth.reset_subtitle")} }
                        }

                        if success() {
                            div { class: "kc-auth-success-block",
                                div { class: "kc-login-status kc-login-status-success",
                                    {i18n.t("reset_password.success")}
                                }
                                button {
                                    class: "kc-login-button",
                                    r#type: "button",
                                    onclick: move |_| { nav.push(Route::Login {}); },
                                    span { {i18n.t("reset_password.go_login")} }
                                }
                            }
                        } else {
                            if let Some(msg) = error_msg() {
                                div { class: "kc-login-status kc-login-status-error", role: "alert", "{msg}" }
                            }
                            form { onsubmit: on_submit,
                                div { class: "kc-login-form-group",
                                    label { class: "kc-login-form-label", r#for: "reset-password",
                                        {i18n.t("account_settings.new_password")}
                                    }
                                    input {
                                        id: "reset-password",
                                        class: "kc-login-form-input",
                                        r#type: "password",
                                        autocomplete: "new-password",
                                        placeholder: i18n.t("account_settings.new_password_placeholder"),
                                        value: "{password}",
                                        oninput: move |event| password.set(event.value()),
                                        disabled: submitting(),
                                    }
                                    div { class: "kc-login-input-glow" }
                                }
                                div { class: "kc-login-form-group",
                                    label { class: "kc-login-form-label", r#for: "reset-password-confirm",
                                        {i18n.t("auth.confirm_password")}
                                    }
                                    input {
                                        id: "reset-password-confirm",
                                        class: "kc-login-form-input",
                                        r#type: "password",
                                        autocomplete: "new-password",
                                        placeholder: i18n.t("account_settings.confirm_password_placeholder"),
                                        value: "{confirm}",
                                        oninput: move |event| confirm.set(event.value()),
                                        disabled: submitting(),
                                    }
                                    div { class: "kc-login-input-glow" }
                                }
                                button {
                                    class: "kc-login-button",
                                    r#type: "submit",
                                    disabled: submitting(),
                                    span {
                                        if submitting() { {i18n.t("auth.sending")} } else { {i18n.t("reset_password.submit")} }
                                    }
                                }
                            }
                        }

                        if !success() {
                            div { class: "kc-login-signup",
                                button {
                                    class: "kc-login-signup-link",
                                    r#type: "button",
                                    onclick: move |_| { nav.push(Route::Login {}); },
                                    {i18n.t("auth.back_to_login")}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
