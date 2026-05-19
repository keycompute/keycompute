use dioxus::prelude::*;

/// 页脚组件
#[component]
pub fn Footer() -> Element {
    rsx! {
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
    }
}
