use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::i18n::{I18n, Lang};
use crate::router::Route;
use crate::services::api_client::{get_client, user_error_message};
use crate::services::auth_service;
use crate::stores::auth_store::AuthStore;
use crate::stores::user_store::{UserInfo, UserStore};
use ui::components::modal::Modal;

/// 首页组件 - 现代化自适应设计
#[component]
pub fn Home() -> Element {
    let nav = use_navigator();

    // 弹窗状态管理
    let mut show_login_modal = use_signal(|| false);
    let mut show_register_modal = use_signal(|| false);

    // 主题状态
    let mut theme = use_context::<Signal<String>>();
    let is_dark = theme().as_str() == "dark";

    // 语言状态：首页本地维护，从 localStorage 读取初值
    let mut lang = use_signal(|| {
        #[cfg(target_arch = "wasm32")]
        {
            read_lang_from_storage().unwrap_or_else(|| "zh".to_string())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            "zh".to_string()
        }
    });
    let current_lang = lang();
    let i18n = I18n::new(Lang::from_str(&current_lang));
    let is_zh = current_lang == "zh";

    // 检查是否已登录
    let auth_store = use_context::<AuthStore>();
    let is_authenticated = auth_store.is_authenticated();

    // 提前提取所有 i18n 文本
    let t_toggle_theme = i18n.t("home.toggle_theme");
    let t_toggle_lang = if is_zh {
        i18n.t("layout.switch_to_en")
    } else {
        i18n.t("layout.switch_to_zh")
    };
    let t_login = i18n.t("home.login");
    let t_register = i18n.t("home.register");
    let t_tagline_1 = i18n.t("login.tagline_1");
    let t_tagline_highlight = i18n.t("login.tagline_highlight");
    let t_tagline_2 = i18n.t("login.tagline_2");
    let t_description = i18n.t("login.description");
    let t_features_title = i18n.t("home.features.title");
    let t_feature_routing = i18n.t("login.feature_routing");
    let t_feature_billing = i18n.t("login.feature_billing");
    let t_feature_ha = i18n.t("login.feature_ha");
    let t_feature_api = i18n.t("login.feature_api");
    let t_feature_metering = i18n.t("login.feature_metering");
    let t_feature_custom = i18n.t("login.feature_custom");
    let t_routing_title = i18n.t("home.features.routing.title");
    let t_routing_desc = i18n.t("home.features.routing.desc");
    let t_billing_title = i18n.t("home.features.billing.title");
    let t_billing_desc = i18n.t("home.features.billing.desc");
    let t_cluster_title = i18n.t("home.features.cluster.title");
    let t_cluster_desc = i18n.t("home.features.cluster.desc");
    let t_node_rental_title = i18n.t("home.features.node_rental.title");
    let t_node_rental_desc = i18n.t("home.features.node_rental.desc");
    let t_distribution_title = i18n.t("home.features.distribution.title");
    let t_distribution_desc = i18n.t("home.features.distribution.desc");
    let t_custom_title = i18n.t("home.features.custom.title");
    let t_custom_desc = i18n.t("home.features.custom.desc");

    // 已登录则跳转到仪表板
    use_effect(move || {
        if is_authenticated {
            nav.replace(Route::Dashboard {});
        }
    });

    rsx! {
        div {
            class: "kc-home-page",
            // 背景动画效果 - 根据主题显示不同背景
            if is_dark {
                // 黑暗主题：星空背景
                div { class: "kc-home-stars-container",
                    // 生成星星（减少到50个，增大尺寸，增强缩放效果）
                    {(0..50).map(|i| {
                        let style = format!(
                            "--star-left: {}%; --star-top: {}%; --star-size: {}; --star-delay: {}s; --star-duration: {}s;",
                            (i * 37 % 100),
                            (i * 53 % 100),
                            if i % 5 == 0 { "4px" } else if i % 5 == 1 { "5px" } else if i % 5 == 2 { "3px" } else { "2px" },
                            (i % 10) as f64 * 0.5,
                            (2 + (i % 4)) as f64
                        );
                        rsx! {
                            div {
                                class: "kc-home-star",
                                style: "{style}",
                            }
                        }
                    })}
                }
                div { class: "kc-home-shooting-stars",
                    {(0..5).map(|i| {
                        let style = format!(
                            "--shooting-delay: {}s; --shooting-duration: {}s; --shooting-top: {}%;",
                            (i * 3) as f64 * 1.5,
                            1.5 + (i % 3) as f64 * 0.5,
                            10 + (i * 20) % 70
                        );
                        rsx! {
                            div {
                                class: "kc-home-shooting-star",
                                style: "{style}",
                            }
                        }
                    })}
                }
            } else {
                // 明亮主题：晴空白云
                div { class: "kc-home-sky" }
                div { class: "kc-home-clouds-container",
                    {(0..8).map(|i| {
                        let style = format!(
                            "--cloud-left: {}%; --cloud-top: {}%; --cloud-scale: {}; --cloud-delay: {}s; --cloud-duration: {}s; --cloud-opacity: {};",
                            (i * 47 % 100),
                            5 + (i * 23 % 60),
                            0.4 + (i % 5) as f64 * 0.2,
                            (i % 8) as f64 * 2.0,
                            30.0 + (i % 5) as f64 * 10.0,
                            0.6 + (i % 4) as f64 * 0.1
                        );
                        rsx! {
                            div {
                                class: "kc-home-cloud",
                                style: "{style}",
                            }
                        }
                    })}
                }
            }
            div { class: "kc-home-grid-overlay" }

            // 导航栏
            header {
                class: "kc-home-header",
                div {
                    class: "kc-home-nav container",
                    // Logo 区域
                    div {
                        class: "kc-home-logo",
                        img {
                            src: asset!("/assets/logo.jpg"),
                            alt: "KeyCompute",
                        }
                        span { class: "kc-home-logo-text", "KeyCompute" }
                    }

                    // 导航功能区
                    div {
                        class: "kc-home-nav-actions",
                        // GitHub 仓库链接
                        a {
                            class: "kc-home-github-link",
                            href: "https://github.com/keycompute",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            title: "GitHub",
                            aria_label: "GitHub",
                            svg {
                                width: "20",
                                height: "20",
                                view_box: "0 0 24 24",
                                fill: "currentColor",
                                path {
                                    d: "M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.4 3-.405 1.02.005 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12"
                                }
                            }
                        }
                        // 主题切换
                        button {
                            class: "kc-home-theme-toggle",
                            r#type: "button",
                            onclick: move |_| {
                                let new_theme = if theme().as_str() == "dark" {
                                    "light".to_string()
                                } else {
                                    "dark".to_string()
                                };
                                *theme.write() = new_theme.clone();

                                // 添加切换动画类
                                #[cfg(target_arch = "wasm32")]
                                {
                                    apply_theme_with_animation(&new_theme);
                                }
                            },
                            title: "{t_toggle_theme}",
                            if is_dark {
                                svg {
                                    width: "20",
                                    height: "20",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "2",
                                    path { d: "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" }
                                }
                            } else {
                                svg {
                                    width: "20",
                                    height: "20",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "2",
                                    circle { cx: "12", cy: "12", r: "5" }
                                    line { x1: "12", y1: "1", x2: "12", y2: "3" }
                                    line { x1: "12", y1: "21", x2: "12", y2: "23" }
                                    line { x1: "4.22", y1: "4.22", x2: "5.64", y2: "5.64" }
                                    line { x1: "18.36", y1: "18.36", x2: "19.78", y2: "19.78" }
                                    line { x1: "1", y1: "12", x2: "3", y2: "12" }
                                    line { x1: "21", y1: "12", x2: "23", y2: "12" }
                                    line { x1: "4.22", y1: "19.78", x2: "5.64", y2: "18.36" }
                                    line { x1: "18.36", y1: "5.64", x2: "19.78", y2: "4.22" }
                                }
                            }
                        }

                        // 语言切换
                        button {
                            class: "kc-home-lang-toggle",
                            r#type: "button",
                            title: "{t_toggle_lang}",
                            onclick: move |_| {
                                let new_lang = if lang().as_str() == "zh" {
                                    "en".to_string()
                                } else {
                                    "zh".to_string()
                                };
                                *lang.write() = new_lang.clone();
                                #[cfg(target_arch = "wasm32")]
                                {
                                    save_lang_to_storage(&new_lang);
                                }
                            },
                            span { class: "kc-home-lang-toggle-text",
                                if is_zh { "EN" } else { "中" }
                            }
                        }

                        // 登录/注册按钮
                        if !is_authenticated {
                            div {
                                class: "kc-home-auth-buttons",
                                button {
                                    class: "kc-home-btn-login",
                                    r#type: "button",
                                    onclick: move |_| show_login_modal.set(true),
                                    "{t_login}"
                                }
                                button {
                                    class: "kc-home-btn-register",
                                    r#type: "button",
                                    onclick: move |_| show_register_modal.set(true),
                                    "{t_register}"
                                }
                            }
                        }
                    }
                }
            }

            // 主要内容区域
            main {
                class: "kc-home-main",
                // Hero 区域
                section {
                    class: "kc-home-hero",
                    div {
                        class: "container",
                        div {
                            class: "kc-home-hero-content",
                            h1 {
                                class: "kc-home-hero-title",
                                span { class: "kc-home-hero-highlight", "{t_tagline_1} {t_tagline_highlight}" }
                            }
                            if !t_tagline_2.is_empty() {
                                p { class: "kc-home-hero-slogan", "{t_tagline_2}" }
                            }
                            p {
                                class: "kc-home-hero-description",
                                "{t_description}"
                            }

                            // 功能特性标签
                            div {
                                class: "kc-home-features-tags",
                                span { class: "kc-home-feature-tag", "{t_feature_routing}" }
                                span { class: "kc-home-feature-tag", "{t_feature_billing}" }
                                span { class: "kc-home-feature-tag", "{t_feature_ha}" }
                                span { class: "kc-home-feature-tag", "{t_feature_api}" }
                                span { class: "kc-home-feature-tag", "{t_feature_metering}" }
                                span { class: "kc-home-feature-tag", "{t_feature_custom}" }
                            }
                        }
                    }
                }

                // 核心功能区域
                section {
                    class: "kc-home-features",
                    div {
                        class: "container",
                        h2 { class: "kc-home-section-title", "{t_features_title}" }
                        div {
                            class: "kc-home-features-grid",
                            // 智能路由卡片
                            div {
                                class: "kc-home-feature-card",
                                div {
                                    class: "kc-home-feature-icon",
                                    svg {
                                        width: "32",
                                        height: "32",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        path { d: "M22 12h-4l-3 9L9 3l-3 9H2" }
                                    }
                                }
                                h3 { "{t_routing_title}" }
                                p { "{t_routing_desc}" }
                            }

                            // 实时计费卡片
                            div {
                                class: "kc-home-feature-card",
                                div {
                                    class: "kc-home-feature-icon",
                                    svg {
                                        width: "32",
                                        height: "32",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        line { x1: "12", y1: "1", x2: "12", y2: "23" }
                                        path { d: "M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" }
                                    }
                                }
                                h3 { "{t_billing_title}" }
                                p { "{t_billing_desc}" }
                            }

                            // 分布式集群卡片
                            div {
                                class: "kc-home-feature-card",
                                div {
                                    class: "kc-home-feature-icon",
                                    svg {
                                        width: "32",
                                        height: "32",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        stroke_linecap: "round",
                                        stroke_linejoin: "round",
                                        rect { x: "2", y: "3", width: "20", height: "5", rx: "1", ry: "1" }
                                        rect { x: "2", y: "10", width: "20", height: "5", rx: "1", ry: "1" }
                                        rect { x: "2", y: "17", width: "20", height: "5", rx: "1", ry: "1" }
                                        line { x1: "6", y1: "5.5", x2: "6.01", y2: "5.5" }
                                        line { x1: "6", y1: "12.5", x2: "6.01", y2: "12.5" }
                                        line { x1: "6", y1: "19.5", x2: "6.01", y2: "19.5" }
                                    }
                                }
                                h3 { "{t_cluster_title}" }
                                p { "{t_cluster_desc}" }
                            }

                            // 节点租赁卡片
                            div {
                                class: "kc-home-feature-card",
                                div {
                                    class: "kc-home-feature-icon",
                                    svg {
                                        width: "32",
                                        height: "32",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        rect { x: "2", y: "4", width: "20", height: "12", rx: "2", ry: "2" }
                                        line { x1: "6", y1: "20", x2: "18", y2: "20" }
                                        line { x1: "12", y1: "16", x2: "12", y2: "20" }
                                        line { x1: "6", y1: "10", x2: "10", y2: "10" }
                                    }
                                }
                                h3 { "{t_node_rental_title}" }
                                p { "{t_node_rental_desc}" }
                            }

                            // 传播裂变卡片（二级分销）
                            div {
                                class: "kc-home-feature-card",
                                div {
                                    class: "kc-home-feature-icon",
                                    svg {
                                        width: "32",
                                        height: "32",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        circle { cx: "18", cy: "5", r: "3" }
                                        circle { cx: "6", cy: "12", r: "3" }
                                        circle { cx: "18", cy: "19", r: "3" }
                                        line { x1: "8.59", y1: "13.51", x2: "15.42", y2: "17.49" }
                                        line { x1: "15.41", y1: "6.51", x2: "8.59", y2: "10.49" }
                                    }
                                }
                                h3 { "{t_distribution_title}" }
                                p { "{t_distribution_desc}" }
                            }

                            // 定制服务卡片
                            div {
                                class: "kc-home-feature-card",
                                div {
                                    class: "kc-home-feature-icon",
                                    svg {
                                        width: "32",
                                        height: "32",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        circle { cx: "12", cy: "12", r: "3" }
                                        path { d: "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" }
                                    }
                                }
                                h3 { "{t_custom_title}" }
                                p { "{t_custom_desc}" }
                            }
                        }
                    }
                }
            }

            // 赞赏码区域
            section {
                class: "kc-home-tip-section",
                div {
                    class: "container",
                    h2 { class: "kc-home-tip-title",
                        span { "☕ " }
                        if is_zh { "赞赏支持" } else { "Support Us" }
                    }
                    p { class: "kc-home-tip-subtitle",
                        if is_zh {
                            "如果您喜欢 KeyCompute，欢迎请我们喝杯咖啡 ☕"
                        } else {
                            "If you like KeyCompute, feel free to buy us a coffee ☕"
                        }
                    }
                    div {
                        class: "kc-home-tip-cards",
                        div {
                            class: "kc-home-tip-card",
                            img {
                                class: "kc-home-tip-qr",
                                src: asset!("/assets/wechat_tip.jpg"),
                                alt: if is_zh { "微信赞赏码" } else { "WeChat Tip QR" },
                            }
                            span { class: "kc-home-tip-label",
                                if is_zh { "微信赞赏" } else { "WeChat Pay" }
                            }
                        }
                        div {
                            class: "kc-home-tip-card",
                            img {
                                class: "kc-home-tip-qr",
                                src: asset!("/assets/alipay_tip.png"),
                                alt: if is_zh { "支付宝赞赏码" } else { "Alipay Tip QR" },
                            }
                            span { class: "kc-home-tip-label",
                                if is_zh { "支付宝赞赏" } else { "Alipay" }
                            }
                        }
                    }
                }
            }

            // Footer
            footer { class: "footer",
                span { class: "footer-text",
                    "© 2026 "
                    a {
                        class: "footer-link",
                        href: "https://github.com/aiqubits/keycompute",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        "KeyCompute"
                    }
                    ". All Rights Reserved."
                }
            }

            // 登录弹窗
            LoginModal {
                open: show_login_modal,
                onclose: move |_| show_login_modal.set(false),
                on_switch_to_register: move |_| {
                    show_login_modal.set(false);
                    show_register_modal.set(true);
                },
            }

            // 注册弹窗
            RegisterModal {
                open: show_register_modal,
                onclose: move |_| show_register_modal.set(false),
                on_switch_to_login: move |_| {
                    show_register_modal.set(false);
                    show_login_modal.set(true);
                },
            }
        }
    }
}

