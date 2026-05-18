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

fn main() {
    dioxus::launch(Root);
}

#[component]
fn Root() -> Element {
    let _ = MAIN_CSS;

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        // ECharts 用于图表渲染
        document::Script {
            src: "https://cdn.jsdelivr.net/npm/echarts@5.4.3/dist/echarts.min.js",
            r#type: "text/javascript",
        }
        App {}
    }
}
