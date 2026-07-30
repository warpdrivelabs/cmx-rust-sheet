//! 自动填充序列推断（M10）。纯逻辑，零 DOM。对标 cmx-megasheet 的 FillEngine.ts。
//!
//! 输入一列/一行「源值」，输出填充到目标长度的 CellData 序列。推断策略（对齐 Excel）：
//!  - 纯数字等差：1,2 → 3,4,5…（步长=末两项差；单值默认步长 1）。
//!  - 内置序列：星期（Mon..Sun / 中文）、月份（Jan..Dec）循环推进。
//!  - 前缀+数字文本：Item1,Item2 → Item3…。
//!  - 公式：按行/列偏移平移相对引用（用 formula_ref::translate_formula）。
//!  - 其余：循环复制源序列。

use crate::cell::{CellData, CellValue};
use crate::formula_ref::translate_formula;

const WEEKDAYS_EN: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];
const WEEKDAYS_EN_ABBR: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const MONTHS_EN: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const MONTHS_EN_ABBR: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS_CN: [&str; 7] = [
    "星期日",
    "星期一",
    "星期二",
    "星期三",
    "星期四",
    "星期五",
    "星期六",
];
const WEEKDAYS_CN2: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

/// 填充方向（用于公式引用平移的偏移量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillAxis {
    Down,
    Up,
    Right,
    Left,
}

/// 推断填充序列：source 为源单元格数据，count 为目标格数，axis 决定公式平移方向。
/// 返回长度 count 的 CellData 数组。copy_only=true 时纯复制不递增。
pub fn infer_fill(
    source: &[CellData],
    count: usize,
    axis: FillAxis,
    copy_only: bool,
) -> Vec<CellData> {
    if source.is_empty() || count == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(count);

    // 公式源：按平移量生成
    if source.iter().any(|s| s.formula.is_some()) {
        for i in 0..count {
            let src = &source[i % source.len()];
            let seq_pos = source.len() + i;
            let src_pos = i % source.len();
            let delta = (seq_pos - src_pos) as i64;
            out.push(shift_formula_data(src, axis, delta));
        }
        return out;
    }

    // 纯数字等差
    if !copy_only
        && source
            .iter()
            .all(|s| matches!(s.value, Some(CellValue::Number(_))))
    {
        let nums: Vec<f64> = source
            .iter()
            .map(|s| s.value.as_ref().unwrap().as_number().unwrap())
            .collect();
        let step = if nums.len() >= 2 {
            nums[nums.len() - 1] - nums[nums.len() - 2]
        } else {
            1.0
        };
        let mut last = nums[nums.len() - 1];
        let style_src = &source[source.len() - 1];
        for _ in 0..count {
            last += step;
            out.push(mk_cell(Some(CellValue::Number(last)), style_src));
        }
        return out;
    }

    // 内置序列 / 前缀+数字
    if !copy_only {
        if let Some(seq) = try_sequence(source) {
            let style_src = &source[source.len() - 1];
            for i in 0..count {
                out.push(mk_cell(Some(CellValue::Text(seq(i as i64 + 1))), style_src));
            }
            return out;
        }
    }

    // 兜底：循环复制源
    for i in 0..count {
        let src = &source[i % source.len()];
        out.push(mk_cell(src.value.clone(), src));
    }
    out
}

/// 造 CellData：带 value + 复制源样式。
fn mk_cell(value: Option<CellValue>, style_src: &CellData) -> CellData {
    CellData {
        value,
        formula: None,
        style: style_src.style.clone(),
        rich: None,
    }
}

/// 尝试识别内置序列/前缀数字，返回「相对末项 +step 步的值」闭包；不匹配返回 None。
fn try_sequence(source: &[CellData]) -> Option<Box<dyn Fn(i64) -> String>> {
    let last_val = source.last()?.value.as_ref()?;
    let CellValue::Text(s) = last_val else {
        return None;
    };
    let last = s.trim().to_string();

    // 星期/月份循环
    if let Some(cyc) = match_cyclic(&last) {
        return Some(Box::new(move |step| format_cyclic(&cyc, step)));
    }

    // 前缀 + 尾数字：Item1 → Item2
    let (prefix, digits) = split_trailing_digits(&last)?;
    let start: i64 = digits.parse().ok()?;
    let width = digits.len();
    // 若源多项，用末两项差作步长
    let mut step_delta = 1i64;
    if source.len() >= 2 {
        if let Some(CellValue::Text(prev)) = source[source.len() - 2].value.as_ref() {
            if let Some((pp, pd)) = split_trailing_digits(prev.trim()) {
                if pp == prefix {
                    if let Ok(pn) = pd.parse::<i64>() {
                        step_delta = start - pn;
                    }
                }
            }
        }
    }
    let prefix = prefix.to_string();
    Some(Box::new(move |step| {
        let n = start + step_delta * step;
        format!("{}{:0width$}", prefix, n, width = width)
    }))
}

/// 拆前缀 + 尾部数字段（"Item12" → ("Item","12")）；无尾数字返回 None。
fn split_trailing_digits(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == bytes.len() {
        return None; // 无尾数字
    }
    Some((&s[..i], &s[i..]))
}

#[derive(Clone)]
struct Cyclic {
    list: &'static [&'static str],
    index: usize,
    original: String,
}

