//! 单元格数据记录（值 · 公式 · 样式 · 富文本）。
//!
//! 稀疏存储的槽位内容，对标 cmx-megasheet 的 Cell.ts。设计取舍：
//!  - value 与 formula 并存：formula 为公式源串（不含前导 '='），value 为最近算得的显示值。
//!  - 富文本（M13）与标量 value 并存——value 存纯文本兜底。

use serde::{Deserialize, Serialize};

use crate::style::Style;

/// 单元格可承载的原始值类型。对标 TS `CellValue = string|number|boolean|null`。
/// serde：untagged 反序列化对齐 TS JSON 的裸标量；序列化自定义（见下）以对齐 JS 数字文法
/// （整值 f64 输出无 `.0`，如 `620000` 而非 `620000.0`）——RS-M4 字节级 parity 关键。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum CellValue {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Serialize for CellValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CellValue::Bool(b) => s.serialize_bool(*b),
            CellValue::Text(t) => s.serialize_str(t),
            // 整值且在 JS 安全整数区间内 → 输出整数（无 `.0`），对齐 JSON.stringify。
            CellValue::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                    s.serialize_i64(*n as i64)
                } else {
                    s.serialize_f64(*n)
                }
            }
        }
    }
}

impl CellValue {
    /// 数值（非数值返回 None）。
    pub fn as_number(&self) -> Option<f64> {
        match self {
            CellValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// 显示/比较用文本（对齐 TS `String(v)` 语义）。
    pub fn to_text(&self) -> String {
        match self {
            CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            CellValue::Number(n) => crate::numstr::num_to_string(*n),
            CellValue::Text(s) => s.clone(),
        }
    }
}

impl From<f64> for CellValue {
    fn from(v: f64) -> Self {
        CellValue::Number(v)
    }
}
impl From<i64> for CellValue {
    fn from(v: i64) -> Self {
        CellValue::Number(v as f64)
    }
}
impl From<bool> for CellValue {
    fn from(v: bool) -> Self {
        CellValue::Bool(v)
    }
}
impl From<&str> for CellValue {
    fn from(v: &str) -> Self {
        CellValue::Text(v.to_string())
    }
}
impl From<String> for CellValue {
    fn from(v: String) -> Self {
        CellValue::Text(v)
    }
}

/// 富文本片段（M13）：一段文本 + 可选局部字体样式（覆盖单元格样式）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RichRun {
    pub text: String,
    /// 片段级字体样式（bold/italic/underline/fontSize/fontFamily/foreColor 子集）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub font: Option<RichFont>,
}

/// 富文本 run 的字体子集（对齐 TS `Pick<StyleProps,...>`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RichFont {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub italic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub underline: Option<bool>,
    #[serde(rename = "fontSize", skip_serializing_if = "Option::is_none", default)]
    pub font_size: Option<f64>,
    #[serde(
        rename = "fontFamily",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub font_family: Option<String>,
    #[serde(rename = "foreColor", skip_serializing_if = "Option::is_none", default)]
    pub fore_color: Option<String>,
}

/// 富文本（M13）：格内混合字体/色的文本段序列。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RichText {
    pub runs: Vec<RichRun>,
}

impl RichText {
    /// 富文本 → 纯文本（拼接各 run，供 value 兜底/查找/排序/TSV）。
    pub fn to_plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// 单元格数据：值/公式/样式/富文本，皆可选（稀疏）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CellData {
    pub value: Option<CellValue>,
    /// 公式源串，不含前导 '='（"SUM(A1:A2)"）。
    pub formula: Option<String>,
    pub style: Option<Style>,
    pub rich: Option<RichText>,
}

impl CellData {
    /// 是否空壳（无 value/formula/rich，且 style 为空）——用于稀疏剪枝。
    pub fn is_blank(&self) -> bool {
        self.value.is_none()
            && self.formula.as_ref().is_none_or(|f| f.is_empty())
            && self.rich.is_none()
            && self.style.as_ref().is_none_or(|s| s.is_empty())
    }
}

/// 公式串归一：剥去前导 '='，trim。空→""。对齐 TS normalizeFormula。
pub fn normalize_formula(formula: &str) -> String {
    let f = formula.trim();
    if f.is_empty() {
        return String::new();
    }
    if let Some(stripped) = f.strip_prefix('=') {
        stripped.trim().to_string()
    } else {
        f.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_value_conversions() {
        assert_eq!(CellValue::from(42.0), CellValue::Number(42.0));
        assert_eq!(CellValue::from("x"), CellValue::Text("x".into()));
        assert_eq!(CellValue::from(true), CellValue::Bool(true));
    }

    #[test]
    fn cell_value_to_text() {
        assert_eq!(CellValue::Bool(true).to_text(), "TRUE");
        assert_eq!(CellValue::Bool(false).to_text(), "FALSE");
        assert_eq!(CellValue::Text("hi".into()).to_text(), "hi");
        assert_eq!(CellValue::Number(42.0).to_text(), "42");
    }

    #[test]
    fn normalize_formula_strips_equals() {
        assert_eq!(normalize_formula("=SUM(A1:A2)"), "SUM(A1:A2)");
        assert_eq!(normalize_formula("  =A1 "), "A1");
    }

    #[test]
    fn normalize_formula_bare() {
        assert_eq!(normalize_formula("SUM(A1)"), "SUM(A1)");
    }

    #[test]
    fn normalize_formula_empty() {
        assert_eq!(normalize_formula(""), "");
    }

    #[test]
    fn rich_to_plain() {
        let rt = RichText {
            runs: vec![
                RichRun {
                    text: "Hello ".into(),
                    font: None,
                },
                RichRun {
                    text: "World".into(),
                    font: Some(RichFont {
                        bold: Some(true),
                        ..Default::default()
                    }),
                },
            ],
        };
        assert_eq!(rt.to_plain(), "Hello World");
    }

    #[test]
    fn cell_value_untagged_json() {
        // 整值数字输出无 `.0`（对齐 JS JSON.stringify）；文本/布尔裸标量
        assert_eq!(
            serde_json::to_string(&CellValue::Number(42.0)).unwrap(),
            "42"
        );
        assert_eq!(
            serde_json::to_string(&CellValue::Number(1.5)).unwrap(),
            "1.5"
        );
        assert_eq!(
            serde_json::to_string(&CellValue::Text("hi".into())).unwrap(),
            "\"hi\""
        );
        assert_eq!(
            serde_json::to_string(&CellValue::Bool(true)).unwrap(),
            "true"
        );
        // 反序列化：裸标量 → 正确变体
        assert_eq!(
            serde_json::from_str::<CellValue>("42").unwrap(),
            CellValue::Number(42.0)
        );
        assert_eq!(
            serde_json::from_str::<CellValue>("\"hi\"").unwrap(),
            CellValue::Text("hi".into())
        );
    }
}
