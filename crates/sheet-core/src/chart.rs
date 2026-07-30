//! chart —— 图表数据抽取（M24，非渲染部分）。
//!
//! 对标 cmx-megasheet `SheetRenderer.buildChartData`：从图表数据源区域 + [`ChartSpec`] 取数，
//! 产出中性 [`ChartData`]（类别 + 各系列名/值）。**渲染（drawChart 的 canvas 原语）不移植**
//! —— 本项目「除去前端渲染、绘制功能」，此处只保留可在纯逻辑层复算的取数一环。
//!
//! 语义逐点对齐 TS：
//!  - 类别 = 数据源首列（去表头行），`String(getValue ?? '')`；
//!  - 每数据列一系列，系列名取表头行（无表头→`列{c}`），值 `Number(v) || 0`（非数→0）。

use crate::cell::CellValue;
use crate::numstr::num_to_string;
use crate::worksheet::{ChartSpec, Worksheet};

/// 单条系列：名称 + 数值序列。
#[derive(Debug, Clone, PartialEq)]
pub struct ChartSeries {
    pub name: String,
    pub values: Vec<f64>,
}

/// 图表取数结果：类别轴 + 各系列（+ 可选标题）。渲染层消费，本项目不渲染。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChartData {
    pub categories: Vec<String>,
    pub series: Vec<ChartSeries>,
    pub title: Option<String>,
}