fn match_cyclic(s: &str) -> Option<Cyclic> {
    let lower = s.to_lowercase();
    let en_tables: [&'static [&'static str]; 4] =
        [&WEEKDAYS_EN, &WEEKDAYS_EN_ABBR, &MONTHS_EN, &MONTHS_EN_ABBR];
    for t in en_tables {
        if let Some(idx) = t.iter().position(|&x| x == lower) {
            return Some(Cyclic {
                list: t,
                index: idx,
                original: s.to_string(),
            });
        }
    }
    let cn_tables: [&'static [&'static str]; 2] = [&WEEKDAYS_CN, &WEEKDAYS_CN2];
    for t in cn_tables {
        if let Some(idx) = t.iter().position(|&x| x == s) {
            return Some(Cyclic {
                list: t,
                index: idx,
                original: s.to_string(),
            });
        }
    }
    None
}

/// 循环序列第 step 步的显示值（英文跟随源大小写风格）。
fn format_cyclic(cyc: &Cyclic, step: i64) -> String {
    let len = cyc.list.len() as i64;
    let idx = (((cyc.index as i64 + step) % len + len) % len) as usize;
    let base = cyc.list[idx];
    // 英文：跟随源大小写
    if cyc.original.chars().all(|c| c.is_ascii_alphabetic()) {
        if cyc.original == cyc.original.to_uppercase() {
            return base.to_uppercase();
        }
        if cyc
            .original
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        {
            let mut chars = base.chars();
            if let Some(first) = chars.next() {
                return first.to_uppercase().collect::<String>() + chars.as_str();
            }
        }
        return base.to_string();
    }
    base.to_string() // 中文原样
}

fn shift_formula_data(src: &CellData, axis: FillAxis, delta: i64) -> CellData {
    let Some(f) = &src.formula else {
        return mk_cell(src.value.clone(), src);
    };
    let (d_row, d_col) = match axis {
        FillAxis::Down => (delta, 0),
        FillAxis::Up => (-delta, 0),
        FillAxis::Right => (0, delta),
        FillAxis::Left => (0, -delta),
    };
    CellData {
        value: None,
        formula: Some(translate_formula(f, d_row, d_col)),
        style: src.style.clone(),
        rich: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_v(v: CellValue) -> CellData {
        CellData {
            value: Some(v),
            ..Default::default()
        }
    }
    fn cell_f(f: &str) -> CellData {
        CellData {
            formula: Some(f.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn number_arithmetic() {
        let out = infer_fill(
            &[cell_v(1.into()), cell_v(2.into())],
            3,
            FillAxis::Down,
            false,
        );
        let vals: Vec<f64> = out
            .iter()
            .map(|c| c.value.as_ref().unwrap().as_number().unwrap())
            .collect();
        assert_eq!(vals, vec![3.0, 4.0, 5.0]);
    }

    #[test]
    fn single_number_step_one() {
        let out = infer_fill(&[cell_v(5.into())], 2, FillAxis::Down, false);
        let vals: Vec<f64> = out
            .iter()
            .map(|c| c.value.as_ref().unwrap().as_number().unwrap())
            .collect();
        assert_eq!(vals, vec![6.0, 7.0]);
    }

    #[test]
    fn weekday_cycle() {
        let out = infer_fill(&[cell_v("Mon".into())], 3, FillAxis::Down, false);
        let vals: Vec<String> = out
            .iter()
            .map(|c| c.value.as_ref().unwrap().to_text())
            .collect();
        assert_eq!(vals, vec!["Tue", "Wed", "Thu"]);
    }

    #[test]
    fn month_series() {
        let out = infer_fill(
            &[cell_v("Jan".into()), cell_v("Feb".into())],
            2,
            FillAxis::Down,
            false,
        );
        let vals: Vec<String> = out
            .iter()
            .map(|c| c.value.as_ref().unwrap().to_text())
            .collect();
        assert_eq!(vals, vec!["Mar", "Apr"]);
    }

    #[test]
    fn prefix_number() {
        let out = infer_fill(
            &[cell_v("Item1".into()), cell_v("Item2".into())],
            2,
            FillAxis::Down,
            false,
        );
        let vals: Vec<String> = out
            .iter()
            .map(|c| c.value.as_ref().unwrap().to_text())
            .collect();
        assert_eq!(vals, vec!["Item3", "Item4"]);
    }

    #[test]
    fn formula_shift_down() {
        let out = infer_fill(&[cell_f("A1+1")], 2, FillAxis::Down, false);
        assert_eq!(out[0].formula.as_deref(), Some("A2+1"));
        assert_eq!(out[1].formula.as_deref(), Some("A3+1"));
    }

    #[test]
    fn formula_shift_right() {
        let out = infer_fill(&[cell_f("A1")], 2, FillAxis::Right, false);
        assert_eq!(out[0].formula.as_deref(), Some("B1"));
        assert_eq!(out[1].formula.as_deref(), Some("C1"));
    }

    #[test]
    fn plain_text_copy_fallback() {
        let out = infer_fill(&[cell_v("hello".into())], 2, FillAxis::Down, false);
        let vals: Vec<String> = out
            .iter()
            .map(|c| c.value.as_ref().unwrap().to_text())
            .collect();
        assert_eq!(vals, vec!["hello", "hello"]);
    }
}
