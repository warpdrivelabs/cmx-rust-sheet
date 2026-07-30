//! M17 函数库大扩容：数学/三角/组合/进制、财务年金/折旧/投资、统计扩展、
//! 数据库 D 函数、现代文本+引用+信息 —— 五族对拍 cmx-megasheet 的 builtins/*.ts。
//!
//! 对齐 Excel 语义：域外/结果非有限 → #NUM!；错误参数短路上抛。迭代类
//! （RATE/IRR/XIRR）用 Newton + 二分兜底，不收敛 → #NUM!。正态分布用
//! Abramowitz–Stegun CDF + Acklam 逆 CDF 近似。为对齐 TS 注册规模（130 核心 +
//! 142 五族 = 272），本模块另补 10 个 TS 核心里存在、Rust 基础集尚缺的函数
//! （STDEV.P/S、VAR.P/S、NOW、TODAY、DATEVALUE、NETWORKDAYS、UNICHAR、UNICODE）。
//! 纯逻辑、零 DOM。作为独立模块并入 BuiltinRegistry（镜像 TS 的 `...MATH_BUILTINS` 展开）。

use std::rc::Rc;

use sheet_core::address::col_to_label;
use sheet_core::date_serial::{date_to_serial, parts_to_serial, serial_to_parts};

use crate::evaluator::{
    as_matrix, flatten_arg, flatten_args, scalar_arg, EvalContext, EvaluatedArg, FunctionImpl,
};
use crate::functions::numeric_values;
use crate::value::{to_boolean, to_number, to_text, FormulaError, FormulaValue};

// ── 通用小助手 ──────────────────────────────────────────

fn err(e: FormulaError) -> FormulaValue {
    FormulaValue::Error(e)
}

/// 有限收窄：NaN/±Infinity → #NUM!。
fn finite(x: f64) -> FormulaValue {
    if x.is_finite() {
        FormulaValue::Number(x)
    } else {
        err(FormulaError::Num)
    }
}

/// 必备数值参：区域取左上角，强制为数字（错误透传）。
fn req_num(args: &[EvaluatedArg], i: usize) -> Result<f64, FormulaError> {
    to_number(&scalar_arg(args.get(i)))
}

/// 可选数值参：缺省/空 → default；否则取左上角强制数字。
fn opt_num(args: &[EvaluatedArg], i: usize, default: f64) -> Result<f64, FormulaError> {
    match args.get(i) {
        None => Ok(default),
        Some(a) => {
            let v = scalar_arg(Some(a));
            if v.is_blank() {
                Ok(default)
            } else {
                to_number(&v)
            }
        }
    }
}

/// 单数值参函数模板：取参→错误透传→施 f。f 自返 FormulaValue（可 #NUM! 表域外）。
fn m1(args: &[EvaluatedArg], f: impl Fn(f64) -> FormulaValue) -> FormulaValue {
    match req_num(args, 0) {
        Ok(n) => f(n),
        Err(e) => err(e),
    }
}

/// 双数值参函数模板。
fn m2(args: &[EvaluatedArg], f: impl Fn(f64, f64) -> FormulaValue) -> FormulaValue {
    let a = match req_num(args, 0) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let b = match req_num(args, 1) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    f(a, b)
}

/// 布尔取值（ADDRESS/INDIRECT 的 a1 参、norm 的 cumulative 参）：错误 → 默认 true。
fn bool_of(arg: &EvaluatedArg) -> bool {
    to_boolean(&scalar_arg(Some(arg))).unwrap_or(true)
}

const PI: f64 = std::f64::consts::PI;

// ── 阶乘 / 组合数（乘法式，避免大数溢出）──────────────────

fn factorial(n: f64) -> FormulaValue {
    let n = n.trunc() as i64;
    if n < 0 {
        return err(FormulaError::Num);
    }
    let mut r = 1.0f64;
    let mut i = 2i64;
    while i <= n {
        r *= i as f64;
        if !r.is_finite() {
            return err(FormulaError::Num);
        }
        i += 1;
    }
    FormulaValue::Number(r)
}

fn fact_double(n: f64) -> FormulaValue {
    let n = n.trunc() as i64;
    if n < -1 {
        return err(FormulaError::Num);
    }
    let mut r = 1.0f64;
    let mut i = n;
    while i > 1 {
        r *= i as f64;
        if !r.is_finite() {
            return err(FormulaError::Num);
        }
        i -= 2;
    }
    FormulaValue::Number(r)
}

/// COMBIN(n,k)=n!/(k!(n-k)!)，乘法式逐步约分。
fn combin(n: f64, k: f64) -> FormulaValue {
    let n = n.trunc() as i64;
    let mut k = k.trunc() as i64;
    if n < 0 || k < 0 || k > n {
        return err(FormulaError::Num);
    }
    k = k.min(n - k);
    let mut r = 1.0f64;
    for i in 1..=k {
        r = r * (n - k + i) as f64 / i as f64;
        if !r.is_finite() {
            return err(FormulaError::Num);
        }
    }
    FormulaValue::Number(r.round())
}

fn permut(n: f64, k: f64) -> FormulaValue {
    let n = n.trunc() as i64;
    let k = k.trunc() as i64;
    if n < 0 || k < 0 || k > n {
        return err(FormulaError::Num);
    }
    let mut r = 1.0f64;
    for i in 0..k {
        r *= (n - i) as f64;
        if !r.is_finite() {
            return err(FormulaError::Num);
        }
    }
    FormulaValue::Number(r)
}

// ── 进制转换 ─────────────────────────────────────────────

fn to_base_text(value: f64, radix: f64, min_len: i64) -> FormulaValue {
    let value = value.trunc() as i64;
    let radix = radix.trunc() as i64;
    if !(2..=36).contains(&radix) {
        return err(FormulaError::Num);
    }
    if value < 0 {
        return err(FormulaError::Num);
    }
    let mut s = to_radix_string(value as u64, radix as u32);
    if min_len > s.len() as i64 {
        let pad = min_len as usize - s.len();
        s = "0".repeat(pad) + &s;
    }
    FormulaValue::Text(s)
}

