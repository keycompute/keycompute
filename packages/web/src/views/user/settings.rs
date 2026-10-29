use dioxus::prelude::*;
use ui::PageHeader;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::{api_client::user_error_message, user_service};
use crate::stores::auth_store::AuthStore;
use crate::stores::ui_store::UiStore;
use crate::utils::password::password_rules as evaluate_password_rules;

#[component]
pub fn UserSettings() -> Element {
    let i18n = use_i18n();
    let mut auth_store = use_context::<AuthStore>();
    let mut ui_store = use_context::<UiStore>();
    let nav = use_navigator();
    let mut current_pwd = use_signal(String::new);
    let mut new_pwd = use_signal(String::new);
    let mut confirm_pwd = use_signal(String::new);
    let mut show_current_pwd = use_signal(|| false);
    let mut show_new_pwd = use_signal(|| false);
    let mut show_confirm_pwd = use_signal(|| false);
    let mut saving = use_signal(|| false);
    let mut success_msg = use_signal(|| Option::<String>::None);
    let mut error_msg = use_signal(|| Option::<String>::None);

    let new_password_value = new_pwd();
    let confirm_password_value = confirm_pwd();
    let password_rules = evaluate_password_rules(&new_password_value);
    let passwords_match =
        !confirm_password_value.is_empty() && new_password_value == confirm_password_value;
    let current_password_toggle_label = if show_current_pwd() {
        i18n.t("login.hide_password")
    } else {
        i18n.t("login.show_password")
    };
    let new_password_toggle_label = if show_new_pwd() {
        i18n.t("login.hide_password")
    } else {
        i18n.t("login.show_password")
    };
    let confirm_password_toggle_label = if show_confirm_pwd() {
        i18n.t("login.hide_password")
    } else {
        i18n.t("login.show_password")
    };
    let password_match_label = if passwords_match {
        i18n.t("account_settings.password_match_valid")
    } else {
        i18n.t("account_settings.password_match_invalid")
    };

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        if current_pwd().is_empty() || new_pwd().is_empty() || confirm_pwd().is_empty() {
            error_msg.set(Some(
                i18n.t("account_settings.fill_all_passwords").to_string(),
            ));
            return;
        }
        if new_pwd() != confirm_pwd() {
            error_msg.set(Some(
                i18n.t("account_settings.password_mismatch").to_string(),
            ));
            return;
        }
        let rules = evaluate_password_rules(&new_pwd());
        if !rules.length {
            error_msg.set(Some(
                i18n.t("account_settings.password_too_short").to_string(),
            ));
            return;
        }
        if !rules.uppercase {
            error_msg.set(Some(
                i18n.t("account_settings.password_no_uppercase").to_string(),
            ));
            return;
        }
        if !rules.lowercase {
            error_msg.set(Some(
                i18n.t("account_settings.password_no_lowercase").to_string(),
            ));
            return;
        }
        if !rules.digit {
            error_msg.set(Some(
                i18n.t("account_settings.password_no_digit").to_string(),
            ));
            return;
        }
        if !rules.special {
            error_msg.set(Some(
                i18n.t("account_settings.password_no_special").to_string(),
            ));
            return;
        }
        saving.set(true);
        error_msg.set(None);
        success_msg.set(None);
        let cur = current_pwd();
        let nw = new_pwd();
        spawn(async move {
            let token = auth_store.token().unwrap_or_default();
            match user_service::change_password(&cur, &nw, &token).await {
                Ok(_) => {
                    // The server revokes every existing token after a password
                    // change. Clear the local session immediately so the user
                    // is not left on a page that can only produce 401s.
                    ui_store.show_success(i18n.t("account_settings.password_changed_relogin"));
                    auth_store.logout();
                    nav.replace(Route::Login {});
                    current_pwd.set(String::new());
                    new_pwd.set(String::new());
                    confirm_pwd.set(String::new());
                    saving.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(format!(
                        "{}：{}",
                        i18n.t("account_settings.change_failed"),
                        user_error_message(i18n, &e)
                    )));
                    saving.set(false);
                }
            }
        });
    };

    rsx! {
        div {
            class: "page-container account-settings-page",
            PageHeader {
                title: i18n.t("page.account_settings").to_string(),
                description: i18n.t("account_settings.subtitle").to_string(),
            }

            if let Some(msg) = success_msg() {
                div { class: "alert alert-success", "{msg}" }
            }
            if let Some(err) = error_msg() {
                div { class: "alert alert-error", "{err}" }
            }

            div { class: "settings-section-card account-settings-card",
                div { class: "settings-section-head",
                    div {
                        h2 { class: "settings-section-title", {i18n.t("account_settings.change_password")} }
                        p { class: "settings-section-description",
                            {i18n.t("account_settings.section_desc")}
                        }
                    }
                }
                div { class: "settings-section-body",
                    form {
                        class: "account-settings-form",
                        onsubmit: on_submit,
                        div { class: "setting-row",
                            div { class: "setting-row-main",
                                div { class: "setting-row-meta",
                                    label { class: "setting-label", r#for: "current-password", {i18n.t("account_settings.current_password")} }
                                    p { class: "setting-description", {i18n.t("account_settings.current_password_desc")} }
                                }
                                    div { class: "password-input-wrap setting-control",
                                        input {
                                            id: "current-password",
                                            class: "form-input",
                                            r#type: if show_current_pwd() { "text" } else { "password" },
                                            autocomplete: "current-password",
                                            placeholder: "{i18n.t(\"account_settings.current_password_placeholder\")}",
                                            value: "{current_pwd}",
                                            oninput: move |e| current_pwd.set(e.value()),
                                        }
                                        button {
                                            class: "password-toggle",
                                            r#type: "button",
                                            aria_label: "{current_password_toggle_label}",
                                            onclick: move |_| show_current_pwd.toggle(),
                                            "{current_password_toggle_label}"
                                        }
                                }
                            }
                        }
                        div { class: "setting-row",
                            div { class: "setting-row-main",
                                div { class: "setting-row-meta",
                                    label { class: "setting-label", r#for: "new-password", {i18n.t("account_settings.new_password")} }
                                    p { class: "setting-description", {i18n.t("account_settings.new_password_desc")} }
                                }
                                div { class: "password-input-wrap setting-control",
                                    input {
                                        id: "new-password",
                                        class: "form-input",
                                        r#type: if show_new_pwd() { "text" } else { "password" },
                                        autocomplete: "new-password",
                                        placeholder: "{i18n.t(\"account_settings.new_password_placeholder\")}",
                                        value: "{new_pwd}",
                                        oninput: move |e| new_pwd.set(e.value()),
                                    }
                                    button {
                                        class: "password-toggle",
                                        r#type: "button",
                                        aria_label: "{new_password_toggle_label}",
                                        onclick: move |_| show_new_pwd.toggle(),
                                        "{new_password_toggle_label}"
                                    }
                                }
                                div { class: "password-requirements", aria_live: "polite",
                                    p { class: "setting-description", {i18n.t("account_settings.password_requirements")} }
                                    ul {
                                        li { class: if password_rules.length { "password-rule valid" } else { "password-rule" }, if password_rules.length { "✓" } else { "•" }, " ", {i18n.t("account_settings.password_rule_length")} }
                                        li { class: if password_rules.uppercase { "password-rule valid" } else { "password-rule" }, if password_rules.uppercase { "✓" } else { "•" }, " ", {i18n.t("account_settings.password_rule_uppercase")} }
                                        li { class: if password_rules.lowercase { "password-rule valid" } else { "password-rule" }, if password_rules.lowercase { "✓" } else { "•" }, " ", {i18n.t("account_settings.password_rule_lowercase")} }
                                        li { class: if password_rules.digit { "password-rule valid" } else { "password-rule" }, if password_rules.digit { "✓" } else { "•" }, " ", {i18n.t("account_settings.password_rule_digit")} }
                                        li { class: if password_rules.special { "password-rule valid" } else { "password-rule" }, if password_rules.special { "✓" } else { "•" }, " ", {i18n.t("account_settings.password_rule_special")} }
                                    }
                                }
                            }
                        }
                        div { class: "setting-row",
                            div { class: "setting-row-main",
                                div { class: "setting-row-meta",
                                    label { class: "setting-label", r#for: "confirm-password", {i18n.t("account_settings.confirm_password")} }
                                    p { class: "setting-description", {i18n.t("account_settings.confirm_password_desc")} }
                                }
                                div { class: "password-input-wrap setting-control",
                                    input {
                                        id: "confirm-password",
                                        class: "form-input",
                                        r#type: if show_confirm_pwd() { "text" } else { "password" },
                                        autocomplete: "new-password",
                                        placeholder: "{i18n.t(\"account_settings.confirm_password_placeholder\")}",
                                        value: "{confirm_pwd}",
                                        oninput: move |e| confirm_pwd.set(e.value()),
                                    }
                                    button {
                                        class: "password-toggle",
                                        r#type: "button",
                                        aria_label: "{confirm_password_toggle_label}",
                                        onclick: move |_| show_confirm_pwd.toggle(),
                                        "{confirm_password_toggle_label}"
                                    }
                                }
                                if !confirm_password_value.is_empty() {
                                    p { class: if passwords_match { "password-match valid" } else { "password-match invalid" }, "{password_match_label}" }
                                }
                            }
                        }
                        div { class: "account-settings-actions",
                            button {
                                class: "btn btn-primary",
                                r#type: "submit",
                                disabled: saving(),
                                if saving() { {i18n.t("form.saving")} } else { {i18n.t("form.save_changes")} }
                            }
                        }
                    }
                }
            }
        }
    }
}
