//! 饼图组件
//!
//! 基于 `charming` crate（ECharts WASM 渲染）封装的 Dioxus 饼图组件。
//!
//! # 示例
//! ```rust
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

use charming::{
    Chart, WasmRenderer,
    component::{Legend, Title},
    series::Pie,
};
use dioxus::prelude::*;

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
/// 基于 charming `WasmRenderer` 渲染 Apache ECharts 饼图。
/// 组件挂载后通过 `use_effect` 触发渲染，数据变更时自动重渲染。
#[component]
pub fn PieChart(props: PieChartProps) -> Element {
    let id = props.id.clone();
    let width = props.width;
    let height = props.height;
    let title_text = props.title.clone();
    let pie_data = props.data.clone();

    use_effect(move || {
        let mut chart = Chart::new().legend(Legend::new().top("bottom"));

        if !title_text.is_empty() {
            chart = chart.title(Title::new().text(title_text.as_str()));
        }

        // 构造 ECharts 饼图数据格式 (value, name)
        let data_pairs: Vec<(f64, &str)> = pie_data
            .iter()
            .map(|item| (item.value, item.name.as_str()))
            .collect();

        let pie = Pie::new()
            .radius(vec!["40%", "70%"])
            .center(vec!["50%", "50%"])
            .data(data_pairs);

        chart = chart.series(pie);

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
