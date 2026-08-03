//! 内置函数注册表（M3）。对标 cmx-megasheet 的 formula/functions.ts 的**核心集**
//! （~50 个：聚合/数学/逻辑/文本/信息）。全 272 函数库在 RS-M17 扩容。
//!
//! 分工不变（方案 §5.3）：取数真值由后端算好经 setReportValueMap 下发，本引擎只做聚合层
//! 重算 + 通用函数。QM/QC/JE/FS/REF 在 custom_fn.rs 另注册（volatile）。纯逻辑、零 DOM。
//!
//! Rust 移植取舍：TS 用一个 `Record<string, FunctionImpl>` 字面量；这里每个实现是
//! `Rc<dyn Fn>`（见 evaluator.rs 的 FunctionImpl），在 `builtins()` 里批量构造。
//! TEXT 依赖数字格式引擎——RS-M3 先用聚焦子集（`#,##0.00` 等），RS-M7 全量替换。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::evaluator::{
    first_error, flatten_arg, flatten_args, scalar_arg, EvalContext, EvaluatedArg, FunctionImpl,
    FunctionRegistry,
};
use crate::value::{to_boolean, to_number, to_text, FormulaError, FormulaValue};

/// 收集实参里所有数值（跳过空/纯文本；错误短路）。用于 SUM/AVERAGE/…
pub(crate) fn numeric_values(args: &[EvaluatedArg]) -> Result<Vec<f64>, FormulaError> {
    let mut out = Vec::new();
    for v in flatten_args(args) {
        if let FormulaValue::Error(e) = v {
            return Err(e);
        }
        if v.is_blank() {
            continue;
        }
        match v {
            FormulaValue::Number(n) => out.push(n),
            FormulaValue::Bool(b) => out.push(if b { 1.0 } else { 0.0 }),
            FormulaValue::Text(s) => {
                if let Ok(n) = s.trim().parse::<f64>() {
                    if n.is_finite() {
                        out.push(n);
                    }
                }
                // 非数值文本忽略（对齐 Excel 区域聚合）
            }
            _ => {}
        }
    }
    Ok(out)
}

fn num(v: &FormulaValue) -> Result<f64, FormulaError> {
    to_number(v)
}

/// Excel 半远离零舍入（ROUND 语义）。
pub(crate) fn round_half_away(n: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    let x = n * f;
    let r = if x >= 0.0 {
        (x + 0.5).floor()
    } else {
        (x - 0.5).ceil()
    };
    r / f
}

fn err(e: FormulaError) -> FormulaValue {
    FormulaValue::Error(e)
}

/// 一元数字函数模板：取 arg0 数字，施加 f。
fn unary_num(args: &[EvaluatedArg], f: impl Fn(f64) -> FormulaValue) -> FormulaValue {
    match num(&scalar_arg(args.first())) {
        Ok(n) => f(n),
        Err(e) => err(e),
    }
}

fn two_num(args: &[EvaluatedArg], f: impl Fn(f64, f64) -> FormulaValue) -> FormulaValue {
    let a = match num(&scalar_arg(args.first())) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let b = match num(&scalar_arg(args.get(1))) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    f(a, b)
}

// ── 聚合 ─────────────────────────────────────────────────

fn f_sum(args: &[EvaluatedArg]) -> FormulaValue {
    match numeric_values(args) {
        Ok(ns) => FormulaValue::Number(ns.iter().sum()),
        Err(e) => err(e),
    }
}

fn f_average(args: &[EvaluatedArg]) -> FormulaValue {
    match numeric_values(args) {
        Ok(ns) if ns.is_empty() => err(FormulaError::Div0),
        Ok(ns) => FormulaValue::Number(ns.iter().sum::<f64>() / ns.len() as f64),
        Err(e) => err(e),
    }
}

fn f_count(args: &[EvaluatedArg]) -> FormulaValue {
    let c = flatten_args(args)
        .iter()
        .filter(|v| match v {
            FormulaValue::Number(_) => true,
            FormulaValue::Text(s) => {
                !s.trim().is_empty()
                    && s.trim()
                        .parse::<f64>()
                        .map(|n| n.is_finite())
                        .unwrap_or(false)
            }
            _ => false,
        })
        .count();
    FormulaValue::Number(c as f64)
}

