use dioxus::prelude::*;

use crate::router::Route;

/// 注册页面 - 重定向到首页（注册功能已移至首页弹窗）
#[component]
pub fn Register() -> Element {
    let nav = use_navigator();

    // 直接重定向到首页
    use_effect(move || {
        nav.replace(Route::Home {});
    });

    rsx! {
        div {
            style: "display:flex;align-items:center;justify-content:center;height:100vh;background:#0a0f1a;color:#f0f6ff",
            p { "正在跳转到首页..." }
        }
    }
}
