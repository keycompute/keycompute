//! 折线图组件
//!
//! 通过 JS 互调直接使用 ECharts 渲染折线图，无需 charming 中间层。
//!
//! # 示例
//! ```rust
//! LineChart {
//!     id: "usage-chart",
//!     title: "用量趋势",
//!     x_data: vec!["周一", "周二", "周三", "周四", "周五"],
//!     series: vec![
//!         LineSeriesData { name: "调用次数", data: vec![120.0, 200.0, 150.0, 80.0, 70.0] },
//!     ],
//!     width: 600,
//!     height: 300,
//! }
//! ```

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use serde_json::json;

/// 折线图单条数据系列
#[derive(Clone, PartialEq)]
pub struct LineSeriesData {
    /// 系列名称（图例显示）
    pub name: String,
    /// 数值列表，与 `x_data` 一一对应
    pub data: Vec<f64>,
}

/// 折线图组件 Props
#[derive(Props, Clone, PartialEq)]
pub struct LineChartProps {
    /// 图表容器 DOM id（同一页面多个图表需保证唯一）
    pub id: String,
    /// 图表标题（空字符串则不显示）
    #[props(default)]
    pub title: String,
    /// X 轴分类标签
    pub x_data: Vec<String>,
    /// 数据系列列表
    pub series: Vec<LineSeriesData>,
    /// 容器宽度（像素）
    #[props(default = 500)]
    pub width: u32,
    /// 容器高度（像素）
    #[props(default = 300)]
    pub height: u32,
}

/// 折线图组件
///
/// 通过 JS 互调直接调用 ECharts API 渲染折线图。
/// 组件挂载后通过 `use_effect` 触发渲染，数据变更时自动重渲染。
#[component]
#[allow(unused_variables)]
pub fn LineChart(props: LineChartProps) -> Element {
    let id = props.id.clone();
    let width = props.width;
    let height = props.height;
    let title_text = props.title.clone();
    let x_data = props.x_data.clone();
    let series_data = props.series.clone();

    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            let series_arr: Vec<serde_json::Value> = series_data
                .iter()
                .map(|s| {
                    json!({
                        "type": "line",
                        "name": s.name,
                        "data": s.data,
                        "smooth": true
                    })
                })
                .collect();

            let mut option = json!({
                "grid": { "containLabel": true },
                "xAxis": { "type": "category", "data": x_data },
                "yAxis": { "type": "value" },
                "series": series_arr
            });

            if !title_text.is_empty() {
                option["title"] = json!({ "text": title_text });
            }
            if series_data.len() > 1 {
                option["legend"] = json!({ "bottom": 0 });
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
