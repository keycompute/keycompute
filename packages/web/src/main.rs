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
const ECHARTS_JS: Asset = asset!("/assets/js/echarts.min.js");

fn main() {
    dioxus::launch(Root);
}

#[component]
fn Root() -> Element {
    let _ = MAIN_CSS;
    let _ = FONTS_CSS;

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        // ECharts 用于图表渲染（本地加载）
        document::Script {
            src: ECHARTS_JS,
            r#type: "text/javascript",
        }
        App {}
    }
}
