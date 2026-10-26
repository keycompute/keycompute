use std::collections::HashMap;

use dioxus::prelude::*;
use ui::PageHeader;

use crate::hooks::use_i18n::use_i18n;
use crate::services::{
    api_client::{user_error_message, with_auto_refresh},
    settings_service,
};
use crate::stores::{
    auth_store::AuthStore, public_settings_store::PublicSettingsStore, user_store::UserStore,
};

#[component]
pub fn Settings() -> Element {
    let i18n = use_i18n();
    let user_store = use_context::<UserStore>();
    let auth_store = use_context::<AuthStore>();
    let session_id = (auth_store.state)().session_id;
    let current_user = user_store.info.read().clone();
    let can_manage_platform = current_user
        .as_ref()
        .is_some_and(|user| user.can_manage_platform());
    let can_manage_system_settings = current_user
        .as_ref()
        .is_some_and(|user| user.has_platform_permission("system_settings:manage"));

    let settings = use_resource(move || async move {
        with_auto_refresh(auth_store, |token| async move {
            settings_service::get_all(&token).await
        })
        .await
    });
    let page_description = if can_manage_platform {
        i18n.t("settings.admin_desc")
    } else {
        i18n.t("settings.user_desc")
    };

    rsx! {
        div { class: "page-container settings-console-page",
            PageHeader {
                title: i18n.t("page.settings").to_string(),
                description: page_description.to_string(),
            }
            if !can_manage_platform {
                div { class: "alert alert-info", {i18n.t("settings.admin_only_hint")} }
            }
            match settings() {
                None => rsx! {
                    div { class: "content-loading", role: "status",
                        span { class: "spinner", aria_hidden: "true" }
                        span { {i18n.t("common.loading")} }
                    }
                },
                Some(Err(ref error)) => rsx! {
                    div { class: "alert alert-error", role: "alert",
                        {format!("{}：{}", i18n.t("settings.load_failed"), user_error_message(i18n, error))}
                    }
                },
                Some(Ok(ref values)) => {
                    let value = |key: &str| values.get(key).map(|value| value.to_string_value()).unwrap_or_default();
                    let site_name = value("site_name");
                    let currency = value("default_currency");
                    let min_recharge = value("min_recharge_amount");
                    let max_recharge = value("max_recharge_amount");
                    let alipay_enabled = setting_bool(&value("alipay_enabled"));
                    let wechatpay_enabled = setting_bool(&value("wechatpay_enabled"));
                    let jwt_expire = value("jwt_expire_hours");
                    let distribution_enabled = setting_bool(&value("distribution_enabled"));
                    // Local editor signals intentionally survive ordinary re-renders, but
                    // must be recreated when the authoritative settings snapshot or login
                    // identity changes. Otherwise a later save can overwrite newer values.
                    let editor_key = format!(
                        "{:?}",
                        (
                            session_id,
                            &site_name,
                            &currency,
                            &min_recharge,
                            &max_recharge,
                            alipay_enabled,
                            wechatpay_enabled,
                            &jwt_expire,
                            distribution_enabled,
                        )
                    );
                    rsx! {
                        SettingsEditor {
                            key: "{editor_key}",
                            site_name,
                            currency,
                            min_recharge,
                            max_recharge,
                            alipay_enabled,
                            wechatpay_enabled,
                            jwt_expire,
                            distribution_enabled,
                            editable: can_manage_platform,
                            distribution_editable: can_manage_system_settings,
                        }
                    }
                },
            }
        }
    }
}

fn setting_bool(value: &str) -> bool {
    matches!(value, "true" | "1" | "yes")
}

