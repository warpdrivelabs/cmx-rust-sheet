//! 公式值类型、错误值、强制转换。formula 层求值的基础（M3）。
//!
//! 对标 cmx-megasheet 的 formula/value.ts。TS 用 `number|string|boolean|FormulaError|null`
//! 的联合（错误是特殊字符串）；Rust 用**独立 enum**：`FormulaValue` 与 `FormulaError`
//! 各自成型，`FormulaValue::Error(FormulaError)` 承载错误——类型安全，杜绝「错误串被当普通
//! 文本」的隐患。强制转换语义严格对齐 Excel（文本数字→数字、布尔→1/0、空→0…）。

use std::fmt;

/// Excel 风格错误值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormulaError {
    Div0,
    Value,
    Ref,
    Name,
    Num,
    Na,
    /// 动态数组溢出受阻（M8 预留）。
    Spill,
    /// 循环引用（对齐后端 REF 三色环检测语义）。
    Circ,
}

impl FormulaError {
    /// Excel 显示串（`#DIV/0!` 等）。
    pub fn as_str(&self) -> &'static str {
        match self {
            FormulaError::Div0 => "#DIV/0!",
            FormulaError::Value => "#VALUE!",
            FormulaError::Ref => "#REF!",
            FormulaError::Name => "#NAME?",
            FormulaError::Num => "#NUM!",
            FormulaError::Na => "#N/A",
            FormulaError::Spill => "#SPILL!",
            FormulaError::Circ => "#CIRC!",
        }
    }

    /// 从显示串解析（快照/取数值表回读用）。非错误串返回 None。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<FormulaError> {
        match s {
            "#DIV/0!" => Some(FormulaError::Div0),
            "#VALUE!" => Some(FormulaError::Value),
            "#REF!" => Some(FormulaError::Ref),
            "#NAME?" => Some(FormulaError::Name),
            "#NUM!" => Some(FormulaError::Num),
            "#N/A" => Some(FormulaError::Na),
            "#SPILL!" => Some(FormulaError::Spill),
            "#CIRC!" => Some(FormulaError::Circ),
            _ => None,
        }
    }
}

impl fmt::Display for FormulaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 公式值：数字 / 文本 / 布尔 / 错误 / 空。对标 TS `FormulaValue`。
#[derive(Debug, Clone, PartialEq)]
pub enum FormulaValue {
    Number(f64),
    Text(String),
    Bool(bool),
    Error(FormulaError),
    /// 空格（null）。
    Blank,
}

impl FormulaValue {
    /// 是否错误值。
    pub fn is_error(&self) -> bool {
        matches!(self, FormulaValue::Error(_))
    }

    /// 取错误（若是）。
    pub fn as_error(&self) -> Option<FormulaError> {
        match self {
            FormulaValue::Error(e) => Some(*e),
            _ => None,
        }
    }

    /// 空值判定（Blank 或空串）。
    pub fn is_blank(&self) -> bool {
        match self {
            FormulaValue::Blank => true,
            FormulaValue::Text(s) => s.is_empty(),
            _ => false,
        }
    }
}

impl From<f64> for FormulaValue {
    fn from(v: f64) -> Self {
        FormulaValue::Number(v)
    }
}
impl From<i64> for FormulaValue {
    fn from(v: i64) -> Self {
        FormulaValue::Number(v as f64)
    }
}
impl From<bool> for FormulaValue {
    fn from(v: bool) -> Self {
        FormulaValue::Bool(v)
    }
}
impl From<&str> for FormulaValue {
    fn from(v: &str) -> Self {
        FormulaValue::Text(v.to_string())
    }
}
impl From<String> for FormulaValue {
    fn from(v: String) -> Self {
        FormulaValue::Text(v)
    }
}
impl From<FormulaError> for FormulaValue {
    fn from(e: FormulaError) -> Self {
        FormulaValue::Error(e)
    }
}

