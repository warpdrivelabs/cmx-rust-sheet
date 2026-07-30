//! M8 内置函数扩容：数学/取整、统计/排名、多条件聚合、文本、逻辑/信息、日期时间、查找/引用。
//! 对标 cmx-megasheet functions.ts 的 M8 段。作为独立模块并入 BuiltinRegistry（镜像 TS 的
//! `...MATH_BUILTINS` 展开）。复用 functions.rs 的 numeric_values/round_half_away/matches_criteria
//! /wildcard_to_regex 等 pub(crate) 助手。纯逻辑、零 DOM。

use std::rc::Rc;

use sheet_core::date_serial::{date_to_serial, serial_to_parts, time_to_fraction};

use crate::evaluator::{
    as_matrix, flatten_arg, flatten_args, scalar_arg, EvalContext, EvaluatedArg, FunctionImpl,
};
use crate::functions::{matches_criteria, numeric_values, round_half_away, wildcard_to_regex};
use crate::value::{to_boolean, to_number, to_text, FormulaError, FormulaValue};

fn err(e: FormulaError) -> FormulaValue {
    FormulaValue::Error(e)
}

fn num(v: &FormulaValue) -> Result<f64, FormulaError> {
    to_number(v)
}

/// 一元数字模板。
fn un(args: &[EvaluatedArg], f: impl Fn(f64) -> FormulaValue) -> FormulaValue {
    match num(&scalar_arg(args.first())) {
        Ok(n) => f(n),
        Err(e) => err(e),
    }
}

fn two(args: &[EvaluatedArg], f: impl Fn(f64, f64) -> FormulaValue) -> FormulaValue {
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

// ── 数学 / 取整 ─────────────────────────────────────────

fn gcd2(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// 取整数序列（GCD/LCM 用）；负数或非数 → 错误。
fn int_list(args: &[EvaluatedArg]) -> Result<Vec<i64>, FormulaError> {
    let mut out = Vec::new();
    for v in flatten_args(args) {
        if v.is_blank() {
            continue;
        }
        let n = to_number(&v)?;
        if n < 0.0 {
            return Err(FormulaError::Num);
        }
        out.push(n.trunc() as i64);
    }
    Ok(out)
}

fn median(mut ns: Vec<f64>) -> FormulaValue {
    if ns.is_empty() {
        return err(FormulaError::Num);
    }
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = ns.len() / 2;
    if ns.len() % 2 == 1 {
        FormulaValue::Number(ns[mid])
    } else {
        FormulaValue::Number((ns[mid - 1] + ns[mid]) / 2.0)
    }
}

fn mode(ns: &[f64]) -> FormulaValue {
    use std::collections::HashMap;
    let mut count: HashMap<u64, (f64, usize)> = HashMap::new();
    let mut best: Option<f64> = None;
    let mut best_count = 1usize;
    for &n in ns {
        let key = n.to_bits();
        let e = count.entry(key).or_insert((n, 0));
        e.1 += 1;
        if e.1 > best_count {
            best_count = e.1;
            best = Some(n);
        }
    }
    match best {
        Some(n) => FormulaValue::Number(n),
        None => err(FormulaError::Na),
    }
}

fn nth_order(args: &[EvaluatedArg], large: bool) -> FormulaValue {
    let mut ns: Vec<f64> = flatten_arg(
        args.first()
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    )
    .iter()
    .filter_map(|v| to_number(v).ok())
    .collect();
    let k = match num(&scalar_arg(args.get(1))) {
        Ok(n) => n.trunc() as i64,
        Err(e) => return err(e),
    };
    if k < 1 || k as usize > ns.len() {
        return err(FormulaError::Num);
    }
    ns.sort_by(|a, b| {
        if large {
            b.partial_cmp(a).unwrap()
        } else {
            a.partial_cmp(b).unwrap()
        }
    });
    FormulaValue::Number(ns[(k - 1) as usize])
}

fn percentile(args: &[EvaluatedArg]) -> FormulaValue {
    let mut ns: Vec<f64> = flatten_arg(
        args.first()
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    )
    .iter()
    .filter_map(|v| to_number(v).ok())
    .collect();
    let p = match num(&scalar_arg(args.get(1))) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    if !(0.0..=1.0).contains(&p) || ns.is_empty() {
        return err(FormulaError::Num);
    }
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = p * (ns.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        FormulaValue::Number(ns[lo])
    } else {
        FormulaValue::Number(ns[lo] + (rank - lo as f64) * (ns[hi] - ns[lo]))
    }
}

fn rank_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let target = match num(&scalar_arg(args.first())) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let mut ns: Vec<f64> = flatten_arg(
        args.get(1)
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    )
    .iter()
    .filter_map(|v| to_number(v).ok())
    .collect();
    let ascending = args
        .get(2)
        .map(|a| num(&scalar_arg(Some(a))).map(|n| n != 0.0).unwrap_or(false))
        .unwrap_or(false);
    ns.sort_by(|a, b| {
        if ascending {
            a.partial_cmp(b).unwrap()
        } else {
            b.partial_cmp(a).unwrap()
        }
    });
    match ns.iter().position(|&x| x == target) {
        Some(i) => FormulaValue::Number((i + 1) as f64),
        None => err(FormulaError::Na),
    }
}

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

fn sum_product(args: &[EvaluatedArg]) -> FormulaValue {
    let vectors: Vec<Vec<f64>> = args
        .iter()
        .map(|a| {
            flatten_arg(a)
                .iter()
                .map(|v| match v {
                    FormulaValue::Number(n) => *n,
                    FormulaValue::Blank => 0.0,
                    FormulaValue::Text(s) => s.trim().parse::<f64>().unwrap_or(0.0),
                    _ => 0.0,
                })
                .collect()
        })
        .collect();
    if vectors.is_empty() {
        return FormulaValue::Number(0.0);
    }
    let len = vectors[0].len();
    if vectors.iter().any(|v| v.len() != len) {
        return err(FormulaError::Value);
    }
    let mut total = 0.0;
    for i in 0..len {
        let mut prod = 1.0;
        for v in &vectors {
            prod *= v[i];
        }
        total += prod;
    }
    FormulaValue::Number(total)
}

// ── 多条件聚合 ──────────────────────────────────────────

/// 收集 (criteriaRange, criteria) 对，返回逐格是否全部命中的掩码。
fn build_mask(pairs: &[(&EvaluatedArg, FormulaValue)]) -> Result<Vec<bool>, FormulaError> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let flats: Vec<Vec<FormulaValue>> = pairs.iter().map(|(r, _)| flatten_arg(r)).collect();
    let len = flats[0].len();
    if flats.iter().any(|f| f.len() != len) {
        return Err(FormulaError::Value);
    }
    let mut mask = Vec::with_capacity(len);
    #[allow(clippy::needless_range_loop)] // i 索引多个并行向量 flats[j][i]
    for i in 0..len {
        let ok = pairs
            .iter()
            .enumerate()
            .all(|(j, (_, crit))| matches_criteria(&flats[j][i], crit));
        mask.push(ok);
    }
    Ok(mask)
}

fn pairs_from(args: &[EvaluatedArg], start: usize) -> Vec<(&EvaluatedArg, FormulaValue)> {
    let mut pairs = Vec::new();
    let mut i = start;
    while i + 1 < args.len() {
        pairs.push((&args[i], scalar_arg(args.get(i + 1))));
        i += 2;
    }
    pairs
}