fn f_counta(args: &[EvaluatedArg]) -> FormulaValue {
    let c = flatten_args(args).iter().filter(|v| !v.is_blank()).count();
    FormulaValue::Number(c as f64)
}

fn f_countblank(args: &[EvaluatedArg]) -> FormulaValue {
    let c = flatten_args(args).iter().filter(|v| v.is_blank()).count();
    FormulaValue::Number(c as f64)
}

fn f_max(args: &[EvaluatedArg]) -> FormulaValue {
    match numeric_values(args) {
        Ok(ns) if ns.is_empty() => FormulaValue::Number(0.0),
        Ok(ns) => FormulaValue::Number(ns.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
        Err(e) => err(e),
    }
}

fn f_min(args: &[EvaluatedArg]) -> FormulaValue {
    match numeric_values(args) {
        Ok(ns) if ns.is_empty() => FormulaValue::Number(0.0),
        Ok(ns) => FormulaValue::Number(ns.iter().cloned().fold(f64::INFINITY, f64::min)),
        Err(e) => err(e),
    }
}

fn f_product(args: &[EvaluatedArg]) -> FormulaValue {
    match numeric_values(args) {
        Ok(ns) if ns.is_empty() => FormulaValue::Number(0.0),
        Ok(ns) => FormulaValue::Number(ns.iter().product()),
        Err(e) => err(e),
    }
}

/// 样本/总体方差；transform 用于 STDEV（sqrt）。
fn variance(ns: &[f64], sample: bool, transform: impl Fn(f64) -> f64) -> FormulaValue {
    let n = ns.len();
    if (sample && n < 2) || (!sample && n < 1) {
        return err(FormulaError::Div0);
    }
    let mean = ns.iter().sum::<f64>() / n as f64;
    let ss: f64 = ns.iter().map(|b| (b - mean) * (b - mean)).sum();
    FormulaValue::Number(transform(
        ss / if sample { (n - 1) as f64 } else { n as f64 },
    ))
}

fn stat(args: &[EvaluatedArg], sample: bool, sqrt: bool) -> FormulaValue {
    match numeric_values(args) {
        Ok(ns) => variance(&ns, sample, if sqrt { f64::sqrt } else { |x| x }),
        Err(e) => err(e),
    }
}

// ── 条件聚合 ─────────────────────────────────────────────

/// 通配符（* ?）→ RegExp 源；~ 转义。anchored=true 全串（criteria）。
pub(crate) fn wildcard_to_regex(pattern: &str, anchored: bool) -> regex::Regex {
    let mut out = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '~' {
            if let Some(&nx) = chars.get(i + 1) {
                out.push_str(&regex::escape(&nx.to_string()));
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            _ => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    let src = if anchored {
        format!("(?i)^{out}$")
    } else {
        format!("(?i){out}")
    };
    regex::Regex::new(&src).unwrap_or_else(|_| regex::Regex::new("$^").unwrap())
}

pub(crate) fn matches_criteria(v: &FormulaValue, criteria: &FormulaValue) -> bool {
    if criteria.is_blank() {
        return v.is_blank();
    }
    let cs = to_text(criteria).unwrap_or_default();
    let cs = cs.trim().to_string();
    // 运算符前缀
    let (opr, rhs) = split_operator(&cs);
    let rhs_num = rhs.trim().parse::<f64>().ok();
    let v_num = match v {
        FormulaValue::Number(n) => Some(*n),
        FormulaValue::Text(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    if !opr.is_empty() {
        if let (Some(rn), Some(vn)) = (rhs_num, v_num) {
            return match opr.as_str() {
                ">=" => vn >= rn,
                "<=" => vn <= rn,
                "<>" => vn != rn,
                ">" => vn > rn,
                "<" => vn < rn,
                "=" => vn == rn,
                _ => false,
            };
        }
    }
    // 文本相等（不敏感）；<> 取反；通配符 * ?
    let vs = to_text(v).unwrap_or_default();
    let target = if opr == "=" || opr == "<>" {
        rhs.as_str()
    } else {
        cs.as_str()
    };
    let has_wild = target.contains('*') || target.contains('?');
    let eq = if has_wild {
        wildcard_to_regex(target, true).is_match(&vs)
    } else {
        vs.to_uppercase() == target.to_uppercase()
    };
    if opr == "<>" {
        !eq
    } else {
        eq
    }
}

fn split_operator(cs: &str) -> (String, String) {
    for op in [">=", "<=", "<>", ">", "<", "="] {
        if let Some(rest) = cs.strip_prefix(op) {
            return (op.to_string(), rest.to_string());
        }
    }
    (String::new(), cs.to_string())
}

fn f_sumif(args: &[EvaluatedArg]) -> FormulaValue {
    let Some(range_arg @ EvaluatedArg::Range(_)) = args.first() else {
        return err(FormulaError::Value);
    };
    let criteria = scalar_arg(args.get(1));
    let sum_arg = args.get(2).unwrap_or(range_arg);
    let flat_range = flatten_arg(range_arg);
    let flat_sum = flatten_arg(sum_arg);
    let mut total = 0.0;
    for (i, rv) in flat_range.iter().enumerate() {
        if matches_criteria(rv, &criteria) {
            let target = flat_sum.get(i).unwrap_or(rv);
            if let Ok(n) = to_number(target) {
                total += n;
            }
        }
    }
    FormulaValue::Number(total)
}

fn f_countif(args: &[EvaluatedArg]) -> FormulaValue {
    let Some(range_arg) = args.first() else {
        return FormulaValue::Number(0.0);
    };
    let criteria = scalar_arg(args.get(1));
    let c = flatten_arg(range_arg)
        .iter()
        .filter(|v| matches_criteria(v, &criteria))
        .count();
    FormulaValue::Number(c as f64)
}

fn f_subtotal(args: &[EvaluatedArg]) -> FormulaValue {
    let code = match num(&scalar_arg(args.first())) {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let rest = &args[1.min(args.len())..];
    let c = (code.trunc() as i64) % 100;
    match c {
        1 => f_average(rest),
        2 => f_count(rest),
        3 => f_counta(rest),
        4 => f_max(rest),
        5 => f_min(rest),
        6 => f_product(rest),
        7 => stat(rest, true, true),
        8 => stat(rest, false, true),
        9 => f_sum(rest),
        10 => stat(rest, true, false),
        11 => stat(rest, false, false),
        _ => err(FormulaError::Value),
    }
}

// ── 逻辑 / 信息 ──────────────────────────────────────────

fn f_if(args: &[EvaluatedArg]) -> FormulaValue {
    let cond = match to_boolean(&scalar_arg(args.first())) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let branch = if cond { args.get(1) } else { args.get(2) };
    match branch {
        Some(b) => scalar_arg(Some(b)),
        None => FormulaValue::Bool(cond),
    }
}

fn f_and(args: &[EvaluatedArg]) -> FormulaValue {
    let vs = flatten_args(args);
    if let Some(e) = first_error(&vs) {
        return err(e);
    }
    let mut any = false;
    for v in &vs {
        if v.is_blank() {
            continue;
        }
        match to_boolean(v) {
            Ok(b) => {
                any = true;
                if !b {
                    return FormulaValue::Bool(false);
                }
            }
            Err(e) => return err(e),
        }
    }
    if any {
        FormulaValue::Bool(true)
    } else {
        err(FormulaError::Value)
    }
}

fn f_or(args: &[EvaluatedArg]) -> FormulaValue {
    let vs = flatten_args(args);
    if let Some(e) = first_error(&vs) {
        return err(e);
    }
    for v in &vs {
        if v.is_blank() {
            continue;
        }
        match to_boolean(v) {
            Ok(b) => {
                if b {
                    return FormulaValue::Bool(true);
                }
            }
            Err(e) => return err(e),
        }
    }
    FormulaValue::Bool(false)
}

// ── 文本 ─────────────────────────────────────────────────

fn f_concatenate(args: &[EvaluatedArg]) -> FormulaValue {
    let mut s = String::new();
    for v in flatten_args(args) {
        match to_text(&v) {
            Ok(t) => s.push_str(&t),
            Err(e) => return err(e),
        }
    }
    FormulaValue::Text(s)
}

fn f_left(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let n = if args.len() > 1 {
        match num(&scalar_arg(args.get(1))) {
            Ok(n) => n,
            Err(e) => return err(e),
        }
    } else {
        1.0
    };
    let k = n.trunc().max(0.0) as usize;
    FormulaValue::Text(t.chars().take(k).collect())
}

fn f_right(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let n = if args.len() > 1 {
        match num(&scalar_arg(args.get(1))) {
            Ok(n) => n,
            Err(e) => return err(e),
        }
    } else {
        1.0
    };
    let k = n.trunc().max(0.0) as usize;
    let chars: Vec<char> = t.chars().collect();
    let start = chars.len().saturating_sub(k);
    FormulaValue::Text(chars[start..].iter().collect())
}

fn f_mid(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let start = match num(&scalar_arg(args.get(1))) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let len = match num(&scalar_arg(args.get(2))) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let s = (start.trunc() as i64).max(1) as usize;
    let l = len.trunc().max(0.0) as usize;
    let chars: Vec<char> = t.chars().collect();
    let from = s - 1;
    if from >= chars.len() {
        return FormulaValue::Text(String::new());
    }
    let to = (from + l).min(chars.len());
    FormulaValue::Text(chars[from..to].iter().collect())
}

fn f_len(args: &[EvaluatedArg]) -> FormulaValue {
    match to_text(&scalar_arg(args.first())) {
        Ok(t) => FormulaValue::Number(t.chars().count() as f64),
        Err(e) => err(e),
    }
}

fn f_trim(args: &[EvaluatedArg]) -> FormulaValue {
    match to_text(&scalar_arg(args.first())) {
        // Excel TRIM 只折叠 ASCII 空格（多个→一个），去首尾
        Ok(t) => {
            let collapsed = collapse_spaces(&t);
            FormulaValue::Text(collapsed)
        }
        Err(e) => err(e),
    }
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim_matches(' ').to_string()
}

/// TEXT(value, format) —— 复用 sheet-core 的 numfmt 引擎（与单元格显示单一事实源，RS-M7）。
fn f_text(args: &[EvaluatedArg]) -> FormulaValue {
    let v = scalar_arg(args.first());
    if let FormulaValue::Error(e) = v {
        return err(e);
    }
    let fmt = match to_text(&scalar_arg(args.get(1))) {
        Ok(f) => f,
        Err(e) => return err(e),
    };
    // FormulaValue → core CellValue（数字/布尔进数字段，其余进文本段 @）
    use sheet_core::cell::CellValue;
    let cell = match &v {
        FormulaValue::Number(n) => CellValue::Number(*n),
        FormulaValue::Bool(b) => CellValue::Bool(*b),
        FormulaValue::Error(e) => return err(*e),
        other => CellValue::Text(to_text(other).unwrap_or_default()),
    };
    FormulaValue::Text(sheet_core::numfmt::format_with(&cell, &fmt).text)
}

// ── 注册表 ───────────────────────────────────────────────

fn builtins() -> Vec<(&'static str, FunctionImpl)> {
    macro_rules! f {
        ($name:literal, $imp:expr) => {
            ($name, Rc::new($imp) as FunctionImpl)
        };
    }
    vec![
        // 聚合 / 数学
        f!("SUM", |a: &[EvaluatedArg], _: &EvalContext| f_sum(a)),
        f!("AVERAGE", |a: &[EvaluatedArg], _: &EvalContext| f_average(
            a
        )),
        f!("COUNT", |a: &[EvaluatedArg], _: &EvalContext| f_count(a)),
        f!("COUNTA", |a: &[EvaluatedArg], _: &EvalContext| f_counta(a)),
        f!("COUNTBLANK", |a: &[EvaluatedArg], _: &EvalContext| {
            f_countblank(a)
        }),
        f!("MAX", |a: &[EvaluatedArg], _: &EvalContext| f_max(a)),
        f!("MIN", |a: &[EvaluatedArg], _: &EvalContext| f_min(a)),
        f!("PRODUCT", |a: &[EvaluatedArg], _: &EvalContext| f_product(
            a
        )),
        f!("ABS", |a: &[EvaluatedArg], _: &EvalContext| unary_num(
            a,
            |n| FormulaValue::Number(n.abs())
        )),
        f!("INT", |a: &[EvaluatedArg], _: &EvalContext| unary_num(
            a,
            |n| FormulaValue::Number(n.floor())
        )),
        f!("TRUNC", |a: &[EvaluatedArg], _: &EvalContext| unary_num(
            a,
            |n| FormulaValue::Number(n.trunc())
        )),
        f!("SQRT", |a: &[EvaluatedArg], _: &EvalContext| unary_num(
            a,
            |n| {
                if n < 0.0 {
                    err(FormulaError::Num)
                } else {
                    FormulaValue::Number(n.sqrt())
                }
            }
        )),
        f!("ROUND", |a: &[EvaluatedArg], _: &EvalContext| two_num(
            a,
            |n, d| FormulaValue::Number(round_half_away(n, d.trunc() as i32))
        )),
        f!("ROUNDDOWN", |a: &[EvaluatedArg], _: &EvalContext| two_num(
            a,
            |n, d| {
                let f = 10f64.powi(d.trunc() as i32);
                FormulaValue::Number((n * f).trunc() / f)
            }
        )),
        f!("ROUNDUP", |a: &[EvaluatedArg], _: &EvalContext| two_num(
            a,
            |n, d| {
                let f = 10f64.powi(d.trunc() as i32);
                let x = n * f;
                FormulaValue::Number((if x < 0.0 { x.floor() } else { x.ceil() }) / f)
            }
        )),
        f!("MOD", |a: &[EvaluatedArg], _: &EvalContext| two_num(
            a,
            |x, y| {
                if y == 0.0 {
                    err(FormulaError::Div0)
                } else {
                    FormulaValue::Number(x - y * (x / y).floor())
                }
            }
        )),
        f!("POWER", |a: &[EvaluatedArg], _: &EvalContext| two_num(
            a,
            |x, y| {
                let r = x.powf(y);
                if r.is_finite() {
                    FormulaValue::Number(r)
                } else {
                    err(FormulaError::Num)
                }
            }
        )),
        f!("SUMIF", |a: &[EvaluatedArg], _: &EvalContext| f_sumif(a)),
        f!("COUNTIF", |a: &[EvaluatedArg], _: &EvalContext| f_countif(
            a
        )),
        f!(
            "SUBTOTAL",
            |a: &[EvaluatedArg], _: &EvalContext| f_subtotal(a)
        ),
        f!("STDEV", |a: &[EvaluatedArg], _: &EvalContext| stat(
            a, true, true
        )),
        f!("STDEVP", |a: &[EvaluatedArg], _: &EvalContext| stat(
            a, false, true
        )),
        f!("VAR", |a: &[EvaluatedArg], _: &EvalContext| stat(
            a, true, false
        )),
        f!("VARP", |a: &[EvaluatedArg], _: &EvalContext| stat(
            a, false, false
        )),
        // 逻辑
        f!("IF", |a: &[EvaluatedArg], _: &EvalContext| f_if(a)),
        f!("IFERROR", |a: &[EvaluatedArg], _: &EvalContext| {
            let v = scalar_arg(a.first());
            if v.is_error() {
                scalar_arg(a.get(1))
            } else {
                v
            }
        }),
        f!("IFNA", |a: &[EvaluatedArg], _: &EvalContext| {
            let v = scalar_arg(a.first());
            if v.as_error() == Some(FormulaError::Na) {
                scalar_arg(a.get(1))
            } else {
                v
            }
        }),
        f!("AND", |a: &[EvaluatedArg], _: &EvalContext| f_and(a)),
        f!("OR", |a: &[EvaluatedArg], _: &EvalContext| f_or(a)),
        f!(
            "NOT",
            |a: &[EvaluatedArg], _: &EvalContext| match to_boolean(&scalar_arg(a.first())) {
                Ok(b) => FormulaValue::Bool(!b),
                Err(e) => err(e),
            }
        ),
        f!("TRUE", |_: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(true)
        }),
        f!("FALSE", |_: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(false)
        }),
        f!("ISBLANK", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(scalar_arg(a.first()).is_blank())
        }),
        f!("ISEMPTY", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(scalar_arg(a.first()).is_blank())
        }),
        f!("ISERROR", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(scalar_arg(a.first()).is_error())
        }),
        f!("ISNUMBER", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(matches!(scalar_arg(a.first()), FormulaValue::Number(_)))
        }),
        f!("ISTEXT", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(matches!(scalar_arg(a.first()), FormulaValue::Text(_)))
        }),
        f!("COALESCE", |a: &[EvaluatedArg], _: &EvalContext| {
            for arg in a {
                let v = scalar_arg(Some(arg));
                if !v.is_blank() {
                    return v;
                }
            }
            FormulaValue::Blank
        }),
        // 文本
        f!("CONCATENATE", |a: &[EvaluatedArg], _: &EvalContext| {
            f_concatenate(a)
        }),
        f!("CONCAT", |a: &[EvaluatedArg], _: &EvalContext| {
            f_concatenate(a)
        }),
        f!("LEFT", |a: &[EvaluatedArg], _: &EvalContext| f_left(a)),
        f!("RIGHT", |a: &[EvaluatedArg], _: &EvalContext| f_right(a)),
        f!("MID", |a: &[EvaluatedArg], _: &EvalContext| f_mid(a)),
        f!("LEN", |a: &[EvaluatedArg], _: &EvalContext| f_len(a)),
        f!("TRIM", |a: &[EvaluatedArg], _: &EvalContext| f_trim(a)),
        f!(
            "UPPER",
            |a: &[EvaluatedArg], _: &EvalContext| match to_text(&scalar_arg(a.first())) {
                Ok(t) => FormulaValue::Text(t.to_uppercase()),
                Err(e) => err(e),
            }
        ),
        f!(
            "LOWER",
            |a: &[EvaluatedArg], _: &EvalContext| match to_text(&scalar_arg(a.first())) {
                Ok(t) => FormulaValue::Text(t.to_lowercase()),
                Err(e) => err(e),
            }
        ),
        f!("TEXT", |a: &[EvaluatedArg], _: &EvalContext| f_text(a)),
        f!(
            "VALUE",
            |a: &[EvaluatedArg], _: &EvalContext| match num(&scalar_arg(a.first())) {
                Ok(n) => FormulaValue::Number(n),
                Err(e) => err(e),
            }
        ),
    ]
}

