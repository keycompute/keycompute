//! 折线图组件
//!
//! 基于 `charming` crate（ECharts WASM 渲染）封装的 Dioxus 折线图组件。
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

use charming::{
    Chart, WasmRenderer,
    component::{Axis, Grid, Legend, Title},
    element::AxisType,
    series::Line,
};
use dioxus::prelude::*;

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
/// 基于 charming `WasmRenderer` 渲染 Apache ECharts 折线图。
/// 组件挂载后通过 `use_effect` 触发渲染，数据变更时自动重渲染。
#[component]
pub fn LineChart(props: LineChartProps) -> Element {
    let id = props.id.clone();
    let width = props.width;
    let height = props.height;
    let title_text = props.title.clone();
    let x_data = props.x_data.clone();
    let series_data = props.series.clone();

    use_effect(move || {
        let mut chart = Chart::new().grid(Grid::new());

        if !title_text.is_empty() {
            chart = chart.title(Title::new().text(title_text.as_str()));
        }

        // 图例（多系列时显示）
        if series_data.len() > 1 {
            chart = chart.legend(Legend::new().top("bottom"));
        }

        // X 轴
        let x_labels: Vec<&str> = x_data.iter().map(String::as_str).collect();
        chart = chart.x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(x_labels),
        );

        // Y 轴
        chart = chart.y_axis(Axis::new().type_(AxisType::Value));

        // 数据系列
        for s in &series_data {
            let values: Vec<charming::datatype::CompositeValue> =
                s.data.iter().map(|&v| v.into()).collect();
            let line = Line::new().name(s.name.as_str()).data(values);
            chart = chart.series(line);
        }

        let renderer = WasmRenderer::new(width, height);
        let _ = renderer.render(&id, &chart);
    });

    rsx! {
        div {
            id: "{props.id}",
            style: "width: {width}px; height: {height}px;",
        }
    }
}