fn sumifs(args: &[EvaluatedArg]) -> FormulaValue {
    let sum_range = flatten_arg(
        args.first()
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    );
    let mask = match build_mask(&pairs_from(args, 1)) {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let mut total = 0.0;
    for (i, &m) in mask.iter().enumerate() {
        if m {
            if let Some(v) = sum_range.get(i) {
                if let Ok(n) = to_number(v) {
                    total += n;
                }
            }
        }
    }
    FormulaValue::Number(total)
}

fn countifs(args: &[EvaluatedArg]) -> FormulaValue {
    let mask = match build_mask(&pairs_from(args, 0)) {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    FormulaValue::Number(mask.iter().filter(|&&m| m).count() as f64)
}

fn averageif(args: &[EvaluatedArg]) -> FormulaValue {
    let Some(range_arg) = args.first() else {
        return err(FormulaError::Div0);
    };
    let criteria = scalar_arg(args.get(1));
    let avg_arg = args.get(2).unwrap_or(range_arg);
    let flat_range = flatten_arg(range_arg);
    let flat_avg = flatten_arg(avg_arg);
    let (mut sum, mut cnt) = (0.0, 0usize);
    for (i, rv) in flat_range.iter().enumerate() {
        if matches_criteria(rv, &criteria) {
            if let Ok(n) = to_number(flat_avg.get(i).unwrap_or(rv)) {
                sum += n;
                cnt += 1;
            }
        }
    }
    if cnt == 0 {
        err(FormulaError::Div0)
    } else {
        FormulaValue::Number(sum / cnt as f64)
    }
}

fn averageifs(args: &[EvaluatedArg]) -> FormulaValue {
    let avg_range = flatten_arg(
        args.first()
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    );
    let mask = match build_mask(&pairs_from(args, 1)) {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let (mut sum, mut cnt) = (0.0, 0usize);
    for (i, &m) in mask.iter().enumerate() {
        if m {
            if let Some(Ok(n)) = avg_range.get(i).map(to_number) {
                sum += n;
                cnt += 1;
            }
        }
    }
    if cnt == 0 {
        err(FormulaError::Div0)
    } else {
        FormulaValue::Number(sum / cnt as f64)
    }
}

fn extremum_ifs(args: &[EvaluatedArg], max: bool) -> FormulaValue {
    let val_range = flatten_arg(
        args.first()
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    );
    let mask = match build_mask(&pairs_from(args, 1)) {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let mut best: Option<f64> = None;
    for (i, &m) in mask.iter().enumerate() {
        if !m {
            continue;
        }
        if let Some(Ok(n)) = val_range.get(i).map(to_number) {
            best = Some(match best {
                None => n,
                Some(b) => {
                    if max {
                        b.max(n)
                    } else {
                        b.min(n)
                    }
                }
            });
        }
    }
    FormulaValue::Number(best.unwrap_or(0.0))
}

// ── 文本 ────────────────────────────────────────────────

fn textjoin(args: &[EvaluatedArg]) -> FormulaValue {
    let delim = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let ignore_empty = match to_boolean(&scalar_arg(args.get(1))) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let mut parts = Vec::new();
    for v in flatten_args(&args[2.min(args.len())..]) {
        if ignore_empty && v.is_blank() {
            continue;
        }
        match to_text(&v) {
            Ok(t) => parts.push(t),
            Err(e) => return err(e),
        }
    }
    FormulaValue::Text(parts.join(&delim))
}

fn substitute(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let old = match to_text(&scalar_arg(args.get(1))) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let new = match to_text(&scalar_arg(args.get(2))) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    if old.is_empty() {
        return FormulaValue::Text(t);
    }
    if args.get(3).is_none() {
        return FormulaValue::Text(t.replace(&old, &new));
    }
    let nth = match num(&scalar_arg(args.get(3))) {
        Ok(n) => n.trunc() as i64,
        Err(e) => return err(e),
    };
    if nth < 1 {
        return err(FormulaError::Value);
    }
    // 第 nth 次出现替换
    let mut count = 0;
    let mut search_from = 0;
    while let Some(pos) = t[search_from..].find(&old) {
        let abs = search_from + pos;
        count += 1;
        if count == nth {
            let mut out = String::with_capacity(t.len());
            out.push_str(&t[..abs]);
            out.push_str(&new);
            out.push_str(&t[abs + old.len()..]);
            return FormulaValue::Text(out);
        }
        search_from = abs + old.len();
    }
    FormulaValue::Text(t)
}

fn replace_fn(args: &[EvaluatedArg]) -> FormulaValue {
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
    let new = match to_text(&scalar_arg(args.get(3))) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let chars: Vec<char> = t.chars().collect();
    let s = (start.trunc() as i64).max(1) as usize - 1;
    let l = len.trunc().max(0.0) as usize;
    let mut out = String::new();
    out.extend(chars.iter().take(s.min(chars.len())));
    out.push_str(&new);
    out.extend(chars.iter().skip((s + l).min(chars.len())));
    FormulaValue::Text(out)
}

fn find_in(args: &[EvaluatedArg], case_sensitive: bool) -> FormulaValue {
    let needle = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let hay = match to_text(&scalar_arg(args.get(1))) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let start = match args.get(2) {
        Some(a) => match num(&scalar_arg(Some(a))) {
            Ok(n) => n.trunc() as i64,
            Err(e) => return err(e),
        },
        None => 1,
    };
    let hay_chars: Vec<char> = hay.chars().collect();
    let from = (start - 1).max(0) as usize;
    if from > hay_chars.len() {
        return err(FormulaError::Value);
    }
    let sub: String = hay_chars[from..].iter().collect();
    if case_sensitive {
        match sub.find(&needle) {
            Some(byte_pos) => {
                let char_pos = sub[..byte_pos].chars().count();
                FormulaValue::Number((from + char_pos + 1) as f64)
            }
            None => err(FormulaError::Value),
        }
    } else {
        let re = wildcard_to_regex(&needle, false);
        match re.find(&sub) {
            Some(m) => {
                let char_pos = sub[..m.start()].chars().count();
                FormulaValue::Number((from + char_pos + 1) as f64)
            }
            None => err(FormulaError::Value),
        }
    }
}

fn number_value(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let dec = args
        .get(1)
        .map(|a| to_text(&scalar_arg(Some(a))).unwrap_or_else(|_| ".".into()))
        .unwrap_or_else(|| ".".into());
    let grp = args
        .get(2)
        .map(|a| to_text(&scalar_arg(Some(a))).unwrap_or_else(|_| ",".into()))
        .unwrap_or_else(|| ",".into());
    let mut s = t.replace(&grp, "").replace(&dec, ".");
    s = s.trim().to_string();
    let is_pct = s.ends_with('%');
    let s = s.trim_end_matches('%');
    match s.parse::<f64>() {
        Ok(n) if n.is_finite() => FormulaValue::Number(if is_pct { n / 100.0 } else { n }),
        _ => err(FormulaError::Value),
    }
}

fn proper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alpha = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if prev_alpha {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_alpha = true;
        } else {
            out.push(c);
            prev_alpha = false;
        }
    }
    out
}

// ── 逻辑 / 信息 ─────────────────────────────────────────

fn ifs(args: &[EvaluatedArg]) -> FormulaValue {
    let mut i = 0;
    while i + 1 < args.len() {
        match to_boolean(&scalar_arg(args.get(i))) {
            Ok(true) => return scalar_arg(args.get(i + 1)),
            Ok(false) => {}
            Err(e) => return err(e),
        }
        i += 2;
    }
    err(FormulaError::Na)
}

fn compare_eq(a: &FormulaValue, b: &FormulaValue) -> bool {
    match (a, b) {
        (FormulaValue::Number(x), FormulaValue::Number(y)) => x == y,
        _ => {
            to_text(a).unwrap_or_default().to_uppercase()
                == to_text(b).unwrap_or_default().to_uppercase()
        }
    }
}

fn switch_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let target = scalar_arg(args.first());
    let n = args.len();
    let mut i = 1;
    while i + 1 < n {
        if compare_eq(&scalar_arg(args.get(i)), &target) {
            return scalar_arg(args.get(i + 1));
        }
        i += 2;
    }
    // 尾部单个 → 默认值
    if (n - 1) % 2 == 1 {
        scalar_arg(args.get(n - 1))
    } else {
        err(FormulaError::Na)
    }
}

// ── 日期时间 ────────────────────────────────────────────

/// 年月日（月/日可溢出）→ 序列号，经历法归一。
fn date_overflow(year: i64, month: i64, day: i64) -> f64 {
    // 月溢出：归一到 [1,12] + 年进位
    let mut y = year;
    let mut m = month;
    // 月归一
    y += (m - 1).div_euclid(12);
    m = (m - 1).rem_euclid(12) + 1;
    // 日溢出：先取该年月 1 日序列号，再加 (day-1)
    let base = date_to_serial(y, m as u32, 1);
    base + (day - 1) as f64
}

fn date_part(args: &[EvaluatedArg], part: DatePart) -> FormulaValue {
    let s = match num(&scalar_arg(args.first())) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    if s < 0.0 {
        return err(FormulaError::Num);
    }
    let p = serial_to_parts(s);
    FormulaValue::Number(match part {
        DatePart::Year => p.year as f64,
        DatePart::Month => p.month as f64,
        DatePart::Day => p.day as f64,
        DatePart::Hours => p.hours as f64,
        DatePart::Minutes => p.minutes as f64,
        DatePart::Seconds => p.seconds as f64,
    })
}

#[derive(Clone, Copy)]
enum DatePart {
    Year,
    Month,
    Day,
    Hours,
    Minutes,
    Seconds,
}

fn weekday_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let s = match num(&scalar_arg(args.first())) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let ty = match args.get(1) {
        Some(a) => match num(&scalar_arg(Some(a))) {
            Ok(n) => n.trunc() as i64,
            Err(e) => return err(e),
        },
        None => 1,
    };
    let dow = serial_to_parts(s).weekday as i64; // 0=Sun..6=Sat
    FormulaValue::Number(match ty {
        1 => (dow + 1) as f64,
        2 => ((dow + 6) % 7 + 1) as f64,
        3 => ((dow + 6) % 7) as f64,
        _ => (dow + 1) as f64,
    })
}