/// 登录弹窗组件
#[component]
fn LoginModal(
    open: ReadSignal<bool>,
    onclose: EventHandler<()>,
    on_switch_to_register: EventHandler<()>,
) -> Element {
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

    // 提取 i18n 文本
    let t_title = i18n.t("login.title");
    let t_subtitle = i18n.t("login.subtitle");
    let t_email = i18n.t("auth.email");
    let t_email_placeholder = i18n.t("auth.email_placeholder");
    let t_password = i18n.t("auth.password");
    let t_password_placeholder = i18n.t("auth.password_placeholder");
    let t_remember_me = i18n.t("auth.remember_me");
    let t_forgot_password = i18n.t("auth.forgot_password");
    let t_fill_all = i18n.t("auth.fill_all");
    let t_login_failed = i18n.t("auth.login_failed");
    let t_verifying = i18n.t("login.verifying");
    let t_submit = i18n.t("login.submit");
    let t_no_account = i18n.t("auth.no_account");
    let t_register_now = i18n.t("auth.register_now");

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
                    get_client().set_token(&resp.access_token);
                    auth_store.login_with_persist(resp.access_token.clone(), remember_me());
                    *user_store.info.write() = Some(UserInfo {
                        id: resp.user_id.clone(),
                        email: resp.email.clone(),
                        name: None,
                        role: resp.role.clone(),
                        tenant_id: resp.tenant_id.clone(),
                    });
                    onclose.call(());
                    nav.replace(Route::Dashboard {});
                }
                Err(e) => {
                    let err_text = user_error_message(&e);
                    error_msg.set(Some(format!("{t_login_failed}：{err_text}")));
                    loading.set(false);
                }
            }
        });
    };

    let password_type = if show_password() { "text" } else { "password" };

    rsx! {
        Modal {
            open,
            title: t_title.to_string(),
            onclose,
            max_width: "420px".to_string(),
            div {
                class: "kc-auth-modal",
                p { class: "kc-auth-modal-subtitle", "{t_subtitle}" }

                if let Some(err) = error_msg() {
                    div { class: "kc-auth-status kc-auth-status-error", "{err}" }
                }

                form {
                    onsubmit: on_submit,
                    div {
                        class: "kc-auth-form-group",
                        label { class: "kc-auth-form-label", "{t_email}" }
                        input {
                            class: "kc-auth-form-input",
                            r#type: "email",
                            placeholder: "{t_email_placeholder}",
                            value: "{email}",
                            oninput: move |e| email.set(e.value()),
                        }
                    }

                    div {
                        class: "kc-auth-form-group",
                        label { class: "kc-auth-form-label", "{t_password}" }
                        div {
                            class: "kc-auth-password-wrapper",
                            input {
                                class: "kc-auth-form-input",
                                r#type: "{password_type}",
                                placeholder: "{t_password_placeholder}",
                                value: "{password}",
                                oninput: move |e| password.set(e.value()),
                            }
                            button {
                                class: "kc-auth-toggle-password",
                                r#type: "button",
                                onclick: move |_| show_password.set(!show_password()),
                                if show_password() {
                                    svg {
                                        width: "18",
                                        height: "18",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        path { d: "M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" }
                                        line { x1: "1", y1: "1", x2: "23", y2: "23" }
                                    }
                                } else {
                                    svg {
                                        width: "18",
                                        height: "18",
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        path { d: "M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" }
                                        circle { cx: "12", cy: "12", r: "3" }
                                    }
                                }
                            }
                        }
                    }

                    div {
                        class: "kc-auth-form-options",
                        label {
                            class: "kc-auth-checkbox-label",
                            input {
                                r#type: "checkbox",
                                checked: remember_me(),
                                onclick: move |_| remember_me.set(!remember_me()),
                            }
                            span { "{t_remember_me}" }
                        }
                        button {
                            class: "kc-auth-link-btn",
                            r#type: "button",
                            onclick: move |_| {
                                onclose.call(());
                                nav.push(Route::ForgotPassword {});
                            },
                            "{t_forgot_password}"
                        }
                    }

                    button {
                        class: "kc-auth-submit-btn",
                        r#type: "submit",
                        disabled: loading(),
                        if loading() {
                            span { class: "kc-auth-spinner" }
                            " {t_verifying}"
                        } else {
                            "{t_submit}"
                        }
                    }
                }

                div {
                    class: "kc-auth-footer",
                    "{t_no_account} "
                    button {
                        class: "kc-auth-link-btn kc-auth-link-btn-primary",
                        r#type: "button",
                        onclick: move |_| on_switch_to_register.call(()),
                        "{t_register_now}"
                    }
                }
            }
        }
    }
}

