//! 饼图组件
//!
//! 通过 JS 互调直接使用 ECharts 渲染饼图，无需 charming 中间层。
//!
//! # 示例
//! ```rust,ignore
//! PieChart {
//!     id: "model-dist-chart",
//!     title: "模型调用分布",
//!     data: vec![
//!         PieItem { name: "GPT-4", value: 45.0 },
//!         PieItem { name: "GPT-3.5", value: 30.0 },
//!         PieItem { name: "Claude", value: 25.0 },
//!     ],
//!     width: 400,
//!     height: 300,
//! }
//! ```

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use serde_json::json;

/// 饼图单项数据
#[derive(Clone, PartialEq)]
pub struct PieItem {
    /// 扇区名称
    pub name: String,
    /// 扇区数值
    pub value: f64,
}

/// 饼图组件 Props
#[derive(Props, Clone, PartialEq)]
pub struct PieChartProps {
    /// 图表容器 DOM id（同一页面多个图表需保证唯一）
    pub id: String,
    /// 图表标题（空字符串则不显示）
    #[props(default)]
    pub title: String,
    /// 各扇区数据
    pub data: Vec<PieItem>,
    /// 容器宽度（像素）
    #[props(default = 400)]
    pub width: u32,
    /// 容器高度（像素）
    #[props(default = 300)]
    pub height: u32,
}

/// 饼图组件
///
/// 通过 JS 互调直接调用 ECharts API 渲染饼图。
/// 组件挂载后通过 `use_effect` 触发渲染，数据变更时自动重渲染。
#[component]
#[allow(unused_variables)]
pub fn PieChart(props: PieChartProps) -> Element {
    let id = props.id.clone();
    let width = props.width;
    let height = props.height;
    let title_text = props.title.clone();
    let pie_data = props.data.clone();

    let cleanup_id = props.id.clone();
    use_drop(move || {
        #[cfg(target_arch = "wasm32")]
        {
            crate::charts::echarts_bindgen::dispose_chart(&cleanup_id);
        }
    });

    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            let data_arr: Vec<serde_json::Value> = pie_data
                .iter()
                .map(|item| json!({ "name": item.name, "value": item.value }))
                .collect();

            let mut option = json!({
                "legend": { "bottom": 0 },
                "series": [{
                    "type": "pie",
                    "radius": ["40%", "70%"],
                    "center": ["50%", "50%"],
                    "data": data_arr
                }]
            });

            if !title_text.is_empty() {
                option["title"] = json!({ "text": title_text });
            }

            crate::charts::echarts_bindgen::render_chart(&id, width, height, &option);
        }
    });

    rsx! {
        div {
            id: "{props.id}",
            style: "width: {width}px; height: {height}px;",
        }
    }
}
