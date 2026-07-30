//! 条件格式引擎（M13）。纯计算：给 sheet + 规则集，算出每格的渲染叠加。对标 cmx-megasheet
//! 的 render/condFormat.ts。「无渲染」重解读：算叠加值是**计算件**（不画像素），留 sheet-core。
//!
//! 独立规则子系统，不改单元格数据。类型：cellValue（比较运算→套 style）、colorScale（2/3 色
//! 插值背景）、dataBar（值比例 0..1）、iconSet（三分档图标索引）。含公式的规则由调用方预填值。

use std::collections::BTreeMap;

use crate::cell::CellValue;
use crate::style::Style;
use crate::worksheet::{
    CondFormatOperator, CondFormatType, ConditionalRule, IconSet, RegionRect, Worksheet,
};

/// 单格条件格式叠加结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CondFormatOverlay {
    /// 命中的样式叠加（背景/字色/边框）。
    pub style: Option<Style>,
    /// 数据条：填充比例 0..1 + 颜色。
    pub bar: Option<DataBarOverlay>,
    /// 图标集：图标组 + 档位索引（0=最低）。
    pub icon: Option<IconOverlay>,
    /// 色阶：背景色（十六进制）。
    pub fill: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataBarOverlay {
    pub ratio: f64,
    pub color: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IconOverlay {
    pub set: IconSet,
    pub index: u8,
}

/// 计算区域内每格叠加。返回 BTreeMap<(r,c), overlay>（稳定序）。多规则命中同格后加入覆盖。
pub fn evaluate_rules(
    sheet: &Worksheet,
    rules: &[ConditionalRule],
) -> BTreeMap<(u32, u32), CondFormatOverlay> {
    let mut out: BTreeMap<(u32, u32), CondFormatOverlay> = BTreeMap::new();
    for rule in rules {
        match rule.rule_type {
            CondFormatType::CellValue => apply_cell_value(sheet, rule, &mut out),
            CondFormatType::ColorScale => {
                apply_color_scale(rule, &range_numbers(sheet, &rule.range), &mut out)
            }
            CondFormatType::DataBar => {
                apply_data_bar(rule, &range_numbers(sheet, &rule.range), &mut out)
            }
            CondFormatType::IconSet => {
                apply_icon_set(rule, &range_numbers(sheet, &rule.range), &mut out)
            }
        }
    }
    out
}

struct NumCell {
    row: u32,
    col: u32,
    n: f64,
}

/// 收集区域内各格数值（非数值/空跳过）。
fn range_numbers(sheet: &Worksheet, g: &RegionRect) -> Vec<NumCell> {
    let mut out = Vec::new();
    for r in g.row..g.row + g.row_count {
        for c in g.col..g.col + g.col_count {
            if let Some(n) = numeric_of(sheet.get_value(r, c).as_ref()) {
                out.push(NumCell { row: r, col: c, n });
            }
        }
    }
    out
}

fn numeric_of(v: Option<&CellValue>) -> Option<f64> {
    match v {
        Some(CellValue::Number(n)) if n.is_finite() => Some(*n),
        Some(CellValue::Text(s)) if !s.is_empty() => {
            s.parse::<f64>().ok().filter(|n| n.is_finite())
        }
        _ => None,
    }
}

fn cell_text(sheet: &Worksheet, r: u32, c: u32) -> String {
    crate::find::cell_display(sheet.get_value(r, c).as_ref())
}

fn merge_style(out: &mut BTreeMap<(u32, u32), CondFormatOverlay>, k: (u32, u32), style: &Style) {
    let entry = out.entry(k).or_default();
    let merged = crate::style::merge_style(entry.style.as_ref(), Some(style));
    entry.style = Some(merged);
}

// ── cellValue ────────────────────────────────────────────
fn apply_cell_value(
    sheet: &Worksheet,
    rule: &ConditionalRule,
    out: &mut BTreeMap<(u32, u32), CondFormatOverlay>,
) {
    let Some(style) = &rule.style else { return };
    let g = &rule.range;
    let cells = range_numbers(sheet, g);
    // top/bottom 阈值
    let mut top_th = f64::NEG_INFINITY;
    let mut bottom_th = f64::INFINITY;
    if matches!(
        rule.operator,
        Some(CondFormatOperator::Top) | Some(CondFormatOperator::Bottom)
    ) {
        let n = rule
            .value1
            .as_ref()
            .and_then(|v| v.as_number())
            .unwrap_or(10.0)
            .max(1.0) as usize;
        let mut desc: Vec<f64> = cells.iter().map(|c| c.n).collect();
        desc.sort_by(|a, b| b.partial_cmp(a).unwrap());
        if !desc.is_empty() {
            top_th = desc[n.min(desc.len()) - 1];
        }
        let mut asc = desc.clone();
        asc.reverse();
        if !asc.is_empty() {
            bottom_th = asc[n.min(asc.len()) - 1];
        }
    }
    // duplicate/unique 计数
    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    if matches!(
        rule.operator,
        Some(CondFormatOperator::Duplicate) | Some(CondFormatOperator::Unique)
    ) {
        for r in g.row..g.row + g.row_count {
            for c in g.col..g.col + g.col_count {
                let t = cell_text(sheet, r, c);
                if !t.is_empty() {
                    *counts.entry(t).or_insert(0) += 1;
                }
            }
        }
    }
    for r in g.row..g.row + g.row_count {
        for c in g.col..g.col + g.col_count {
            let v = sheet.get_value(r, c);
            let text = cell_text(sheet, r, c);
            if match_cell_value(rule, v.as_ref(), top_th, bottom_th, &counts, &text) {
                merge_style(out, (r, c), style);
            }
        }
    }
}

fn match_cell_value(
    rule: &ConditionalRule,
    v: Option<&CellValue>,
    top_th: f64,
    bottom_th: f64,
    counts: &std::collections::HashMap<String, u32>,
    text: &str,
) -> bool {
    let n = numeric_of(v);
    let v1 = rule.value1.as_ref().and_then(|x| x.as_number());
    let v2 = rule.value2.as_ref().and_then(|x| x.as_number());
    let v1_text = rule
        .value1
        .as_ref()
        .map(|x| x.as_text())
        .unwrap_or_default();
    match rule.operator {
        Some(CondFormatOperator::Gt) => matches!((n, v1), (Some(a), Some(b)) if a > b),
        Some(CondFormatOperator::Ge) => matches!((n, v1), (Some(a), Some(b)) if a >= b),
        Some(CondFormatOperator::Lt) => matches!((n, v1), (Some(a), Some(b)) if a < b),
        Some(CondFormatOperator::Le) => matches!((n, v1), (Some(a), Some(b)) if a <= b),
        Some(CondFormatOperator::Eq) => match (n, v1) {
            (Some(a), Some(b)) => a == b,
            _ => text == v1_text,
        },
        Some(CondFormatOperator::Ne) => match (n, v1) {
            (Some(a), Some(b)) => a != b,
            _ => text != v1_text,
        },
        Some(CondFormatOperator::Between) => {
            matches!((n, v1, v2), (Some(a), Some(lo), Some(hi)) if a >= lo.min(hi) && a <= lo.max(hi))
        }
        Some(CondFormatOperator::NotBetween) => {
            matches!((n, v1, v2), (Some(a), Some(lo), Some(hi)) if a < lo.min(hi) || a > lo.max(hi))
        }
        Some(CondFormatOperator::Contains) => text.to_lowercase().contains(&v1_text.to_lowercase()),
        Some(CondFormatOperator::NotContains) => {
            !text.to_lowercase().contains(&v1_text.to_lowercase())
        }
        Some(CondFormatOperator::Top) => matches!(n, Some(a) if a >= top_th),
        Some(CondFormatOperator::Bottom) => matches!(n, Some(a) if a <= bottom_th),
        Some(CondFormatOperator::Duplicate) => {
            !text.is_empty() && counts.get(text).copied().unwrap_or(0) > 1
        }
        Some(CondFormatOperator::Unique) => {
            !text.is_empty() && counts.get(text).copied().unwrap_or(0) == 1
        }
        None => false,
    }
}

// ── colorScale ───────────────────────────────────────────
fn apply_color_scale(
    rule: &ConditionalRule,
    cells: &[NumCell],
    out: &mut BTreeMap<(u32, u32), CondFormatOverlay>,
) {
    if cells.is_empty() {
        return;
    }
    let default_colors = vec![
        "#f8696b".to_string(),
        "#ffeb84".to_string(),
        "#63be7b".to_string(),
    ];
    let colors = rule.colors.clone().unwrap_or(default_colors);
    let min = cells.iter().map(|c| c.n).fold(f64::INFINITY, f64::min);
    let max = cells.iter().map(|c| c.n).fold(f64::NEG_INFINITY, f64::max);
    let range = if max - min == 0.0 { 1.0 } else { max - min };
    for c in cells {
        let t = (c.n - min) / range;
        let fill = if colors.len() >= 3 {
            interpolate3(&colors[0], &colors[1], &colors[2], t)
        } else {
            interpolate2(&colors[0], &colors[colors.len() - 1], t)
        };
        out.entry((c.row, c.col)).or_default().fill = Some(fill);
    }
}

// ── dataBar ──────────────────────────────────────────────
fn apply_data_bar(
    rule: &ConditionalRule,
    cells: &[NumCell],
    out: &mut BTreeMap<(u32, u32), CondFormatOverlay>,
) {
    if cells.is_empty() {
        return;
    }
    let min = cells.iter().map(|c| c.n).fold(0.0, f64::min);
    let max = cells.iter().map(|c| c.n).fold(f64::NEG_INFINITY, f64::max);
    let range = if max - min == 0.0 { 1.0 } else { max - min };
    let color = rule
        .bar_color
        .clone()
        .unwrap_or_else(|| "#638ec6".to_string());
    for c in cells {
        let ratio = ((c.n - min) / range).clamp(0.0, 1.0);
        out.entry((c.row, c.col)).or_default().bar = Some(DataBarOverlay {
            ratio,
            color: color.clone(),
        });
    }
}

// ── iconSet ──────────────────────────────────────────────
fn apply_icon_set(
    rule: &ConditionalRule,
    cells: &[NumCell],
    out: &mut BTreeMap<(u32, u32), CondFormatOverlay>,
) {
    if cells.is_empty() {
        return;
    }
    let set = rule.icon_set.unwrap_or(IconSet::Arrows);
    let mut nums: Vec<f64> = cells.iter().map(|c| c.n).collect();
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let t1 = nums[nums.len() / 3];
    let t2 = nums[(2 * nums.len()) / 3];
    for c in cells {
        let index: u8 = if c.n >= t2 {
            2
        } else if c.n >= t1 {
            1
        } else {
            0
        };
        out.entry((c.row, c.col)).or_default().icon = Some(IconOverlay { set, index });
    }
}

// ── 颜色插值 ─────────────────────────────────────────────
fn interpolate2(a: &str, b: &str, t: f64) -> String {
    let ca = hex_to_rgb(a);
    let cb = hex_to_rgb(b);
    rgb_to_hex(
        lerp(ca.0, cb.0, t),
        lerp(ca.1, cb.1, t),
        lerp(ca.2, cb.2, t),
    )
}

fn interpolate3(a: &str, mid: &str, b: &str, t: f64) -> String {
    if t <= 0.5 {
        interpolate2(a, mid, t / 0.5)
    } else {
        interpolate2(mid, b, (t - 0.5) / 0.5)
    }
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// 十六进制颜色 → RGB（支持 #rgb / #rrggbb）。
pub fn hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let full = if h.len() == 3 {
        h.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        h.to_string()
    };
    let parse = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0);
    if full.len() >= 6 {
        (parse(&full[0..2]), parse(&full[2..4]), parse(&full[4..6]))
    } else {
        (0, 0, 0)
    }
}