/// 内置函数注册表（可叠加自定义函数）。
pub struct BuiltinRegistry {
    fns: HashMap<String, FunctionImpl>,
    volatiles: HashSet<String>,
}

impl Default for BuiltinRegistry {
    fn default() -> Self {
        BuiltinRegistry::new()
    }
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        let mut fns = HashMap::new();
        for (name, imp) in builtins() {
            fns.insert(name.to_string(), imp);
        }
        // M8 扩容函数集（镜像 TS 的 ...MATH_BUILTINS 展开）。
        for (name, imp) in crate::builtins_m8::m8_builtins() {
            fns.insert(name.to_string(), imp);
        }
        // M17 五族大扩容（math/financial/statistical/database/textref + 10 个 TS 核心补齐）。
        for (name, imp) in crate::builtins_m17::m17_builtins() {
            fns.insert(name.to_string(), imp);
        }
        // 工程族 + 日期扩容（BIN2DEC/BITAND/ERF/BESSEL/CONVERT/IM* + DAYS/YEARFRAC/…）。
        for (name, imp) in crate::builtins_eng::eng_builtins() {
            fns.insert(name.to_string(), imp);
        }
        BuiltinRegistry {
            fns,
            volatiles: HashSet::new(),
        }
    }

    /// 注册/覆盖一个函数（供 QM/QC/… 及自定义叠加）。
    pub fn register(&mut self, name: &str, imp: FunctionImpl, volatile: bool) {
        let up = name.to_uppercase();
        if volatile {
            self.volatiles.insert(up.clone());
        }
        self.fns.insert(up, imp);
    }

    /// 已注册函数名列表（调试/断言）。
    pub fn names(&self) -> Vec<String> {
        self.fns.keys().cloned().collect()
    }
}