/// u64 → radix 字符串（大写，对齐 JS Number.toString(radix).toUpperCase()）。
fn to_radix_string(mut v: u64, radix: u32) -> String {
    if v == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut buf = Vec::new();
    while v > 0 {
        let d = (v % radix as u64) as usize;
        buf.push(DIGITS[d]);
        v /= radix as u64;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

/// DECIMAL：radix 文本 → 数值（对齐 JS parseInt(t, r)：逐位解析，遇非法位止）。
fn parse_radix(t: &str, radix: u32) -> Option<f64> {
    let s = t.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut acc = 0.0f64;
    let mut any = false;
    for ch in body.chars() {
        let d = ch.to_digit(36)?;
        if d >= radix {
            // parseInt 在首个非法位停止；若尚无有效位则 NaN
            break;
        }
        acc = acc * radix as f64 + d as f64;
        any = true;
    }
    if !any {
        return None;
    }
    Some(if neg { -acc } else { acc })
}

const ROMAN_MAP: [(i64, &str); 13] = [
    (1000, "M"),
    (900, "CM"),
    (500, "D"),
    (400, "CD"),
    (100, "C"),
    (90, "XC"),
    (50, "L"),
    (40, "XL"),
    (10, "X"),
    (9, "IX"),
    (5, "V"),
    (4, "IV"),
    (1, "I"),
];

fn to_roman(n: f64) -> FormulaValue {
    let mut n = n.trunc() as i64;
    if !(0..=3999).contains(&n) {
        return err(FormulaError::Value);
    }
    if n == 0 {
        return FormulaValue::Text(String::new());
    }
    let mut out = String::new();
    for (v, sym) in ROMAN_MAP {
        while n >= v {
            out.push_str(sym);
            n -= v;
        }
    }
    FormulaValue::Text(out)
}

fn from_roman(text: &str) -> FormulaValue {
    let s = text.trim().to_uppercase();
    let val = |c: char| -> Option<i64> {
        Some(match c {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => return None,
        })
    };
    let mut sign = 1i64;
    let mut str_body: &str = &s;
    if let Some(rest) = str_body.strip_prefix('-') {
        sign = -1;
        str_body = rest;
    }
    let chars: Vec<char> = str_body.chars().collect();
    let mut total = 0i64;
    let mut prev = 0i64;
    for &c in chars.iter().rev() {
        let v = match val(c) {
            Some(v) => v,
            None => return err(FormulaError::Value),
        };
        if v < prev {
            total -= v;
        } else {
            total += v;
            prev = v;
        }
    }
    FormulaValue::Number((sign * total) as f64)
}

// ── CEILING/FLOOR 现代化家族 ─────────────────────────────

/// CEILING.MATH / FLOOR.MATH：significance 取绝对值；mode≠0 时负数「远离零」。
fn ceiling_floor_math(args: &[EvaluatedArg], ceil: bool) -> FormulaValue {
    let x = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let sig_raw = match opt_num(args, 1, 1.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let mode_raw = match opt_num(args, 2, 0.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let s = sig_raw.abs();
    if s == 0.0 {
        return FormulaValue::Number(0.0);
    }
    let q = x / s;
    let r = if ceil {
        if x >= 0.0 || mode_raw == 0.0 {
            q.ceil()
        } else {
            q.floor()
        }
    } else if x >= 0.0 || mode_raw == 0.0 {
        q.floor()
    } else {
        q.ceil()
    };
    finite(r * s)
}

/// CEILING.PRECISE / FLOOR.PRECISE / ISO.CEILING：significance 取绝对值，恒朝 ±∞。
fn precise(args: &[EvaluatedArg], ceil: bool) -> FormulaValue {
    let x = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let sig_raw = match opt_num(args, 1, 1.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let s = sig_raw.abs();
    if s == 0.0 {
        return FormulaValue::Number(0.0);
    }
    let q = x / s;
    finite((if ceil { q.ceil() } else { q.floor() }) * s)
}

// ── 数学族注册体 ─────────────────────────────────────────

fn multinomial(args: &[EvaluatedArg]) -> FormulaValue {
    let mut sum = 0.0f64;
    let mut denom = 1.0f64;
    for v in flatten_args(args) {
        let n = match to_number(&v) {
            Ok(n) => n,
            Err(e) => return err(e),
        };
        let t = n.trunc();
        if t < 0.0 {
            return err(FormulaError::Num);
        }
        sum += t;
        match factorial(t) {
            FormulaValue::Number(f) => denom *= f,
            other => return other,
        }
    }
    match factorial(sum) {
        FormulaValue::Number(total) => finite(total / denom),
        other => other,
    }
}

fn seriessum(args: &[EvaluatedArg]) -> FormulaValue {
    let x = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let n_start = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let m = match req_num(args, 2) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let coeffs = match args.get(3) {
        Some(a) => flatten_arg(a),
        None => Vec::new(),
    };
    let mut total = 0.0f64;
    for (i, c) in coeffs.iter().enumerate() {
        let cv = match to_number(c) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        total += cv * x.powf(n_start + i as f64 * m);
    }
    finite(total)
}

// ── 财务：年金核心恒等式 ─────────────────────────────────

fn pow1p(rate: f64, n: f64) -> f64 {
    (1.0 + rate).powf(n)
}

fn fv_of(rate: f64, nper: f64, pmt: f64, pv: f64, type_: f64) -> f64 {
    if rate == 0.0 {
        return -(pv + pmt * nper);
    }
    let f = pow1p(rate, nper);
    -(pv * f + pmt * (1.0 + rate * type_) * (f - 1.0) / rate)
}

fn pmt_of(rate: f64, nper: f64, pv: f64, fv: f64, type_: f64) -> f64 {
    if nper == 0.0 {
        return f64::NAN;
    }
    if rate == 0.0 {
        return -(pv + fv) / nper;
    }
    let f = pow1p(rate, nper);
    -(pv * f + fv) * rate / ((1.0 + rate * type_) * (f - 1.0))
}

fn pv_of(rate: f64, nper: f64, pmt: f64, fv: f64, type_: f64) -> f64 {
    if rate == 0.0 {
        return -(fv + pmt * nper);
    }
    let f = pow1p(rate, nper);
    -(fv + pmt * (1.0 + rate * type_) * (f - 1.0) / rate) / f
}

fn nper_of(rate: f64, pmt: f64, pv: f64, fv: f64, type_: f64) -> f64 {
    if rate == 0.0 {
        if pmt == 0.0 {
            return f64::NAN;
        }
        return -(pv + fv) / pmt;
    }
    let t = pmt * (1.0 + rate * type_) / rate;
    let num = t - fv;
    let den = pv + t;
    if den == 0.0 || num / den <= 0.0 {
        return f64::NAN;
    }
    (num / den).ln() / (1.0 + rate).ln()
}

/// IPMT/PPMT 用：第 per 期利息（错误表域外 #NUM!）。
fn ipmt_of(
    rate: f64,
    per: f64,
    nper: f64,
    pv: f64,
    fv: f64,
    type_: f64,
) -> Result<f64, FormulaError> {
    if per < 1.0 || per > nper {
        return Err(FormulaError::Num);
    }
    let pmt = pmt_of(rate, nper, pv, fv, type_);
    let ip = if type_ == 0.0 {
        let bal = fv_of(rate, per - 1.0, pmt, pv, 0.0);
        bal * rate
    } else if per == 1.0 {
        0.0
    } else {
        let bal = fv_of(rate, per - 2.0, pmt, pv, 1.0);
        bal * rate
    };
    Ok(ip)
}

/// Newton 迭代求根（数值导数），发散退二分。
fn solve_rate(f: impl Fn(f64) -> f64, guess: f64) -> f64 {
    let mut r = guess;
    for _ in 0..80 {
        let y = f(r);
        if !y.is_finite() {
            break;
        }
        if y.abs() < 1e-9 {
            return r;
        }
        let dr = 1e-6;
        let dy = (f(r + dr) - y) / dr;
        if !dy.is_finite() || dy == 0.0 {
            break;
        }
        let next = r - y / dy;
        if !next.is_finite() {
            break;
        }
        if (next - r).abs() < 1e-9 {
            return next;
        }
        r = next;
    }
    // 二分兜底
    let mut lo = -0.999999f64;
    let mut hi = 10.0f64;
    let mut flo = f(lo);
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        let fm = f(mid);
        if !fm.is_finite() {
            return f64::NAN;
        }
        if fm.abs() < 1e-9 {
            return mid;
        }
        if (flo < 0.0) != (fm < 0.0) {
            hi = mid;
        } else {
            lo = mid;
            flo = fm;
        }
    }
    f64::NAN
}

fn npv_at(rate: f64, flows: &[f64]) -> f64 {
    let mut sum = 0.0;
    for (i, cf) in flows.iter().enumerate() {
        sum += cf / (1.0 + rate).powi(i as i32 + 1);
    }
    sum
}

fn xnpv_at(rate: f64, flows: &[f64], dates: &[f64]) -> f64 {
    let d0 = dates[0];
    let mut sum = 0.0;
    for i in 0..flows.len() {
        sum += flows[i] / (1.0 + rate).powf((dates[i] - d0) / 365.0);
    }
    sum
}

fn num_result(x: f64) -> FormulaValue {
    if x.is_finite() {
        FormulaValue::Number(x)
    } else {
        err(FormulaError::Num)
    }
}

/// 严格数值向量：非数值文本 → #VALUE!；空跳过；错误短路。
fn strict_numbers(values: &[FormulaValue]) -> Result<Vec<f64>, FormulaError> {
    let mut out = Vec::new();
    for v in values {
        if let FormulaValue::Error(e) = v {
            return Err(*e);
        }
        if v.is_blank() {
            continue;
        }
        out.push(to_number(v)?);
    }
    Ok(out)
}

/// CUMIPMT / CUMPRINC：区间 [start,end] 累计利息/本金。
fn cumulative(args: &[EvaluatedArg], interest: bool) -> FormulaValue {
    let rate = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let nper = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let pv = match req_num(args, 2) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let start = match req_num(args, 3) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let end = match req_num(args, 4) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let type_ = match opt_num(args, 5, 0.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let s = start.trunc() as i64;
    let e = end.trunc() as i64;
    if rate <= 0.0 || nper <= 0.0 || pv <= 0.0 {
        return err(FormulaError::Num);
    }
    if s < 1 || e < s || (e as f64) > nper {
        return err(FormulaError::Num);
    }
    let pmt = pmt_of(rate, nper, pv, 0.0, type_);
    let mut total = 0.0;
    for p in s..=e {
        let ip = match ipmt_of(rate, p as f64, nper, pv, 0.0, type_) {
            Ok(v) => v,
            Err(er) => return err(er),
        };
        total += if interest { ip } else { pmt - ip };
    }
    num_result(total)
}

fn depreciation_db(args: &[EvaluatedArg]) -> FormulaValue {
    let cost = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let salvage = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let life = match req_num(args, 2) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let per = match req_num(args, 3) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let month = match opt_num(args, 4, 12.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if life <= 0.0 || cost < 0.0 {
        return err(FormulaError::Num);
    }
    if cost == 0.0 {
        return FormulaValue::Number(0.0);
    }
    let rate = ((1.0 - (salvage / cost).powf(1.0 / life)) * 1000.0).round() / 1000.0;
    let mut book = cost;
    let mut dep = 0.0;
    let p_int = per.trunc() as i64;
    let life_int = life.trunc() as i64;
    for p in 1..=p_int {
        if p == 1 {
            dep = cost * rate * month / 12.0;
        } else if p == life_int + 1 {
            dep = book * rate * (12.0 - month) / 12.0;
        } else {
            dep = book * rate;
        }
        book -= dep;
    }
    num_result(dep)
}

fn depreciation_ddb(args: &[EvaluatedArg]) -> FormulaValue {
    let cost = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let salvage = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let life = match req_num(args, 2) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let per = match req_num(args, 3) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let factor = match opt_num(args, 4, 2.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if life <= 0.0 || per < 1.0 || per > life {
        return err(FormulaError::Num);
    }
    let mut book = cost;
    let mut dep = 0.0;
    for _p in 1..=(per.trunc() as i64) {
        dep = (book * factor / life).min((book - salvage).max(0.0));
        book -= dep;
    }
    num_result(dep)
}

fn dollar_de(args: &[EvaluatedArg]) -> FormulaValue {
    let frac = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let denom = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let d = denom.trunc();
    if d < 0.0 {
        return err(FormulaError::Num);
    }
    if d == 0.0 {
        return err(FormulaError::Div0);
    }
    let whole = frac.trunc();
    let frac_part = frac - whole;
    let digits = (d.log10()).ceil();
    finite(whole + (frac_part * 10f64.powf(digits)) / d)
}

fn dollar_fr(args: &[EvaluatedArg]) -> FormulaValue {
    let dec = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let denom = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let d = denom.trunc();
    if d < 0.0 {
        return err(FormulaError::Num);
    }
    if d == 0.0 {
        return err(FormulaError::Div0);
    }
    let whole = dec.trunc();
    let frac_part = dec - whole;
    let digits = (d.log10()).ceil();
    finite(whole + (frac_part * d) / 10f64.powf(digits))
}

// ── 统计：取数与回归 ─────────────────────────────────────

/// 「A 变体」取数：文本→0，布尔→1/0，空跳过；错误短路。
fn collect_numbers_a(args: &[EvaluatedArg]) -> Result<Vec<f64>, FormulaError> {
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
                let n = s.trim().parse::<f64>().ok();
                out.push(if let Some(x) = n {
                    if x.is_finite() {
                        x
                    } else {
                        0.0
                    }
                } else {
                    0.0
                });
            }
            FormulaValue::Blank => {}
            FormulaValue::Error(e) => return Err(e),
        }
    }
    Ok(out)
}

fn mean(ns: &[f64]) -> f64 {
    ns.iter().sum::<f64>() / ns.len() as f64
}

/// 「数值收集」——跳过空/纯文本，错误短路（对齐 collectNumbers）。
fn collect_numbers(args: &[EvaluatedArg]) -> Result<Vec<f64>, FormulaError> {
    numeric_values(args)
}

/// 从单个参数取数值（跳过空/文本/布尔转数）——numsFrom 语义。
fn nums_from(arg: Option<&EvaluatedArg>) -> Vec<f64> {
    let flat = match arg {
        Some(a) => flatten_arg(a),
        None => Vec::new(),
    };
    flat.iter()
        .filter(|v| !v.is_blank())
        .filter_map(|v| match v {
            FormulaValue::Number(n) => Some(*n),
            FormulaValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            FormulaValue::Text(s) => s.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
            _ => None,
        })
        .collect()
}

struct Regression {
    slope: f64,
    intercept: f64,
    r: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
    n: usize,
}

/// 两参平行向量（严格等长；短路错误；成对缺失跳过）。
fn pair_vectors(
    a: Option<&EvaluatedArg>,
    b: Option<&EvaluatedArg>,
) -> Result<(Vec<f64>, Vec<f64>), FormulaError> {
    let ya = a.map(flatten_arg).unwrap_or_default();
    let xb = b.map(flatten_arg).unwrap_or_default();
    if ya.len() != xb.len() {
        return Err(FormulaError::Na);
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for i in 0..ya.len() {
        let yv = &ya[i];
        let xv = &xb[i];
        if let FormulaValue::Error(e) = yv {
            return Err(*e);
        }
        if let FormulaValue::Error(e) = xv {
            return Err(*e);
        }
        if yv.is_blank() || xv.is_blank() {
            continue;
        }
        let yn = match yv {
            FormulaValue::Bool(bl) => Some(if *bl { 1.0 } else { 0.0 }),
            FormulaValue::Number(n) => Some(*n),
            FormulaValue::Text(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        };
        let xn = match xv {
            FormulaValue::Bool(bl) => Some(if *bl { 1.0 } else { 0.0 }),
            FormulaValue::Number(n) => Some(*n),
            FormulaValue::Text(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        };
        match (yn, xn) {
            (Some(y), Some(x)) if y.is_finite() && x.is_finite() => {
                ys.push(y);
                xs.push(x);
            }
            _ => {}
        }
    }
    Ok((xs, ys))
}

fn regress(xs: &[f64], ys: &[f64]) -> Result<Regression, FormulaError> {
    let n = xs.len();
    if n < 2 {
        return Err(FormulaError::Div0);
    }
    let mx = mean(xs);
    let my = mean(ys);
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let dx = xs[i] - mx;
        let dy = ys[i] - my;
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    if sxx == 0.0 {
        return Err(FormulaError::Div0);
    }
    let slope = sxy / sxx;
    let intercept = my - slope * mx;
    let r = if sxx == 0.0 || syy == 0.0 {
        0.0
    } else {
        sxy / (sxx * syy).sqrt()
    };
    Ok(Regression {
        slope,
        intercept,
        r,
        sxx,
        syy,
        sxy,
        n,
    })
}

fn bivar(args: &[EvaluatedArg], pick: impl Fn(&Regression) -> f64) -> FormulaValue {
    let (xs, ys) = match pair_vectors(args.first(), args.get(1)) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let reg = match regress(&xs, &ys) {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    finite(pick(&reg))
}

fn bivar_pop(args: &[EvaluatedArg], sample: bool) -> FormulaValue {
    let (xs, ys) = match pair_vectors(args.first(), args.get(1)) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let n = xs.len();
    if (sample && n < 2) || (!sample && n < 1) {
        return err(FormulaError::Div0);
    }
    let mx = mean(&xs);
    let my = mean(&ys);
    let mut sxy = 0.0;
    for i in 0..n {
        sxy += (xs[i] - mx) * (ys[i] - my);
    }
    FormulaValue::Number(sxy / if sample { (n - 1) as f64 } else { n as f64 })
}

fn forecast_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let x = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let (xs, ys) = match pair_vectors(args.get(1), args.get(2)) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let reg = match regress(&xs, &ys) {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    finite(reg.intercept + reg.slope * x)
}

fn steyx(args: &[EvaluatedArg]) -> FormulaValue {
    let (xs, ys) = match pair_vectors(args.first(), args.get(1)) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let reg = match regress(&xs, &ys) {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let n = reg.n;
    if n < 3 {
        return err(FormulaError::Div0);
    }
    let se_sq = (reg.syy - (reg.sxy * reg.sxy) / reg.sxx) / (n - 2) as f64;
    finite(se_sq.max(0.0).sqrt())
}

// 方差/标准差原始值（A 变体与离差共用）。
fn sample_var_raw(ns: &[f64]) -> f64 {
    let n = ns.len();
    let m = mean(ns);
    ns.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n - 1) as f64
}
fn pop_var_raw(ns: &[f64]) -> f64 {
    let n = ns.len();
    let m = mean(ns);
    ns.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n as f64
}
fn sample_var(ns: &[f64]) -> FormulaValue {
    if ns.len() < 2 {
        err(FormulaError::Div0)
    } else {
        FormulaValue::Number(sample_var_raw(ns))
    }
}
fn pop_var(ns: &[f64]) -> FormulaValue {
    if ns.is_empty() {
        err(FormulaError::Div0)
    } else {
        FormulaValue::Number(pop_var_raw(ns))
    }
}
fn sample_std(ns: &[f64]) -> FormulaValue {
    if ns.len() < 2 {
        err(FormulaError::Div0)
    } else {
        FormulaValue::Number(sample_var_raw(ns).sqrt())
    }
}
fn pop_std(ns: &[f64]) -> FormulaValue {
    if ns.is_empty() {
        err(FormulaError::Div0)
    } else {
        FormulaValue::Number(pop_var_raw(ns).sqrt())
    }
}

// ── 正态分布（Abramowitz–Stegun / Acklam 近似）─────────────

fn norm_pdf(x: f64, mu: f64, sigma: f64) -> f64 {
    (-((x - mu).powi(2)) / (2.0 * sigma * sigma)).exp() / (sigma * (2.0 * PI).sqrt())
}

fn norm_s_cdf(z: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * z.abs());
    let d = 0.398_942_280_401_432_7 * (-z * z / 2.0).exp();
    let p = d
        * t
        * (0.319_381_53
            + t * (-0.356_563_782
                + t * (1.781_477_937 + t * (-1.821_255_978 + t * 1.330_274_429))));
    if z >= 0.0 {
        1.0 - p
    } else {
        p
    }
}

fn norm_s_inv(p: f64) -> FormulaValue {
    if p <= 0.0 || p >= 1.0 {
        return err(FormulaError::Num);
    }
    let a = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239e0,
    ];
    let b = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    let c = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838e0,
        -2.549_732_539_343_734e0,
        4.374_664_141_464_968e0,
        2.938_163_982_698_783e0,
    ];
    let d = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996e0,
        3.754_408_661_907_416e0,
    ];
    let plow = 0.02425;
    let phigh = 1.0 - plow;
    let x = if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    } else if p <= phigh {
        let q = p - 0.5;
        let r = q * q;
        (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
            / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    };
    FormulaValue::Number(x)
}

/// 线性插值分位（PERCENTILE.INC 语义），供 QUARTILE.INC。sorted 已排序。
fn percentile_inc(sorted: &[f64], p: f64) -> f64 {
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (rank - lo as f64) * (sorted[hi] - sorted[lo])
    }
}

/// PERCENTILE.EXC：p∈(1/(n+1), n/(n+1))，否则 #NUM!。sorted 已排序。
fn percentile_exc(sorted: &[f64], p: f64) -> FormulaValue {
    let n = sorted.len();
    let rank = p * (n + 1) as f64 - 1.0;
    if rank < 0.0 || rank > (n - 1) as f64 {
        return err(FormulaError::Num);
    }
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        FormulaValue::Number(sorted[lo])
    } else {
        FormulaValue::Number(sorted[lo] + (rank - lo as f64) * (sorted[hi] - sorted[lo]))
    }
}

fn sorted_asc(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v
}

fn quartile(args: &[EvaluatedArg], inc: bool) -> FormulaValue {
    let data = nums_from(args.first());
    let q = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let qi = q.trunc() as i64;
    if data.is_empty() {
        return err(FormulaError::Num);
    }
    let s = sorted_asc(data);
    if inc {
        if !(0..=4).contains(&qi) {
            return err(FormulaError::Num);
        }
        FormulaValue::Number(percentile_inc(&s, qi as f64 / 4.0))
    } else {
        if !(1..=3).contains(&qi) {
            return err(FormulaError::Num);
        }
        percentile_exc(&s, qi as f64 / 4.0)
    }
}

fn percent_rank(args: &[EvaluatedArg], inc: bool) -> FormulaValue {
    let data = sorted_asc(nums_from(args.first()));
    let x = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let sig = match opt_num(args, 2, 3.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let n = data.len();
    if n == 0 {
        return err(FormulaError::Num);
    }
    if x < data[0] || x > data[n - 1] {
        return err(FormulaError::Na);
    }
    let idx = data.iter().position(|&v| v == x);
    let rank = if let Some(i) = idx {
        if inc {
            i as f64 / (n - 1) as f64
        } else {
            (i + 1) as f64 / (n + 1) as f64
        }
    } else {
        let mut lo = 0usize;
        while lo < n - 1 && data[lo + 1] < x {
            lo += 1;
        }
        let frac = (x - data[lo]) / (data[lo + 1] - data[lo]);
        if inc {
            (lo as f64 + frac) / (n - 1) as f64
        } else {
            (lo as f64 + 1.0 + frac) / (n + 1) as f64
        }
    };
    let f = 10f64.powi(sig.trunc() as i32);
    FormulaValue::Number((rank * f).floor() / f)
}

fn rank_avg(args: &[EvaluatedArg]) -> FormulaValue {
    let target = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let data = nums_from(args.get(1));
    let order = match opt_num(args, 2, 0.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ascending = order != 0.0;
    let mut sorted = data.clone();
    sorted.sort_by(|a, b| {
        if ascending {
            a.partial_cmp(b).unwrap()
        } else {
            b.partial_cmp(a).unwrap()
        }
    });
    let mut first: i64 = -1;
    let mut count = 0i64;
    for (i, &v) in sorted.iter().enumerate() {
        if v == target {
            if first < 0 {
                first = i as i64;
            }
            count += 1;
        }
    }
    if first < 0 {
        return err(FormulaError::Na);
    }
    FormulaValue::Number((first + 1) as f64 + (count - 1) as f64 / 2.0)
}

fn norm_dist(args: &[EvaluatedArg]) -> FormulaValue {
    let x = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let mu = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let sigma = match req_num(args, 2) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if sigma <= 0.0 {
        return err(FormulaError::Num);
    }
    let cum = args.get(3).map(bool_of).unwrap_or(true);
    if cum {
        FormulaValue::Number(norm_s_cdf((x - mu) / sigma))
    } else {
        FormulaValue::Number(norm_pdf(x, mu, sigma))
    }
}

fn norm_inv(args: &[EvaluatedArg]) -> FormulaValue {
    let p = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let mu = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let sigma = match req_num(args, 2) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if sigma <= 0.0 {
        return err(FormulaError::Num);
    }
    match norm_s_inv(p) {
        FormulaValue::Number(z) => FormulaValue::Number(mu + sigma * z),
        other => other,
    }
}

fn confidence(args: &[EvaluatedArg]) -> FormulaValue {
    let alpha = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let sigma = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let size = match req_num(args, 2) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if alpha <= 0.0 || alpha >= 1.0 || sigma <= 0.0 || size < 1.0 {
        return err(FormulaError::Num);
    }
    match norm_s_inv(1.0 - alpha / 2.0) {
        FormulaValue::Number(z) => FormulaValue::Number(z * sigma / size.trunc().sqrt()),
        other => other,
    }
}

// 离差族。
fn devsq(args: &[EvaluatedArg]) -> FormulaValue {
    let ns = match collect_numbers(args) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if ns.is_empty() {
        return FormulaValue::Number(0.0);
    }
    let m = mean(&ns);
    FormulaValue::Number(ns.iter().map(|n| (n - m).powi(2)).sum())
}

fn avedev(args: &[EvaluatedArg]) -> FormulaValue {
    let ns = match collect_numbers(args) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if ns.is_empty() {
        return err(FormulaError::Num);
    }
    let m = mean(&ns);
    FormulaValue::Number(ns.iter().map(|n| (n - m).abs()).sum::<f64>() / ns.len() as f64)
}

fn skew(args: &[EvaluatedArg], population: bool) -> FormulaValue {
    let ns = match collect_numbers(args) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let n = ns.len();
    if population {
        if n < 1 {
            return err(FormulaError::Div0);
        }
        let m = mean(&ns);
        let sd = pop_var_raw(&ns).sqrt();
        if sd == 0.0 {
            return err(FormulaError::Div0);
        }
        let sum: f64 = ns.iter().map(|x| ((x - m) / sd).powi(3)).sum();
        FormulaValue::Number(sum / n as f64)
    } else {
        if n < 3 {
            return err(FormulaError::Div0);
        }
        let m = mean(&ns);
        let sd = sample_var_raw(&ns).sqrt();
        if sd == 0.0 {
            return err(FormulaError::Div0);
        }
        let sum: f64 = ns.iter().map(|x| ((x - m) / sd).powi(3)).sum();
        FormulaValue::Number((n as f64 / ((n - 1) as f64 * (n - 2) as f64)) * sum)
    }
}

fn kurt(args: &[EvaluatedArg]) -> FormulaValue {
    let ns = match collect_numbers(args) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let n = ns.len();
    if n < 4 {
        return err(FormulaError::Div0);
    }
    let m = mean(&ns);
    let sd = sample_var_raw(&ns).sqrt();
    if sd == 0.0 {
        return err(FormulaError::Div0);
    }
    let sum: f64 = ns.iter().map(|x| ((x - m) / sd).powi(4)).sum();
    let nf = n as f64;
    FormulaValue::Number(
        (nf * (nf + 1.0)) / ((nf - 1.0) * (nf - 2.0) * (nf - 3.0)) * sum
            - (3.0 * (nf - 1.0).powi(2)) / ((nf - 2.0) * (nf - 3.0)),
    )
}

fn geomean(args: &[EvaluatedArg]) -> FormulaValue {
    let ns = match collect_numbers(args) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if ns.is_empty() {
        return err(FormulaError::Num);
    }
    let mut logsum = 0.0;
    for n in &ns {
        if *n <= 0.0 {
            return err(FormulaError::Num);
        }
        logsum += n.ln();
    }
    finite((logsum / ns.len() as f64).exp())
}

fn harmean(args: &[EvaluatedArg]) -> FormulaValue {
    let ns = match collect_numbers(args) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if ns.is_empty() {
        return err(FormulaError::Num);
    }
    let mut recip = 0.0;
    for n in &ns {
        if *n <= 0.0 {
            return err(FormulaError::Num);
        }
        recip += 1.0 / n;
    }
    finite(ns.len() as f64 / recip)
}

fn trimmean(args: &[EvaluatedArg]) -> FormulaValue {
    let ns0: Vec<f64> = match args.first() {
        Some(a) => flatten_arg(a)
            .iter()
            .filter_map(|v| to_number(v).ok())
            .filter(|n| n.is_finite())
            .collect(),
        None => Vec::new(),
    };
    let pct = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if !(0.0..1.0).contains(&pct) {
        return err(FormulaError::Num);
    }
    let s = sorted_asc(ns0);
    let cut = ((s.len() as f64 * pct) / 2.0).floor() as usize;
    if cut * 2 >= s.len() {
        return err(FormulaError::Num);
    }
    let kept = &s[cut..s.len() - cut];
    if kept.is_empty() {
        return err(FormulaError::Num);
    }
    FormulaValue::Number(mean(kept))
}

fn averagea(args: &[EvaluatedArg]) -> FormulaValue {
    let ns = match collect_numbers_a(args) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if ns.is_empty() {
        err(FormulaError::Div0)
    } else {
        FormulaValue::Number(mean(&ns))
    }
}

// ── 数据库 D 函数 ────────────────────────────────────────

/// 列标题 → 列索引（0-based）；未找到 -1。
fn header_index(headers: &[FormulaValue], name: &str) -> i64 {
    let target = name.trim().to_uppercase();
    for (c, h) in headers.iter().enumerate() {
        if to_text(h).unwrap_or_default().trim().to_uppercase() == target {
            return c as i64;
        }
    }
    -1
}

/// 单条件匹配（复用 SUMIF 家族语义：运算符前缀 + 文本相等 + 通配符）。
fn match_one(cell: &FormulaValue, criteria: &FormulaValue) -> bool {
    if criteria.is_blank() {
        return true; // 空条件格 = 不约束
    }
    let cs = to_text(criteria).unwrap_or_default();
    let cs = cs.trim().to_string();
    let (opr, rhs) = split_operator_db(&cs);
    let rhs_num = rhs.trim().parse::<f64>().ok();
    let v_num = match cell {
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
    let vs = to_text(cell).unwrap_or_default();
    let target = if opr == "=" || opr == "<>" {
        rhs.as_str()
    } else {
        cs.as_str()
    };
    let has_wild = target.contains('*') || target.contains('?');
    let eq = if has_wild {
        crate::functions::wildcard_to_regex(target, true).is_match(&vs)
    } else {
        vs.to_uppercase() == target.to_uppercase()
    };
    if opr == "<>" {
        !eq
    } else {
        eq
    }
}

fn split_operator_db(cs: &str) -> (String, String) {
    for op in [">=", "<=", "<>", ">", "<", "="] {
        if let Some(rest) = cs.strip_prefix(op) {
            return (op.to_string(), rest.to_string());
        }
    }
    (String::new(), cs.to_string())
}

/// 选出满足 criteria 的记录里，field 列的所有值。
fn select_column(
    db_arg: Option<&EvaluatedArg>,
    field_arg: Option<&EvaluatedArg>,
    crit_arg: Option<&EvaluatedArg>,
) -> Result<Vec<FormulaValue>, FormulaError> {
    let db = as_matrix(db_arg);
    if db.is_empty() {
        return Err(FormulaError::Value);
    }
    let headers = &db[0];
    let field_val = scalar_arg(field_arg);
    let field_idx: i64 = match &field_val {
        FormulaValue::Number(n) => n.trunc() as i64 - 1,
        other => header_index(headers, &to_text(other).unwrap_or_default()),
    };
    if field_idx < 0 || field_idx >= headers.len() as i64 {
        return Err(FormulaError::Value);
    }
    let field_idx = field_idx as usize;

    let crit = as_matrix(crit_arg);
    if crit.is_empty() {
        return Err(FormulaError::Value);
    }
    let crit_headers = &crit[0];
    let crit_col_map: Vec<i64> = crit_headers
        .iter()
        .map(|h| header_index(headers, &to_text(h).unwrap_or_default()))
        .collect();

    let mut out = Vec::new();
    for record in db.iter().skip(1) {
        let selected = if crit.len() == 1 {
            true
        } else {
            let mut sel = false;
            for cond_row in crit.iter().skip(1) {
                let mut all_ok = true;
                for (cc, &db_col) in crit_col_map.iter().enumerate() {
                    let cond = cond_row.get(cc).cloned().unwrap_or(FormulaValue::Blank);
                    if cond.is_blank() {
                        continue;
                    }
                    if db_col < 0 {
                        all_ok = false;
                        break;
                    }
                    let cell = record
                        .get(db_col as usize)
                        .cloned()
                        .unwrap_or(FormulaValue::Blank);
                    if !match_one(&cell, &cond) {
                        all_ok = false;
                        break;
                    }
                }
                if all_ok {
                    sel = true;
                    break;
                }
            }
            sel
        };
        if selected {
            out.push(
                record
                    .get(field_idx)
                    .cloned()
                    .unwrap_or(FormulaValue::Blank),
            );
        }
    }
    Ok(out)
}

/// D 函数取数：选中列里的数值（跳过空/纯文本，错误跳过——对齐 toNumber 非错误）。
fn d_nums(vals: &[FormulaValue]) -> Vec<f64> {
    let mut out = Vec::new();
    for v in vals {
        if v.is_blank() {
            continue;
        }
        if let Ok(n) = to_number(v) {
            out.push(n);
        }
    }
    out
}

fn dfn(args: &[EvaluatedArg], reduce: impl Fn(&[FormulaValue]) -> FormulaValue) -> FormulaValue {
    match select_column(args.first(), args.get(1), args.get(2)) {
        Ok(sel) => reduce(&sel),
        Err(e) => err(e),
    }
}

// ── 现代文本 + 引用 + 信息 ───────────────────────────────

/// 千分位插入。
fn group_thousands(int_part: &str) -> String {
    let neg = int_part.starts_with('-');
    let digits = if neg { &int_part[1..] } else { int_part };
    let bytes = digits.as_bytes();
    let mut out = String::new();
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// 定点格式化（FIXED/DOLLAR 共用）。decimals<0 时从整数左侧舍入。
fn fixed_format(value: f64, decimals: i64, commas: bool) -> String {
    let neg = value < 0.0;
    let mut abs = value.abs();
    let s: String;
    if decimals >= 0 {
        let d = decimals.min(100) as usize;
        s = format!("{abs:.d$}");
    } else {
        let f = 10f64.powi((-decimals) as i32);
        abs = (abs / f).round() * f;
        s = format!("{abs:.0}");
    }
    let (int_part, frac_part) = match s.find('.') {
        Some(dot) => (s[..dot].to_string(), s[dot..].to_string()),
        None => (s.clone(), String::new()),
    };
    let int_part = if commas {
        group_thousands(&int_part)
    } else {
        int_part
    };
    format!("{}{}{}", if neg { "-" } else { "" }, int_part, frac_part)
}

/// TEXTBEFORE/TEXTAFTER 共用。
fn text_split(args: &[EvaluatedArg], before: bool) -> FormulaValue {
    let text = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let delim = match to_text(&scalar_arg(args.get(1))) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let instance = match opt_num(args, 2, 1.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let if_not_found = match args.get(3) {
        Some(a) => scalar_arg(Some(a)),
        None => FormulaValue::Error(FormulaError::Na),
    };
    if delim.is_empty() {
        return if before {
            FormulaValue::Text(String::new())
        } else {
            FormulaValue::Text(text)
        };
    }
    let inst = instance.trunc() as i64;
    // 所有分隔符起点（字符位）。
    let text_chars: Vec<char> = text.chars().collect();
    let delim_chars: Vec<char> = delim.chars().collect();
    let mut positions: Vec<usize> = Vec::new();
    if delim_chars.len() <= text_chars.len() {
        let mut i = 0;
        while i + delim_chars.len() <= text_chars.len() {
            if text_chars[i..i + delim_chars.len()] == delim_chars[..] {
                positions.push(i);
                i += 1;
            } else {
                i += 1;
            }
        }
    }
    if positions.is_empty() {
        return if_not_found;
    }
    let hit = if inst > 0 {
        if inst as usize > positions.len() {
            return if_not_found;
        }
        positions[inst as usize - 1]
    } else if inst < 0 {
        let k = positions.len() as i64 + inst;
        if k < 0 {
            return if_not_found;
        }
        positions[k as usize]
    } else {
        return err(FormulaError::Value);
    };
    let result: String = if before {
        text_chars[..hit].iter().collect()
    } else {
        text_chars[hit + delim_chars.len()..].iter().collect()
    };
    FormulaValue::Text(result)
}

fn fixed_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let n = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let dec = match opt_num(args, 1, 2.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let no_commas = args.get(2).map(bool_of).unwrap_or(false);
    FormulaValue::Text(fixed_format(n, dec.trunc() as i64, !no_commas))
}

fn dollar_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let n = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let dec = match opt_num(args, 1, 2.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let body = fixed_format(n.abs(), dec.trunc() as i64, true);
    if n < 0.0 {
        FormulaValue::Text(format!("(${body})"))
    } else {
        FormulaValue::Text(format!("${body}"))
    }
}

fn clean_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let out: String = t.chars().filter(|c| (*c as u32) >= 32).collect();
    FormulaValue::Text(out)
}

/// ADDRESS(row, col, [abs=1], [a1=TRUE], [sheet]) → 地址文本。
fn address(args: &[EvaluatedArg]) -> FormulaValue {
    let row = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let col = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let abs_num = match opt_num(args, 2, 1.0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let a1 = args.get(3).map(bool_of).unwrap_or(true);
    let sheet = match args.get(4) {
        Some(a) => match to_text(&scalar_arg(Some(a))) {
            Ok(t) => t,
            Err(e) => return err(e),
        },
        None => String::new(),
    };
    let r = row.trunc() as i64;
    let c = col.trunc() as i64;
    let ab = abs_num.trunc() as i64;
    if r < 1 || c < 1 || !(1..=4).contains(&ab) {
        return err(FormulaError::Value);
    }
    let row_abs = ab == 1 || ab == 2;
    let col_abs = ab == 1 || ab == 3;
    let core = if a1 {
        format!(
            "{}{}{}{}",
            if col_abs { "$" } else { "" },
            col_to_label((c - 1) as u32),
            if row_abs { "$" } else { "" },
            r
        )
    } else {
        let r_part = if row_abs {
            format!("R{r}")
        } else {
            format!("R[{r}]")
        };
        let c_part = if col_abs {
            format!("C{c}")
        } else {
            format!("C[{c}]")
        };
        format!("{r_part}{c_part}")
    };
    if !sheet.is_empty() {
        let need_quote = sheet
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
        let sh = if need_quote {
            format!("'{}'", sheet.replace('\'', "''"))
        } else {
            sheet
        };
        FormulaValue::Text(format!("{sh}!{core}"))
    } else {
        FormulaValue::Text(core)
    }
}

/// INDIRECT(ref_text, [a1=TRUE]) → 目标值（区域取左上角）。
fn indirect(args: &[EvaluatedArg], ctx: &EvalContext) -> FormulaValue {
    let ref_text = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let a1 = args.get(1).map(bool_of).unwrap_or(true);
    let mut refr = ref_text.trim().to_string();
    if refr.is_empty() {
        return err(FormulaError::Ref);
    }
    if !a1 {
        // R1C1 绝对 → A1
        let up = refr.to_uppercase();
        let parsed = parse_r1c1(&up);
        match parsed {
            Some((r, c)) => {
                refr = format!("{}{}", col_to_label(c - 1), r);
            }
            None => return err(FormulaError::Ref),
        }
    }
    if let Some(colon) = refr.find(':') {
        let vals = ctx
            .accessor
            .get_range_values(&refr[..colon], &refr[colon + 1..]);
        vals.first()
            .and_then(|r| r.first())
            .cloned()
            .unwrap_or(FormulaValue::Blank)
    } else {
        ctx.accessor.get_cell_value(&refr)
    }
}

/// 解析绝对 R1C1（RnCn）→ (row1based, col1based)。
fn parse_r1c1(s: &str) -> Option<(u32, u32)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'R') {
        return None;
    }
    let c_pos = s.find('C')?;
    let r_digits = &s[1..c_pos];
    let c_digits = &s[c_pos + 1..];
    if r_digits.is_empty() || c_digits.is_empty() {
        return None;
    }
    let r: u32 = r_digits.parse().ok()?;
    let c: u32 = c_digits.parse().ok()?;
    Some((r, c))
}

fn xor_fn(args: &[EvaluatedArg]) -> FormulaValue {
    let mut count = 0i64;
    for a in args {
        for v in flatten_arg(a) {
            if v.is_blank() {
                continue;
            }
            match to_boolean(&v) {
                Ok(b) => {
                    if b {
                        count += 1;
                    }
                }
                Err(e) => return err(e),
            }
        }
    }
    FormulaValue::Bool(count % 2 == 1)
}

fn error_type(args: &[EvaluatedArg]) -> FormulaValue {
    let v = scalar_arg(args.first());
    match v.as_error() {
        Some(e) => {
            let code = match e {
                // #NULL! 无独立枚举；映射按 TS 表其余各值。
                FormulaError::Div0 => 2.0,
                FormulaError::Value => 3.0,
                FormulaError::Ref => 4.0,
                FormulaError::Name => 5.0,
                FormulaError::Num => 6.0,
                FormulaError::Na => 7.0,
                FormulaError::Spill => 7.0,
                FormulaError::Circ => 7.0,
            };
            FormulaValue::Number(code)
        }
        None => err(FormulaError::Na),
    }
}

fn is_ref(args: &[EvaluatedArg]) -> FormulaValue {
    FormulaValue::Bool(matches!(args.first(), Some(EvaluatedArg::Range(_))))
}

// ── 补：TS 核心存在、Rust 基础集尚缺的 10 函数 ─────────────

fn variance_agg(ns: &[f64], sample: bool, sqrt: bool) -> FormulaValue {
    let n = ns.len();
    if (sample && n < 2) || (!sample && n < 1) {
        return err(FormulaError::Div0);
    }
    let m = mean(ns);
    let ss: f64 = ns.iter().map(|x| (x - m) * (x - m)).sum();
    let v = ss / if sample { (n - 1) as f64 } else { n as f64 };
    FormulaValue::Number(if sqrt { v.sqrt() } else { v })
}

fn date_value(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match to_text(&scalar_arg(args.first())) {
        Ok(t) => t,
        Err(e) => return err(e),
    };
    let s = t.trim();
    // YYYY-M-D 或 YYYY/M/D
    if let Some((y, m, d)) = parse_ymd(s, true) {
        return FormulaValue::Number(date_to_serial(y, m, d));
    }
    // M-D-YYYY 或 M/D/YYYY
    if let Some((y, m, d)) = parse_ymd(s, false) {
        return FormulaValue::Number(date_to_serial(y, m, d));
    }
    err(FormulaError::Value)
}

/// 解析日期文本。ymd_first=true → YYYY[-/]M[-/]D；false → M[-/]D[-/]YYYY。
fn parse_ymd(s: &str, ymd_first: bool) -> Option<(i64, u32, u32)> {
    let sep = if s.contains('-') {
        '-'
    } else if s.contains('/') {
        '/'
    } else {
        return None;
    };
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() != 3 {
        return None;
    }
    let a: i64 = parts[0].parse().ok()?;
    let b: i64 = parts[1].parse().ok()?;
    let c: i64 = parts[2].parse().ok()?;
    if ymd_first {
        // 需 4 位年在前
        if parts[0].len() != 4 {
            return None;
        }
        Some((a, b as u32, c as u32))
    } else {
        if parts[2].len() != 4 {
            return None;
        }
        Some((c, a as u32, b as u32))
    }
}

fn network_days(args: &[EvaluatedArg]) -> FormulaValue {
    let s1 = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let s2 = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let mut holidays = std::collections::HashSet::new();
    if let Some(a) = args.get(2) {
        for v in flatten_arg(a) {
            if let Ok(n) = to_number(&v) {
                holidays.insert(n.trunc() as i64);
            }
        }
    }
    let i1 = s1.trunc() as i64;
    let i2 = s2.trunc() as i64;
    let start = i1.min(i2);
    let end = i1.max(i2);
    let mut count = 0i64;
    for d in start..=end {
        let dow = serial_to_parts(d as f64).weekday; // 0=Sun..6=Sat
        if dow == 0 || dow == 6 {
            continue;
        }
        if holidays.contains(&d) {
            continue;
        }
        count += 1;
    }
    FormulaValue::Number(if i1 <= i2 {
        count as f64
    } else {
        -count as f64
    })
}

// ── 注册 ────────────────────────────────────────────────

/// M17 函数集（名称 → 实现）。BuiltinRegistry::new 并入。
pub(crate) fn m17_builtins() -> Vec<(&'static str, FunctionImpl)> {
    macro_rules! f {
        ($name:literal, $imp:expr) => {
            ($name, Rc::new($imp) as FunctionImpl)
        };
    }
    vec![
        // ── 数学 / 三角 / 进制 ──
        f!("PI", |_: &[EvaluatedArg], _ctx: &EvalContext| {
            FormulaValue::Number(PI)
        }),
        f!("EXP", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            finite(x.exp())
        })),
        f!("LN", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            if x <= 0.0 {
                err(FormulaError::Num)
            } else {
                finite(x.ln())
            }
        })),
        f!("LOG10", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| if x <= 0.0 {
                err(FormulaError::Num)
            } else {
                finite(x.log10())
            }
        )),
        f!("LOG", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let x = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let base = match opt_num(a, 1, 10.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if x <= 0.0 || base <= 0.0 || base == 1.0 {
                return err(FormulaError::Num);
            }
            finite(x.ln() / base.ln())
        }),
        f!("SQRTPI", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| if x < 0.0 {
                err(FormulaError::Num)
            } else {
                finite((x * PI).sqrt())
            }
        )),
        f!("SIN", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            finite(x.sin())
        })),
        f!("COS", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            finite(x.cos())
        })),
        f!("TAN", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            finite(x.tan())
        })),
        f!("ASIN", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| if !(-1.0..=1.0).contains(&x) {
                err(FormulaError::Num)
            } else {
                finite(x.asin())
            }
        )),
        f!("ACOS", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| if !(-1.0..=1.0).contains(&x) {
                err(FormulaError::Num)
            } else {
                finite(x.acos())
            }
        )),
        f!("ATAN", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(x.atan())
        )),
        // Excel ATAN2(x_num, y_num) = atan2(y, x)
        f!("ATAN2", |a: &[EvaluatedArg], _ctx: &EvalContext| m2(
            a,
            |xnum, ynum| {
                if xnum == 0.0 && ynum == 0.0 {
                    err(FormulaError::Div0)
                } else {
                    finite(ynum.atan2(xnum))
                }
            }
        )),
        f!("SINH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(x.sinh())
        )),
        f!("COSH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(x.cosh())
        )),
        f!("TANH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(x.tanh())
        )),
        f!("ASINH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(x.asinh())
        )),
        f!("ACOSH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| if x < 1.0 {
                err(FormulaError::Num)
            } else {
                finite(x.acosh())
            }
        )),
        f!("ATANH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| if x <= -1.0 || x >= 1.0 {
                err(FormulaError::Num)
            } else {
                finite(x.atanh())
            }
        )),
        f!("SEC", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            finite(1.0 / x.cos())
        })),
        f!("CSC", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            finite(1.0 / x.sin())
        })),
        f!("COT", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(a, |x| {
            finite(1.0 / x.tan())
        })),
        f!("SECH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(1.0 / x.cosh())
        )),
        f!("CSCH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(1.0 / x.sinh())
        )),
        f!("COTH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(1.0 / x.tanh())
        )),
        f!("ACOT", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(PI / 2.0 - x.atan())
        )),
        f!("ACOTH", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| if x.abs() <= 1.0 {
                err(FormulaError::Num)
            } else {
                finite((1.0 / x).atanh())
            }
        )),
        f!("DEGREES", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(x * 180.0 / PI)
        )),
        f!("RADIANS", |a: &[EvaluatedArg], _ctx: &EvalContext| m1(
            a,
            |x| finite(x * PI / 180.0)
        )),
        f!(
            "FACT",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(n) => factorial(n),
                Err(e) => err(e),
            }
        ),
        f!(
            "FACTDOUBLE",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(n) => fact_double(n),
                Err(e) => err(e),
            }
        ),
        f!("COMBIN", |a: &[EvaluatedArg], _ctx: &EvalContext| m2(
            a, combin
        )),
        f!("COMBINA", |a: &[EvaluatedArg], _ctx: &EvalContext| m2(
            a,
            |n, k| {
                let nn = n.trunc() as i64;
                let kk = k.trunc() as i64;
                if nn < 0 || kk < 0 {
                    return err(FormulaError::Num);
                }
                if nn == 0 && kk == 0 {
                    return FormulaValue::Number(1.0);
                }
                combin((nn + kk - 1) as f64, kk as f64)
            }
        )),
        f!("PERMUT", |a: &[EvaluatedArg], _ctx: &EvalContext| m2(
            a, permut
        )),
        f!("PERMUTATIONA", |a: &[EvaluatedArg], _ctx: &EvalContext| m2(
            a,
            |n, k| {
                let nn = n.trunc() as i64;
                let kk = k.trunc() as i64;
                if nn < 0 || kk < 0 {
                    return err(FormulaError::Num);
                }
                finite((nn as f64).powf(kk as f64))
            }
        )),
        f!("MULTINOMIAL", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            multinomial(a)
        }),
        f!("QUOTIENT", |a: &[EvaluatedArg], _ctx: &EvalContext| m2(
            a,
            |x, y| {
                if y == 0.0 {
                    err(FormulaError::Div0)
                } else {
                    FormulaValue::Number((x / y).trunc())
                }
            }
        )),
        f!("CEILING.MATH", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            ceiling_floor_math(a, true)
        }),
        f!("FLOOR.MATH", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            ceiling_floor_math(a, false)
        }),
        f!(
            "CEILING.PRECISE",
            |a: &[EvaluatedArg], _ctx: &EvalContext| precise(a, true)
        ),
        f!("ISO.CEILING", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            precise(a, true)
        }),
        f!("FLOOR.PRECISE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            precise(a, false)
        }),
        f!("BASE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let v = match req_num(a, 0) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let radix = match req_num(a, 1) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let min_len = match opt_num(a, 2, 0.0) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            to_base_text(v, radix, min_len.trunc() as i64)
        }),
        f!("DECIMAL", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let t = match to_text(&scalar_arg(a.first())) {
                Ok(t) => t,
                Err(e) => return err(e),
            };
            let radix = match req_num(a, 1) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let r = radix.trunc() as i64;
            if !(2..=36).contains(&r) {
                return err(FormulaError::Num);
            }
            match parse_radix(&t, r as u32) {
                Some(n) if n.is_finite() => FormulaValue::Number(n),
                _ => err(FormulaError::Num),
            }
        }),
        f!(
            "ROMAN",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(n) => to_roman(n),
                Err(e) => err(e),
            }
        ),
        f!(
            "ARABIC",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match to_text(&scalar_arg(a.first())) {
                Ok(t) => from_roman(&t),
                Err(e) => err(e),
            }
        ),
        f!("SERIESSUM", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            seriessum(a)
        }),
        // RAND/RANDBETWEEN：本引擎无确定性随机源，测试不校验具体值；
        // RAND 返 0.5、RANDBETWEEN 返下界（对齐 prompt 约定，保证可复算）。
        f!("RAND", |_: &[EvaluatedArg], _ctx: &EvalContext| {
            FormulaValue::Number(0.5)
        }),
        f!("RANDBETWEEN", |a: &[EvaluatedArg], _ctx: &EvalContext| m2(
            a,
            |lo, hi| {
                let a2 = lo.ceil();
                let b2 = hi.floor();
                if a2 > b2 {
                    err(FormulaError::Num)
                } else {
                    FormulaValue::Number(a2)
                }
            }
        )),
        // ── 财务 ──
        f!("PMT", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let nper = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match opt_num(a, 3, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let type_ = match opt_num(a, 4, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            num_result(pmt_of(rate, nper, pv, fv, type_))
        }),
        f!("FV", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let nper = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pmt = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match opt_num(a, 3, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let type_ = match opt_num(a, 4, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            num_result(fv_of(rate, nper, pmt, pv, type_))
        }),
        f!("PV", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let nper = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pmt = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match opt_num(a, 3, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let type_ = match opt_num(a, 4, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            num_result(pv_of(rate, nper, pmt, fv, type_))
        }),
        f!("NPER", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pmt = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match opt_num(a, 3, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let type_ = match opt_num(a, 4, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            num_result(nper_of(rate, pmt, pv, fv, type_))
        }),
        f!("RATE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let nper = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pmt = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match opt_num(a, 3, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let type_ = match opt_num(a, 4, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let guess = match opt_num(a, 5, 0.1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let r = solve_rate(|rr| fv_of(rr, nper, pmt, pv, type_) - fv, guess);
            num_result(r)
        }),
        f!("IPMT", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let per = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let nper = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match req_num(a, 3) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match opt_num(a, 4, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let type_ = match opt_num(a, 5, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            match ipmt_of(rate, per.trunc(), nper, pv, fv, type_) {
                Ok(ip) => num_result(ip),
                Err(e) => err(e),
            }
        }),
        f!("PPMT", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let per = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let nper = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match req_num(a, 3) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match opt_num(a, 4, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let type_ = match opt_num(a, 5, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            match ipmt_of(rate, per.trunc(), nper, pv, fv, type_) {
                Ok(ip) => {
                    let pmt = pmt_of(rate, nper, pv, fv, type_);
                    num_result(pmt - ip)
                }
                Err(e) => err(e),
            }
        }),
        f!("CUMIPMT", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            cumulative(a, true)
        }),
        f!("CUMPRINC", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            cumulative(a, false)
        }),
        f!("NPV", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let flat = flatten_args(&a[1.min(a.len())..]);
            let flows = match strict_numbers(&flat) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            num_result(npv_at(rate, &flows))
        }),
        f!("IRR", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let flat = match a.first() {
                Some(x) => flatten_arg(x),
                None => Vec::new(),
            };
            let flows = match strict_numbers(&flat) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let guess = match opt_num(a, 1, 0.1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if flows.len() < 2 {
                return err(FormulaError::Num);
            }
            let r = solve_rate(
                |rr| {
                    flows
                        .iter()
                        .enumerate()
                        .map(|(i, cf)| cf / (1.0 + rr).powi(i as i32))
                        .sum()
                },
                guess,
            );
            num_result(r)
        }),
        f!("MIRR", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let flat = match a.first() {
                Some(x) => flatten_arg(x),
                None => Vec::new(),
            };
            let flows = match strict_numbers(&flat) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let finance_rate = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let reinvest_rate = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let n = flows.len();
            if n < 2 {
                return err(FormulaError::Div0);
            }
            let mut pv_neg = 0.0;
            let mut fv_pos = 0.0;
            for (i, cf) in flows.iter().enumerate() {
                if *cf < 0.0 {
                    pv_neg += cf / (1.0 + finance_rate).powi(i as i32);
                } else {
                    fv_pos += cf * (1.0 + reinvest_rate).powi((n - 1 - i) as i32);
                }
            }
            if pv_neg == 0.0 || fv_pos == 0.0 {
                return err(FormulaError::Div0);
            }
            num_result((-fv_pos / pv_neg).powf(1.0 / (n - 1) as f64) - 1.0)
        }),
        f!("XNPV", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let f1 = match a.get(1) {
                Some(x) => flatten_arg(x),
                None => Vec::new(),
            };
            let flows = match strict_numbers(&f1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let d1 = match a.get(2) {
                Some(x) => flatten_arg(x),
                None => Vec::new(),
            };
            let dates = match strict_numbers(&d1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if flows.len() != dates.len() || flows.is_empty() {
                return err(FormulaError::Num);
            }
            num_result(xnpv_at(rate, &flows, &dates))
        }),
        f!("XIRR", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let f1 = match a.first() {
                Some(x) => flatten_arg(x),
                None => Vec::new(),
            };
            let flows = match strict_numbers(&f1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let d1 = match a.get(1) {
                Some(x) => flatten_arg(x),
                None => Vec::new(),
            };
            let dates = match strict_numbers(&d1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let guess = match opt_num(a, 2, 0.1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if flows.len() != dates.len() || flows.len() < 2 {
                return err(FormulaError::Num);
            }
            let r = solve_rate(|rr| xnpv_at(rr, &flows, &dates), guess);
            num_result(r)
        }),
        f!("SLN", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let cost = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let salvage = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let life = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if life == 0.0 {
                err(FormulaError::Div0)
            } else {
                FormulaValue::Number((cost - salvage) / life)
            }
        }),
        f!("SYD", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let cost = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let salvage = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let life = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let per = match req_num(a, 3) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if life <= 0.0 || per < 1.0 || per > life {
                return err(FormulaError::Num);
            }
            FormulaValue::Number(
                (cost - salvage) * (life - per + 1.0) * 2.0 / (life * (life + 1.0)),
            )
        }),
        f!("DDB", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            depreciation_ddb(a)
        }),
        f!("DB", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            depreciation_db(a)
        }),
        f!("EFFECT", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let nominal = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let npery = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let np = npery.trunc() as i64;
            if nominal <= 0.0 || np < 1 {
                return err(FormulaError::Num);
            }
            finite((1.0 + nominal / np as f64).powi(np as i32) - 1.0)
        }),
        f!("NOMINAL", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let effect = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let npery = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let np = npery.trunc() as i64;
            if effect <= 0.0 || np < 1 {
                return err(FormulaError::Num);
            }
            finite(((1.0 + effect).powf(1.0 / np as f64) - 1.0) * np as f64)
        }),
        f!("DOLLARDE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            dollar_de(a)
        }),
        f!("DOLLARFR", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            dollar_fr(a)
        }),
        f!("PDURATION", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let rate = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if rate <= 0.0 || pv <= 0.0 || fv <= 0.0 {
                return err(FormulaError::Num);
            }
            finite((fv.ln() - pv.ln()) / (1.0 + rate).ln())
        }),
        f!("RRI", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let nper = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let pv = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let fv = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if nper <= 0.0 || pv == 0.0 {
                return err(FormulaError::Num);
            }
            num_result((fv / pv).powf(1.0 / nper) - 1.0)
        }),
        // ── 统计扩展 ──
        f!("GEOMEAN", |a: &[EvaluatedArg], _ctx: &EvalContext| geomean(
            a
        )),
        f!("HARMEAN", |a: &[EvaluatedArg], _ctx: &EvalContext| harmean(
            a
        )),
        f!("TRIMMEAN", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            trimmean(a)
        }),
        f!("AVERAGEA", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            averagea(a)
        }),
        f!(
            "MAXA",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match collect_numbers_a(a) {
                Ok(ns) => {
                    if ns.is_empty() {
                        FormulaValue::Number(0.0)
                    } else {
                        FormulaValue::Number(ns.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
                    }
                }
                Err(e) => err(e),
            }
        ),
        f!(
            "MINA",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match collect_numbers_a(a) {
                Ok(ns) => {
                    if ns.is_empty() {
                        FormulaValue::Number(0.0)
                    } else {
                        FormulaValue::Number(ns.iter().cloned().fold(f64::INFINITY, f64::min))
                    }
                }
                Err(e) => err(e),
            }
        ),
        f!(
            "STDEVA",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match collect_numbers_a(a) {
                Ok(ns) => sample_std(&ns),
                Err(e) => err(e),
            }
        ),
        f!(
            "STDEVPA",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match collect_numbers_a(a) {
                Ok(ns) => pop_std(&ns),
                Err(e) => err(e),
            }
        ),
        f!(
            "VARA",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match collect_numbers_a(a) {
                Ok(ns) => sample_var(&ns),
                Err(e) => err(e),
            }
        ),
        f!(
            "VARPA",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match collect_numbers_a(a) {
                Ok(ns) => pop_var(&ns),
                Err(e) => err(e),
            }
        ),
        f!("DEVSQ", |a: &[EvaluatedArg], _ctx: &EvalContext| devsq(a)),
        f!("AVEDEV", |a: &[EvaluatedArg], _ctx: &EvalContext| avedev(a)),
        f!("SKEW", |a: &[EvaluatedArg], _ctx: &EvalContext| skew(
            a, false
        )),
        f!("SKEW.P", |a: &[EvaluatedArg], _ctx: &EvalContext| skew(
            a, true
        )),
        f!("KURT", |a: &[EvaluatedArg], _ctx: &EvalContext| kurt(a)),
        f!("CORREL", |a: &[EvaluatedArg], _ctx: &EvalContext| bivar(
            a,
            |r| r.r
        )),
        f!("PEARSON", |a: &[EvaluatedArg], _ctx: &EvalContext| bivar(
            a,
            |r| r.r
        )),
        f!("RSQ", |a: &[EvaluatedArg], _ctx: &EvalContext| bivar(
            a,
            |r| r.r * r.r
        )),
        f!("SLOPE", |a: &[EvaluatedArg], _ctx: &EvalContext| bivar(
            a,
            |r| r.slope
        )),
        f!("INTERCEPT", |a: &[EvaluatedArg], _ctx: &EvalContext| bivar(
            a,
            |r| r.intercept
        )),
        f!("COVAR", |a: &[EvaluatedArg], _ctx: &EvalContext| bivar_pop(
            a, false
        )),
        f!("COVARIANCE.P", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            bivar_pop(a, false)
        }),
        f!("COVARIANCE.S", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            bivar_pop(a, true)
        }),
        f!("STEYX", |a: &[EvaluatedArg], _ctx: &EvalContext| steyx(a)),
        f!("FORECAST", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            forecast_fn(a)
        }),
        f!(
            "FORECAST.LINEAR",
            |a: &[EvaluatedArg], _ctx: &EvalContext| forecast_fn(a)
        ),
        f!("QUARTILE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            quartile(a, true)
        }),
        f!("QUARTILE.INC", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            quartile(a, true)
        }),
        f!("QUARTILE.EXC", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            quartile(a, false)
        }),
        f!(
            "PERCENTILE.EXC",
            |a: &[EvaluatedArg], _ctx: &EvalContext| {
                let data = nums_from(a.first());
                let p = match req_num(a, 1) {
                    Ok(v) => v,
                    Err(e) => return err(e),
                };
                if data.is_empty() {
                    return err(FormulaError::Num);
                }
                percentile_exc(&sorted_asc(data), p)
            }
        ),
        f!("PERCENTRANK", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            percent_rank(a, true)
        }),
        f!(
            "PERCENTRANK.INC",
            |a: &[EvaluatedArg], _ctx: &EvalContext| percent_rank(a, true)
        ),
        f!(
            "PERCENTRANK.EXC",
            |a: &[EvaluatedArg], _ctx: &EvalContext| percent_rank(a, false)
        ),
        f!("RANK.AVG", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            rank_avg(a)
        }),
        f!("NORM.DIST", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            norm_dist(a)
        }),
        f!("NORMDIST", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            norm_dist(a)
        }),
        f!("NORM.S.DIST", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let z = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let cum = a.get(1).map(bool_of).unwrap_or(true);
            if cum {
                FormulaValue::Number(norm_s_cdf(z))
            } else {
                FormulaValue::Number(norm_pdf(z, 0.0, 1.0))
            }
        }),
        f!(
            "NORMSDIST",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(z) => FormulaValue::Number(norm_s_cdf(z)),
                Err(e) => err(e),
            }
        ),
        f!("NORM.INV", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            norm_inv(a)
        }),
        f!(
            "NORMINV",
            |a: &[EvaluatedArg], _ctx: &EvalContext| norm_inv(a)
        ),
        f!(
            "NORM.S.INV",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(p) => norm_s_inv(p),
                Err(e) => err(e),
            }
        ),
        f!(
            "NORMSINV",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(p) => norm_s_inv(p),
                Err(e) => err(e),
            }
        ),
        f!("STANDARDIZE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let x = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let mu = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let sigma = match req_num(a, 2) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if sigma <= 0.0 {
                return err(FormulaError::Num);
            }
            FormulaValue::Number((x - mu) / sigma)
        }),
        f!(
            "CONFIDENCE.NORM",
            |a: &[EvaluatedArg], _ctx: &EvalContext| confidence(a)
        ),
        f!("CONFIDENCE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            confidence(a)
        }),
        f!(
            "GAUSS",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(z) => FormulaValue::Number(norm_s_cdf(z) - 0.5),
                Err(e) => err(e),
            }
        ),
        f!(
            "PHI",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match req_num(a, 0) {
                Ok(x) => FormulaValue::Number(norm_pdf(x, 0.0, 1.0)),
                Err(e) => err(e),
            }
        ),
        // ── 数据库 D 函数 ──
        f!("DSUM", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| { FormulaValue::Number(d_nums(v).iter().sum()) }
        )),
        f!("DAVERAGE", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.is_empty() {
                    err(FormulaError::Div0)
                } else {
                    FormulaValue::Number(mean(&n))
                }
            }
        )),
        f!("DCOUNT", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| { FormulaValue::Number(d_nums(v).len() as f64) }
        )),
        f!("DCOUNTA", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| { FormulaValue::Number(v.iter().filter(|x| !x.is_blank()).count() as f64) }
        )),
        f!("DMAX", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.is_empty() {
                    FormulaValue::Number(0.0)
                } else {
                    FormulaValue::Number(n.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
                }
            }
        )),
        f!("DMIN", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.is_empty() {
                    FormulaValue::Number(0.0)
                } else {
                    FormulaValue::Number(n.iter().cloned().fold(f64::INFINITY, f64::min))
                }
            }
        )),
        f!("DPRODUCT", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.is_empty() {
                    FormulaValue::Number(0.0)
                } else {
                    FormulaValue::Number(n.iter().product())
                }
            }
        )),
        f!("DGET", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let non_blank: Vec<&FormulaValue> = v.iter().filter(|x| !x.is_blank()).collect();
                if non_blank.is_empty() {
                    err(FormulaError::Value)
                } else if non_blank.len() > 1 {
                    err(FormulaError::Num)
                } else {
                    non_blank[0].clone()
                }
            }
        )),
        f!("DSTDEV", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.len() < 2 {
                    err(FormulaError::Div0)
                } else {
                    FormulaValue::Number(sample_var_raw(&n).sqrt())
                }
            }
        )),
        f!("DSTDEVP", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.is_empty() {
                    err(FormulaError::Div0)
                } else {
                    FormulaValue::Number(pop_var_raw(&n).sqrt())
                }
            }
        )),
        f!("DVAR", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.len() < 2 {
                    err(FormulaError::Div0)
                } else {
                    FormulaValue::Number(sample_var_raw(&n))
                }
            }
        )),
        f!("DVARP", |a: &[EvaluatedArg], _ctx: &EvalContext| dfn(
            a,
            |v| {
                let n = d_nums(v);
                if n.is_empty() {
                    err(FormulaError::Div0)
                } else {
                    FormulaValue::Number(pop_var_raw(&n))
                }
            }
        )),
        // ── 现代文本 + 引用 + 信息 ──
        f!("TEXTBEFORE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            text_split(a, true)
        }),
        f!("TEXTAFTER", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            text_split(a, false)
        }),
        f!("FIXED", |a: &[EvaluatedArg], _ctx: &EvalContext| fixed_fn(
            a
        )),
        f!(
            "DOLLAR",
            |a: &[EvaluatedArg], _ctx: &EvalContext| dollar_fn(a)
        ),
        f!("CLEAN", |a: &[EvaluatedArg], _ctx: &EvalContext| clean_fn(
            a
        )),
        f!("ADDRESS", |a: &[EvaluatedArg], _ctx: &EvalContext| address(
            a
        )),
        f!(
            "INDIRECT",
            |a: &[EvaluatedArg], ctx: &EvalContext| indirect(a, ctx)
        ),
        f!("XOR", |a: &[EvaluatedArg], _ctx: &EvalContext| xor_fn(a)),
        f!("ERROR.TYPE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            error_type(a)
        }),
        f!("ISREF", |a: &[EvaluatedArg], _ctx: &EvalContext| is_ref(a)),
        // ── 补：TS 核心存在、Rust 基础集尚缺 ──
        f!(
            "STDEV.P",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match numeric_values(a) {
                Ok(ns) => variance_agg(&ns, false, true),
                Err(e) => err(e),
            }
        ),
        f!(
            "STDEV.S",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match numeric_values(a) {
                Ok(ns) => variance_agg(&ns, true, true),
                Err(e) => err(e),
            }
        ),
        f!(
            "VAR.P",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match numeric_values(a) {
                Ok(ns) => variance_agg(&ns, false, false),
                Err(e) => err(e),
            }
        ),
        f!(
            "VAR.S",
            |a: &[EvaluatedArg], _ctx: &EvalContext| match numeric_values(a) {
                Ok(ns) => variance_agg(&ns, true, false),
                Err(e) => err(e),
            }
        ),
        f!("NOW", |_: &[EvaluatedArg], _ctx: &EvalContext| {
            let d = now_parts();
            FormulaValue::Number(parts_to_serial(d.0, d.1, d.2, d.3, d.4, d.5))
        }),
        f!("TODAY", |_: &[EvaluatedArg], _ctx: &EvalContext| {
            let d = now_parts();
            FormulaValue::Number(date_to_serial(d.0, d.1, d.2))
        }),
        f!("DATEVALUE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            date_value(a)
        }),
        f!("NETWORKDAYS", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            network_days(a)
        }),
        f!("UNICHAR", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let n = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let k = n.trunc() as i64;
            if k < 1 {
                return err(FormulaError::Value);
            }
            match char::from_u32(k as u32) {
                Some(c) => FormulaValue::Text(c.to_string()),
                None => err(FormulaError::Value),
            }
        }),
        f!("UNICODE", |a: &[EvaluatedArg], _ctx: &EvalContext| {
            let t = match to_text(&scalar_arg(a.first())) {
                Ok(t) => t,
                Err(e) => return err(e),
            };
            match t.chars().next() {
                Some(c) => FormulaValue::Number(c as u32 as f64),
                None => err(FormulaError::Value),
            }
        }),
    ]
}

