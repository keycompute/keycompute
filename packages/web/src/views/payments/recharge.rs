use dioxus::prelude::*;
use gloo_timers::future::sleep;
use std::time::Duration;

use client_api::api::payment::CreatePaymentOrderRequest;

use crate::router::Route;
use crate::services::payment_service;
use crate::stores::auth_store::AuthStore;
use crate::stores::ui_store::UiStore;

/// 支付方式枚举
#[derive(Clone, PartialEq)]
enum PayMethod {
    Alipay,
    WechatPay,
}

impl PayMethod {
    fn label(&self) -> &'static str {
        match self {
            PayMethod::Alipay => "支付宝",
            PayMethod::WechatPay => "微信支付",
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            PayMethod::Alipay => "💳",
            PayMethod::WechatPay => "📱",
        }
    }
}

/// 订单状态
#[derive(Clone, PartialEq)]
enum OrderState {
    /// 尚未创建订单
    Idle,
    /// 创建成功，等待支付（含 pay_url）
    Pending {
        out_trade_no: String,
        pay_url: Option<String>,
        payment_type: String,
        qr_code: Option<String>,
        qr_code_image_url: Option<String>,
    },
    /// 支付完成
    Paid { out_trade_no: String },
    /// 支付失败
    Failed { reason: String },
}