fn rgb_to_hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worksheet::{CondValue, Worksheet};

    fn data_sheet() -> Worksheet {
        let mut ws = Worksheet::with_size("S", 10, 6);
        for (i, v) in [10, 20, 30, 40, 50].iter().enumerate() {
            ws.set_value(i as u32, 0, Some((*v as i64).into()));
        }
        ws
    }

    fn rule(rt: CondFormatType) -> ConditionalRule {
        ConditionalRule {
            range: RegionRect::new(0, 0, 5, 1),
            rule_type: rt,
            operator: None,
            value1: None,
            value2: None,
            style: None,
            colors: None,
            bar_color: None,
            icon_set: None,
        }
    }

    #[test]
    fn cell_value_gt() {
        let ws = data_sheet();
        let mut r = rule(CondFormatType::CellValue);
        r.operator = Some(CondFormatOperator::Gt);
        r.value1 = Some(CondValue::Number(25.0));
        r.style = Some(Style {
            back_color: Some("#ff0000".into()),
            ..Default::default()
        });
        let o = evaluate_rules(&ws, &[r]);
        assert!(o.get(&(0, 0)).and_then(|x| x.style.as_ref()).is_none()); // 10<25
        assert_eq!(
            o[&(2, 0)].style.as_ref().unwrap().back_color.as_deref(),
            Some("#ff0000")
        );
        assert_eq!(
            o[&(4, 0)].style.as_ref().unwrap().back_color.as_deref(),
            Some("#ff0000")
        );
    }

    #[test]
    fn cell_value_between() {
        let ws = data_sheet();
        let mut r = rule(CondFormatType::CellValue);
        r.operator = Some(CondFormatOperator::Between);
        r.value1 = Some(CondValue::Number(20.0));
        r.value2 = Some(CondValue::Number(40.0));
        r.style = Some(Style {
            bold: Some(true),
            ..Default::default()
        });
        let o = evaluate_rules(&ws, &[r]);
        assert!(o.get(&(0, 0)).and_then(|x| x.style.as_ref()).is_none());
        assert_eq!(o[&(1, 0)].style.as_ref().unwrap().bold, Some(true));
        assert_eq!(o[&(3, 0)].style.as_ref().unwrap().bold, Some(true));
        assert!(o.get(&(4, 0)).and_then(|x| x.style.as_ref()).is_none());
    }

    #[test]
    fn top_n() {
        let ws = data_sheet();
        let mut r = rule(CondFormatType::CellValue);
        r.operator = Some(CondFormatOperator::Top);
        r.value1 = Some(CondValue::Number(2.0));
        r.style = Some(Style {
            back_color: Some("#0f0".into()),
            ..Default::default()
        });
        let o = evaluate_rules(&ws, &[r]);
        assert_eq!(
            o[&(4, 0)].style.as_ref().unwrap().back_color.as_deref(),
            Some("#0f0")
        );
        assert_eq!(
            o[&(3, 0)].style.as_ref().unwrap().back_color.as_deref(),
            Some("#0f0")
        );
        assert!(o.get(&(2, 0)).and_then(|x| x.style.as_ref()).is_none());
    }

    #[test]
    fn duplicate() {
        let mut ws = Worksheet::with_size("S", 10, 6);
        ws.set_value(0, 0, Some("a".into()));
        ws.set_value(1, 0, Some("b".into()));
        ws.set_value(2, 0, Some("a".into()));
        let mut r = ConditionalRule {
            range: RegionRect::new(0, 0, 3, 1),
            rule_type: CondFormatType::CellValue,
            operator: Some(CondFormatOperator::Duplicate),
            value1: None,
            value2: None,
            style: Some(Style {
                back_color: Some("#ff0".into()),
                ..Default::default()
            }),
            colors: None,
            bar_color: None,
            icon_set: None,
        };
        r.operator = Some(CondFormatOperator::Duplicate);
        let o = evaluate_rules(&ws, &[r]);
        assert_eq!(
            o[&(0, 0)].style.as_ref().unwrap().back_color.as_deref(),
            Some("#ff0")
        );
        assert_eq!(
            o[&(2, 0)].style.as_ref().unwrap().back_color.as_deref(),
            Some("#ff0")
        );
        assert!(o.get(&(1, 0)).and_then(|x| x.style.as_ref()).is_none());
    }

    #[test]
    fn color_scale_3() {
        let ws = data_sheet();
        let mut r = rule(CondFormatType::ColorScale);
        r.colors = Some(vec!["#ff0000".into(), "#ffff00".into(), "#00ff00".into()]);
        let o = evaluate_rules(&ws, &[r]);
        assert_eq!(o[&(0, 0)].fill.as_deref(), Some("#ff0000"));
        assert_eq!(o[&(4, 0)].fill.as_deref(), Some("#00ff00"));
        assert_eq!(o[&(2, 0)].fill.as_deref(), Some("#ffff00"));
    }

    #[test]
    fn data_bar_ratio() {
        let ws = data_sheet();
        let r = rule(CondFormatType::DataBar);
        let o = evaluate_rules(&ws, &[r]);
        assert!((o[&(0, 0)].bar.as_ref().unwrap().ratio - 0.2).abs() < 1e-5);
        assert!((o[&(4, 0)].bar.as_ref().unwrap().ratio - 1.0).abs() < 1e-5);
    }

    #[test]
    fn icon_set_thirds() {
        let ws = data_sheet();
        let mut r = rule(CondFormatType::IconSet);
        r.icon_set = Some(IconSet::Arrows);
        let o = evaluate_rules(&ws, &[r]);
        assert_eq!(o[&(0, 0)].icon.as_ref().unwrap().index, 0);
        assert_eq!(o[&(4, 0)].icon.as_ref().unwrap().index, 2);
    }
}
