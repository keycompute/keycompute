use crate::hooks::use_i18n::use_i18n;
use crate::stores::ui_store::UiStore;
use crate::utils::copy::on_copy_toast;
use dioxus::prelude::*;
use ui::{Modal, icons::IconCopy};

/// 模型浏览弹窗中的一行模型信息。
///
/// 账号页面只有模型 ID，因此 `owner` 为空；API Key 页面还会展示
/// `/v1/models` 返回的 `owned_by`，帮助用户在模型名称相近时辨认来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelListEntry {
    pub id: String,
    pub owner: Option<String>,
    pub is_default: bool,
}

impl ModelListEntry {
    pub fn new(id: impl Into<String>, owner: Option<String>) -> Self {
        Self {
            id: id.into(),
            owner,
            is_default: false,
        }
    }
}

fn filter_models(models: &[ModelListEntry], query: &str) -> Vec<ModelListEntry> {
    let query = query.trim().to_lowercase();
    models
        .iter()
        .filter(|model| {
            query.is_empty()
                || model.id.to_lowercase().contains(&query)
                || model
                    .owner
                    .as_deref()
                    .map(|owner| owner.to_lowercase().contains(&query))
                    .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// 统一的完整模型列表弹窗。
///
/// 两个页面共用同一套视觉和键盘交互，调用方只负责提供当前上下文中的
/// 模型集合。列表默认按页面上的折叠摘要展示，点击“更多”后才打开此弹窗。
#[component]
pub fn ModelListModal(
    open: ReadSignal<bool>,
    title: String,
    description: String,
    models: Vec<ModelListEntry>,
    #[props(default)] onclose: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let ui_store = use_context::<UiStore>();
    let mut search = use_signal(String::new);

    // 关闭后清除上一次搜索，下一次打开时始终从完整列表开始，避免用户
    // 误以为模型缺失。
    use_effect(move || {
        if !open() && !search().is_empty() {
            search.set(String::new());
        }
    });

    let filtered_models = filter_models(&models, &search());
    let result_summary = format!("{}/{}", filtered_models.len(), models.len());

    rsx! {
        Modal {
            open,
            title,
            close_label: i18n.t("common.close").to_string(),
            max_width: "640px".to_string(),
            onclose,
            div { class: "kc-model-browser",
                if !description.is_empty() {
                    p { class: "kc-model-browser-description", "{description}" }
                }
                div { class: "kc-model-browser-toolbar",
                    input {
                        class: "input-field kc-model-browser-search",
                        r#type: "search",
                        value: "{search}",
                        placeholder: "{i18n.t(\"common.model_search_placeholder\")}",
                        aria_label: i18n.t("common.model_search_placeholder"),
                        oninput: move |event| search.set(event.value()),
                    }
                    span { class: "kc-model-browser-summary", "{result_summary}" }
                }
                if filtered_models.is_empty() {
                    div { class: "kc-model-browser-empty", {i18n.t("common.no_matching_models")} }
                } else {
                    div { class: "kc-model-browser-list",
                        for entry in filtered_models.iter() {
                            div { class: "kc-model-browser-row",
                                div { class: "kc-model-browser-info",
                                    code { class: "kc-model-browser-id", "{entry.id}" }
                                    if let Some(owner) = &entry.owner {
                                        if !owner.is_empty() {
                                            span { class: "kc-model-browser-owner", "{owner}" }
                                        }
                                    }
                                    if entry.is_default {
                                        span { class: "kc-model-browser-default", {i18n.t("common.default")} }
                                    }
                                }
                                button {
                                    class: "btn btn-ghost btn-sm kc-model-browser-copy",
                                    r#type: "button",
                                    aria_label: "{i18n.t(\"common.copy_model_id\")}: {entry.id}",
                                    title: i18n.t("common.copy_model_id"),
                                    onclick: on_copy_toast(
                                        entry.id.clone(),
                                        i18n.t("common.copied"),
                                        i18n.t("common.copy_manual_hint"),
                                        ui_store,
                                    ),
                                    IconCopy { size: 14 }
                                    {i18n.t("common.copy")}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelListEntry, filter_models};

    fn entries() -> Vec<ModelListEntry> {
        vec![
            ModelListEntry::new("gpt-5.5", Some("openai".to_string())),
            ModelListEntry::new("claude-3-5-sonnet", Some("anthropic".to_string())),
            ModelListEntry::new("local-model", None),
        ]
    }

    #[test]
    fn model_filter_is_case_insensitive_and_matches_owner() {
        let models = entries();

        assert_eq!(filter_models(&models, "GPT"), vec![models[0].clone()]);
        assert_eq!(filter_models(&models, "ANTHROPIC"), vec![models[1].clone()]);
    }

    #[test]
    fn blank_model_filter_keeps_the_complete_list() {
        let models = entries();
        assert_eq!(filter_models(&models, "  "), models);
    }
}