/// 当前本地时间零件 (year, month, day, hour, minute, second)。
/// 非确定性——仅供 NOW/TODAY（volatile），测试不校验具体值。
fn now_parts() -> (i64, u32, u32, i64, i64, i64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Unix 天数 → Excel 序列号零件（复用 serial_to_parts；Unix epoch = 序列 25569）。
    let serial = 25569.0 + secs as f64 / 86400.0;
    let p = serial_to_parts(serial);
    (
        p.year,
        p.month,
        p.day,
        p.hours as i64,
        p.minutes as i64,
        p.seconds as i64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::{CellAccessor, Evaluator, FunctionRegistry};
    use crate::functions::BuiltinRegistry;
    use crate::parse::parse_formula;
    use sheet_core::address::{label_to_col, parse_addr};
    use std::collections::HashMap;

    /// 内存网格 accessor（同 functionsM8.test 风格），钳到 12×12。
    struct MapAccessor {
        cells: HashMap<(u32, u32), FormulaValue>,
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
            let col = label_to_col(&clean).unwrap_or(0);
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
            let (rows, cols) = (12i64, 12i64);
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
    }

    fn ev(src: &str, cells: &[((u32, u32), FormulaValue)]) -> FormulaValue {
        let acc = MapAccessor {
            cells: cells.iter().cloned().collect(),
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
    fn near(v: FormulaValue, x: f64) {
        let got = n(v);
        assert!((got - x).abs() < 1e-4, "near: got {got}, want {x}");
    }
    fn near_p(v: FormulaValue, x: f64, tol: f64) {
        let got = n(v);
        assert!(
            (got - x).abs() < tol,
            "near_p: got {got}, want {x} (tol {tol})"
        );
    }

    fn ab_grid() -> Vec<((u32, u32), FormulaValue)> {
        vec![
            ((0, 0), 1.into()),
            ((1, 0), 2.into()),
            ((2, 0), 3.into()),
            ((3, 0), 4.into()),
            ((4, 0), 5.into()),
            ((0, 1), 2.into()),
            ((1, 1), 4.into()),
            ((2, 1), 6.into()),
            ((3, 1), 8.into()),
            ((4, 1), 10.into()),
        ]
    }

    fn db_grid() -> Vec<((u32, u32), FormulaValue)> {
        vec![
            ((0, 0), "Name".into()),
            ((0, 1), "Cat".into()),
            ((0, 2), "Qty".into()),
            ((1, 0), "a".into()),
            ((1, 1), "x".into()),
            ((1, 2), 10.into()),
            ((2, 0), "b".into()),
            ((2, 1), "y".into()),
            ((2, 2), 20.into()),
            ((3, 0), "c".into()),
            ((3, 1), "x".into()),
            ((3, 2), 30.into()),
            ((0, 4), "Cat".into()),
            ((1, 4), "x".into()),
            ((0, 6), "Cat".into()),
            ((1, 6), "z".into()),
        ]
    }

    #[test]
    fn registration_scale() {
        let reg = BuiltinRegistry::new();
        assert!(
            reg.names().len() >= 270,
            "builtin count {} < 270",
            reg.names().len()
        );
        for f in [
            "SIN",
            "PMT",
            "GEOMEAN",
            "DSUM",
            "TEXTBEFORE",
            "NORM.S.DIST",
            "CEILING.MATH",
            "ROMAN",
            "ADDRESS",
            "INDIRECT",
        ] {
            assert!(reg.get(f).is_some(), "{f} not registered");
        }
    }

    #[test]
    fn math_trig() {
        near(ev("SIN(PI()/6)", &[]), 0.5);
        near(ev("COS(0)", &[]), 1.0);
        near(ev("TAN(PI()/4)", &[]), 1.0);
        near(ev("ASIN(1)", &[]), PI / 2.0);
        near(ev("ATAN2(1,1)", &[]), PI / 4.0);
        near(ev("SINH(0)", &[]), 0.0);
        near(ev("DEGREES(PI())", &[]), 180.0);
        near(ev("RADIANS(180)", &[]), PI);
    }

    #[test]
    fn math_log_exp() {
        near(ev("LN(EXP(1))", &[]), 1.0);
        near(ev("LOG10(1000)", &[]), 3.0);
        near(ev("LOG(8,2)", &[]), 3.0);
        near(ev("LOG(100)", &[]), 2.0);
        assert_eq!(ev("LN(0)", &[]), err(FormulaError::Num));
        assert_eq!(ev("LN(-1)", &[]), err(FormulaError::Num));
    }

    #[test]
    fn math_combinatorics() {
        assert_eq!(ev("FACT(5)", &[]), FormulaValue::Number(120.0));
        assert_eq!(ev("FACT(0)", &[]), FormulaValue::Number(1.0));
        assert_eq!(ev("FACTDOUBLE(7)", &[]), FormulaValue::Number(105.0));
        assert_eq!(ev("COMBIN(5,2)", &[]), FormulaValue::Number(10.0));
        assert_eq!(ev("COMBINA(4,3)", &[]), FormulaValue::Number(20.0));
        assert_eq!(ev("PERMUT(5,2)", &[]), FormulaValue::Number(20.0));
        assert_eq!(ev("MULTINOMIAL(2,3,4)", &[]), FormulaValue::Number(1260.0));
        assert_eq!(ev("QUOTIENT(17,5)", &[]), FormulaValue::Number(3.0));
    }

    #[test]
    fn math_rounding_siblings() {
        assert_eq!(ev("CEILING.MATH(6.7)", &[]), FormulaValue::Number(7.0));
        assert_eq!(ev("CEILING.MATH(-8.1,2)", &[]), FormulaValue::Number(-8.0));
        assert_eq!(
            ev("CEILING.MATH(-8.1,2,1)", &[]),
            FormulaValue::Number(-10.0)
        );
        assert_eq!(ev("FLOOR.MATH(5.9)", &[]), FormulaValue::Number(5.0));
        assert_eq!(ev("FLOOR.MATH(-5.5,2,1)", &[]), FormulaValue::Number(-4.0));
        assert_eq!(
            ev("CEILING.PRECISE(-4.3,2)", &[]),
            FormulaValue::Number(-4.0)
        );
        assert_eq!(ev("FLOOR.PRECISE(-4.3,2)", &[]), FormulaValue::Number(-6.0));
    }

    #[test]
    fn math_base_roman() {
        assert_eq!(ev("BASE(255,16)", &[]), "FF".into());
        assert_eq!(ev("BASE(7,2,8)", &[]), "00000111".into());
        assert_eq!(ev("DECIMAL(\"FF\",16)", &[]), FormulaValue::Number(255.0));
        assert_eq!(ev("DECIMAL(\"111\",2)", &[]), FormulaValue::Number(7.0));
        assert_eq!(ev("ROMAN(1994)", &[]), "MCMXCIV".into());
        assert_eq!(ev("ARABIC(\"MCMXCIV\")", &[]), FormulaValue::Number(1994.0));
    }

    #[test]
    fn math_seriessum_sqrtpi() {
        assert_eq!(
            ev("SERIESSUM(1,0,1,{1,2,3})", &[]),
            FormulaValue::Number(6.0)
        );
        near(ev("SQRTPI(1)", &[]), PI.sqrt());
    }

    #[test]
    fn financial_annuity() {
        near_p(ev("PMT(0.005,360,-200000)", &[]), 1199.10, 0.05);
        near_p(ev("FV(0.005,120,-100)", &[]), 16387.93, 0.05);
        near_p(ev("PV(0.08,20,-500)", &[]), 4909.07, 0.05);
        near_p(ev("NPER(0.01,-100,5000)", &[]), 69.66, 0.05);
        near(ev("RATE(360,-1199.10,200000)", &[]), 0.005);
    }

    #[test]
    fn financial_ipmt_ppmt() {
        let pmt = n(ev("PMT(0.005,360,-200000)", &[]));
        let ip = n(ev("IPMT(0.005,1,360,-200000)", &[]));
        let pp = n(ev("PPMT(0.005,1,360,-200000)", &[]));
        assert!((ip + pp - pmt).abs() < 0.01);
        assert!((ip - 1000.0).abs() < 0.01);
    }

    #[test]
    fn financial_cumulative() {
        let p1 = n(ev("PPMT(0.005,1,360,200000)", &[]));
        let p2 = n(ev("PPMT(0.005,2,360,200000)", &[]));
        assert!((n(ev("CUMPRINC(0.005,360,200000,1,2,0)", &[])) - (p1 + p2)).abs() < 0.01);
        assert!(n(ev("CUMIPMT(0.005,360,200000,1,12,0)", &[])) < 0.0);
    }

    #[test]
    fn financial_investment() {
        near_p(ev("NPV(0.1,100,200,300)", &[]), 481.59, 0.01);
        near_p(ev("IRR({-100,50,60,70})", &[]), 0.3387, 0.001);
        near_p(
            ev("MIRR({-1000,300,400,500,600},0.1,0.12)", &[]),
            0.2014,
            0.001,
        );
    }

    #[test]
    fn financial_xnpv_xirr() {
        near_p(ev("XNPV(0.1,{-1000,1100},{40000,40365})", &[]), 0.0, 0.05);
        near_p(ev("XIRR({-1000,1100},{40000,40365})", &[]), 0.1, 0.001);
    }

    #[test]
    fn financial_depreciation() {
        assert_eq!(ev("SLN(10000,1000,5)", &[]), FormulaValue::Number(1800.0));
        assert_eq!(ev("SYD(10000,1000,5,1)", &[]), FormulaValue::Number(3000.0));
        near_p(ev("DDB(10000,1000,5,1)", &[]), 4000.0, 0.01);
        near_p(ev("DB(10000,1000,5,1)", &[]), 3690.0, 0.5);
    }

    #[test]
    fn financial_rate_conversion() {
        near(ev("EFFECT(0.10,4)", &[]), 0.1038);
        near_p(ev("NOMINAL(0.1038,4)", &[]), 0.10, 0.001);
        near_p(ev("RRI(8,10000,20000)", &[]), 0.0905, 0.001);
        near_p(ev("PDURATION(0.05,1000,2000)", &[]), 14.21, 0.01);
    }

    #[test]
    fn stat_means() {
        near(ev("GEOMEAN(4,9)", &[]), 6.0);
        near(ev("HARMEAN(1,2,4)", &[]), 12.0 / 7.0);
        assert_eq!(
            ev("TRIMMEAN({1,2,3,4,100},0.4)", &[]),
            FormulaValue::Number(3.0)
        );
        assert_eq!(ev("AVERAGEA(2,4,\"x\")", &[]), FormulaValue::Number(2.0));
    }

    #[test]
    fn stat_dispersion() {
        assert_eq!(ev("DEVSQ(2,4,6)", &[]), FormulaValue::Number(8.0));
        near(ev("AVEDEV(2,4,6)", &[]), 4.0 / 3.0);
        near_p(ev("SKEW(1,2,3,4,5)", &[]), 0.0, 1e-6);
        near_p(ev("KURT(1,2,3,4,5)", &[]), -1.2, 0.1);
    }

    #[test]
    fn stat_bivariate() {
        let g = ab_grid();
        near(ev("CORREL(A1:A5,B1:B5)", &g), 1.0);
        near(ev("RSQ(B1:B5,A1:A5)", &g), 1.0);
        near(ev("SLOPE(B1:B5,A1:A5)", &g), 2.0);
        near(ev("INTERCEPT(B1:B5,A1:A5)", &g), 0.0);
        near(ev("FORECAST(6,B1:B5,A1:A5)", &g), 12.0);
        near(ev("COVAR(A1:A5,B1:B5)", &g), 4.0);
    }

    #[test]
    fn stat_order() {
        let g = ab_grid();
        assert_eq!(ev("QUARTILE(A1:A5,2)", &g), FormulaValue::Number(3.0));
        assert_eq!(ev("QUARTILE(A1:A5,0)", &g), FormulaValue::Number(1.0));
        assert_eq!(ev("QUARTILE.EXC(A1:A5,1)", &g), FormulaValue::Number(1.5));
        near(ev("PERCENTRANK(A1:A5,3)", &g), 0.5);
        assert_eq!(ev("RANK.AVG(3,{1,3,3,5})", &[]), FormulaValue::Number(2.5));
    }

    #[test]
    fn stat_normal() {
        near(ev("NORM.S.DIST(0,TRUE)", &[]), 0.5);
        near_p(ev("NORM.S.DIST(1.96,TRUE)", &[]), 0.975, 0.001);
        near_p(ev("NORM.S.INV(0.975)", &[]), 1.96, 0.01);
        near(ev("NORM.DIST(40,40,5,TRUE)", &[]), 0.5);
        near(ev("STANDARDIZE(50,40,5)", &[]), 2.0);
        near_p(ev("CONFIDENCE(0.05,2.5,50)", &[]), 0.693, 0.01);
    }

    #[test]
    fn database_functions() {
        let d = db_grid();
        assert_eq!(
            ev("DSUM(A1:C4,\"Qty\",E1:E2)", &d),
            FormulaValue::Number(40.0)
        );
        assert_eq!(ev("DSUM(A1:C4,3,E1:E2)", &d), FormulaValue::Number(40.0));
        assert_eq!(
            ev("DCOUNT(A1:C4,\"Qty\",E1:E2)", &d),
            FormulaValue::Number(2.0)
        );
        assert_eq!(
            ev("DAVERAGE(A1:C4,\"Qty\",E1:E2)", &d),
            FormulaValue::Number(20.0)
        );
        assert_eq!(
            ev("DMAX(A1:C4,\"Qty\",E1:E2)", &d),
            FormulaValue::Number(30.0)
        );
        assert_eq!(
            ev("DMIN(A1:C4,\"Qty\",E1:E2)", &d),
            FormulaValue::Number(10.0)
        );
    }

    #[test]
    fn database_dget_dproduct() {
        let d = db_grid();
        assert_eq!(
            ev("DGET(A1:C4,\"Qty\",G1:G2)", &d),
            err(FormulaError::Value)
        );
        assert_eq!(ev("DGET(A1:C4,\"Qty\",E1:E2)", &d), err(FormulaError::Num));
        assert_eq!(
            ev("DPRODUCT(A1:C4,\"Qty\",E1:E2)", &d),
            FormulaValue::Number(300.0)
        );
        near(ev("DSTDEV(A1:C4,\"Qty\",E1:E2)", &d), (200.0f64).sqrt());
    }

    #[test]
    fn textref_text() {
        assert_eq!(ev("TEXTBEFORE(\"a-b-c\",\"-\")", &[]), "a".into());
        assert_eq!(ev("TEXTAFTER(\"a-b-c\",\"-\")", &[]), "b-c".into());
        assert_eq!(ev("TEXTAFTER(\"a-b-c\",\"-\",2)", &[]), "c".into());
        assert_eq!(ev("TEXTBEFORE(\"a-b-c\",\"-\",-1)", &[]), "a-b".into());
        assert_eq!(ev("TEXTBEFORE(\"abc\",\"-\")", &[]), err(FormulaError::Na));
        assert_eq!(
            ev("TEXTBEFORE(\"abc\",\"-\",1,\"none\")", &[]),
            "none".into()
        );
    }

    #[test]
    fn textref_fixed_dollar_clean() {
        assert_eq!(ev("FIXED(1234.567,1)", &[]), "1,234.6".into());
        assert_eq!(ev("FIXED(1234.567,1,TRUE)", &[]), "1234.6".into());
        assert_eq!(ev("DOLLAR(1234.5)", &[]), "$1,234.50".into());
        assert_eq!(ev("DOLLAR(-1234.5)", &[]), "($1,234.50)".into());
        assert_eq!(ev("CLEAN(\"a\"&CHAR(9)&\"b\")", &[]), "ab".into());
    }

    #[test]
    fn textref_address() {
        assert_eq!(ev("ADDRESS(2,3)", &[]), "$C$2".into());
        assert_eq!(ev("ADDRESS(2,3,2)", &[]), "C$2".into());
        assert_eq!(ev("ADDRESS(2,3,4)", &[]), "C2".into());
        assert_eq!(ev("ADDRESS(2,3,1,FALSE)", &[]), "R2C3".into());
        assert_eq!(
            ev("ADDRESS(2,3,1,TRUE,\"Sheet1\")", &[]),
            "Sheet1!$C$2".into()
        );
    }

    #[test]
    fn textref_indirect() {
        let c = [((0u32, 0u32), 111.into()), ((4, 2), 999.into())];
        assert_eq!(ev("INDIRECT(\"A1\")", &c), FormulaValue::Number(111.0));
        assert_eq!(ev("INDIRECT(\"C5\")", &c), FormulaValue::Number(999.0));
        assert_eq!(
            ev("INDIRECT(\"R5C3\",FALSE)", &c),
            FormulaValue::Number(999.0)
        );
        assert_eq!(ev("INDIRECT(\"A1:C5\")", &c), FormulaValue::Number(111.0));
    }

    #[test]
    fn textref_info() {
        assert_eq!(ev("XOR(TRUE,FALSE,FALSE)", &[]), FormulaValue::Bool(true));
        assert_eq!(ev("XOR(TRUE,TRUE)", &[]), FormulaValue::Bool(false));
        assert_eq!(ev("ERROR.TYPE(NA())", &[]), FormulaValue::Number(7.0));
        assert_eq!(ev("ERROR.TYPE(1/0)", &[]), FormulaValue::Number(2.0));
        assert_eq!(ev("ISREF(A1:B2)", &[]), FormulaValue::Bool(true));
        assert_eq!(ev("ISREF(5)", &[]), FormulaValue::Bool(false));
    }

    #[test]
    fn error_propagation() {
        assert_eq!(ev("ASIN(2)", &[]), err(FormulaError::Num));
        assert_eq!(ev("SQRTPI(-1)", &[]), err(FormulaError::Num));
        assert_eq!(ev("FACT(-1)", &[]), err(FormulaError::Num));
        assert_eq!(ev("COMBIN(2,5)", &[]), err(FormulaError::Num));
        assert_eq!(ev("GEOMEAN(-1,2)", &[]), err(FormulaError::Num));
        assert_eq!(ev("SIN(1/0)", &[]), err(FormulaError::Div0));
        assert_eq!(ev("PMT(NA(),10,100)", &[]), err(FormulaError::Na));
        assert_eq!(ev("DEGREES(SQRT(-1))", &[]), err(FormulaError::Num));
    }
}