fn edate(args: &[EvaluatedArg], end_of_month: bool) -> FormulaValue {
    let s = match num(&scalar_arg(args.first())) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let months = match num(&scalar_arg(args.get(1))) {
        Ok(n) => n.trunc() as i64,
        Err(e) => return err(e),
    };
    let p = serial_to_parts(s);
    let total = p.year * 12 + (p.month as i64 - 1) + months;
    let yr = total.div_euclid(12);
    let mo = (total.rem_euclid(12) + 1) as u32;
    let last_day = last_day_of_month(yr, mo);
    if end_of_month {
        FormulaValue::Number(date_to_serial(yr, mo, last_day))
    } else {
        FormulaValue::Number(date_to_serial(yr, mo, p.day.min(last_day)))
    }
}

fn last_day_of_month(year: i64, month: u32) -> u32 {
    let next = date_to_serial(
        if month == 12 { year + 1 } else { year },
        if month == 12 { 1 } else { month + 1 },
        1,
    );
    let first = date_to_serial(year, month, 1);
    (next - first) as u32
}

fn datedif(args: &[EvaluatedArg]) -> FormulaValue {
    let s1 = match num(&scalar_arg(args.first())) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let s2 = match num(&scalar_arg(args.get(1))) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let unit = match to_text(&scalar_arg(args.get(2))) {
        Ok(t) => t.to_uppercase(),
        Err(e) => return err(e),
    };
    if s2 < s1 {
        return err(FormulaError::Num);
    }
    let a = serial_to_parts(s1);
    let b = serial_to_parts(s2);
    match unit.as_str() {
        "D" => FormulaValue::Number((s2.trunc() - s1.trunc()).abs()),
        "M" => {
            let mut m = (b.year - a.year) * 12 + (b.month as i64 - a.month as i64);
            if b.day < a.day {
                m -= 1;
            }
            FormulaValue::Number(m as f64)
        }
        "Y" => {
            let mut y = b.year - a.year;
            if b.month < a.month || (b.month == a.month && b.day < a.day) {
                y -= 1;
            }
            FormulaValue::Number(y as f64)
        }
        "YM" => {
            let mut m = b.month as i64 - a.month as i64;
            if b.day < a.day {
                m -= 1;
            }
            if m < 0 {
                m += 12;
            }
            FormulaValue::Number(m as f64)
        }
        _ => err(FormulaError::Num),
    }
}

// ── 查找 / 引用 ─────────────────────────────────────────

fn match_exact(a: &FormulaValue, b: &FormulaValue) -> bool {
    match (a, b) {
        (FormulaValue::Number(x), FormulaValue::Number(y)) => x == y,
        _ => {
            let bs = to_text(b).unwrap_or_default();
            if bs.contains('*') || bs.contains('?') {
                wildcard_to_regex(&bs, true).is_match(&to_text(a).unwrap_or_default())
            } else {
                to_text(a).unwrap_or_default().to_uppercase() == bs.to_uppercase()
            }
        }
    }
}

/// 松散比较：数字按数值、文本大小写不敏感字典序；不可比返回 None。
fn compare_loose(a: &FormulaValue, b: &FormulaValue) -> Option<std::cmp::Ordering> {
    if a.is_blank() {
        return None;
    }
    match (a, b) {
        (FormulaValue::Number(x), FormulaValue::Number(y)) => x.partial_cmp(y),
        _ => Some(
            to_text(a)
                .unwrap_or_default()
                .to_uppercase()
                .cmp(&to_text(b).unwrap_or_default().to_uppercase()),
        ),
    }
}

fn lookup_row_col(
    table: &[Vec<FormulaValue>],
    target: &FormulaValue,
    idx: usize,
    approx: bool,
    vertical: bool,
) -> FormulaValue {
    let keys: Vec<FormulaValue> = if vertical {
        table
            .iter()
            .map(|r| r.first().cloned().unwrap_or(FormulaValue::Blank))
            .collect()
    } else {
        table.first().cloned().unwrap_or_default()
    };
    if approx {
        let mut found: i64 = -1;
        for (i, k) in keys.iter().enumerate() {
            match compare_loose(k, target) {
                Some(std::cmp::Ordering::Greater) => break,
                Some(_) => found = i as i64,
                None => {}
            }
        }
        if found < 0 {
            return err(FormulaError::Na);
        }
        let fi = found as usize;
        return get_cell(table, fi, idx, vertical);
    }
    for (i, k) in keys.iter().enumerate() {
        if match_exact(k, target) {
            return get_cell(table, i, idx, vertical);
        }
    }
    err(FormulaError::Na)
}

