use dioxus::prelude::*;

use crate::hooks::use_i18n::use_i18n;
use crate::router::Route;
use crate::services::api_client::user_error_message;
use crate::services::auth_service;

#[cfg(any(test, target_arch = "wasm32"))]
fn read_query_param_from_search(search: &str, name: &str) -> Option<String> {
    for pair in search.trim_start_matches('?').split('&') {
        if pair.is_empty() {
            continue;
        }

        let mut parts = pair.splitn(2, '=');
        if let Some(key) = parts.next()
            && key == name
        {
            let value = parts.next().unwrap_or("").trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

#[cfg(target_arch = "wasm32")]
fn read_query_param(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    read_query_param_from_search(&search, name)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_query_param(_name: &str) -> Option<String> {
    None
}

fn read_ref_from_query() -> Option<String> {
    read_query_param("ref")
}

#[component]
pub fn Register() -> Element {
    let i18n = use_i18n();
    let nav = use_navigator();

    let mut name = use_signal(String::new);
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut confirm_password = use_signal(String::new);
    let mut verification_code = use_signal(String::new);
    let referral_code = use_signal(read_ref_from_query);
    let mut locked_email = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| false);
    let mut code_requested = use_signal(|| false);
    let mut completed = use_signal(|| false);
    let mut error_msg = use_signal(|| Option::<String>::None);
    let mut success_msg = use_signal(|| Option::<String>::None);

    let t_fill_required = i18n.t("auth.fill_required");
    let t_pwd_mismatch = i18n.t("form.password_mismatch");
    let t_register_failed = i18n.t("auth.register_failed");
    let t_request_code_failed = i18n.t("auth.request_code_failed");
    let t_code_required = i18n.t("auth.code_required");

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
            let referral_code_val = referral_code();
            spawn(async move {
                match auth_service::request_registration_code(
                    &email_val,
                    referral_code_val.as_deref(),
                )
                .await
                {
                    Ok(resp) => {
                        email.set(resp.email.clone());
                        locked_email.set(Some(resp.email.clone()));
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

        if password() != confirm_password() {
            error_msg.set(Some(t_pwd_mismatch.to_string()));
            return;
        }

        loading.set(true);
        error_msg.set(None);

        let name_val = name();
        let email_val = match locked_email() {
            Some(email_val) => email_val,
            None => email(),
        };
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
                Ok(resp) => {
                    completed.set(true);
                    code_requested.set(false);
                    success_msg.set(Some(resp.message));
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

    let on_change_email = move |_| {
        if loading() {
            return;
        }

        locked_email.set(None);
        code_requested.set(false);
        verification_code.set(String::new());
        success_msg.set(None);
        error_msg.set(None);
    };

    let on_resend_code = move |_| {
        if loading() {
            return;
        }

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
        let referral_code_val = referral_code();
        spawn(async move {
            match auth_service::request_registration_code(&email_val, referral_code_val.as_deref())
                .await
            {
                Ok(resp) => {
                    email.set(resp.email.clone());
                    locked_email.set(Some(resp.email.clone()));
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
    };

    rsx! {
        div {
            class: "auth-page",
            div {
                class: "auth-card",
                div {
                    class: "auth-header",
                    h1 { class: "auth-title", {i18n.t("auth.register")} }
                    p { class: "auth-subtitle", {i18n.t("auth.register_subtitle")} }
                }

                if let Some(msg) = success_msg() {
                    div { class: "alert alert-success", "{msg}" }
                }

                if let Some(err) = error_msg() {
                    div { class: "alert alert-error", "{err}" }
                }

                if completed() {
                    div {
                        class: "auth-footer",
                        p { class: "text-secondary", {i18n.t("auth.registration_success")} }
                        button {
                            class: "btn btn-primary btn-full",
                            r#type: "button",
                            onclick: move |_| {
                                nav.push(Route::Login {});
                            },
                            {i18n.t("auth.login_now")}
                        }
                    }
                } else {
                    form {
                        onsubmit: on_submit,
                        div {
                            class: "form-group",
                            label { class: "form-label", {i18n.t("auth.name")} }
                            input {
                                class: "form-input",
                                r#type: "text",
                                placeholder: i18n.t("auth.name_placeholder"),
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
                                placeholder: i18n.t("auth.email_placeholder"),
                                value: "{email}",
                                disabled: code_requested(),
                                oninput: move |e| email.set(e.value()),
                            }
                        }
                        div {
                            class: "form-group",
                            label { class: "form-label", {i18n.t("auth.password")} }
                            input {
                                class: "form-input",
                                r#type: "password",
                                placeholder: i18n.t("auth.password_min8"),
                                value: "{password}",
                                oninput: move |e| password.set(e.value()),
                            }
                        }
                        div {
                            class: "form-group",
                            label { class: "form-label", {i18n.t("auth.confirm_password")} }
                            input {
                                class: "form-input",
                                r#type: "password",
                                placeholder: i18n.t("auth.confirm_password_placeholder"),
                                value: "{confirm_password}",
                                oninput: move |e| confirm_password.set(e.value()),
                            }
                        }

                        if code_requested() {
                            div {
                                class: "form-group",
                                label { class: "form-label", {i18n.t("auth.verification_code")} }
                                input {
                                    class: "form-input",
                                    r#type: "text",
                                    maxlength: "6",
                                    placeholder: i18n.t("auth.verification_code_placeholder"),
                                    value: "{verification_code}",
                                    oninput: move |e| verification_code.set(e.value()),
                                }
                            }
                            if let Some(current_email) = locked_email() {
                                p {
                                    class: "text-secondary",
                                    "{i18n.t(\"auth.code_sent_to\")} {current_email}"
                                }
                            }
                            p {
                                class: "text-secondary",
                                {i18n.t("auth.code_sent_hint")}
                            }
                        }

                        button {
                            class: "btn btn-primary btn-full",
                            r#type: "submit",
                            disabled: loading(),
                            if loading() {
                                if code_requested() {
                                    {i18n.t("auth.registering")}
                                } else {
                                    {i18n.t("auth.requesting_code")}
                                }
                            } else if code_requested() {
                                {i18n.t("auth.complete_registration")}
                            } else {
                                {i18n.t("auth.request_code")}
                            }
                        }

                        if code_requested() {
                            div {
                                class: "auth-footer",
                                button {
                                    class: "btn btn-secondary btn-full",
                                    r#type: "button",
                                    disabled: loading(),
                                    onclick: on_resend_code,
                                    {i18n.t("auth.resend_code")}
                                }
                            }
                            div {
                                class: "auth-footer",
                                button {
                                    class: "link",
                                    r#type: "button",
                                    disabled: loading(),
                                    onclick: on_change_email,
                                    {i18n.t("auth.change_email")}
                                }
                            }
                        }
                    }
                }

                div {
                    class: "auth-footer",
                    {i18n.t("auth.has_account")}
                    button {
                        class: "link",
                        r#type: "button",
                        onclick: move |_| {
                            nav.push(Route::Login {});
                        },
                        " "
                        {i18n.t("auth.login_now")}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_query_param_from_search;

    #[test]
    fn read_query_param_from_search_reads_ref_param() {
        let ref_value = read_query_param_from_search("?ref=abc123&source=campaign", "ref");

        assert_eq!(ref_value.as_deref(), Some("abc123"));
    }

    #[test]
    fn read_query_param_from_search_rejects_unknown_param() {
        let ref_value = read_query_param_from_search("?legacy=abc123", "ref");

        assert_eq!(ref_value, None);
    }
}
