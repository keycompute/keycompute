use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::get_client;
use crate::services::auth_service;
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::{UserInfo, UserStore};

#[component]
pub fn Login() -> Element {
    let i18n = use_i18n();
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut loading = use_signal(|| false);
    let mut error_msg = use_signal(|| Option::<String>::None);
    let mut show_password = use_signal(|| false);
    let mut remember_me = use_signal(|| false);
    let mut auth_store = use_context::<AuthStore>();
    let mut user_store = use_context::<UserStore>();
    let nav = use_navigator();

    // 提前提取 &'static str，闭包只捕获 Copy 类型避免成为 FnOnce
    let t_fill_all = i18n.t("auth.fill_all");
    let t_login_failed = i18n.t("auth.login_failed");

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        let email_val = email();
        let password_val = password();
        if email_val.is_empty() || password_val.is_empty() {
            error_msg.set(Some(t_fill_all.to_string()));
            return;
        }
        loading.set(true);
        error_msg.set(None);
        spawn(async move {
            match auth_service::login(&email_val, &password_val).await {
                Ok(resp) => {
                    // 设置 API 客户端 token
                    get_client().set_token(&resp.access_token);
                    // 更新 auth_store
                    auth_store.login(resp.access_token.clone());
                    // 使用登录响应中的用户信息
                    *user_store.info.write() = Some(UserInfo {
                        id: resp.user_id.clone(),
                        email: resp.email.clone(),
                        name: None,
                        role: resp.role.clone(),
                        tenant_id: resp.tenant_id.clone(),
                    });
                    nav.push(Route::Dashboard {});
                }
                Err(e) => {
                    error_msg.set(Some(format!("{t_login_failed}：{e}")));
                    loading.set(false);
                }
            }
        });
    };

    let password_type = if show_password() { "text" } else { "password" };

    rsx! {
        div {
            class: "kc-login-page",
            div { class: "kc-login-bg-grid" }
            div { class: "kc-login-bg-glow kc-login-glow-one" }
            div { class: "kc-login-bg-glow kc-login-glow-two" }
            div {
                class: "kc-login-container",
                div {
                    class: "kc-login-brand-panel",
                    div {
                        class: "kc-login-brand-content",
                        div {
                            class: "kc-login-logo",
                            div { class: "kc-login-logo-icon" }
                            div { class: "kc-login-logo-text", "KeyCompute" }
                        }
                        h1 {
                            class: "kc-login-tagline",
                            "为 "
                            span { "AI 应用" }
                            " 提供"
                            br {}
                            "高性能算力中转"
                        }
                        p {
                            class: "kc-login-description",
                            "统一大模型接入、智能路由调度、实时计费结算与全链路可观测性。开箱即用的企业级 AI Token 管理平台。"
                        }
                        div {
                            class: "kc-login-features",
                            for label in ["智能路由", "实时计费", "高可用性", "API 管理"] {
                                div {
                                    class: "kc-login-feature-badge",
                                    div { class: "kc-login-feature-dot" }
                                    "{label}"
                                }
                            }
                        }
                    }
                    div {
                        class: "kc-login-tech-circles",
                        div { class: "kc-login-circle kc-login-circle-one" }
                        div { class: "kc-login-circle kc-login-circle-two" }
                        div { class: "kc-login-circle kc-login-circle-three" }
                    }
                }

                div {
                    class: "kc-login-panel",
                    div {
                        class: "kc-login-card",
                        div {
                            class: "kc-login-header",
                            h2 { class: "kc-login-title", "登录您的账户" }
                            p { class: "kc-login-subtitle", "管理您的 AI Token 与算力资源" }
                        }

                        if let Some(err) = error_msg() {
                            div {
                                class: "kc-login-status kc-login-status-error",
                                "{err}"
                            }
                        }

                        form {
                            onsubmit: on_submit,
                            div {
                                class: "kc-login-form-group",
                                label {
                                    class: "kc-login-form-label",
                                    r#for: "kc-login-email",
                                    "邮箱地址"
                                }
                                input {
                                    id: "kc-login-email",
                                    class: "kc-login-form-input",
                                    r#type: "email",
                                    placeholder: "admin@keycompute.local",
                                    autocomplete: "email",
                                    required: true,
                                    value: "{email}",
                                    oninput: move |e| email.set(e.value()),
                                }
                                div { class: "kc-login-input-glow" }
                            }

                            div {
                                class: "kc-login-form-group",
                                label {
                                    class: "kc-login-form-label",
                                    r#for: "kc-login-password",
                                    "密码"
                                }
                                div {
                                    class: "kc-login-password-wrapper",
                                    input {
                                        id: "kc-login-password",
                                        class: "kc-login-form-input kc-login-password-input",
                                        r#type: "{password_type}",
                                        placeholder: "••••••••",
                                        autocomplete: "current-password",
                                        required: true,
                                        value: "{password}",
                                        oninput: move |e| password.set(e.value()),
                                    }
                                    button {
                                        class: "kc-login-toggle-password",
                                        r#type: "button",
                                        aria_label: if show_password() { "隐藏密码" } else { "显示密码" },
                                        onclick: move |_| show_password.set(!show_password()),
                                        if show_password() {
                                            svg {
                                                width: "20",
                                                height: "20",
                                                view_box: "0 0 24 24",
                                                fill: "none",
                                                stroke: "currentColor",
                                                stroke_width: "2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                                path { d: "M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" }
                                                line { x1: "1", y1: "1", x2: "23", y2: "23" }
                                            }
                                        } else {
                                            svg {
                                                width: "20",
                                                height: "20",
                                                view_box: "0 0 24 24",
                                                fill: "none",
                                                stroke: "currentColor",
                                                stroke_width: "2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                                path { d: "M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" }
                                                circle { cx: "12", cy: "12", r: "3" }
                                            }
                                        }
                                    }
                                }
                                div { class: "kc-login-input-glow" }
                            }

                            div {
                                class: "kc-login-form-options",
                                label {
                                    class: "kc-login-remember",
                                    input {
                                        r#type: "checkbox",
                                        checked: remember_me(),
                                        onclick: move |_| remember_me.set(!remember_me()),
                                    }
                                    div { class: "kc-login-checkbox-custom" }
                                    span { "记住我" }
                                }
                                button {
                                    class: "kc-login-forgot",
                                    r#type: "button",
                                    onclick: move |_| { nav.push(Route::ForgotPassword {}); },
                                    "忘记密码？"
                                }
                            }

                            button {
                                class: "kc-login-button",
                                r#type: "submit",
                                disabled: loading(),
                                span {
                                    if loading() { "验证中..." } else { "登录到控制台" }
                                }
                            }
                        }

                        div {
                            class: "kc-login-signup",
                            "还没有账号？"
                            button {
                                class: "kc-login-signup-link",
                                r#type: "button",
                                onclick: move |_| { nav.push(Route::Register {}); },
                                "立即注册"
                            }
                        }
                    }
                }
            }
        }
    }
}