#[component]
pub fn Recharge() -> Element {
    let auth_store = use_context::<AuthStore>();
    let mut ui_store = use_context::<UiStore>();
    let nav = use_navigator();

    let mut amount = use_signal(String::new);
    let mut pay_method = use_signal(|| PayMethod::Alipay);
    let mut loading = use_signal(|| false);
    let mut order_state = use_signal(|| OrderState::Idle);
    // 订单手动轮询计数器，变化时触发 use_resource 重执行
    let mut poll_tick = use_signal(|| 0u32);
    // 轮询世代计数器：每次启动新轮询循环时递增，实现防竞态
    // 当 loop 中读到的 gen 与当前不一致时，说明旧 loop 应退出
    let mut poll_gen = use_signal(|| 0u32);
    // 自动轮询是否激活（进入 Pending 后开始，离开后停止）
    let mut auto_poll_active = use_signal(|| false);

    // 手动触发的订单状态查询
    let _poll = use_resource(move || async move {
        let tick = poll_tick();
        if tick == 0 {
            return;
        }
        let no = match order_state() {
            OrderState::Pending {
                ref out_trade_no, ..
            } => out_trade_no.clone(),
            _ => return,
        };
        let token = auth_store.token().unwrap_or_default();
        if let Ok(order) = payment_service::sync_order(&no, &token).await {
            match order.status.as_str() {
                "paid" | "success" => {
                    order_state.set(OrderState::Paid {
                        out_trade_no: no.clone(),
                    });
                    ui_store.show_success("支付成功！余额将尽快入账");
                }
                "failed" | "cancelled" => {
                    order_state.set(OrderState::Failed {
                        reason: format!("订单状态：{}", order.status),
                    });
                }
                _ => {} // 仍在处理中
            }
        }
    });

    // 自动轮询：进入 Pending 状态后每 5 秒自动检查一次
    // 防竞态：每次开启时捕证当前 gen，循环中检测 gen 变化即退出旧 loop
    use_effect(move || {
        if auto_poll_active() {
            // 单调递增，捕证本次开启对应的 generation
            let my_gen = poll_gen();
            spawn(async move {
                loop {
                    sleep(Duration::from_secs(5)).await;
                    // gen 发生变化（新轮询已开启），旧 loop 直接退出
                    if poll_gen() != my_gen {
                        break;
                    }
                    // 若状态已不是 Pending，停止轮询
                    match order_state() {
                        OrderState::Pending {
                            ref out_trade_no, ..
                        } => {
                            let no = out_trade_no.clone();
                            let token = auth_store.token().unwrap_or_default();
                            if let Ok(order) = payment_service::sync_order(&no, &token).await {
                                match order.status.as_str() {
                                    "paid" | "success" => {
                                        order_state.set(OrderState::Paid { out_trade_no: no });
                                        ui_store.show_success("支付成功！余额已入账");
                                        auto_poll_active.set(false);
                                        break;
                                    }
                                    "failed" | "cancelled" => {
                                        order_state.set(OrderState::Failed {
                                            reason: format!("订单已失效：{}", order.status),
                                        });
                                        auto_poll_active.set(false);
                                        break;
                                    }
                                    _ => {} // 继续轮询
                                }
                            }
                        }
                        _ => break, // 状态已变更，停止
                    }
                }
            });
        }
    });

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        let amount_str = amount();
        if amount_str.is_empty() {
            ui_store.show_error("请输入充値金额");
            return;
        }
        let amount_val: f64 = match amount_str.parse() {
            Ok(v) if v > 0.0 => v,
            _ => {
                ui_store.show_error("请输入有效金额（大于 0）");
                return;
            }
        };
        let payment_type = match pay_method() {
            PayMethod::Alipay => "page",
            PayMethod::WechatPay => "qr",
        };
        loading.set(true);
        order_state.set(OrderState::Idle);
        spawn(async move {
            let token = auth_store.token().unwrap_or_default();
            let req = CreatePaymentOrderRequest::new(amount_val, "账户充值", payment_type)
                .with_body(format!("KeyCompute 账户充值 {} 元", amount_val));
            match payment_service::create_order(req, &token).await {
                Ok(order) => {
                    loading.set(false);
                    order_state.set(OrderState::Pending {
                        out_trade_no: order.out_trade_no.clone(),
                        pay_url: order.pay_url.clone(),
                        payment_type: order.payment_type.clone(),
                        qr_code: order.qr_code.clone(),
                        qr_code_image_url: order.qr_code_image_url.clone(),
                    });
                    amount.set(String::new());
                    // 递增 gen，使旧轮询 loop 自动退出，再将 active 设为 true 开启新轮询
                    *poll_gen.write() += 1;
                    auto_poll_active.set(true);
                }
                Err(e) => {
                    loading.set(false);
                    order_state.set(OrderState::Failed {
                        reason: format!("创建订单失败：{e}"),
                    });
                }
            }
        });
    };

    rsx! {
        div {
            class: "page-container",
            div {
                class: "page-header",
                button {
                    class: "btn btn-ghost btn-sm",
                    r#type: "button",
                    onclick: move |_| { nav.push(Route::PaymentsOverview {}); },
                    "← 返回"
                }
                h1 { class: "page-title", "账户充値" }
            }

            // 充値表单区
            match order_state() {
                OrderState::Idle | OrderState::Failed { .. } => rsx! {
                    div { class: "card",
                        div { class: "card-header",
                            h3 { class: "card-title", "选择充値方式" }
                        }
                        div { class: "card-body",
                            // 失败提示
                            if let OrderState::Failed { ref reason } = order_state() {
                                div { class: "alert alert-error",
                                    span { class: "alert-icon", "✕" }
                                    div { class: "alert-content",
                                        p { class: "alert-body", "{reason}" }
                                    }
                                }
                            }

                            // 支付方式选择
                            div { class: "form-group",
                                label { class: "form-label", "支付方式" }
                                div { class: "pay-method-grid",
                                    for method in [PayMethod::Alipay, PayMethod::WechatPay] {
                                        {
                                            let is_active = pay_method() == method;
                                            let m = method.clone();
                                            rsx! {
                                                button {
                                                    class: if is_active { "pay-method-card active" } else { "pay-method-card" },
                                                    r#type: "button",
                                                    onclick: move |_| pay_method.set(m.clone()),
                                                    span { class: "pay-method-icon", "{method.icon()}" }
                                                    span { class: "pay-method-label", "{method.label()}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // 就常金额选择
                            div { class: "form-group",
                                label { class: "form-label", "充値金额（元）" }
                                div { class: "amount-presets",
                                    for preset in ["10", "30", "50", "100", "200", "500"] {
                                        button {
                                            class: if amount() == preset { "btn btn-primary btn-sm" } else { "btn btn-outline btn-sm" },
                                            r#type: "button",
                                            onclick: move |_| amount.set(preset.to_string()),
                                            "¥{preset}"
                                        }
                                    }
                                }
                                input {
                                    class: "form-input",
                                    style: "margin-top: 8px",
                                    r#type: "number",
                                    min: "1",
                                    step: "1",
                                    placeholder: "或输入自定义金额",
                                    value: "{amount}",
                                    oninput: move |e| amount.set(e.value()),
                                }
                            }

                            // 提交按钮
                            form {
                                onsubmit: on_submit,
                                button {
                                    class: "btn btn-primary btn-full",
                                    r#type: "submit",
                                    disabled: loading(),
                                    if loading() { "订单创建中…" } else {
                                        {
                                            let amt_label = if amount().is_empty() {
                                                String::new()
                                            } else {
                                                format!(" CNY {}", amount())
                                            };
                                            format!("{} 确认充值{}", pay_method().icon(), amt_label)
                                        }
                                    }
                                }
                            }

                            // 说明
                            div { class: "alert alert-info", style: "margin-top: 16px",
                                span { class: "alert-icon", "ℹ" }
                                div { class: "alert-content",
                                    p { class: "alert-body",
                                        "充値完成后余额通常几秒内到账。若长时间未到账请联系客服。"
                                    }
                                }
                            }
                        }
                    }
                },

                // 订单已创建，等待支付
                OrderState::Pending { ref out_trade_no, ref pay_url, ref payment_type, ref qr_code, ref qr_code_image_url } => rsx! {
                    div { class: "card",
                        div { class: "card-header",
                            h3 { class: "card-title", "请完成支付" }
                        }
                        div { class: "card-body",
                            div { class: "alert alert-warning",
                                span { class: "alert-icon", "⏳" }
                                div { class: "alert-content",
                                    p { class: "alert-title", "订单已创建，等待支付" }
                                    p { class: "alert-body", "订单号：" code { "{out_trade_no}" } }
                                }
                            }

                            // 如果有支付跳转链接
                            if let Some(url) = pay_url {
                                div { class: "pay-qr-area",
                                    p { class: "pay-qr-tip",
                                        if payment_type == "page" { "💳 请打开支付宝支付页面完成付款" }
                                        else if payment_type == "wap" { "📱 请在手机端打开支付链接完成付款" }
                                        else { "请点击下方按钮完成支付" }
                                    }
                                    a {
                                        href: "{url}",
                                        target: "_blank",
                                        rel: "noopener noreferrer",
                                        class: "btn btn-primary btn-full",
                                        style: "text-decoration:none;display:block;text-align:center",
                                        "打开支付页面"
                                    }
                                    p {
                                        style: "font-size:12px;color:var(--text-secondary);margin-top:8px;text-align:center",
                                        "支付完成后点击【已完成支付】按钮刷新状态"
                                    }
                                }
                            }

                            if let Some(image_url) = qr_code_image_url {
                                div { class: "pay-qr-area",
                                    p { class: "pay-qr-tip", "请使用支付宝或其他扫码工具完成支付" }
                                    img {
                                        src: "{image_url}",
                                        alt: "支付二维码",
                                        style: "width:220px;height:220px;display:block;margin:0 auto;border-radius:16px;border:1px solid var(--border-color);background:white;padding:12px"
                                    }
                                    if let Some(code) = qr_code {
                                        p {
                                            style: "font-size:12px;color:var(--text-secondary);margin-top:8px;text-align:center;word-break:break-all",
                                            "二维码内容：{code}"
                                        }
                                    }
                                }
                            }

                            // 轮询按钮
                            div { class: "pay-actions",
                                button {
                                    class: "btn btn-primary",
                                    r#type: "button",
                                    onclick: move |_| *poll_tick.write() += 1,
                                    "已完成支付，点此确认"
                                }
                                button {
                                    class: "btn btn-ghost",
                                    r#type: "button",
                                    onclick: move |_| {
                                        auto_poll_active.set(false);
                                        order_state.set(OrderState::Idle);
                                    },
                                    "取消订单"
                                }
                            }
                        }
                    }
                },

                // 支付完成
                OrderState::Paid { ref out_trade_no } => rsx! {
                    div { class: "card",
                        div { class: "card-body",
                            div { class: "pay-success",
                                div { class: "pay-success-icon", "✅" }
                                h3 { class: "pay-success-title", "充値成功！" }
                                p { class: "pay-success-no", "订单号：" code { "{out_trade_no}" } }
                                p { style: "color:var(--text-secondary);margin-bottom:24px",
                                    "余额已入账，可立即使用 API"
                                }
                                div { class: "pay-success-actions",
                                    button {
                                        class: "btn btn-primary",
                                        r#type: "button",
                                        onclick: move |_| { nav.push(Route::PaymentsOverview {}); },
                                        "查看余额"
                                    }
                                    button {
                                        class: "btn btn-ghost",
                                        r#type: "button",
                                        onclick: move |_| {
                                            order_state.set(OrderState::Idle);
                                            amount.set(String::new());
                                        },
                                        "继续充値"
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}