fn get_cell(table: &[Vec<FormulaValue>], key_i: usize, idx: usize, vertical: bool) -> FormulaValue {
    let cell = if vertical {
        table.get(key_i).and_then(|r| r.get(idx))
    } else {
        table.get(idx).and_then(|r| r.get(key_i))
    };
    cell.cloned()
        .unwrap_or(FormulaValue::Error(FormulaError::Ref))
}

fn vlookup(args: &[EvaluatedArg]) -> FormulaValue {
    let target = scalar_arg(args.first());
    let table = as_matrix(args.get(1));
    let col_idx = match num(&scalar_arg(args.get(2))) {
        Ok(n) => n.trunc() as i64 - 1,
        Err(e) => return err(e),
    };
    if col_idx < 0 {
        return err(FormulaError::Value);
    }
    let approx = args
        .get(3)
        .map(|a| to_boolean(&scalar_arg(Some(a))).unwrap_or(true))
        .unwrap_or(true);
    lookup_row_col(&table, &target, col_idx as usize, approx, true)
}

fn hlookup(args: &[EvaluatedArg]) -> FormulaValue {
    let target = scalar_arg(args.first());
    let table = as_matrix(args.get(1));
    let row_idx = match num(&scalar_arg(args.get(2))) {
        Ok(n) => n.trunc() as i64 - 1,
        Err(e) => return err(e),
    };
    if row_idx < 0 {
        return err(FormulaError::Value);
    }
    let approx = args
        .get(3)
        .map(|a| to_boolean(&scalar_arg(Some(a))).unwrap_or(true))
        .unwrap_or(true);
    lookup_row_col(&table, &target, row_idx as usize, approx, false)
}

fn index_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let table = as_matrix(args.first());
    let row_n = match num(&scalar_arg(args.get(1))) {
        Ok(n) => n.trunc() as i64,
        Err(e) => return err(e),
    };
    let col_arg = args.get(2).map(|a| num(&scalar_arg(Some(a))));
    if let Some(Err(e)) = col_arg {
        return err(e);
    }
    let col_n = col_arg.map(|r| r.unwrap().trunc() as i64);
    match col_n {
        None => {
            // 单行/单列：第一参即位置
            if table.len() == 1 {
                return table[0]
                    .get((row_n - 1) as usize)
                    .cloned()
                    .unwrap_or(FormulaValue::Error(FormulaError::Ref));
            }
            if table.first().map(|r| r.len()).unwrap_or(0) == 1 {
                return table
                    .get((row_n - 1) as usize)
                    .and_then(|r| r.first())
                    .cloned()
                    .unwrap_or(FormulaValue::Error(FormulaError::Ref));
            }
            err(FormulaError::Ref)
        }
        Some(c) => {
            let rr = if row_n == 0 { 1 } else { row_n };
            let cc = if c == 0 { 1 } else { c };
            table
                .get((rr - 1) as usize)
                .and_then(|r| r.get((cc - 1) as usize))
                .cloned()
                .unwrap_or(FormulaValue::Error(FormulaError::Ref))
        }
    }
}

fn match_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let target = scalar_arg(args.first());
    let Some(range_arg) = args.get(1) else {
        return err(FormulaError::Na);
    };
    let mt = args
        .get(2)
        .map(|a| {
            num(&scalar_arg(Some(a)))
                .map(|n| n.trunc() as i64)
                .unwrap_or(1)
        })
        .unwrap_or(1);
    let flat = flatten_arg(range_arg);
    if mt == 0 {
        for (i, v) in flat.iter().enumerate() {
            if match_exact(v, &target) {
                return FormulaValue::Number((i + 1) as f64);
            }
        }
        return err(FormulaError::Na);
    }
    if mt > 0 {
        let mut found: i64 = -1;
        for (i, v) in flat.iter().enumerate() {
            match compare_loose(v, &target) {
                Some(std::cmp::Ordering::Greater) => break,
                Some(_) => found = i as i64,
                None => {}
            }
        }
        return if found < 0 {
            err(FormulaError::Na)
        } else {
            FormulaValue::Number((found + 1) as f64)
        };
    }
    // mt < 0：降序，最小的 ≥ target
    let mut found: i64 = -1;
    for (i, v) in flat.iter().enumerate() {
        match compare_loose(v, &target) {
            Some(std::cmp::Ordering::Less) => break,
            Some(_) => found = i as i64,
            None => {}
        }
    }
    if found < 0 {
        err(FormulaError::Na)
    } else {
        FormulaValue::Number((found + 1) as f64)
    }
}

fn lookup_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let target = scalar_arg(args.first());
    let vector = flatten_arg(
        args.get(1)
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    );
    let result = args
        .get(2)
        .map(flatten_arg)
        .unwrap_or_else(|| vector.clone());
    let mut found: i64 = -1;
    for (i, v) in vector.iter().enumerate() {
        match compare_loose(v, &target) {
            Some(std::cmp::Ordering::Greater) => break,
            Some(_) => found = i as i64,
            None => {}
        }
    }
    if found < 0 {
        err(FormulaError::Na)
    } else {
        result
            .get(found as usize)
            .cloned()
            .unwrap_or(FormulaValue::Error(FormulaError::Na))
    }
}

fn xlookup(args: &[EvaluatedArg]) -> FormulaValue {
    let target = scalar_arg(args.first());
    let lookup_arr = flatten_arg(
        args.get(1)
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    );
    let return_arr = flatten_arg(
        args.get(2)
            .unwrap_or(&EvaluatedArg::Value(FormulaValue::Blank)),
    );
    let if_not_found = args
        .get(3)
        .map(|a| scalar_arg(Some(a)))
        .unwrap_or(FormulaValue::Error(FormulaError::Na));
    for (i, v) in lookup_arr.iter().enumerate() {
        if match_exact(v, &target) {
            return return_arr
                .get(i)
                .cloned()
                .unwrap_or(FormulaValue::Error(FormulaError::Na));
        }
    }
    if_not_found
}

fn choose(args: &[EvaluatedArg]) -> FormulaValue {
    let k = match num(&scalar_arg(args.first())) {
        Ok(n) => n.trunc() as i64,
        Err(e) => return err(e),
    };
    if k >= 1 && (k as usize) < args.len() {
        scalar_arg(args.get(k as usize))
    } else {
        err(FormulaError::Value)
    }
}

// ── 注册 ────────────────────────────────────────────────

