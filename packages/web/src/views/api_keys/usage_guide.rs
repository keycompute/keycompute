//! Available for existing keys as well as newly created keys; examples never
//! manufacture a model or mix a late response from another access mode.
use super::examples::{anthropic_examples, openai_examples, responses_examples};
use crate::{
    hooks::use_i18n::use_i18n,
    services::{
        api_client::{public_api_root_url, user_error_message, with_auto_refresh},
        model_service,
    },
    stores::{auth_store::AuthStore, ui_store::UiStore},
    utils::{
        on_copy,
        resource::{KeyedResourceValue, current_keyed_value},
    },
    views::shared::model_controls::{mode_description, mode_label},
};
use client_api::api::admin::ModelAccessMode;
use dioxus::prelude::*;

pub fn mode_base(root: &str, mode: ModelAccessMode) -> String {
    format!(
        "{}/{}",
        root.trim_end_matches('/'),
        if mode == ModelAccessMode::Passthrough {
            "pt/v1"
        } else {
            "v1"
        }
    )
}
#[component]
pub fn ModelUsageGuide(api_key: Option<String>) -> Element {
    let i = use_i18n();
    let mut shown = use_signal(|| api_key.is_some());
    let mut mode = use_signal(|| ModelAccessMode::AccountPool);
    rsx! {
        section {class:"kc-mode-workflow",
            button {class:"btn btn-ghost",r#type:"button",aria_expanded:shown(),onclick:move|_|{shown.toggle();},{i.t("models.invocation_guide")}}
            if shown() {
                p {{i.t("models.guide_help")}}
                div {class:"kc-inline-actions",role:"group",aria_label:i.t("models.access_mode"),
                    for value in [ModelAccessMode::AccountPool,ModelAccessMode::Passthrough,ModelAccessMode::NodeDispatch] {
                        button {class:if mode()==value {"btn btn-primary"}else{"btn btn-secondary"},r#type:"button",aria_pressed:mode()==value,onclick:move|_|{mode.set(value);},{mode_label(i,value)}}
                    }
                }
                for selected in [mode()] {GuideMode {key:"{selected.as_str()}",mode:selected,api_key:api_key.clone()}}
            }
        }
    }
}
#[component]
fn GuideMode(mode: ModelAccessMode, api_key: Option<String>) -> Element {
    let i = use_i18n();
    let auth = use_context::<AuthStore>();
    let ui = use_context::<UiStore>();
    let mut surface = use_signal(|| "chat_completions".to_string());
    let mut model = use_signal(String::new);
    let mut tab = use_signal(|| "curl".to_string());
    let mut copied = use_signal(|| false);
    let mut refresh = use_signal(|| 0u32);
    let data = use_resource(move || {
        let key = (mode, surface(), refresh(), (auth.state)().session_id);
        async move {
            let protocol = if key.1 == "messages" {
                "anthropic"
            } else {
                "openai"
            };
            let capability = key.1.clone();
            let result = with_auto_refresh(auth, move |token| {
                let capability = capability.clone();
                async move {
                    model_service::list_models(mode.as_str(), protocol, &capability, &token).await
                }
            })
            .await;
            KeyedResourceValue::new(key, result)
        }
    });
    let key = (mode, surface(), refresh(), (auth.state)().session_id);
    let result = current_keyed_value(&key, data.state().cloned(), data());
    let selected = result
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .and_then(|r| {
            r.data
                .iter()
                .find(|m| m.id == model())
                .or_else(|| r.data.first())
        })
        .map(|m| m.id.clone());
    let root = public_api_root_url();
    let base = if mode == ModelAccessMode::Passthrough {
        mode_base(&root, mode)
    } else {
        crate::services::api_client::public_openai_api_base_url()
    };
    let displayed_base = if surface() == "messages" {
        root.clone()
    } else {
        base.clone()
    };
    let credential = api_key
        .clone()
        .unwrap_or_else(|| "YOUR_PLATFORM_KEY".to_string());
    let examples = selected.as_ref().map(|model| match surface().as_str() {
        "responses" => responses_examples(
            &base,
            &credential,
            model,
            i.t("api_keys.example_env_comment"),
        ),
        "messages" => anthropic_examples(
            &root,
            &credential,
            model,
            i.t("api_keys.example_env_comment"),
        ),
        _ => openai_examples(
            &base,
            &credential,
            model,
            i.t("api_keys.example_env_comment"),
        ),
    });
    rsx! {
        p {{mode_description(i,mode)}}
        if mode==ModelAccessMode::NodeDispatch {p {class:"form-hint",{i.t("models.node_stream_help")}}}
        if api_key.is_none() {p {class:"form-hint",{i.t("models.placeholder_key")}}}
        label {class:"form-label",{i.t("models.api_surface")}}
        select {class:"input-field",aria_label:i.t("models.api_surface"),value:"{surface}",onchange:move|e|{surface.set(e.value());model.set(String::new());tab.set("curl".into());copied.set(false);},
            option {value:"chat_completions","OpenAI · Chat Completions"}
            if mode==ModelAccessMode::AccountPool {option {value:"responses","OpenAI · Responses"} option {value:"messages","Anthropic · Messages"}}
        }
        p {code {"Base URL: {displayed_base}"}}
        match result {
            None=>rsx!{p {role:"status",{i.t("common.loading")}}},
            Some(Err(e))=>rsx!{div {class:"alert alert-error",role:"alert",{user_error_message(&e)}}},
            Some(Ok(r)) if r.data.is_empty()=>rsx!{p {class:"form-hint",{i.t("models.no_examples")}}},
            Some(Ok(r))=>rsx!{
                label {class:"form-label",{i.t("models.choose_model")}}
                select {class:"input-field",aria_label:i.t("models.choose_model"),value:"{selected.clone().unwrap_or_default()}",onchange:move|e|{model.set(e.value());copied.set(false);},
                    for entry in r.data {option {value:"{entry.id}","{entry.id}"}}
                }
            }
        }
        button {class:"btn btn-ghost btn-sm",r#type:"button",onclick:move|_|{refresh+=1;copied.set(false);},{i.t("common.refresh")}}
        if let Some(examples)=examples {
            div {class:"kc-inline-actions",
                for format in ["curl","python","node","env"] {button {class:if tab()==format {"btn btn-secondary btn-sm"}else{"btn btn-ghost btn-sm"},r#type:"button",onclick:move|_|{tab.set(format.into());copied.set(false);},"{format}"}}
            }
            div {class:"kc-api-copy-block",
                pre {class:"kc-api-example","{examples.for_tab(&tab())}"}
                button {class:"btn btn-secondary",r#type:"button",onclick:on_copy(examples.for_tab(&tab()).to_string(),i.t("common.copy_manual_hint").to_string(),ui,copied),{i.t(if copied(){"api_keys.copied"}else{"api_keys.copy"})}}
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base_urls_preserve_reverse_proxy_prefix_and_never_contain_model_ids() {
        assert_eq!(
            mode_base("https://example.test/ai/", ModelAccessMode::AccountPool),
            "https://example.test/ai/v1"
        );
        assert_eq!(
            mode_base("https://example.test/ai/", ModelAccessMode::Passthrough),
            "https://example.test/ai/pt/v1"
        );
        assert_eq!(
            mode_base("https://example.test/ai/", ModelAccessMode::NodeDispatch),
            "https://example.test/ai/v1"
        );
    }
}