async fn save_values(
    auth: AuthStore,
    values: impl IntoIterator<Item = (&'static str, String)>,
) -> client_api::Result<()> {
    let payload = values
        .into_iter()
        .map(|(key, value)| (key.to_string(), serde_json::Value::String(value)))
        .collect::<HashMap<_, _>>();
    let token = auth.token().unwrap_or_default();
    settings_service::update_all(payload, &token).await?;
    Ok(())
}

#[component]
fn SettingsEditor(
    site_name: String,
    currency: String,
    min_recharge: String,
    max_recharge: String,
    alipay_enabled: bool,
    wechatpay_enabled: bool,
    jwt_expire: String,
    distribution_enabled: bool,
    editable: bool,
    distribution_editable: bool,
) -> Element {
    let i18n = use_i18n();
    let auth = use_context::<AuthStore>();
    let mut public_settings = use_context::<PublicSettingsStore>();

    let site = use_signal(|| site_name.clone());
    let mut currency_value = use_signal(|| currency.clone());
    let minimum = use_signal(|| min_recharge.clone());
    let maximum = use_signal(|| max_recharge.clone());
    let alipay = use_signal(|| alipay_enabled);
    let wechat = use_signal(|| wechatpay_enabled);
    let jwt = use_signal(|| jwt_expire.clone());
    let distribution = use_signal(|| distribution_enabled);

    let mut basic_baseline = use_signal(|| (site_name, currency, min_recharge, max_recharge));
    let mut payment_baseline = use_signal(|| (alipay_enabled, wechatpay_enabled));
    let mut security_baseline = use_signal(|| jwt_expire);
    let mut distribution_baseline = use_signal(|| distribution_enabled);
    let mut saving_section = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut saved_section = use_signal(String::new);

    let basic_dirty = move || {
        let baseline = basic_baseline();
        (site(), currency_value(), minimum(), maximum()) != baseline
    };
    let payment_dirty = move || (alipay(), wechat()) != payment_baseline();
    let security_dirty = move || jwt() != security_baseline();
    let distribution_dirty = move || distribution() != distribution_baseline();

    crate::views::tenant::workspace_switcher::use_workspace_dirty_blocker(move || {
        basic_dirty() || payment_dirty() || security_dirty() || distribution_dirty()
    });

    let save_basic = move |_| {
        if !saving_section().is_empty() {
            return;
        }
        let site_value = site().trim().to_string();
        let currency = currency_value();
        let min_text = minimum().trim().to_string();
        let max_text = maximum().trim().to_string();
        let numbers = min_text
            .parse::<f64>()
            .ok()
            .zip(max_text.parse::<f64>().ok())
            .filter(|(min, max)| min.is_finite() && max.is_finite() && *min >= 0.0 && min <= max);
        if site_value.is_empty() || site_value.chars().count() > 100 {
            error.set(i18n.t("settings.site_name_invalid").to_string());
            return;
        }
        if !matches!(currency.as_str(), "CNY" | "USD") {
            error.set(i18n.t("settings.currency_invalid").to_string());
            return;
        }
        if numbers.is_none() {
            error.set(i18n.t("settings.recharge_range_invalid").to_string());
            return;
        }
        saving_section.set("basic".to_string());
        error.set(String::new());
        saved_section.set(String::new());
        spawn(async move {
            let result = save_values(
                auth,
                [
                    ("site_name", site_value.clone()),
                    ("default_currency", currency.clone()),
                    ("min_recharge_amount", min_text.clone()),
                    ("max_recharge_amount", max_text.clone()),
                ],
            )
            .await;
            saving_section.set(String::new());
            match result {
                Ok(()) => {
                    basic_baseline.set((site_value, currency, min_text, max_text));
                    saved_section.set("basic".to_string());
                }
                Err(err) => error.set(format!(
                    "{}：{}",
                    i18n.t("settings.save_failed"),
                    user_error_message(i18n, &err)
                )),
            }
        });
    };

    let save_payment = move |_| {
        if !saving_section().is_empty() {
            return;
        }
        let alipay_value = alipay();
        let wechat_value = wechat();
        saving_section.set("payment".to_string());
        error.set(String::new());
        saved_section.set(String::new());
        spawn(async move {
            let result = save_values(
                auth,
                [
                    ("alipay_enabled", alipay_value.to_string()),
                    ("wechatpay_enabled", wechat_value.to_string()),
                ],
            )
            .await;
            saving_section.set(String::new());
            match result {
                Ok(()) => {
                    payment_baseline.set((alipay_value, wechat_value));
                    saved_section.set("payment".to_string());
                }
                Err(err) => error.set(format!(
                    "{}：{}",
                    i18n.t("settings.save_failed"),
                    user_error_message(i18n, &err)
                )),
            }
        });
    };

    let save_security = move |_| {
        if !saving_section().is_empty() {
            return;
        }
        let value = jwt().trim().to_string();
        if value
            .parse::<i64>()
            .ok()
            .is_none_or(|hours| !(1..=8760).contains(&hours))
        {
            error.set(i18n.t("settings.jwt_range_invalid").to_string());
            return;
        }
        saving_section.set("security".to_string());
        error.set(String::new());
        saved_section.set(String::new());
        spawn(async move {
            let result = save_values(auth, [("jwt_expire_hours", value.clone())]).await;
            saving_section.set(String::new());
            match result {
                Ok(()) => {
                    security_baseline.set(value);
                    saved_section.set("security".to_string());
                }
                Err(err) => error.set(format!(
                    "{}：{}",
                    i18n.t("settings.save_failed"),
                    user_error_message(i18n, &err)
                )),
            }
        });
    };

    let save_distribution = move |_| {
        if !saving_section().is_empty() {
            return;
        }
        let value = distribution();
        saving_section.set("distribution".to_string());
        error.set(String::new());
        saved_section.set(String::new());
        spawn(async move {
            let result = save_values(auth, [("distribution_enabled", value.to_string())]).await;
            saving_section.set(String::new());
            match result {
                Ok(()) => {
                    distribution_baseline.set(value);
                    public_settings.set_distribution_enabled(value);
                    saved_section.set("distribution".to_string());
                }
                Err(err) => error.set(format!(
                    "{}：{}",
                    i18n.t("settings.save_failed"),
                    user_error_message(i18n, &err)
                )),
            }
        });
    };

    rsx! {
        if !error().is_empty() { div { class: "alert alert-error", role: "alert", "{error}" } }
        div { class: "settings-console-stack",
            SettingsSection { title: i18n.t("settings.basic_title"), description: i18n.t("settings.basic_desc"), dirty: basic_dirty(), saved: saved_section() == "basic" && !basic_dirty(), saving: saving_section() == "basic", busy: !saving_section().is_empty(), editable, on_save: save_basic,
                SettingTextRow { label: i18n.t("settings.site_name_label"), description: i18n.t("settings.site_name_desc"), value: site, editable }
                div { class: "setting-row", div { class: "setting-row-main", div { class: "setting-row-meta", span { class: "setting-label", {i18n.t("settings.default_currency_label")} } p { class: "setting-description", {i18n.t("settings.default_currency_desc")} } }
                    if editable { select { class: "input-field setting-control", value: "{currency_value}", onchange: move |event| currency_value.set(event.value()), option { value: "CNY", {i18n.t("pricing.currency_cny")} } option { value: "USD", {i18n.t("pricing.currency_usd")} } } }
                    else { span { class: "setting-value setting-readonly", "{currency_value}" } }
                } }
                SettingNumberRow { label: i18n.t("settings.min_recharge_label"), description: i18n.t("settings.min_recharge_desc"), value: minimum, editable }
                SettingNumberRow { label: i18n.t("settings.max_recharge_label"), description: i18n.t("settings.max_recharge_desc"), value: maximum, editable }
            }
            SettingsSection { title: i18n.t("settings.payment_title"), description: i18n.t("settings.payment_desc"), dirty: payment_dirty(), saved: saved_section() == "payment" && !payment_dirty(), saving: saving_section() == "payment", busy: !saving_section().is_empty(), editable, on_save: save_payment,
                SettingToggleRow { label: i18n.t("recharge.alipay"), description: i18n.t("settings.alipay_enabled_desc"), value: alipay, editable }
                SettingToggleRow { label: i18n.t("recharge.wechat_pay"), description: i18n.t("settings.wechatpay_enabled_desc"), value: wechat, editable }
            }
            SettingsSection { title: i18n.t("settings.security_title"), description: i18n.t("settings.security_desc"), dirty: security_dirty(), saved: saved_section() == "security" && !security_dirty(), saving: saving_section() == "security", busy: !saving_section().is_empty(), editable, on_save: save_security,
                SettingNumberRow { label: i18n.t("settings.jwt_expire_label"), description: i18n.t("settings.jwt_expire_desc"), value: jwt, editable }
            }
            SettingsSection { title: i18n.t("settings.distribution_title"), description: i18n.t("settings.distribution_desc"), dirty: distribution_dirty(), saved: saved_section() == "distribution" && !distribution_dirty(), saving: saving_section() == "distribution", busy: !saving_section().is_empty(), editable: distribution_editable, on_save: save_distribution,
                SettingToggleRow {
                    label: i18n.t("settings.distribution_enabled_label"),
                    description: if distribution_editable { i18n.t("settings.distribution_enabled_desc") } else { i18n.t("settings.distribution_enabled_system_only_desc") },
                    value: distribution,
                    editable: distribution_editable,
                }
            }
        }
    }
}

#[component]
fn SettingsSection(
    title: &'static str,
    description: &'static str,
    dirty: bool,
    saved: bool,
    saving: bool,
    busy: bool,
    editable: bool,
    on_save: EventHandler<MouseEvent>,
    children: Element,
) -> Element {
    let i18n = use_i18n();
    rsx! {
        section { class: "settings-section-card",
            div { class: "settings-section-head",
                div { h2 { class: "settings-section-title", "{title}" } p { class: "settings-section-description", "{description}" } }
                if editable { div { class: "settings-section-actions",
                    if dirty { span { class: "badge badge-warning", {i18n.t("settings.unsaved")} } }
                    if saved { span { class: "text-success", role: "status", {i18n.t("settings.saved")} } }
                    button { class: "btn btn-primary btn-sm", r#type: "button", disabled: !dirty || busy, onclick: move |event| on_save.call(event), if saving { {i18n.t("form.saving")} } else { {i18n.t("settings.save_section")} } }
                } }
            }
            div { class: "settings-section-body", {children} }
        }
    }
}

#[component]
fn SettingTextRow(
    label: &'static str,
    description: &'static str,
    mut value: Signal<String>,
    editable: bool,
) -> Element {
    rsx! { div { class: "setting-row", div { class: "setting-row-main", div { class: "setting-row-meta", span { class: "setting-label", "{label}" } p { class: "setting-description", "{description}" } }
        if editable { input { class: "input-field setting-control", value: "{value}", oninput: move |event| value.set(event.value()) } }
        else { span { class: "setting-value setting-readonly", "{value}" } }
    } } }
}

#[component]
fn SettingNumberRow(
    label: &'static str,
    description: &'static str,
    mut value: Signal<String>,
    editable: bool,
) -> Element {
    rsx! { div { class: "setting-row", div { class: "setting-row-main", div { class: "setting-row-meta", span { class: "setting-label", "{label}" } p { class: "setting-description", "{description}" } }
        if editable { input { class: "input-field setting-control", r#type: "number", min: "0", value: "{value}", oninput: move |event| value.set(event.value()) } }
        else { span { class: "setting-value setting-readonly", "{value}" } }
    } } }
}

#[component]
fn SettingToggleRow(
    label: &'static str,
    description: &'static str,
    mut value: Signal<bool>,
    editable: bool,
) -> Element {
    let i18n = use_i18n();
    rsx! { div { class: "setting-row", div { class: "setting-row-main", div { class: "setting-row-meta", span { class: "setting-label", "{label}" } p { class: "setting-description", "{description}" } }
        if editable { div { class: "setting-toggle-row", label { class: "toggle-switch", input { r#type: "checkbox", checked: value(), onchange: move |event| value.set(event.checked()) } span { class: "toggle-slider" } } span { class: if value() { "setting-toggle-state is-enabled" } else { "setting-toggle-state" }, if value() { {i18n.t("common.enabled")} } else { {i18n.t("common.disabled")} } } } }
        else { span { class: "setting-value setting-readonly", if value() { {i18n.t("common.enabled")} } else { {i18n.t("common.disabled")} } } }
    } } }
}