/// 强制为数字（算术上下文）。错误透传；非数值文本→#VALUE!；空→0；布尔→1/0。
pub fn to_number(v: &FormulaValue) -> Result<f64, FormulaError> {
    match v {
        FormulaValue::Error(e) => Err(*e),
        FormulaValue::Blank => Ok(0.0),
        FormulaValue::Number(n) => {
            if n.is_finite() {
                Ok(*n)
            } else {
                Err(FormulaError::Num)
            }
        }
        FormulaValue::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        FormulaValue::Text(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(0.0);
            }
            match parse_excel_number(t) {
                Some(n) if n.is_finite() => Ok(n),
                _ => Err(FormulaError::Value),
            }
        }
    }
}

/// 强制为文本（连接上下文）。错误透传。
pub fn to_text(v: &FormulaValue) -> Result<String, FormulaError> {
    match v {
        FormulaValue::Error(e) => Err(*e),
        FormulaValue::Blank => Ok(String::new()),
        FormulaValue::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_string()),
        FormulaValue::Number(n) => Ok(number_to_text(*n)),
        FormulaValue::Text(s) => Ok(s.clone()),
    }
}

/// 强制为布尔（逻辑上下文）。错误透传；数字非0→true；"TRUE"/"FALSE" 不敏感；空→false。
pub fn to_boolean(v: &FormulaValue) -> Result<bool, FormulaError> {
    match v {
        FormulaValue::Error(e) => Err(*e),
        FormulaValue::Blank => Ok(false),
        FormulaValue::Bool(b) => Ok(*b),
        FormulaValue::Number(n) => Ok(*n != 0.0),
        FormulaValue::Text(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(false);
            }
            let up = t.to_uppercase();
            if up == "TRUE" {
                return Ok(true);
            }
            if up == "FALSE" {
                return Ok(false);
            }
            match parse_excel_number(t) {
                Some(n) if n.is_finite() => Ok(n != 0.0),
                _ => Err(FormulaError::Value),
            }
        }
    }
}

/// 数字→显示文本（去尾零，限 15 位有效数字，对齐 Excel 精度 + JS Number.toString）。
pub fn number_to_text(n: f64) -> String {
    if !n.is_finite() {
        return if n.is_nan() {
            "NaN".to_string()
        } else if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if n == n.trunc() && n.abs() < 1e21 {
        return sheet_core::numstr::num_to_string(n);
    }
    // 限 15 位有效数字（Excel），再走 JS 风格 toString 去尾零
    let rounded = round_to_precision(n, 15);
    sheet_core::numstr::num_to_string(rounded)
}

/// 保留 sig 位有效数字（对齐 JS Number.prototype.toPrecision 的数值语义）。
fn round_to_precision(n: f64, sig: u32) -> f64 {
    if n == 0.0 {
        return 0.0;
    }
    let d = (sig as i32) - 1 - n.abs().log10().floor() as i32;
    let factor = 10f64.powi(d);
    (n * factor).round() / factor
}

/// 解析 Excel/JS 语义的数字文本。对齐 JS `Number(s)`：接受前后空白、科学计数、
/// 前导正负号、"Infinity"；空串已在调用方处理。返回 None 表示非数字。
fn parse_excel_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // Rust f64::parse 已覆盖 12 / 1.5e3 / .5 / -3 / +2 / inf。JS "Infinity" 特判。
    match t {
        "Infinity" | "+Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ => t.parse::<f64>().ok(),
    }
}

/// 比较两个值（=, <>, <, >, <=, >=）。返回布尔或错误。
/// 数字按数值；文本按字典序（不区分大小写，对齐 Excel）；类型不同按 数字 < 文本 < 布尔。
pub fn compare_values(a: &FormulaValue, b: &FormulaValue, op: &str) -> Result<bool, FormulaError> {
    if let FormulaValue::Error(e) = a {
        return Err(*e);
    }
    if let FormulaValue::Error(e) = b {
        return Err(*e);
    }
    let cmp = raw_compare(a, b);
    match op {
        "=" => Ok(cmp == 0),
        "<>" => Ok(cmp != 0),
        "<" => Ok(cmp < 0),
        ">" => Ok(cmp > 0),
        "<=" => Ok(cmp <= 0),
        ">=" => Ok(cmp >= 0),
        _ => Err(FormulaError::Value),
    }
}