impl FunctionRegistry for BuiltinRegistry {
    fn get(&self, name: &str) -> Option<FunctionImpl> {
        self.fns.get(&name.to_uppercase()).cloned()
    }
    fn is_volatile(&self, name: &str) -> bool {
        self.volatiles.contains(&name.to_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::{CellAccessor, EvalContext, Evaluator, FunctionRegistry};
    use crate::parse::parse_formula;
    use sheet_core::address::parse_addr;
    use std::collections::HashMap;

    struct MapAccessor {
        cells: HashMap<(u32, u32), FormulaValue>,
    }
    fn addr_of(reference: &str) -> Option<(u32, u32)> {
        let local = match reference.find('!') {
            Some(b) => &reference[b + 1..],
            None => reference,
        };
        parse_addr(&local.replace('$', "")).map(|p| (p.row, p.col))
    }
    impl CellAccessor for MapAccessor {
        fn get_cell_value(&self, reference: &str) -> FormulaValue {
            addr_of(reference)
                .and_then(|rc| self.cells.get(&rc).cloned())
                .unwrap_or(FormulaValue::Blank)
        }
        fn get_range_values(&self, start: &str, end: &str) -> Vec<Vec<FormulaValue>> {
            let (Some(a), Some(b)) = (addr_of(start), addr_of(end)) else {
                return vec![vec![FormulaValue::Error(FormulaError::Ref)]];
            };
            let (r1, r2) = (a.0.min(b.0), a.0.max(b.0));
            let (c1, c2) = (a.1.min(b.1), a.1.max(b.1));
            (r1..=r2)
                .map(|r| {
                    (c1..=c2)
                        .map(|c| {
                            self.cells
                                .get(&(r, c))
                                .cloned()
                                .unwrap_or(FormulaValue::Blank)
                        })
                        .collect()
                })
                .collect()
        }
    }

    fn eval(src: &str, cells: &[((u32, u32), FormulaValue)]) -> FormulaValue {
        let acc = MapAccessor {
            cells: cells.iter().cloned().collect(),
        };
        let reg = BuiltinRegistry::new();
        let ev = Evaluator::new(&reg);
        let ctx = EvalContext {
            accessor: &acc,
            row: 0,
            col: 0,
            sheet_name: "Sheet1",
        };
        ev.evaluate(&parse_formula(src).unwrap(), &ctx)
    }
    fn n(v: FormulaValue) -> f64 {
        match v {
            FormulaValue::Number(x) => x,
            o => panic!("expected number: {o:?}"),
        }
    }

    #[test]
    fn aggregates() {
        let c = [
            ((0u32, 0u32), FormulaValue::Number(10.0)),
            ((0, 1), FormulaValue::Number(20.0)),
            ((0, 2), FormulaValue::Number(30.0)),
            ((0, 3), FormulaValue::Text("".into())),
        ];
        assert_eq!(n(eval("SUM(A1:D1)", &c)), 60.0);
        assert_eq!(n(eval("AVERAGE(A1:C1)", &c)), 20.0);
        assert_eq!(n(eval("MAX(A1:C1)", &c)), 30.0);
        assert_eq!(n(eval("MIN(A1:C1)", &c)), 10.0);
        assert_eq!(n(eval("COUNT(A1:D1)", &c)), 3.0);
    }

    #[test]
    fn round_family() {
        assert_eq!(n(eval("ROUND(2.71828, 2)", &[])), 2.72);
        assert_eq!(n(eval("ROUNDUP(3.1, 0)", &[])), 4.0);
        assert_eq!(n(eval("ROUNDDOWN(3.9, 0)", &[])), 3.0);
        assert_eq!(n(eval("ABS(-7)", &[])), 7.0);
        assert_eq!(n(eval("INT(3.9)", &[])), 3.0);
        assert_eq!(n(eval("MOD(10, 3)", &[])), 1.0);
    }

    #[test]
    fn logical() {
        assert_eq!(
            eval("IF(1>0, \"yes\", \"no\")", &[]),
            FormulaValue::Text("yes".into())
        );
        assert_eq!(
            eval("IF(1<0, \"yes\", \"no\")", &[]),
            FormulaValue::Text("no".into())
        );
        assert_eq!(
            eval("IFERROR(1/0, \"err\")", &[]),
            FormulaValue::Text("err".into())
        );
        assert_eq!(eval("AND(1>0, 2>1)", &[]), FormulaValue::Bool(true));
        assert_eq!(eval("OR(1<0, 2>1)", &[]), FormulaValue::Bool(true));
        assert_eq!(eval("NOT(1>0)", &[]), FormulaValue::Bool(false));
    }

    #[test]
    fn text_functions() {
        assert_eq!(
            eval("CONCATENATE(\"a\",\"b\",\"c\")", &[]),
            FormulaValue::Text("abc".into())
        );
        assert_eq!(
            eval("LEFT(\"hello\", 2)", &[]),
            FormulaValue::Text("he".into())
        );
        assert_eq!(
            eval("RIGHT(\"hello\", 2)", &[]),
            FormulaValue::Text("lo".into())
        );
        assert_eq!(
            eval("MID(\"hello\", 2, 3)", &[]),
            FormulaValue::Text("ell".into())
        );
        assert_eq!(n(eval("LEN(\"hello\")", &[])), 5.0);
        assert_eq!(eval("UPPER(\"ab\")", &[]), FormulaValue::Text("AB".into()));
        assert_eq!(
            eval("TEXT(1234.5, \"#,##0.00\")", &[]),
            FormulaValue::Text("1,234.50".into())
        );
    }

    #[test]
    fn sumif_countif() {
        let c = [
            ((0u32, 0u32), FormulaValue::Number(5.0)),
            ((1, 0), FormulaValue::Number(15.0)),
            ((2, 0), FormulaValue::Number(25.0)),
        ];
        assert_eq!(n(eval("SUMIF(A1:A3, \">10\")", &c)), 40.0);
        assert_eq!(n(eval("COUNTIF(A1:A3, \">10\")", &c)), 2.0);
    }

    #[test]
    fn subtotal_fn() {
        let c = [
            ((0u32, 0u32), FormulaValue::Number(1.0)),
            ((1, 0), FormulaValue::Number(2.0)),
            ((2, 0), FormulaValue::Number(3.0)),
        ];
        assert_eq!(n(eval("SUBTOTAL(9, A1:A3)", &c)), 6.0);
        assert_eq!(n(eval("SUBTOTAL(1, A1:A3)", &c)), 2.0);
    }

    #[test]
    fn info_and_errors() {
        assert_eq!(n(eval("COALESCE(Z1, Z2, 7)", &[])), 7.0);
        assert_eq!(eval("ISEMPTY(Z1)", &[]), FormulaValue::Bool(true));
        assert_eq!(eval("ISBLANK(Z1)", &[]), FormulaValue::Bool(true));
        assert_eq!(
            eval("NOSUCHFN(1)", &[]),
            FormulaValue::Error(FormulaError::Name)
        );
        assert_eq!(
            eval("SUM(1, 1/0)", &[]),
            FormulaValue::Error(FormulaError::Div0)
        );
    }

    #[test]
    fn registry_surface() {
        let reg = BuiltinRegistry::new();
        assert!(reg.names().len() >= 40);
        assert!(reg.get("sum").is_some());
        let mut reg2 = BuiltinRegistry::new();
        reg2.register(
            "MYFN",
            Rc::new(|_: &[EvaluatedArg], _: &EvalContext| FormulaValue::Number(42.0)),
            true,
        );
        assert!(reg2.get("MYFN").is_some());
        assert!(reg2.is_volatile("myfn"));
        assert!(!reg2.is_volatile("SUM"));
    }
}