/// 注册弹窗组件
#[component]
fn RegisterModal(
    open: ReadSignal<bool>,
    onclose: EventHandler<()>,
    on_switch_to_login: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let nav = use_navigator();

    let mut name = use_signal(String::new);
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut confirm_password = use_signal(String::new);
    let mut verification_code = use_signal(String::new);
    let mut loading = use_signal(|| false);
    let mut code_requested = use_signal(|| false);
    let mut completed = use_signal(|| false);
    let mut error_msg = use_signal(|| Option::<String>::None);
    let mut success_msg = use_signal(|| Option::<String>::None);

    // 提取 i18n 文本
    let t_register = i18n.t("auth.register");
    let t_register_subtitle = i18n.t("auth.register_subtitle");
    let t_name = i18n.t("auth.name");
    let t_name_placeholder = i18n.t("auth.name_placeholder");
    let t_email = i18n.t("auth.email");
    let t_email_placeholder = i18n.t("auth.email_placeholder");
    let t_password = i18n.t("auth.password");
    let t_password_min8 = i18n.t("auth.password_min8");
    let t_confirm_password = i18n.t("auth.confirm_password");
    let t_confirm_password_placeholder = i18n.t("auth.confirm_password_placeholder");
    let t_verification_code = i18n.t("auth.verification_code");
    let t_verification_code_placeholder = i18n.t("auth.verification_code_placeholder");
    let t_code_sent_hint = i18n.t("auth.code_sent_hint");
    let t_fill_required = i18n.t("auth.fill_required");
    let t_pwd_mismatch = i18n.t("form.password_mismatch");
    let t_register_failed = i18n.t("auth.register_failed");
    let t_request_code_failed = i18n.t("auth.request_code_failed");
    let t_code_required = i18n.t("auth.code_required");
    let t_registering = i18n.t("auth.registering");
    let t_requesting_code = i18n.t("auth.requesting_code");
    let t_complete_registration = i18n.t("auth.complete_registration");
    let t_request_code = i18n.t("auth.request_code");
    let t_registration_success = i18n.t("auth.registration_success");
    let t_login_now = i18n.t("auth.login_now");
    let t_has_account = i18n.t("auth.has_account");

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();

        if completed() {
            return;
        }

        if !code_requested() {
            if name().is_empty() || email().is_empty() || password().is_empty() {
                error_msg.set(Some(t_fill_required.to_string()));
                return;
            }

            if password() != confirm_password() {
                error_msg.set(Some(t_pwd_mismatch.to_string()));
                return;
            }

            loading.set(true);
            error_msg.set(None);
            success_msg.set(None);

            let email_val = email();
            spawn(async move {
                match auth_service::request_registration_code(&email_val, None).await {
                    Ok(resp) => {
                        email.set(resp.email.clone());
                        code_requested.set(true);
                        success_msg.set(Some(resp.message));
                        loading.set(false);
                    }
                    Err(e) => {
                        error_msg.set(Some(format!(
                            "{t_request_code_failed}：{}",
                            user_error_message(&e)
                        )));
                        loading.set(false);
                    }
                }
            });
            return;
        }

        if verification_code().is_empty() {
            error_msg.set(Some(t_code_required.to_string()));
            return;
        }

        loading.set(true);
        error_msg.set(None);

        let name_val = name();
        let email_val = email();
        let password_val = password();
        let code_val = verification_code();

        spawn(async move {
            match auth_service::complete_registration(
                &email_val,
                &code_val,
                &password_val,
                Some(name_val.as_str()),
            )
            .await
            {
                Ok(_resp) => {
                    completed.set(true);
                    code_requested.set(false);
                    success_msg.set(Some(t_registration_success.to_string()));
                    verification_code.set(String::new());
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(format!(
                        "{t_register_failed}：{}",
                        user_error_message(&e)
                    )));
                    loading.set(false);
                }
            }
        });
    };

    rsx! {
        Modal {
            open,
            title: t_register.to_string(),
            onclose,
            max_width: "420px".to_string(),
            div {
                class: "kc-auth-modal",
                p { class: "kc-auth-modal-subtitle", "{t_register_subtitle}" }

                if let Some(msg) = success_msg() {
                    div { class: "kc-auth-status kc-auth-status-success", "{msg}" }
                }

                if let Some(err) = error_msg() {
                    div { class: "kc-auth-status kc-auth-status-error", "{err}" }
                }

                if completed() {
                    div {
                        class: "kc-auth-success-block",
                        button {
                            class: "kc-auth-submit-btn",
                            r#type: "button",
                            onclick: move |_| {
                                onclose.call(());
                                nav.push(Route::Login {});
                            },
                            "{t_login_now}"
                        }
                    }
                } else {
                    form {
                        onsubmit: on_submit,
                        div {
                            class: "kc-auth-form-group",
                            label { class: "kc-auth-form-label", "{t_name}" }
                            input {
                                class: "kc-auth-form-input",
                                r#type: "text",
                                placeholder: "{t_name_placeholder}",
                                value: "{name}",
                                oninput: move |e| name.set(e.value()),
                            }
                        }

                        div {
                            class: "kc-auth-form-group",
                            label { class: "kc-auth-form-label", "{t_email}" }
                            input {
                                class: "kc-auth-form-input",
                                r#type: "email",
                                placeholder: "{t_email_placeholder}",
                                value: "{email}",
                                disabled: code_requested(),
                                oninput: move |e| email.set(e.value()),
                            }
                        }

                        div {
                            class: "kc-auth-form-group",
                            label { class: "kc-auth-form-label", "{t_password}" }
                            input {
                                class: "kc-auth-form-input",
                                r#type: "password",
                                placeholder: "{t_password_min8}",
                                value: "{password}",
                                oninput: move |e| password.set(e.value()),
                            }
                        }

                        div {
                            class: "kc-auth-form-group",
                            label { class: "kc-auth-form-label", "{t_confirm_password}" }
                            input {
                                class: "kc-auth-form-input",
                                r#type: "password",
                                placeholder: "{t_confirm_password_placeholder}",
                                value: "{confirm_password}",
                                oninput: move |e| confirm_password.set(e.value()),
                            }
                        }

                        if code_requested() {
                            div {
                                class: "kc-auth-form-group",
                                label { class: "kc-auth-form-label", "{t_verification_code}" }
                                input {
                                    class: "kc-auth-form-input",
                                    r#type: "text",
                                    maxlength: "6",
                                    placeholder: "{t_verification_code_placeholder}",
                                    value: "{verification_code}",
                                    oninput: move |e| verification_code.set(e.value()),
                                }
                            }
                            p {
                                class: "kc-auth-hint",
                                "{t_code_sent_hint}"
                            }
                        }

                        button {
                            class: "kc-auth-submit-btn",
                            r#type: "submit",
                            disabled: loading(),
                            if loading() {
                                if code_requested() {
                                    "{t_registering}"
                                } else {
                                    "{t_requesting_code}"
                                }
                            } else if code_requested() {
                                "{t_complete_registration}"
                            } else {
                                "{t_request_code}"
                            }
                        }
                    }
                }

                div {
                    class: "kc-auth-footer",
                    "{t_has_account} "
                    button {
                        class: "kc-auth-link-btn kc-auth-link-btn-primary",
                        r#type: "button",
                        onclick: move |_| on_switch_to_login.call(()),
                        "{t_login_now}"
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn apply_theme_with_animation(theme: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };

    // 设置 <html data-theme="...">
    if let Some(root) = document.document_element() {
        let _ = root.set_attribute("data-theme", theme);
    }

    // 添加切换动画类到 body，500ms 后移除
    if let Some(body) = document.body() {
        let class_list = body.class_list();
        let _ = class_list.add_1("kc-theme-switching");
        let body_clone = body.clone();
        let timeout = gloo_timers::callback::Timeout::new(500, move || {
            let _ = body_clone.class_list().remove_1("kc-theme-switching");
        });
        timeout.forget();
    }

    // 保存到 localStorage
    if let Ok(Some(storage)) = window.local_storage() {
        let _ = storage.set_item("theme", theme);
    }
}

#[cfg(target_arch = "wasm32")]
fn read_lang_from_storage() -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item("lang")
        .ok()?
}

#[cfg(target_arch = "wasm32")]
fn save_lang_to_storage(lang: &str) {
    if let Some(window) = web_sys::window() {
        if let Ok(Some(storage)) = window.local_storage() {
            let _ = storage.set_item("lang", lang);
        }
    }
}