/// 值 → 数字（对齐 TS `typeof v === 'number' ? v : Number(v) || 0`）。
/// 数值直取；布尔 true/false→1/0；数字串转数；其余（含空、非数串、None）→0。
fn to_number_or_zero(v: Option<&CellValue>) -> f64 {
    match v {
        Some(CellValue::Number(n)) => *n,
        // JS：Number(true)=1, Number(false)=0（typeof 非 number 故走 Number 分支）。
        Some(CellValue::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        // JS：Number('')=0, Number(' ')=0, Number('abc')=NaN→`||0`→0, Number('12')=12。
        Some(CellValue::Text(t)) => {
            let tt = t.trim();
            if tt.is_empty() {
                0.0
            } else {
                tt.parse::<f64>().unwrap_or(0.0)
            }
        }
        None => 0.0,
    }
}

/// 值 → 类别文本（对齐 TS `String(v ?? '')`）。None→""，数→JS 数字串，bool→"true"/"false"。
fn js_string(v: Option<&CellValue>) -> String {
    match v {
        None => String::new(),
        Some(CellValue::Text(t)) => t.clone(),
        Some(CellValue::Number(n)) => num_to_string(*n),
        Some(CellValue::Bool(b)) => if *b { "true" } else { "false" }.to_string(),
    }
}

/// 从图表数据源区域取数（对齐 TS buildChartData）。firstRowHeader/firstColHeader 拆表头/标签。
/// 二者缺省均视为 true（同 TS `?? true`）。
pub fn extract_chart_data(sheet: &Worksheet, spec: &ChartSpec) -> ChartData {
    let g = spec.data_range;
    let row_hdr = spec.first_row_header.unwrap_or(true);
    let col_hdr = spec.first_col_header.unwrap_or(true);
    let data_r0 = g.row + if row_hdr { 1 } else { 0 };
    let data_c0 = g.col + if col_hdr { 1 } else { 0 };

    // 类别 = 首列（去表头行）。
    let mut categories: Vec<String> = Vec::new();
    for r in data_r0..g.row + g.row_count {
        categories.push(js_string(sheet.get_value(r, g.col).as_ref()));
    }

    // 每数据列一系列。
    let mut series: Vec<ChartSeries> = Vec::new();
    for c in data_c0..g.col + g.col_count {
        let name = if row_hdr {
            let v = sheet.get_value(g.row, c);
            if v.is_none() {
                format!("列{c}")
            } else {
                js_string(v.as_ref())
            }
        } else {
            format!("列{c}")
        };
        let mut values: Vec<f64> = Vec::new();
        for r in data_r0..g.row + g.row_count {
            values.push(to_number_or_zero(sheet.get_value(r, c).as_ref()));
        }
        series.push(ChartSeries { name, values });
    }

    ChartData {
        categories,
        series,
        // TS `if (spec.title)`：undefined 或空串均不设。
        title: spec.title.as_ref().filter(|t| !t.is_empty()).cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worksheet::{ChartType, RegionRect};

    fn spec(dr: RegionRect, ct: ChartType) -> ChartSpec {
        ChartSpec {
            chart_type: ct,
            data_range: dr,
            title: None,
            first_row_header: None,
            first_col_header: None,
            options: None,
        }
    }

    /// 造一张典型「表头行 + 标签列」数据源：
    /// ```text
    ///        (col0)  S1   S2
    /// (row0)   ''    S1   S2
    ///           A    10    5
    ///           B    20   15
    ///           C    15   10
    ///           D    25   20
    /// ```
    fn typical() -> Worksheet {
        let mut ws = Worksheet::with_size("S", 20, 10);
        ws.set_value(0, 1, Some("S1".into()));
        ws.set_value(0, 2, Some("S2".into()));
        let cats = ["A", "B", "C", "D"];
        let s1 = [10.0, 20.0, 15.0, 25.0];
        let s2 = [5.0, 15.0, 10.0, 20.0];
        for (i, cat) in cats.iter().enumerate() {
            let r = (i + 1) as u32;
            ws.set_value(r, 0, Some((*cat).into()));
            ws.set_value(r, 1, Some(s1[i].into()));
            ws.set_value(r, 2, Some(s2[i].into()));
        }
        ws
    }

    #[test]
    fn extract_categories_and_series() {
        let ws = typical();
        let d = extract_chart_data(&ws, &spec(RegionRect::new(0, 0, 5, 3), ChartType::Column));
        assert_eq!(d.categories, vec!["A", "B", "C", "D"]);
        assert_eq!(d.series.len(), 2);
        assert_eq!(d.series[0].name, "S1");
        assert_eq!(d.series[0].values, vec![10.0, 20.0, 15.0, 25.0]);
        assert_eq!(d.series[1].name, "S2");
        assert_eq!(d.series[1].values, vec![5.0, 15.0, 10.0, 20.0]);
    }

    #[test]
    fn no_row_header_uses_synthetic_names() {
        let mut ws = Worksheet::with_size("S", 20, 10);
        // 无表头行：全是数据。首列作标签。
        ws.set_value(0, 0, Some("A".into()));
        ws.set_value(0, 1, Some(10.into()));
        ws.set_value(1, 0, Some("B".into()));
        ws.set_value(1, 1, Some(20.into()));
        let mut sp = spec(RegionRect::new(0, 0, 2, 2), ChartType::Bar);
        sp.first_row_header = Some(false);
        let d = extract_chart_data(&ws, &sp);
        assert_eq!(d.categories, vec!["A", "B"]);
        assert_eq!(d.series.len(), 1);
        assert_eq!(d.series[0].name, "列1"); // 合成名
        assert_eq!(d.series[0].values, vec![10.0, 20.0]);
    }

    #[test]
    fn non_numeric_and_blank_values_become_zero() {
        let mut ws = Worksheet::with_size("S", 20, 10);
        ws.set_value(0, 1, Some("S".into())); // 表头
        ws.set_value(1, 0, Some("r1".into()));
        ws.set_value(1, 1, Some("abc".into())); // 非数 → 0
        ws.set_value(2, 0, Some("r2".into())); // (2,1) 空 → 0
        ws.set_value(3, 0, Some("r3".into()));
        ws.set_value(3, 1, Some("42".into())); // 数字串 → 42
        let d = extract_chart_data(&ws, &spec(RegionRect::new(0, 0, 4, 2), ChartType::Line));
        assert_eq!(d.series[0].values, vec![0.0, 0.0, 42.0]);
    }

    #[test]
    fn no_col_header_all_columns_are_series() {
        let ws = typical();
        // 关掉列表头：首列也成一条系列（含表头行数据）。
        let mut sp = spec(RegionRect::new(0, 0, 5, 3), ChartType::Column);
        sp.first_col_header = Some(false);
        sp.first_row_header = Some(true);
        let d = extract_chart_data(&ws, &sp);
        // 数据从 row1 起（去表头行），列从 col0 起（不去标签列）→ 3 系列。
        assert_eq!(d.series.len(), 3);
        // 首列（原标签列）此时作系列，名取表头行 col0（空）→ 合成名 "列0"。
        assert_eq!(d.series[0].name, "列0");
        // 其值 = 标签文本 A/B/C/D → 非数 → 0。
        assert_eq!(d.series[0].values, vec![0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn title_set_only_when_present_and_nonempty() {
        let ws = typical();
        let mut sp = spec(RegionRect::new(0, 0, 5, 3), ChartType::Pie);
        assert_eq!(extract_chart_data(&ws, &sp).title, None);
        sp.title = Some(String::new());
        assert_eq!(extract_chart_data(&ws, &sp).title, None); // 空串不设
        sp.title = Some("销售".to_string());
        assert_eq!(extract_chart_data(&ws, &sp).title, Some("销售".to_string()));
    }

    #[test]
    fn bool_values_coerce_like_js() {
        let mut ws = Worksheet::with_size("S", 20, 10);
        ws.set_value(0, 1, Some("S".into()));
        ws.set_value(1, 0, Some("r".into()));
        ws.set_value(1, 1, Some(true.into())); // Number(true)=1
        ws.set_value(2, 0, Some("r2".into()));
        ws.set_value(2, 1, Some(false.into())); // Number(false)=0
        let d = extract_chart_data(&ws, &spec(RegionRect::new(0, 0, 3, 2), ChartType::Column));
        assert_eq!(d.series[0].values, vec![1.0, 0.0]);
    }
}