fn type_rank(v: &FormulaValue) -> u8 {
    match v {
        FormulaValue::Blank => 0,
        FormulaValue::Number(_) => 0,
        FormulaValue::Text(s) if s.is_empty() => 0,
        FormulaValue::Text(_) => 1,
        FormulaValue::Bool(_) => 2,
        FormulaValue::Error(_) => 3,
    }
}

fn raw_compare(a: &FormulaValue, b: &FormulaValue) -> i32 {
    let ra = type_rank(a);
    let rb = type_rank(b);
    if ra != rb {
        return if ra < rb { -1 } else { 1 };
    }
    match ra {
        0 => {
            let na = numeric_of(a);
            let nb = numeric_of(b);
            if na < nb {
                -1
            } else if na > nb {
                1
            } else {
                0
            }
        }
        1 => {
            let sa = text_of(a).to_uppercase();
            let sb = text_of(b).to_uppercase();
            match sa.cmp(&sb) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Greater => 1,
                std::cmp::Ordering::Equal => 0,
            }
        }
        _ => {
            let ba = matches!(a, FormulaValue::Bool(true)) as i32;
            let bb = matches!(b, FormulaValue::Bool(true)) as i32;
            ba - bb
        }
    }
}

fn numeric_of(v: &FormulaValue) -> f64 {
    match v {
        FormulaValue::Number(n) => *n,
        _ => 0.0, // Blank / 空串 当作 0 域
    }
}

fn text_of(v: &FormulaValue) -> String {
    match v {
        FormulaValue::Text(s) => s.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_number_coercions() {
        assert_eq!(to_number(&"42".into()), Ok(42.0));
        assert_eq!(to_number(&FormulaValue::Bool(true)), Ok(1.0));
        assert_eq!(to_number(&"".into()), Ok(0.0));
        assert_eq!(to_number(&"abc".into()), Err(FormulaError::Value));
    }

    #[test]
    fn to_text_coercions() {
        assert_eq!(to_text(&FormulaValue::Number(42.0)), Ok("42".to_string()));
        assert_eq!(to_text(&FormulaValue::Bool(true)), Ok("TRUE".to_string()));
        assert_eq!(to_text(&FormulaValue::Number(1.5)), Ok("1.5".to_string()));
    }

    #[test]
    fn to_boolean_coercions() {
        assert_eq!(to_boolean(&FormulaValue::Number(0.0)), Ok(false));
        assert_eq!(to_boolean(&FormulaValue::Number(3.0)), Ok(true));
        assert_eq!(to_boolean(&"TRUE".into()), Ok(true));
    }

    #[test]
    fn compare_basic() {
        assert_eq!(compare_values(&1.0.into(), &2.0.into(), "<"), Ok(true));
        assert_eq!(compare_values(&"a".into(), &"b".into(), "<"), Ok(true));
        assert_eq!(compare_values(&5.0.into(), &5.0.into(), "="), Ok(true));
    }

    #[test]
    fn number_text_formatting() {
        assert_eq!(number_to_text(42.0), "42");
        assert_eq!(number_to_text(1.5), "1.5");
        assert_eq!(number_to_text(-0.0), "0");
        assert_eq!(number_to_text(1234.5), "1234.5");
    }

    #[test]
    fn error_round_trips() {
        for e in [
            FormulaError::Div0,
            FormulaError::Value,
            FormulaError::Ref,
            FormulaError::Name,
            FormulaError::Num,
            FormulaError::Na,
            FormulaError::Circ,
        ] {
            assert_eq!(FormulaError::from_str(e.as_str()), Some(e));
        }
    }
}
