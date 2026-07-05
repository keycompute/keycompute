#![allow(clippy::clone_on_copy)]

use dioxus::prelude::*;

mod app;
mod hooks;
mod i18n;
mod router;
mod services;
mod stores;
mod utils;
mod views;

use app::App;

const FAVICON: Asset = asset!("/assets/favicon.ico");
const MAIN_CSS: Asset = asset!(
    "/assets/main.css",
    AssetOptions::css().with_static_head(true)
);
const FONTS_CSS: Asset = asset!(
    "/assets/fonts.css",
    AssetOptions::css().with_static_head(true)
);

// 注册字体文件 — Dioxus 不会自动追踪 CSS url() 内的资源，需显式注册
const FONT_INTER_LATIN: Asset = asset!("/assets/fonts/inter-latin.woff2");
const FONT_INTER_LATIN_EXT: Asset = asset!("/assets/fonts/inter-latin-ext.woff2");
const FONT_SPACE_GROTESK_LATIN: Asset = asset!("/assets/fonts/space-grotesk-latin.woff2");
const FONT_SPACE_GROTESK_LATIN_EXT: Asset = asset!("/assets/fonts/space-grotesk-latin-ext.woff2");

const ECHARTS_JS: Asset = asset!("/assets/js/echarts.min.js");

fn main() {
    dioxus::launch(Root);
}

#[component]
fn Root() -> Element {
    let _ = MAIN_CSS;
    let _ = FONTS_CSS;
    // 确保字体 asset 不会被 tree-shaking 移除
    let _ = FONT_INTER_LATIN;
    let _ = FONT_INTER_LATIN_EXT;
    let _ = FONT_SPACE_GROTESK_LATIN;
    let _ = FONT_SPACE_GROTESK_LATIN_EXT;

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        // ECharts 用于图表渲染（本地加载）
        document::Script { src: ECHARTS_JS, r#type: "text/javascript" }
        App {}
    }
}