/// M8 函数集（名称 → 实现）。BuiltinRegistry::new 并入。
pub(crate) fn m8_builtins() -> Vec<(&'static str, FunctionImpl)> {
    macro_rules! f {
        ($name:literal, $imp:expr) => {
            ($name, Rc::new($imp) as FunctionImpl)
        };
    }
    vec![
        // 数学 / 取整
        f!("MROUND", |a: &[EvaluatedArg], _: &EvalContext| two(
            a,
            |n, m| {
                if m == 0.0 {
                    return FormulaValue::Number(0.0);
                }
                if (n < 0.0) != (m < 0.0) {
                    return err(FormulaError::Num);
                }
                FormulaValue::Number(round_half_away(n / m, 0) * m)
            }
        )),
        f!("CEILING", |a: &[EvaluatedArg], _: &EvalContext| {
            let n = match num(&scalar_arg(a.first())) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let s = match a.get(1) {
                Some(x) => match num(&scalar_arg(Some(x))) {
                    Ok(v) => v,
                    Err(e) => return err(e),
                },
                None => 1.0,
            };
            if s == 0.0 {
                return FormulaValue::Number(0.0);
            }
            if (n < 0.0) != (s < 0.0) && n != 0.0 {
                return err(FormulaError::Num);
            }
            FormulaValue::Number((n / s).ceil() * s)
        }),
        f!("FLOOR", |a: &[EvaluatedArg], _: &EvalContext| {
            let n = match num(&scalar_arg(a.first())) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let s = match a.get(1) {
                Some(x) => match num(&scalar_arg(Some(x))) {
                    Ok(v) => v,
                    Err(e) => return err(e),
                },
                None => 1.0,
            };
            if s == 0.0 {
                return FormulaValue::Number(0.0);
            }
            if (n < 0.0) != (s < 0.0) && n != 0.0 {
                return err(FormulaError::Num);
            }
            FormulaValue::Number((n / s).floor() * s)
        }),
        f!("EVEN", |a: &[EvaluatedArg], _: &EvalContext| un(a, |n| {
            let k = (n.abs() / 2.0).ceil() * 2.0;
            FormulaValue::Number(if n < 0.0 { -k } else { k })
        })),
        f!("ODD", |a: &[EvaluatedArg], _: &EvalContext| un(a, |n| {
            let mut k = n.abs().ceil();
            if (k as i64) % 2 == 0 {
                k += 1.0;
            }
            FormulaValue::Number(if n < 0.0 { -k } else { k })
        })),
        f!("SIGN", |a: &[EvaluatedArg], _: &EvalContext| un(a, |n| {
            FormulaValue::Number(if n > 0.0 {
                1.0
            } else if n < 0.0 {
                -1.0
            } else {
                0.0
            })
        })),
        f!(
            "GCD",
            |a: &[EvaluatedArg], _: &EvalContext| match int_list(a) {
                Ok(ns) => FormulaValue::Number(ns.into_iter().fold(0i64, gcd2) as f64),
                Err(e) => err(e),
            }
        ),
        f!(
            "LCM",
            |a: &[EvaluatedArg], _: &EvalContext| match int_list(a) {
                Ok(ns) => FormulaValue::Number(ns.into_iter().fold(1i64, |acc, b| {
                    if acc == 0 || b == 0 {
                        0
                    } else {
                        (acc / gcd2(acc, b) * b).abs()
                    }
                }) as f64),
                Err(e) => err(e),
            }
        ),
        f!(
            "SUMSQ",
            |a: &[EvaluatedArg], _: &EvalContext| match numeric_values(a) {
                Ok(ns) => FormulaValue::Number(ns.iter().map(|n| n * n).sum()),
                Err(e) => err(e),
            }
        ),
        f!("SUMPRODUCT", |a: &[EvaluatedArg], _: &EvalContext| {
            sum_product(a)
        }),
        // 统计
        f!(
            "MEDIAN",
            |a: &[EvaluatedArg], _: &EvalContext| match numeric_values(a) {
                Ok(ns) => median(ns),
                Err(e) => err(e),
            }
        ),
        f!(
            "MODE",
            |a: &[EvaluatedArg], _: &EvalContext| match numeric_values(a) {
                Ok(ns) => mode(&ns),
                Err(e) => err(e),
            }
        ),
        f!(
            "MODE.SNGL",
            |a: &[EvaluatedArg], _: &EvalContext| match numeric_values(a) {
                Ok(ns) => mode(&ns),
                Err(e) => err(e),
            }
        ),
        f!(
            "VARP",
            |a: &[EvaluatedArg], _: &EvalContext| match numeric_values(a) {
                Ok(ns) => variance(&ns, false, |x| x),
                Err(e) => err(e),
            }
        ),
        f!("RANK", |a: &[EvaluatedArg], _: &EvalContext| rank_fn(a)),
        f!("RANK.EQ", |a: &[EvaluatedArg], _: &EvalContext| rank_fn(a)),
        f!("LARGE", |a: &[EvaluatedArg], _: &EvalContext| nth_order(
            a, true
        )),
        f!("SMALL", |a: &[EvaluatedArg], _: &EvalContext| nth_order(
            a, false
        )),
        f!("PERCENTILE", |a: &[EvaluatedArg], _: &EvalContext| {
            percentile(a)
        }),
        f!("PERCENTILE.INC", |a: &[EvaluatedArg], _: &EvalContext| {
            percentile(a)
        }),
        // 多条件聚合
        f!("SUMIFS", |a: &[EvaluatedArg], _: &EvalContext| sumifs(a)),
        f!("COUNTIFS", |a: &[EvaluatedArg], _: &EvalContext| countifs(
            a
        )),
        f!(
            "AVERAGEIF",
            |a: &[EvaluatedArg], _: &EvalContext| averageif(a)
        ),
        f!("AVERAGEIFS", |a: &[EvaluatedArg], _: &EvalContext| {
            averageifs(a)
        }),
        f!(
            "MAXIFS",
            |a: &[EvaluatedArg], _: &EvalContext| extremum_ifs(a, true)
        ),
        f!(
            "MINIFS",
            |a: &[EvaluatedArg], _: &EvalContext| extremum_ifs(a, false)
        ),
        // 文本
        f!("TEXTJOIN", |a: &[EvaluatedArg], _: &EvalContext| textjoin(
            a
        )),
        f!("SUBSTITUTE", |a: &[EvaluatedArg], _: &EvalContext| {
            substitute(a)
        }),
        f!("REPLACE", |a: &[EvaluatedArg], _: &EvalContext| replace_fn(
            a
        )),
        f!("FIND", |a: &[EvaluatedArg], _: &EvalContext| find_in(
            a, true
        )),
        f!("SEARCH", |a: &[EvaluatedArg], _: &EvalContext| find_in(
            a, false
        )),
        f!("REPT", |a: &[EvaluatedArg], _: &EvalContext| {
            let t = match to_text(&scalar_arg(a.first())) {
                Ok(t) => t,
                Err(e) => return err(e),
            };
            let n = match num(&scalar_arg(a.get(1))) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            FormulaValue::Text(if n <= 0 {
                String::new()
            } else {
                t.repeat(n as usize)
            })
        }),
        f!(
            "PROPER",
            |a: &[EvaluatedArg], _: &EvalContext| match to_text(&scalar_arg(a.first())) {
                Ok(t) => FormulaValue::Text(proper(&t)),
                Err(e) => err(e),
            }
        ),
        f!("EXACT", |a: &[EvaluatedArg], _: &EvalContext| {
            let x = match to_text(&scalar_arg(a.first())) {
                Ok(t) => t,
                Err(e) => return err(e),
            };
            let y = match to_text(&scalar_arg(a.get(1))) {
                Ok(t) => t,
                Err(e) => return err(e),
            };
            FormulaValue::Bool(x == y)
        }),
        f!("CHAR", |a: &[EvaluatedArg], _: &EvalContext| {
            let n = match num(&scalar_arg(a.first())) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            if !(1..=65535).contains(&n) {
                return err(FormulaError::Value);
            }
            match char::from_u32(n as u32) {
                Some(c) => FormulaValue::Text(c.to_string()),
                None => err(FormulaError::Value),
            }
        }),
        f!(
            "CODE",
            |a: &[EvaluatedArg], _: &EvalContext| match to_text(&scalar_arg(a.first())) {
                Ok(t) => match t.chars().next() {
                    Some(c) => FormulaValue::Number(c as u32 as f64),
                    None => err(FormulaError::Value),
                },
                Err(e) => err(e),
            }
        ),
        f!("NUMBERVALUE", |a: &[EvaluatedArg], _: &EvalContext| {
            number_value(a)
        }),
        // 逻辑 / 信息
        f!("IFS", |a: &[EvaluatedArg], _: &EvalContext| ifs(a)),
        f!("SWITCH", |a: &[EvaluatedArg], _: &EvalContext| switch_fn(a)),
        f!("ISNA", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(scalar_arg(a.first()).as_error() == Some(FormulaError::Na))
        }),
        f!("ISERR", |a: &[EvaluatedArg], _: &EvalContext| {
            let v = scalar_arg(a.first());
            FormulaValue::Bool(v.is_error() && v.as_error() != Some(FormulaError::Na))
        }),
        f!("ISLOGICAL", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(matches!(scalar_arg(a.first()), FormulaValue::Bool(_)))
        }),
        f!("ISNONTEXT", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Bool(!matches!(scalar_arg(a.first()), FormulaValue::Text(_)))
        }),
        f!("ISODD", |a: &[EvaluatedArg], _: &EvalContext| un(a, |n| {
            FormulaValue::Bool((n.abs().trunc() as i64) % 2 == 1)
        })),
        f!("ISEVEN", |a: &[EvaluatedArg], _: &EvalContext| un(a, |n| {
            FormulaValue::Bool((n.abs().trunc() as i64) % 2 == 0)
        })),
        f!("NA", |_: &[EvaluatedArg], _: &EvalContext| err(
            FormulaError::Na
        )),
        f!("N", |a: &[EvaluatedArg], _: &EvalContext| {
            let v = scalar_arg(a.first());
            match v {
                FormulaValue::Error(e) => err(e),
                FormulaValue::Number(n) => FormulaValue::Number(n),
                FormulaValue::Bool(b) => FormulaValue::Number(if b { 1.0 } else { 0.0 }),
                _ => FormulaValue::Number(0.0),
            }
        }),
        f!("T", |a: &[EvaluatedArg], _: &EvalContext| {
            let v = scalar_arg(a.first());
            match v {
                FormulaValue::Error(e) => err(e),
                FormulaValue::Text(s) => FormulaValue::Text(s),
                _ => FormulaValue::Text(String::new()),
            }
        }),
        f!("TYPE", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Number(match scalar_arg(a.first()) {
                FormulaValue::Number(_) => 1.0,
                FormulaValue::Text(_) => 2.0,
                FormulaValue::Bool(_) => 4.0,
                FormulaValue::Error(_) => 16.0,
                FormulaValue::Blank => 1.0,
            })
        }),
        // 日期时间
        f!("DATE", |a: &[EvaluatedArg], _: &EvalContext| {
            let y = match num(&scalar_arg(a.first())) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            let m = match num(&scalar_arg(a.get(1))) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            let d = match num(&scalar_arg(a.get(2))) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            let yy = if (0..1900).contains(&y) { 1900 + y } else { y };
            FormulaValue::Number(date_overflow(yy, m, d))
        }),
        f!("TIME", |a: &[EvaluatedArg], _: &EvalContext| {
            let h = match num(&scalar_arg(a.first())) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            let mi = match num(&scalar_arg(a.get(1))) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            let s = match num(&scalar_arg(a.get(2))) {
                Ok(n) => n.trunc() as i64,
                Err(e) => return err(e),
            };
            let frac = time_to_fraction(h, mi, s);
            FormulaValue::Number(frac - frac.floor())
        }),
        f!("YEAR", |a: &[EvaluatedArg], _: &EvalContext| date_part(
            a,
            DatePart::Year
        )),
        f!("MONTH", |a: &[EvaluatedArg], _: &EvalContext| date_part(
            a,
            DatePart::Month
        )),
        f!("DAY", |a: &[EvaluatedArg], _: &EvalContext| date_part(
            a,
            DatePart::Day
        )),
        f!("HOUR", |a: &[EvaluatedArg], _: &EvalContext| date_part(
            a,
            DatePart::Hours
        )),
        f!("MINUTE", |a: &[EvaluatedArg], _: &EvalContext| date_part(
            a,
            DatePart::Minutes
        )),
        f!("SECOND", |a: &[EvaluatedArg], _: &EvalContext| date_part(
            a,
            DatePart::Seconds
        )),
        f!("WEEKDAY", |a: &[EvaluatedArg], _: &EvalContext| weekday_fn(
            a
        )),
        f!("EDATE", |a: &[EvaluatedArg], _: &EvalContext| edate(
            a, false
        )),
        f!("EOMONTH", |a: &[EvaluatedArg], _: &EvalContext| edate(
            a, true
        )),
        f!("DATEDIF", |a: &[EvaluatedArg], _: &EvalContext| datedif(a)),
        // 查找 / 引用
        f!("VLOOKUP", |a: &[EvaluatedArg], _: &EvalContext| vlookup(a)),
        f!("HLOOKUP", |a: &[EvaluatedArg], _: &EvalContext| hlookup(a)),
        f!("INDEX", |a: &[EvaluatedArg], _: &EvalContext| index_fn(a)),
        f!("MATCH", |a: &[EvaluatedArg], _: &EvalContext| match_fn(a)),
        f!("LOOKUP", |a: &[EvaluatedArg], _: &EvalContext| lookup_fn(a)),
        f!("XLOOKUP", |a: &[EvaluatedArg], _: &EvalContext| xlookup(a)),
        f!("CHOOSE", |a: &[EvaluatedArg], _: &EvalContext| choose(a)),
        f!("ROWS", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Number(as_matrix(a.first()).len() as f64)
        }),
        f!("COLUMNS", |a: &[EvaluatedArg], _: &EvalContext| {
            FormulaValue::Number(as_matrix(a.first()).first().map(|r| r.len()).unwrap_or(0) as f64)
        }),
        f!(
            "ROW",
            |a: &[EvaluatedArg], ctx: &EvalContext| if a.is_empty() {
                FormulaValue::Number((ctx.row + 1) as f64)
            } else {
                err(FormulaError::Value)
            }
        ),
        f!(
            "COLUMN",
            |a: &[EvaluatedArg], ctx: &EvalContext| if a.is_empty() {
                FormulaValue::Number((ctx.col + 1) as f64)
            } else {
                err(FormulaError::Value)
            }
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::{CellAccessor, Evaluator};
    use crate::functions::BuiltinRegistry;
    use crate::parse::parse_formula;
    use sheet_core::address::parse_addr;
    use sheet_core::date_serial::parts_to_serial;
    use std::collections::HashMap;

    struct MapAccessor {
        cells: HashMap<(u32, u32), FormulaValue>,
        names: HashMap<String, String>,
    }
    fn endpoint(reference: &str) -> (i64, i64) {
        let local = match reference.find('!') {
            Some(b) => &reference[b + 1..],
            None => reference,
        };
        let clean = local.replace('$', "");
        if let Some(p) = parse_addr(&clean) {
            return (p.row as i64, p.col as i64);
        }
        if !clean.is_empty() && clean.chars().all(|c| c.is_ascii_alphabetic()) {
            let col = sheet_core::address::label_to_col(&clean).unwrap_or(0);
            return (-1, col as i64);
        }
        if let Ok(r) = clean.parse::<i64>() {
            return (r - 1, -1);
        }
        (-1, -1)
    }
    impl CellAccessor for MapAccessor {
        fn get_cell_value(&self, reference: &str) -> FormulaValue {
            let (r, c) = endpoint(reference);
            if r < 0 || c < 0 {
                return FormulaValue::Error(FormulaError::Ref);
            }
            self.cells
                .get(&(r as u32, c as u32))
                .cloned()
                .unwrap_or(FormulaValue::Blank)
        }
        fn get_range_values(&self, start: &str, end: &str) -> Vec<Vec<FormulaValue>> {
            let (ar, ac) = endpoint(start);
            let (br, bc) = endpoint(end);
            let (rows, cols) = (8i64, 8i64);
            let ar = if ar < 0 { 0 } else { ar };
            let br = if br < 0 { rows - 1 } else { br };
            let ac = if ac < 0 { 0 } else { ac };
            let bc = if bc < 0 { cols - 1 } else { bc };
            let (r1, r2) = (ar.min(br), ar.max(br));
            let (c1, c2) = (ac.min(bc), ac.max(bc));
            (r1..=r2)
                .map(|r| {
                    (c1..=c2)
                        .map(|c| {
                            self.cells
                                .get(&(r as u32, c as u32))
                                .cloned()
                                .unwrap_or(FormulaValue::Blank)
                        })
                        .collect()
                })
                .collect()
        }
        fn resolve_name_ref(&self, name: &str) -> Option<String> {
            self.names.get(&name.to_uppercase()).cloned()
        }
    }

    fn ev(src: &str, cells: &[((u32, u32), FormulaValue)], names: &[(&str, &str)]) -> FormulaValue {
        let acc = MapAccessor {
            cells: cells.iter().cloned().collect(),
            names: names
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let reg = BuiltinRegistry::new();
        let e = Evaluator::new(&reg);
        let ctx = EvalContext {
            accessor: &acc,
            row: 0,
            col: 0,
            sheet_name: "Sheet1",
        };
        e.evaluate(&parse_formula(src).unwrap(), &ctx)
    }
    fn n(v: FormulaValue) -> f64 {
        match v {
            FormulaValue::Number(x) => x,
            o => panic!("expected number: {o:?}"),
        }
    }
    fn grid() -> Vec<((u32, u32), FormulaValue)> {
        vec![
            ((0, 0), 10.into()),
            ((1, 0), 20.into()),
            ((2, 0), 30.into()),
            ((3, 0), 40.into()),
            ((4, 0), 50.into()),
            ((0, 1), "x".into()),
            ((1, 1), "y".into()),
            ((2, 1), "x".into()),
            ((3, 1), "y".into()),
            ((4, 1), "x".into()),
        ]
    }

    #[test]
    fn math_rounding() {
        assert_eq!(n(ev("MROUND(10, 3)", &[], &[])), 9.0);
        assert_eq!(n(ev("MROUND(-10, -3)", &[], &[])), -9.0);
        assert_eq!(n(ev("CEILING(2.1, 1)", &[], &[])), 3.0);
        assert_eq!(n(ev("FLOOR(2.9, 1)", &[], &[])), 2.0);
        assert_eq!(n(ev("CEILING(-2.1, -1)", &[], &[])), -3.0);
        assert_eq!(n(ev("EVEN(3)", &[], &[])), 4.0);
        assert_eq!(n(ev("ODD(2)", &[], &[])), 3.0);
        assert_eq!(n(ev("EVEN(-1)", &[], &[])), -2.0);
        assert_eq!(n(ev("SIGN(-5)", &[], &[])), -1.0);
        assert_eq!(n(ev("SIGN(0)", &[], &[])), 0.0);
        assert_eq!(n(ev("GCD(12, 18)", &[], &[])), 6.0);
        assert_eq!(n(ev("LCM(4, 6)", &[], &[])), 12.0);
        assert_eq!(n(ev("SUMSQ(3, 4)", &[], &[])), 25.0);
        assert_eq!(n(ev("PRODUCT(2, 3, 4)", &[], &[])), 24.0);
    }

    #[test]
    fn statistics() {
        assert_eq!(n(ev("MEDIAN(1, 2, 3, 4)", &[], &[])), 2.5);
        assert_eq!(n(ev("MEDIAN(1, 2, 3)", &[], &[])), 2.0);
        assert_eq!(n(ev("MODE(1, 2, 2, 3)", &[], &[])), 2.0);
        assert_eq!(n(ev("VAR(2, 4, 6)", &[], &[])), 4.0);
        assert!((n(ev("VARP(2, 4, 6)", &[], &[])) - 2.6667).abs() < 1e-3);
        assert_eq!(n(ev("STDEV(2, 4, 6)", &[], &[])), 2.0);
        let g = grid();
        assert_eq!(n(ev("RANK(30, A1:A5)", &g, &[])), 3.0);
        assert_eq!(n(ev("LARGE(A1:A5, 2)", &g, &[])), 40.0);
        assert_eq!(n(ev("SMALL(A1:A5, 1)", &g, &[])), 10.0);
        assert_eq!(n(ev("PERCENTILE(A1:A5, 0.5)", &g, &[])), 30.0);
        let c = [
            ((0u32, 0u32), 1.into()),
            ((1, 0), 2.into()),
            ((0, 1), 3.into()),
            ((1, 1), 4.into()),
        ];
        assert_eq!(n(ev("SUMPRODUCT(A1:A2, B1:B2)", &c, &[])), 11.0);
    }

    #[test]
    fn multi_criteria() {
        let g = grid();
        assert_eq!(n(ev("SUMIFS(A1:A5, B1:B5, \"x\")", &g, &[])), 90.0);
        assert_eq!(n(ev("COUNTIFS(B1:B5, \"x\")", &g, &[])), 3.0);
        assert_eq!(n(ev("SUMIFS(A1:A5, A1:A5, \">=30\")", &g, &[])), 120.0);
        assert_eq!(n(ev("AVERAGEIF(B1:B5, \"x\", A1:A5)", &g, &[])), 30.0);
        assert_eq!(n(ev("AVERAGEIFS(A1:A5, B1:B5, \"y\")", &g, &[])), 30.0);
        assert_eq!(n(ev("MAXIFS(A1:A5, B1:B5, \"x\")", &g, &[])), 50.0);
        assert_eq!(n(ev("MINIFS(A1:A5, B1:B5, \"y\")", &g, &[])), 20.0);
    }

    #[test]
    fn text_functions() {
        assert_eq!(
            ev("TEXTJOIN(\"-\", TRUE, \"a\", \"\", \"b\")", &[], &[]),
            "a-b".into()
        );
        assert_eq!(
            ev("TEXTJOIN(\"-\", FALSE, \"a\", \"\", \"b\")", &[], &[]),
            "a--b".into()
        );
        assert_eq!(
            ev("SUBSTITUTE(\"a-b-c\", \"-\", \"+\")", &[], &[]),
            "a+b+c".into()
        );
        assert_eq!(
            ev("SUBSTITUTE(\"a-b-c\", \"-\", \"+\", 2)", &[], &[]),
            "a-b+c".into()
        );
        assert_eq!(
            ev("REPLACE(\"abcdef\", 2, 3, \"XY\")", &[], &[]),
            "aXYef".into()
        );
        assert_eq!(n(ev("FIND(\"b\", \"abc\")", &[], &[])), 2.0);
        assert_eq!(
            ev("FIND(\"B\", \"abc\")", &[], &[]),
            FormulaValue::Error(FormulaError::Value)
        );
        assert_eq!(n(ev("SEARCH(\"B\", \"abc\")", &[], &[])), 2.0);
        assert_eq!(ev("REPT(\"ab\", 3)", &[], &[]), "ababab".into());
        assert_eq!(
            ev("PROPER(\"hello WORLD\")", &[], &[]),
            "Hello World".into()
        );
        assert_eq!(
            ev("EXACT(\"a\", \"A\")", &[], &[]),
            FormulaValue::Bool(false)
        );
        assert_eq!(ev("CHAR(65)", &[], &[]), "A".into());
        assert_eq!(n(ev("CODE(\"A\")", &[], &[])), 65.0);
        assert_eq!(n(ev("NUMBERVALUE(\"1,234.5\")", &[], &[])), 1234.5);
    }

    #[test]
    fn logic_info() {
        assert_eq!(ev("IFS(1>2, \"a\", 2>1, \"b\")", &[], &[]), "b".into());
        assert_eq!(
            ev("IFS(1>2, \"a\", 3>4, \"b\")", &[], &[]),
            FormulaValue::Error(FormulaError::Na)
        );
        assert_eq!(
            ev("SWITCH(2, 1, \"one\", 2, \"two\", \"def\")", &[], &[]),
            "two".into()
        );
        assert_eq!(ev("SWITCH(9, 1, \"one\", \"def\")", &[], &[]), "def".into());
        assert_eq!(ev("ISNA(NA())", &[], &[]), FormulaValue::Bool(true));
        assert_eq!(ev("ISLOGICAL(TRUE)", &[], &[]), FormulaValue::Bool(true));
        assert_eq!(ev("ISODD(3)", &[], &[]), FormulaValue::Bool(true));
        assert_eq!(ev("ISEVEN(4)", &[], &[]), FormulaValue::Bool(true));
        assert_eq!(n(ev("N(TRUE)", &[], &[])), 1.0);
        assert_eq!(n(ev("TYPE(\"hi\")", &[], &[])), 2.0);
    }

    #[test]
    fn datetime() {
        let d = date_to_serial(2024, 3, 15);
        assert_eq!(n(ev("DATE(2024, 3, 15)", &[], &[])), d);
        assert_eq!(n(ev(&format!("YEAR({d})"), &[], &[])), 2024.0);
        assert_eq!(n(ev(&format!("MONTH({d})"), &[], &[])), 3.0);
        assert_eq!(n(ev(&format!("DAY({d})"), &[], &[])), 15.0);
        assert_eq!(
            n(ev("DATE(2024, 13, 1)", &[], &[])),
            date_to_serial(2025, 1, 1)
        );
        assert_eq!(
            n(ev(
                &format!("WEEKDAY({})", date_to_serial(2024, 1, 15)),
                &[],
                &[]
            )),
            2.0
        );
        assert_eq!(
            n(ev(
                &format!("EDATE({}, 1)", date_to_serial(2024, 1, 31)),
                &[],
                &[]
            )),
            date_to_serial(2024, 2, 29)
        );
        assert_eq!(
            n(ev(
                &format!("EOMONTH({}, 0)", date_to_serial(2024, 2, 10)),
                &[],
                &[]
            )),
            date_to_serial(2024, 2, 29)
        );
        assert_eq!(
            n(ev(
                &format!(
                    "DATEDIF({}, {}, \"M\")",
                    date_to_serial(2024, 1, 1),
                    date_to_serial(2024, 3, 1)
                ),
                &[],
                &[]
            )),
            2.0
        );
        let t = parts_to_serial(2024, 6, 15, 13, 45, 30);
        assert_eq!(n(ev(&format!("HOUR({t})"), &[], &[])), 13.0);
        assert_eq!(n(ev(&format!("MINUTE({t})"), &[], &[])), 45.0);
    }

    #[test]
    fn lookup_reference() {
        let t = vec![
            ((0u32, 0u32), "apple".into()),
            ((0, 1), 3.into()),
            ((0, 2), "red".into()),
            ((1, 0), "banana".into()),
            ((1, 1), 5.into()),
            ((1, 2), "yellow".into()),
            ((2, 0), "cherry".into()),
            ((2, 1), 7.into()),
            ((2, 2), "dark".into()),
        ];
        assert_eq!(n(ev("VLOOKUP(\"banana\", A1:C3, 2, FALSE)", &t, &[])), 5.0);
        assert_eq!(
            ev("VLOOKUP(\"banana\", A1:C3, 3, FALSE)", &t, &[]),
            "yellow".into()
        );
        assert_eq!(
            ev("VLOOKUP(\"grape\", A1:C3, 2, FALSE)", &t, &[]),
            FormulaValue::Error(FormulaError::Na)
        );
        assert_eq!(ev("INDEX(A1:C3, 2, 3)", &t, &[]), "yellow".into());
        assert_eq!(n(ev("MATCH(\"cherry\", A1:A3, 0)", &t, &[])), 3.0);
        assert_eq!(
            n(ev("INDEX(B1:B3, MATCH(\"cherry\", A1:A3, 0))", &t, &[])),
            7.0
        );
        assert_eq!(ev("CHOOSE(2, \"a\", \"b\", \"c\")", &[], &[]), "b".into());
        assert_eq!(n(ev("XLOOKUP(\"cherry\", A1:A3, B1:B3)", &t, &[])), 7.0);
        assert_eq!(n(ev("XLOOKUP(\"grape\", A1:A3, B1:B3, -1)", &t, &[])), -1.0);
        assert_eq!(n(ev("ROWS(A1:C3)", &t, &[])), 3.0);
        assert_eq!(n(ev("COLUMNS(A1:C3)", &t, &[])), 3.0);
        let v = [
            ((0u32, 0u32), 1.into()),
            ((1, 0), 3.into()),
            ((2, 0), 5.into()),
            ((0, 1), "a".into()),
            ((1, 1), "b".into()),
            ((2, 1), "c".into()),
        ];
        assert_eq!(ev("LOOKUP(4, A1:A3, B1:B3)", &v, &[]), "b".into());
    }

    #[test]
    fn whole_col_and_names() {
        let g = grid();
        assert_eq!(n(ev("SUM(A:A)", &g, &[])), 150.0);
        assert_eq!(n(ev("COUNT(A:A)", &g, &[])), 5.0);
        let c = [
            ((0u32, 0u32), 1.into()),
            ((0, 1), 2.into()),
            ((0, 2), 3.into()),
        ];
        assert_eq!(n(ev("SUM(1:1)", &c, &[])), 6.0);
        assert_eq!(n(ev("SUM({1,2;3,4})", &[], &[])), 10.0);
        assert_eq!(n(ev("SUMPRODUCT({1,2,3}, {4,5,6})", &[], &[])), 32.0);
        assert_eq!(n(ev("MAX({5,3,9,1})", &[], &[])), 9.0);
        let names = [("SALES", "A1:A5"), ("TAXRATE", "A1")];
        assert_eq!(n(ev("SUM(SALES)", &g, &names)), 150.0);
        assert_eq!(n(ev("TAXRATE*2", &g, &names)), 20.0);
        assert_eq!(n(ev("AVERAGE(SALES)", &g, &names)), 30.0);
        assert_eq!(
            ev("SUM(UNKNOWNRANGE)", &g, &[]),
            FormulaValue::Error(FormulaError::Name)
        );
    }
}
