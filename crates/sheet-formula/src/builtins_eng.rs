//! 工程函数族（Excel「工程」类别，cmx 原零覆盖）—— 对拍 cmx-megasheet 的
//! `builtins/engineering.ts`，值语义逐一对齐。
//!
//! 覆盖：进制转换（BIN/OCT/DEC/HEX 互转，10 位补码）、位运算（BITAND/OR/XOR/
//! LSHIFT/RSHIFT，48 位无符号）、DELTA/GESTEP、误差函数 ERF/ERFC(+.PRECISE)、
//! 贝塞尔 BESSELI/J/K/Y、单位换算 CONVERT、复数 COMPLEX + IM* 一族。
//!
//! 对拍 Excel：域外/溢出 → #NUM!；非法 → #VALUE!；结果非有限 → #NUM!。
//! 纯逻辑、零 DOM。作为独立模块并入 BuiltinRegistry（镜像 builtins_m8/m17）。
//!
//! 贝塞尔函数里的 `0.636619772`（=2/π）等系数是 Numerical Recipes 原算法字面量，
//! 与 TS `builtins/engineering.ts` 逐字对齐以保跨引擎值 parity——故此模块整体
//! 豁免 `clippy::approx_constant`（用 `FRAC_2_PI` 常量会与 TS 侧的字面量产生末位差异）。
#![allow(clippy::approx_constant)]

use std::rc::Rc;

use sheet_core::date_serial::{date_to_serial, serial_to_parts};

use crate::evaluator::{flatten_arg, scalar_arg, EvalContext, EvaluatedArg, FunctionImpl};
use crate::value::{to_boolean, to_number, to_text, FormulaError, FormulaValue};

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

/// 必备数值参：区域取左上角，强制为数字。
fn req_num(args: &[EvaluatedArg], i: usize) -> Result<f64, FormulaError> {
    to_number(&scalar_arg(args.get(i)))
}

/// 可选数值参：缺省/空 → default。
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

/// 必备文本参。
fn req_text(args: &[EvaluatedArg], i: usize) -> Result<String, FormulaError> {
    to_text(&scalar_arg(args.get(i)))
}

// ── 进制转换（10 位补码；BIN/OCT/HEX 负数用 2^10/8^10/16^10 补偿）──

#[derive(Clone, Copy)]
enum Radix {
    Bin,
    Oct,
    Hex,
}

impl Radix {
    fn base(self) -> u32 {
        match self {
            Radix::Bin => 2,
            Radix::Oct => 8,
            Radix::Hex => 16,
        }
    }
    /// 负数补偿基 2^bits（bits=10*log2(base)）。
    fn neg(self) -> f64 {
        match self {
            Radix::Bin => 1024.0,         // 2^10
            Radix::Oct => 8f64.powi(10),  // 8^10
            Radix::Hex => 16f64.powi(10), // 16^10
        }
    }
}

/// 解析源进制字符串 → 十进制整数（补码负数）。
fn parse_radix(text: &str, radix: Radix) -> Result<f64, FormulaError> {
    let t = text.trim().to_ascii_uppercase();
    if t.is_empty() {
        return Ok(0.0);
    }
    if t.len() > 10 {
        return Err(FormulaError::Num);
    }
    let ok = t.chars().all(|c| c.is_digit(radix.base()));
    if !ok {
        return Err(FormulaError::Num);
    }
    let val = i64::from_str_radix(&t, radix.base()).map_err(|_| FormulaError::Num)? as f64;
    let neg = radix.neg();
    if t.len() == 10 && val >= neg / 2.0 {
        Ok(val - neg)
    } else {
        Ok(val)
    }
}

/// 十进制整数 → 目标进制字符串（补码负数），可选左补零到 places。
fn to_radix(n: f64, radix: Radix, places: Option<i64>) -> FormulaValue {
    let n = n.trunc();
    let neg = radix.neg();
    if n < -neg / 2.0 || n > neg / 2.0 - 1.0 {
        return err(FormulaError::Num);
    }
    let base = radix.base() as i64;
    let mag = if n < 0.0 { (n + neg) as i64 } else { n as i64 };
    let mut s = radix_string(mag, base);
    if let Some(p) = places {
        if n < 0.0 {
            return FormulaValue::Text(s); // 负数忽略 places
        }
        if !(0..=10).contains(&p) {
            return err(FormulaError::Num);
        }
        if s.len() as i64 > p {
            return err(FormulaError::Num);
        }
        while (s.len() as i64) < p {
            s.insert(0, '0');
        }
    }
    FormulaValue::Text(s)
}

/// 非负整数 → base 进制大写字符串。
fn radix_string(mut n: i64, base: i64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789ABCDEF";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % base) as usize]);
        n /= base;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

// ── 位运算（48 位无符号）──
const BIT_MAX: i64 = 281474976710655; // 2^48 - 1

fn bit_check(n: f64) -> Result<i64, FormulaError> {
    let n = n.trunc();
    if n < 0.0 || n > BIT_MAX as f64 {
        return Err(FormulaError::Num);
    }
    Ok(n as i64)
}

/* <!--RENG1--> */

// ── 误差函数 erf/erfc（Abramowitz–Stegun 7.1.26）──
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let ax = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * ax);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-ax * ax).exp();
    sign * y
}
fn erfc(x: f64) -> f64 {
    1.0 - erf(x)
}

// ── 贝塞尔（对拍 TS：级数 + 渐近）──
fn bessel_j0(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 8.0 {
        let y = x * x;
        let p1 = -2957821389.0
            + y * (7062834065.0
                + y * (-512359803.6 + y * (10879881.29 + y * (-86327.92757 + y * 228.4622733))));
        let p2 = 40076544269.0
            + y * (745249964.8 + y * (7189466.438 + y * (47447.26470 + y * (226.1030244 + y))));
        p1 / p2
    } else {
        let z = 8.0 / ax;
        let y = z * z;
        let xx = ax - 0.785398164;
        let p1 = 1.0
            + y * (-0.1098628627e-2
                + y * (0.2734510407e-4 + y * (-0.2073370639e-5 + y * 0.2093887211e-6)));
        let p2 = -0.1562499995e-1
            + y * (0.1430488765e-3
                + y * (-0.6911147651e-5 + y * (0.7621095161e-6 + y * (-0.934935152e-7))));
        (0.636619772 / ax).sqrt() * (xx.cos() * p1 - z * xx.sin() * p2)
    }
}
fn bessel_j1(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 8.0 {
        let y = x * x;
        let p1 = x
            * (72362614232.0
                + y * (-7895059235.0
                    + y * (242396853.1
                        + y * (-2972611.439 + y * (15704.48260 + y * (-30.16036606))))));
        let p2 = 144725228442.0
            + y * (2300535178.0 + y * (18583304.74 + y * (99447.43394 + y * (376.9991397 + y))));
        p1 / p2
    } else {
        let z = 8.0 / ax;
        let y = z * z;
        let xx = ax - 2.356194491;
        let p1 = 1.0
            + y * (0.183105e-2
                + y * (-0.3516396496e-4 + y * (0.2457520174e-5 + y * (-0.240337019e-6))));
        let p2 = 0.04687499995
            + y * (-0.2002690873e-3
                + y * (0.8449199096e-5 + y * (-0.88228987e-6 + y * 0.105787412e-6)));
        let ans = (0.636619772 / ax).sqrt() * (xx.cos() * p1 - z * xx.sin() * p2);
        if x < 0.0 {
            -ans
        } else {
            ans
        }
    }
}
fn bessel_jn(n: f64, x: f64) -> Result<f64, FormulaError> {
    let n = n.trunc() as i64;
    if n < 0 {
        return Err(FormulaError::Num);
    }
    if n == 0 {
        return Ok(bessel_j0(x));
    }
    if n == 1 {
        return Ok(bessel_j1(x));
    }
    if x == 0.0 {
        return Ok(0.0);
    }
    let ax = x.abs();
    let mut ans;
    if ax > n as f64 {
        let tox = 2.0 / ax;
        let mut bjm = bessel_j0(ax);
        let mut bj = bessel_j1(ax);
        for j in 1..n {
            let bjp = j as f64 * tox * bj - bjm;
            bjm = bj;
            bj = bjp;
        }
        ans = bj;
    } else {
        let tox = 2.0 / ax;
        let m = 2 * ((n as f64 + (40.0 * n as f64).sqrt()).floor() as i64 / 2);
        let mut jsum = false;
        let mut bjp = 0.0;
        let mut bj = 1.0;
        let mut sum = 0.0;
        ans = 0.0;
        for j in (1..=m).rev() {
            let bjm = j as f64 * tox * bj - bjp;
            bjp = bj;
            bj = bjm;
            if bj.abs() > 1e10 {
                bj *= 1e-10;
                bjp *= 1e-10;
                ans *= 1e-10;
                sum *= 1e-10;
            }
            if jsum {
                sum += bj;
            }
            jsum = !jsum;
            if j == n {
                ans = bjp;
            }
        }
        sum = 2.0 * sum - bj;
        ans /= sum;
    }
    Ok(if x < 0.0 && n % 2 == 1 { -ans } else { ans })
}
fn bessel_y0(x: f64) -> f64 {
    if x < 8.0 {
        let y = x * x;
        let p1 = -2957821389.0
            + y * (7062834065.0
                + y * (-512359803.6 + y * (10879881.29 + y * (-86327.92757 + y * 228.4622733))));
        let p2 = 40076544269.0
            + y * (745249964.8 + y * (7189466.438 + y * (47447.26470 + y * (226.1030244 + y))));
        p1 / p2 + 0.636619772 * bessel_j0(x) * x.ln()
    } else {
        let z = 8.0 / x;
        let y = z * z;
        let xx = x - 0.785398164;
        let p1 = 1.0
            + y * (-0.1098628627e-2
                + y * (0.2734510407e-4 + y * (-0.2073370639e-5 + y * 0.2093887211e-6)));
        let p2 = -0.1562499995e-1
            + y * (0.1430488765e-3
                + y * (-0.6911147651e-5 + y * (0.7621095161e-6 + y * (-0.934935152e-7))));
        (0.636619772 / x).sqrt() * (xx.sin() * p1 + z * xx.cos() * p2)
    }
}
fn bessel_y1(x: f64) -> f64 {
    if x < 8.0 {
        let y = x * x;
        let p1 = x
            * (-4.900604943e13
                + y * (1.275274390e13
                    + y * (-5.153438139e11
                        + y * (7.349264551e9 + y * (-4.237922726e7 + y * 8.511937935e4)))));
        let p2 = 2.499580570e14
            + y * (4.244419664e12
                + y * (3.733650367e10
                    + y * (2.245904002e8 + y * (1.020426050e6 + y * (3.549632885e3 + y)))));
        p1 / p2 + 0.636619772 * (bessel_j1(x) * x.ln() - 1.0 / x)
    } else {
        let z = 8.0 / x;
        let y = z * z;
        let xx = x - 2.356194491;
        let p1 = 1.0
            + y * (0.183105e-2
                + y * (-0.3516396496e-4 + y * (0.2457520174e-5 + y * (-0.240337019e-6))));
        let p2 = 0.04687499995
            + y * (-0.2002690873e-3
                + y * (0.8449199096e-5 + y * (-0.88228987e-6 + y * 0.105787412e-6)));
        (0.636619772 / x).sqrt() * (xx.sin() * p1 + z * xx.cos() * p2)
    }
}
fn bessel_yn(n: f64, x: f64) -> Result<f64, FormulaError> {
    let n = n.trunc() as i64;
    if n < 0 || x <= 0.0 {
        return Err(FormulaError::Num);
    }
    if n == 0 {
        return Ok(bessel_y0(x));
    }
    if n == 1 {
        return Ok(bessel_y1(x));
    }
    let tox = 2.0 / x;
    let mut by = bessel_y1(x);
    let mut bym = bessel_y0(x);
    for j in 1..n {
        let byp = j as f64 * tox * by - bym;
        bym = by;
        by = byp;
    }
    Ok(by)
}
fn bessel_i0(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 3.75 {
        let y = (x / 3.75).powi(2);
        1.0 + y
            * (3.5156229
                + y * (3.0899424
                    + y * (1.2067492 + y * (0.2659732 + y * (0.360768e-1 + y * 0.45813e-2)))))
    } else {
        let y = 3.75 / ax;
        (ax.exp() / ax.sqrt())
            * (0.39894228
                + y * (0.1328592e-1
                    + y * (0.225319e-2
                        + y * (-0.157565e-2
                            + y * (0.916281e-2
                                + y * (-0.2057706e-1
                                    + y * (0.2635537e-1
                                        + y * (-0.1647633e-1 + y * 0.392377e-2))))))))
    }
}
fn bessel_i1(x: f64) -> f64 {
    let ax = x.abs();
    let ans = if ax < 3.75 {
        let y = (x / 3.75).powi(2);
        ax * (0.5
            + y * (0.87890594
                + y * (0.51498869
                    + y * (0.15084934 + y * (0.2658733e-1 + y * (0.301532e-2 + y * 0.32411e-3))))))
    } else {
        let y = 3.75 / ax;
        let p = 0.2282967e-1 + y * (-0.2895312e-1 + y * (0.1787654e-1 - y * 0.420059e-2));
        let q = 0.39894228
            + y * (-0.3988024e-1
                + y * (-0.362018e-2 + y * (0.163801e-2 + y * (-0.1031555e-1 + y * p))));
        (ax.exp() / ax.sqrt()) * q
    };
    if x < 0.0 {
        -ans
    } else {
        ans
    }
}
fn bessel_in(n: f64, x: f64) -> Result<f64, FormulaError> {
    let n = n.trunc() as i64;
    if n < 0 {
        return Err(FormulaError::Num);
    }
    if n == 0 {
        return Ok(bessel_i0(x));
    }
    if n == 1 {
        return Ok(bessel_i1(x));
    }
    if x == 0.0 {
        return Ok(0.0);
    }
    let tox = 2.0 / x.abs();
    let mut bip = 0.0;
    let mut bi = 1.0;
    let mut ans = 0.0;
    let m = 2 * (n + (40.0 * n as f64).sqrt().floor() as i64);
    for j in (1..=m).rev() {
        let bim = bip + j as f64 * tox * bi;
        bip = bi;
        bi = bim;
        if bi.abs() > 1e10 {
            ans *= 1e-10;
            bi *= 1e-10;
            bip *= 1e-10;
        }
        if j == n {
            ans = bip;
        }
    }
    ans *= bessel_i0(x) / bi;
    Ok(if x < 0.0 && n % 2 == 1 { -ans } else { ans })
}
fn bessel_k0(x: f64) -> f64 {
    if x <= 2.0 {
        let y = x * x / 4.0;
        -(x / 2.0).ln() * bessel_i0(x)
            + (-0.57721566
                + y * (0.42278420
                    + y * (0.23069756
                        + y * (0.3488590e-1 + y * (0.262698e-2 + y * (0.10750e-3 + y * 0.74e-5))))))
    } else {
        let y = 2.0 / x;
        ((-x).exp() / x.sqrt())
            * (1.25331414
                + y * (-0.7832358e-1
                    + y * (0.2189568e-1
                        + y * (-0.1062446e-1
                            + y * (0.587872e-2 + y * (-0.251540e-2 + y * 0.53208e-3))))))
    }
}
fn bessel_k1(x: f64) -> f64 {
    if x <= 2.0 {
        let y = x * x / 4.0;
        (x / 2.0).ln() * bessel_i1(x)
            + (1.0 / x)
                * (1.0
                    + y * (0.15443144
                        + y * (-0.67278579
                            + y * (-0.18156897
                                + y * (-0.1919402e-1 + y * (-0.110404e-2 + y * (-0.4686e-4)))))))
    } else {
        let y = 2.0 / x;
        ((-x).exp() / x.sqrt())
            * (1.25331414
                + y * (0.23498619
                    + y * (-0.3655620e-1
                        + y * (0.1504268e-1
                            + y * (-0.780353e-2 + y * (0.325614e-2 + y * (-0.68245e-3)))))))
    }
}
fn bessel_kn(n: f64, x: f64) -> Result<f64, FormulaError> {
    let n = n.trunc() as i64;
    if n < 0 || x <= 0.0 {
        return Err(FormulaError::Num);
    }
    if n == 0 {
        return Ok(bessel_k0(x));
    }
    if n == 1 {
        return Ok(bessel_k1(x));
    }
    let tox = 2.0 / x;
    let mut bkm = bessel_k0(x);
    let mut bk = bessel_k1(x);
    for j in 1..n {
        let bkp = bkm + j as f64 * tox * bk;
        bkm = bk;
        bk = bkp;
    }
    Ok(bk)
}

/* <!--RENG2--> */

// ── 复数 "a+bi" / "a+bj" ──
#[derive(Clone, Copy)]
struct Cx {
    re: f64,
    im: f64,
    suf: char, // 'i' | 'j'
}

fn parse_cx(text: &str) -> Result<Cx, FormulaError> {
    let t = text.trim();
    if t.is_empty() {
        return Err(FormulaError::Num);
    }
    // 纯实数（不以 i/j 结尾）
    let ends_ij = t.ends_with('i') || t.ends_with('j') || t.ends_with('I') || t.ends_with('J');
    if !ends_ij {
        return match t.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(Cx {
                re: v,
                im: 0.0,
                suf: 'i',
            }),
            _ => Err(FormulaError::Num),
        };
    }
    let suf = if t.ends_with('j') || t.ends_with('J') {
        'j'
    } else {
        'i'
    };
    let body = &t[..t.len() - 1];
    // 找主号（非指数号）分割 re / im
    let bytes = body.as_bytes();
    let mut split: isize = -1;
    for k in 1..bytes.len() {
        let c = bytes[k] as char;
        let prev = bytes[k - 1] as char;
        if (c == '+' || c == '-') && prev != 'e' && prev != 'E' {
            split = k as isize;
        }
    }
    let (re_s, im_s) = if split == -1 {
        ("", body)
    } else {
        (&body[..split as usize], &body[split as usize..])
    };
    let re = if re_s.is_empty() {
        0.0
    } else {
        re_s.parse::<f64>().map_err(|_| FormulaError::Num)?
    };
    let im = if im_s.is_empty() || im_s == "+" {
        1.0
    } else if im_s == "-" {
        -1.0
    } else {
        im_s.parse::<f64>().map_err(|_| FormulaError::Num)?
    };
    if !re.is_finite() || !im.is_finite() {
        return Err(FormulaError::Num);
    }
    Ok(Cx { re, im, suf })
}

/// 数字 → Excel 复数分量文本（对齐 JS Number.toString 的最短表示）。
fn num_str(x: f64) -> String {
    if !x.is_finite() {
        return "0".to_string();
    }
    // 对齐 JS Number.toString()：|x|<1e-6 或 |x|>=1e21 用指数记法（如 6.123e-17），
    // 其余用最短十进制。Rust `{}` 默认对小数走十进制（0.00000…）与 JS 不一致，
    // 复数分量文本须与 TS 逐字相同，故此处显式仿 JS 阈值。
    if x == 0.0 {
        return "0".to_string();
    }
    let ax = x.abs();
    if !(1e-6..1e21).contains(&ax) {
        js_exponential(x)
    } else {
        format!("{}", x)
    }
}

/// 仿 JS 指数记法：尾数最短表示 + `e±exp`（exp 无前导零、正号保留）。
fn js_exponential(x: f64) -> String {
    // Rust `{:e}` 产出 `6.123233995736766e-17` 形式（尾数最短、指数无前导零、负号有、
    // 正号无）。JS 对正指数带 '+'（1e+21），负指数带 '-'。补正号即对齐。
    let s = format!("{:e}", x);
    if let Some(pos) = s.find('e') {
        let (mant, exp) = s.split_at(pos);
        let exp_digits = &exp[1..]; // 去掉 'e'
        if let Some(stripped) = exp_digits.strip_prefix('-') {
            format!("{mant}e-{stripped}")
        } else {
            format!("{mant}e+{exp_digits}")
        }
    } else {
        s
    }
}

fn fmt_cx(re: f64, im: f64, suf: char) -> String {
    if im == 0.0 {
        return num_str(re);
    }
    if re == 0.0 {
        if im == 1.0 {
            return suf.to_string();
        }
        if im == -1.0 {
            return format!("-{}", suf);
        }
        return format!("{}{}", num_str(im), suf);
    }
    let im_part = if im == 1.0 {
        format!("+{}", suf)
    } else if im == -1.0 {
        format!("-{}", suf)
    } else if im > 0.0 {
        format!("+{}{}", num_str(im), suf)
    } else {
        format!("{}{}", num_str(im), suf)
    };
    format!("{}{}", num_str(re), im_part)
}

fn req_cx(args: &[EvaluatedArg], i: usize) -> Result<Cx, FormulaError> {
    let t = to_text(&scalar_arg(args.get(i)))?;
    parse_cx(&t)
}

/// 复数一元运算工厂：闭包返回 (re,im) 或错误，保留输入尾缀。
fn cx1(args: &[EvaluatedArg], f: impl Fn(Cx) -> Result<(f64, f64), FormulaError>) -> FormulaValue {
    let z = match req_cx(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    match f(z) {
        Ok((re, im)) if re.is_finite() && im.is_finite() => {
            FormulaValue::Text(fmt_cx(re, im, z.suf))
        }
        Ok(_) => err(FormulaError::Num),
        Err(e) => err(e),
    }
}

fn cx_tan(z: Cx) -> Result<(f64, f64), FormulaError> {
    let s = (z.re.sin() * z.im.cosh(), z.re.cos() * z.im.sinh());
    let c = (z.re.cos() * z.im.cosh(), -z.re.sin() * z.im.sinh());
    let d = c.0 * c.0 + c.1 * c.1;
    if d == 0.0 {
        return Err(FormulaError::Num);
    }
    Ok(((s.0 * c.0 + s.1 * c.1) / d, (s.1 * c.0 - s.0 * c.1) / d))
}
fn cx_recip(re: f64, im: f64) -> Result<(f64, f64), FormulaError> {
    let d = re * re + im * im;
    if d == 0.0 {
        return Err(FormulaError::Num);
    }
    Ok((re / d, -im / d))
}

// ── CONVERT ──
fn unit_factor(u: &str) -> Option<(&'static str, f64)> {
    Some(match u {
        // 重量 w（基准 g）
        "g" => ("w", 1.0),
        "kg" => ("w", 1000.0),
        "mg" => ("w", 0.001),
        "lbm" => ("w", 453.59237),
        "ozm" => ("w", 28.349523125),
        "u" => ("w", 1.66053886e-24),
        "sg" => ("w", 14593.9029),
        "stone" => ("w", 6350.29318),
        "ton" => ("w", 907184.74),
        // 距离 d（基准 m）
        "m" => ("d", 1.0),
        "km" => ("d", 1000.0),
        "cm" => ("d", 0.01),
        "mm" => ("d", 0.001),
        "mi" => ("d", 1609.344),
        "in" => ("d", 0.0254),
        "ft" => ("d", 0.3048),
        "yd" => ("d", 0.9144),
        "ang" => ("d", 1e-10),
        "ly" => ("d", 9.4607304725808e15),
        "Nmi" => ("d", 1852.0),
        "pica" => ("d", 0.0254 / 6.0),
        // 时间 t（基准 s）
        "sec" | "s" => ("t", 1.0),
        "min" => ("t", 60.0),
        "hr" => ("t", 3600.0),
        "day" => ("t", 86400.0),
        "yr" => ("t", 31557600.0),
        // 压强 p（基准 Pa）
        "Pa" => ("p", 1.0),
        "atm" => ("p", 101325.0),
        "mmHg" => ("p", 133.322),
        "psi" => ("p", 6894.75729),
        "Torr" => ("p", 133.322368),
        // 力 F（基准 N）
        "N" => ("F", 1.0),
        "dyn" => ("F", 1e-5),
        "lbf" => ("F", 4.4482216152605),
        "pond" => ("F", 0.00980665),
        // 能量 e（基准 J）
        "J" => ("e", 1.0),
        "e" => ("e", 1e-7),
        "cal" => ("e", 4.1868),
        "c" => ("e", 4.184),
        "eV" => ("e", 1.602176634e-19),
        "HPh" => ("e", 2684519.5376961725),
        "Wh" => ("e", 3600.0),
        "flb" => ("e", 1.3558179483314004),
        "BTU" => ("e", 1055.05585262),
        // 功率 P（基准 W）
        "W" => ("P", 1.0),
        "HP" => ("P", 745.6998715822702),
        "PS" => ("P", 735.49875),
        // 体积 v（基准 L）
        "L" | "l" => ("v", 1.0),
        "tsp" => ("v", 0.00492892159375),
        "tbs" => ("v", 0.01478676478125),
        "oz" => ("v", 0.0295735295625),
        "cup" => ("v", 0.2365882365),
        "pt" => ("v", 0.473176473),
        "qt" => ("v", 0.946352946),
        "gal" => ("v", 3.785411784),
        "m3" => ("v", 1000.0),
        _ => return None,
    })
}

fn convert(val: f64, from: &str, to: &str) -> FormulaValue {
    fn is_temp(u: &str) -> bool {
        matches!(u, "C" | "F" | "K" | "cel" | "fah" | "kel")
    }
    fn norm(u: &str) -> &str {
        match u {
            "cel" => "C",
            "fah" => "F",
            "kel" => "K",
            other => other,
        }
    }
    if is_temp(from) || is_temp(to) {
        if !is_temp(from) || !is_temp(to) {
            return err(FormulaError::Na);
        }
        let nf = norm(from);
        let nt = norm(to);
        let c = match nf {
            "C" => val,
            "F" => (val - 32.0) * 5.0 / 9.0,
            _ => val - 273.15,
        };
        return finite(match nt {
            "C" => c,
            "F" => c * 9.0 / 5.0 + 32.0,
            _ => c + 273.15,
        });
    }
    match (unit_factor(from), unit_factor(to)) {
        (Some((qf, ff)), Some((qt, ft))) if qf == qt => finite(val * ff / ft),
        _ => err(FormulaError::Na),
    }
}

/* <!--RENG3--> */

/// 进制转换工厂函数（src → dec 返回数字；src → dst 返回补零字符串）。
fn conv_to_dec(args: &[EvaluatedArg], src: Radix) -> FormulaValue {
    let t = match req_text(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    match parse_radix(&t, src) {
        Ok(d) => FormulaValue::Number(d),
        Err(e) => err(e),
    }
}
fn conv_radix(args: &[EvaluatedArg], src: Radix, dst: Radix) -> FormulaValue {
    let t = match req_text(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let dec = match parse_radix(&t, src) {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let places = match places_arg(args, 1) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    to_radix(dec, dst, places)
}
fn dec_to(args: &[EvaluatedArg], dst: Radix) -> FormulaValue {
    let n = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let places = match places_arg(args, 1) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    to_radix(n, dst, places)
}
fn places_arg(args: &[EvaluatedArg], i: usize) -> Result<Option<i64>, FormulaError> {
    match args.get(i) {
        None => Ok(None),
        Some(_) => Ok(Some(req_num(args, i)?.trunc() as i64)),
    }
}

/// 位移运算通用体。
fn bit_shift(args: &[EvaluatedArg], left: bool) -> FormulaValue {
    let a = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let shift = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ca = match bit_check(a) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let sh = shift.trunc() as i64;
    if sh.abs() > 53 {
        return err(FormulaError::Num);
    }
    // left=true：正 shift 左移；left=false：正 shift 右移。负号反向。
    let effective = if left { sh } else { -sh };
    let r: i128 = if effective >= 0 {
        (ca as i128) << effective
    } else {
        (ca as i128) >> (-effective)
    };
    if r < 0 || r > BIT_MAX as i128 {
        return err(FormulaError::Num);
    }
    FormulaValue::Number(r as f64)
}

/// IMSUM / IMPRODUCT 多参聚合。
fn im_agg(args: &[EvaluatedArg], f: impl Fn(Cx, Cx) -> (f64, f64)) -> FormulaValue {
    let mut acc: Option<Cx> = None;
    for i in 0..args.len() {
        let z = match req_cx(args, i) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        acc = Some(match acc {
            None => z,
            Some(a) => {
                let (re, im) = f(a, z);
                Cx { re, im, suf: a.suf }
            }
        });
    }
    match acc {
        Some(a) => FormulaValue::Text(fmt_cx(a.re, a.im, a.suf)),
        None => err(FormulaError::Value),
    }
}

pub(crate) fn eng_builtins() -> Vec<(&'static str, FunctionImpl)> {
    macro_rules! f {
        ($name:literal, $imp:expr) => {
            ($name, Rc::new($imp) as FunctionImpl)
        };
    }
    vec![
        // ── 进制转换 ──
        f!("BIN2DEC", |a: &[EvaluatedArg], _c: &EvalContext| {
            conv_to_dec(a, Radix::Bin)
        }),
        f!(
            "BIN2OCT",
            |a: &[EvaluatedArg], _c: &EvalContext| conv_radix(a, Radix::Bin, Radix::Oct)
        ),
        f!(
            "BIN2HEX",
            |a: &[EvaluatedArg], _c: &EvalContext| conv_radix(a, Radix::Bin, Radix::Hex)
        ),
        f!("OCT2DEC", |a: &[EvaluatedArg], _c: &EvalContext| {
            conv_to_dec(a, Radix::Oct)
        }),
        f!(
            "OCT2BIN",
            |a: &[EvaluatedArg], _c: &EvalContext| conv_radix(a, Radix::Oct, Radix::Bin)
        ),
        f!(
            "OCT2HEX",
            |a: &[EvaluatedArg], _c: &EvalContext| conv_radix(a, Radix::Oct, Radix::Hex)
        ),
        f!("HEX2DEC", |a: &[EvaluatedArg], _c: &EvalContext| {
            conv_to_dec(a, Radix::Hex)
        }),
        f!(
            "HEX2BIN",
            |a: &[EvaluatedArg], _c: &EvalContext| conv_radix(a, Radix::Hex, Radix::Bin)
        ),
        f!(
            "HEX2OCT",
            |a: &[EvaluatedArg], _c: &EvalContext| conv_radix(a, Radix::Hex, Radix::Oct)
        ),
        f!("DEC2BIN", |a: &[EvaluatedArg], _c: &EvalContext| dec_to(
            a,
            Radix::Bin
        )),
        f!("DEC2OCT", |a: &[EvaluatedArg], _c: &EvalContext| dec_to(
            a,
            Radix::Oct
        )),
        f!("DEC2HEX", |a: &[EvaluatedArg], _c: &EvalContext| dec_to(
            a,
            Radix::Hex
        )),
        // ── 位运算 ──
        f!("BITAND", |a: &[EvaluatedArg], _c: &EvalContext| bit2(
            a,
            |x, y| x & y
        )),
        f!("BITOR", |a: &[EvaluatedArg], _c: &EvalContext| bit2(
            a,
            |x, y| x | y
        )),
        f!("BITXOR", |a: &[EvaluatedArg], _c: &EvalContext| bit2(
            a,
            |x, y| x ^ y
        )),
        f!("BITLSHIFT", |a: &[EvaluatedArg], _c: &EvalContext| {
            bit_shift(a, true)
        }),
        f!("BITRSHIFT", |a: &[EvaluatedArg], _c: &EvalContext| {
            bit_shift(a, false)
        }),
        // ── DELTA / GESTEP ──
        f!("DELTA", |a: &[EvaluatedArg], _c: &EvalContext| {
            let x = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let y = match opt_num(a, 1, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            FormulaValue::Number(if x == y { 1.0 } else { 0.0 })
        }),
        f!("GESTEP", |a: &[EvaluatedArg], _c: &EvalContext| {
            let x = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let step = match opt_num(a, 1, 0.0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            FormulaValue::Number(if x >= step { 1.0 } else { 0.0 })
        }),
        // ── 误差函数 ──
        f!("ERF", |a: &[EvaluatedArg], _c: &EvalContext| {
            let lo = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            if a.len() > 1 {
                let hi = match req_num(a, 1) {
                    Ok(v) => v,
                    Err(e) => return err(e),
                };
                finite(erf(hi) - erf(lo))
            } else {
                finite(erf(lo))
            }
        }),
        f!("ERF.PRECISE", |a: &[EvaluatedArg], _c: &EvalContext| {
            match req_num(a, 0) {
                Ok(x) => finite(erf(x)),
                Err(e) => err(e),
            }
        }),
        f!(
            "ERFC",
            |a: &[EvaluatedArg], _c: &EvalContext| match req_num(a, 0) {
                Ok(x) => finite(erfc(x)),
                Err(e) => err(e),
            }
        ),
        f!("ERFC.PRECISE", |a: &[EvaluatedArg], _c: &EvalContext| {
            match req_num(a, 0) {
                Ok(x) => finite(erfc(x)),
                Err(e) => err(e),
            }
        }),
        // ── 贝塞尔 ──
        f!("BESSELJ", |a: &[EvaluatedArg], _c: &EvalContext| bessel(
            a, bessel_jn
        )),
        f!("BESSELY", |a: &[EvaluatedArg], _c: &EvalContext| bessel(
            a, bessel_yn
        )),
        f!("BESSELI", |a: &[EvaluatedArg], _c: &EvalContext| bessel(
            a, bessel_in
        )),
        f!("BESSELK", |a: &[EvaluatedArg], _c: &EvalContext| bessel(
            a, bessel_kn
        )),
        // ── CONVERT ──
        f!("CONVERT", |a: &[EvaluatedArg], _c: &EvalContext| {
            let v = match req_num(a, 0) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let from = match req_text(a, 1) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let to = match req_text(a, 2) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            convert(v, from.trim(), to.trim())
        }),
        // ── 复数 ──
        f!("COMPLEX", |a: &[EvaluatedArg], _c: &EvalContext| {
            let re = match req_num(a, 0) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let im = match req_num(a, 1) {
                Ok(x) => x,
                Err(e) => return err(e),
            };
            let suf = if a.len() > 2 {
                match req_text(a, 2) {
                    Ok(s) => s,
                    Err(e) => return err(e),
                }
            } else {
                "i".to_string()
            };
            if suf != "i" && suf != "j" {
                return err(FormulaError::Value);
            }
            FormulaValue::Text(fmt_cx(re, im, suf.chars().next().unwrap()))
        }),
        f!(
            "IMREAL",
            |a: &[EvaluatedArg], _c: &EvalContext| match req_cx(a, 0) {
                Ok(z) => FormulaValue::Number(z.re),
                Err(e) => err(e),
            }
        ),
        f!(
            "IMAGINARY",
            |a: &[EvaluatedArg], _c: &EvalContext| match req_cx(a, 0) {
                Ok(z) => FormulaValue::Number(z.im),
                Err(e) => err(e),
            }
        ),
        f!(
            "IMABS",
            |a: &[EvaluatedArg], _c: &EvalContext| match req_cx(a, 0) {
                Ok(z) => finite(z.re.hypot(z.im)),
                Err(e) => err(e),
            }
        ),
        f!(
            "IMARGUMENT",
            |a: &[EvaluatedArg], _c: &EvalContext| match req_cx(a, 0) {
                Ok(z) => {
                    if z.re == 0.0 && z.im == 0.0 {
                        err(FormulaError::Div0)
                    } else {
                        FormulaValue::Number(z.im.atan2(z.re))
                    }
                }
                Err(e) => err(e),
            }
        ),
        f!("IMCONJUGATE", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| Ok((z.re, -z.im))
        )),
        f!("IMSUM", |a: &[EvaluatedArg], _c: &EvalContext| im_agg(
            a,
            |x, y| (x.re + y.re, x.im + y.im)
        )),
        f!("IMSUB", |a: &[EvaluatedArg], _c: &EvalContext| {
            let x = match req_cx(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let y = match req_cx(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            FormulaValue::Text(fmt_cx(x.re - y.re, x.im - y.im, x.suf))
        }),
        f!("IMPRODUCT", |a: &[EvaluatedArg], _c: &EvalContext| {
            im_agg(a, |x, y| {
                (x.re * y.re - x.im * y.im, x.re * y.im + x.im * y.re)
            })
        }),
        f!("IMDIV", |a: &[EvaluatedArg], _c: &EvalContext| {
            let x = match req_cx(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let y = match req_cx(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let d = y.re * y.re + y.im * y.im;
            if d == 0.0 {
                return err(FormulaError::Num);
            }
            FormulaValue::Text(fmt_cx(
                (x.re * y.re + x.im * y.im) / d,
                (x.im * y.re - x.re * y.im) / d,
                x.suf,
            ))
        }),
        f!("IMEXP", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| {
                let e = z.re.exp();
                Ok((e * z.im.cos(), e * z.im.sin()))
            }
        )),
        f!("IMLN", |a: &[EvaluatedArg], _c: &EvalContext| cx1(a, |z| {
            let m = z.re.hypot(z.im);
            if m == 0.0 {
                Err(FormulaError::Num)
            } else {
                Ok((m.ln(), z.im.atan2(z.re)))
            }
        })),
        f!("IMLOG10", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| {
                let m = z.re.hypot(z.im);
                if m == 0.0 {
                    Err(FormulaError::Num)
                } else {
                    Ok((m.log10(), z.im.atan2(z.re) / std::f64::consts::LN_10))
                }
            }
        )),
        f!("IMLOG2", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| {
                let m = z.re.hypot(z.im);
                if m == 0.0 {
                    Err(FormulaError::Num)
                } else {
                    Ok((m.log2(), z.im.atan2(z.re) / std::f64::consts::LN_2))
                }
            }
        )),
        f!("IMSQRT", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| {
                let m = z.re.hypot(z.im);
                let arg = z.im.atan2(z.re) / 2.0;
                let r = m.sqrt();
                Ok((r * arg.cos(), r * arg.sin()))
            }
        )),
        f!("IMPOWER", |a: &[EvaluatedArg], _c: &EvalContext| {
            let z = match req_cx(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let p = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let m = z.re.hypot(z.im);
            let arg = z.im.atan2(z.re);
            let rm = m.powf(p);
            let ra = arg * p;
            FormulaValue::Text(fmt_cx(rm * ra.cos(), rm * ra.sin(), z.suf))
        }),
        f!("IMSIN", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| Ok((z.re.sin() * z.im.cosh(), z.re.cos() * z.im.sinh()))
        )),
        f!("IMCOS", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| Ok((z.re.cos() * z.im.cosh(), -z.re.sin() * z.im.sinh()))
        )),
        f!("IMTAN", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a, cx_tan
        )),
        f!("IMSINH", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| Ok((z.re.sinh() * z.im.cos(), z.re.cosh() * z.im.sin()))
        )),
        f!("IMCOSH", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| Ok((z.re.cosh() * z.im.cos(), z.re.sinh() * z.im.sin()))
        )),
        f!("IMSEC", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| cx_recip(z.re.cos() * z.im.cosh(), -z.re.sin() * z.im.sinh())
        )),
        f!("IMCSC", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| cx_recip(z.re.sin() * z.im.cosh(), z.re.cos() * z.im.sinh())
        )),
        f!("IMCOT", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| {
                let (re, im) = cx_tan(z)?;
                cx_recip(re, im)
            }
        )),
        f!("IMSECH", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| cx_recip(z.re.cosh() * z.im.cos(), z.re.sinh() * z.im.sin())
        )),
        f!("IMCSCH", |a: &[EvaluatedArg], _c: &EvalContext| cx1(
            a,
            |z| cx_recip(z.re.sinh() * z.im.cos(), z.re.cosh() * z.im.sin())
        )),
        // ── 日期扩容 ──
        f!("DAYS", |a: &[EvaluatedArg], _c: &EvalContext| {
            let end = match req_num(a, 0) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            let start = match req_num(a, 1) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            FormulaValue::Number(end.trunc() - start.trunc())
        }),
        f!("DAYS360", |a: &[EvaluatedArg], _c: &EvalContext| days360(a)),
        f!(
            "YEARFRAC",
            |a: &[EvaluatedArg], _c: &EvalContext| year_frac(a)
        ),
        f!("WEEKNUM", |a: &[EvaluatedArg], _c: &EvalContext| week_num(
            a
        )),
        f!("ISOWEEKNUM", |a: &[EvaluatedArg], _c: &EvalContext| {
            iso_week_num(a)
        }),
        f!("TIMEVALUE", |a: &[EvaluatedArg], _c: &EvalContext| {
            time_value(a)
        }),
        f!("WORKDAY", |a: &[EvaluatedArg], _c: &EvalContext| {
            let mut weekend = std::collections::HashSet::new();
            weekend.insert(0u32);
            weekend.insert(6u32);
            workday_core(a.first(), a.get(1), &weekend, a.get(2))
        }),
        f!("WORKDAY.INTL", |a: &[EvaluatedArg], _c: &EvalContext| {
            let weekend = if a.len() > 2 {
                match weekend_set(&scalar_arg(a.get(2))) {
                    Ok(w) => w,
                    Err(e) => return err(e),
                }
            } else {
                default_weekend()
            };
            workday_core(a.first(), a.get(1), &weekend, a.get(3))
        }),
        f!(
            "NETWORKDAYS.INTL",
            |a: &[EvaluatedArg], _c: &EvalContext| { network_days_intl(a) }
        ),
    ]
}

/// 位运算双参通用体。
fn bit2(args: &[EvaluatedArg], f: impl Fn(i64, i64) -> i64) -> FormulaValue {
    let a = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let b = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let ca = match bit_check(a) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let cb = match bit_check(b) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let r = f(ca, cb);
    if !(0..=BIT_MAX).contains(&r) {
        return err(FormulaError::Num);
    }
    FormulaValue::Number(r as f64)
}

/// 贝塞尔双参通用体（x, n）。
fn bessel(
    args: &[EvaluatedArg],
    f: impl Fn(f64, f64) -> Result<f64, FormulaError>,
) -> FormulaValue {
    let x = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let n = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    match f(n, x) {
        Ok(r) => finite(r),
        Err(e) => err(e),
    }
}

// ── 日期扩容助手（对拍 TS functions.ts 的 days360/yearFrac/weekNum/…）──

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days360(args: &[EvaluatedArg]) -> FormulaValue {
    let s1 = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let s2 = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let euro = match args.get(2) {
        None => false,
        Some(_) => match to_boolean(&scalar_arg(args.get(2))) {
            Ok(v) => v,
            Err(e) => return err(e),
        },
    };
    let a = serial_to_parts(s1.trunc());
    let b = serial_to_parts(s2.trunc());
    let mut d1 = a.day as i64;
    let mut d2 = b.day as i64;
    if euro {
        if d1 == 31 {
            d1 = 30;
        }
        if d2 == 31 {
            d2 = 30;
        }
    } else {
        if d1 == 31 {
            d1 = 30;
        }
        if d2 == 31 && d1 == 30 {
            d2 = 30;
        }
    }
    FormulaValue::Number(
        ((b.year - a.year) * 360 + (b.month as i64 - a.month as i64) * 30 + (d2 - d1)) as f64,
    )
}

fn year_frac(args: &[EvaluatedArg]) -> FormulaValue {
    let mut s1 = match req_num(args, 0) {
        Ok(v) => v.trunc(),
        Err(e) => return err(e),
    };
    let mut s2 = match req_num(args, 1) {
        Ok(v) => v.trunc(),
        Err(e) => return err(e),
    };
    let basis = match args.get(2) {
        None => 0.0,
        Some(_) => match req_num(args, 2) {
            Ok(v) => v,
            Err(e) => return err(e),
        },
    };
    if s1 == s2 {
        return FormulaValue::Number(0.0);
    }
    if s1 > s2 {
        std::mem::swap(&mut s1, &mut s2);
    }
    let b = basis.trunc() as i64;
    let a = serial_to_parts(s1);
    let c = serial_to_parts(s2);
    match b {
        0 => {
            let mut d1 = a.day as i64;
            let mut d2 = c.day as i64;
            if d1 == 31 {
                d1 = 30;
            }
            if d2 == 31 && d1 == 30 {
                d2 = 30;
            }
            let days = (c.year - a.year) * 360 + (c.month as i64 - a.month as i64) * 30 + (d2 - d1);
            FormulaValue::Number(days as f64 / 360.0)
        }
        1 => {
            let yrs = (c.year - a.year + 1) as f64;
            let mut days_in_years = 0.0;
            for y in a.year..=c.year {
                days_in_years += if is_leap(y) { 366.0 } else { 365.0 };
            }
            let avg = days_in_years / yrs;
            FormulaValue::Number((s2 - s1) / avg)
        }
        2 => FormulaValue::Number((s2 - s1) / 360.0),
        3 => FormulaValue::Number((s2 - s1) / 365.0),
        4 => {
            let mut d1 = a.day as i64;
            let mut d2 = c.day as i64;
            if d1 == 31 {
                d1 = 30;
            }
            if d2 == 31 {
                d2 = 30;
            }
            let days = (c.year - a.year) * 360 + (c.month as i64 - a.month as i64) * 30 + (d2 - d1);
            FormulaValue::Number(days as f64 / 360.0)
        }
        _ => err(FormulaError::Num),
    }
}

fn week_num(args: &[EvaluatedArg]) -> FormulaValue {
    let s = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let t = match args.get(1) {
        None => 1,
        Some(_) => match req_num(args, 1) {
            Ok(v) => v.trunc() as i64,
            Err(e) => return err(e),
        },
    };
    if t == 21 {
        return iso_week_num(args);
    }
    // 周首日对应的 dow（0=Sun..6=Sat）
    let start_dow = match t {
        1 | 17 => 0,
        2 | 11 => 1,
        12 => 2,
        13 => 3,
        14 => 4,
        15 => 5,
        16 => 6,
        _ => return err(FormulaError::Num),
    };
    let p = serial_to_parts(s.trunc());
    let jan1 = date_to_serial(p.year, 1, 1);
    let jan1_dow = serial_to_parts(jan1).weekday as i64;
    let offset = (jan1_dow - start_dow + 7) % 7;
    let day_of_year = s.trunc() - jan1;
    FormulaValue::Number((((day_of_year as i64 + offset) / 7) + 1) as f64)
}

fn iso_week_num(args: &[EvaluatedArg]) -> FormulaValue {
    let s = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let serial = s.trunc();
    // 0=Mon..6=Sun
    let dow = (serial_to_parts(serial).weekday as i64 + 6) % 7;
    let thursday = serial - dow as f64 + 3.0;
    let p = serial_to_parts(thursday);
    let jan1 = date_to_serial(p.year, 1, 1);
    FormulaValue::Number(((((thursday - jan1) as i64) / 7) + 1) as f64)
}

fn time_value(args: &[EvaluatedArg]) -> FormulaValue {
    let t = match req_text(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    // 手写解析 "h:mm[:ss] [AM/PM]"（无 regex 依赖，对齐 date_serial 手写风格）
    let up = t.trim().to_ascii_uppercase();
    let (body, ap) = if let Some(rest) = up.strip_suffix("PM") {
        (rest.trim().to_string(), "PM")
    } else if let Some(rest) = up.strip_suffix("AM") {
        (rest.trim().to_string(), "AM")
    } else {
        (up.clone(), "")
    };
    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return err(FormulaError::Value);
    }
    let parse = |x: &str| x.trim().parse::<i64>().ok();
    let (h, mi, se) = match (
        parse(parts[0]),
        parse(parts[1]),
        if parts.len() == 3 {
            parse(parts[2])
        } else {
            Some(0)
        },
    ) {
        (Some(h), Some(mi), Some(se)) => (h, mi, se),
        _ => return err(FormulaError::Value),
    };
    let mut h = h;
    if ap == "PM" && h < 12 {
        h += 12;
    }
    if ap == "AM" && h == 12 {
        h = 0;
    }
    if h > 24 || mi > 59 || se > 59 || h < 0 || mi < 0 || se < 0 {
        return err(FormulaError::Value);
    }
    FormulaValue::Number((h * 3600 + mi * 60 + se) as f64 / 86400.0)
}

fn default_weekend() -> std::collections::HashSet<u32> {
    let mut s = std::collections::HashSet::new();
    s.insert(0);
    s.insert(6);
    s
}

/// weekend 掩码：数字码或 7 位字符串 → 周末 dow 集合（0=Sun..6=Sat）。
fn weekend_set(weekend: &FormulaValue) -> Result<std::collections::HashSet<u32>, FormulaError> {
    if let FormulaValue::Text(w) = weekend {
        if w.len() == 7 && w.chars().all(|c| c == '0' || c == '1') {
            let mut set = std::collections::HashSet::new();
            // 位序 周一..周日 → dow: Mon=1..Sat=6,Sun=0
            let dow_by_pos = [1u32, 2, 3, 4, 5, 6, 0];
            for (i, ch) in w.chars().enumerate() {
                if ch == '1' {
                    set.insert(dow_by_pos[i]);
                }
            }
            return Ok(set);
        }
    }
    let n = to_number(weekend)?.trunc() as i64;
    let mut set = std::collections::HashSet::new();
    match n {
        1 => {
            set.insert(6);
            set.insert(0);
        }
        2 => {
            set.insert(0);
            set.insert(1);
        }
        3 => {
            set.insert(1);
            set.insert(2);
        }
        4 => {
            set.insert(2);
            set.insert(3);
        }
        5 => {
            set.insert(3);
            set.insert(4);
        }
        6 => {
            set.insert(4);
            set.insert(5);
        }
        7 => {
            set.insert(5);
            set.insert(6);
        }
        11 => {
            set.insert(0);
        }
        12 => {
            set.insert(1);
        }
        13 => {
            set.insert(2);
        }
        14 => {
            set.insert(3);
        }
        15 => {
            set.insert(4);
        }
        16 => {
            set.insert(5);
        }
        17 => {
            set.insert(6);
        }
        _ => return Err(FormulaError::Num),
    }
    Ok(set)
}

fn collect_holidays(arg: Option<&EvaluatedArg>) -> std::collections::HashSet<i64> {
    let mut h = std::collections::HashSet::new();
    if let Some(a) = arg {
        for v in flatten_arg(a) {
            if let Ok(n) = to_number(&v) {
                h.insert(n.trunc() as i64);
            }
        }
    }
    h
}

fn workday_core(
    start_arg: Option<&EvaluatedArg>,
    days_arg: Option<&EvaluatedArg>,
    weekend: &std::collections::HashSet<u32>,
    hol_arg: Option<&EvaluatedArg>,
) -> FormulaValue {
    let s = match to_number(&scalar_arg(start_arg)) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let days = match to_number(&scalar_arg(days_arg)) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if weekend.len() >= 7 {
        return err(FormulaError::Num);
    }
    let hol = collect_holidays(hol_arg);
    let mut d = s.trunc() as i64;
    let mut remaining = days.trunc() as i64;
    let step: i64 = if remaining >= 0 { 1 } else { -1 };
    remaining = remaining.abs();
    while remaining > 0 {
        d += step;
        let dow = serial_to_parts(d as f64).weekday;
        if weekend.contains(&dow) || hol.contains(&d) {
            continue;
        }
        remaining -= 1;
    }
    FormulaValue::Number(d as f64)
}

fn network_days_intl(args: &[EvaluatedArg]) -> FormulaValue {
    let s1 = match req_num(args, 0) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let s2 = match req_num(args, 1) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let weekend = if args.len() > 2 {
        match weekend_set(&scalar_arg(args.get(2))) {
            Ok(w) => w,
            Err(e) => return err(e),
        }
    } else {
        default_weekend()
    };
    let hol = collect_holidays(args.get(3));
    let start = s1.trunc().min(s2.trunc()) as i64;
    let end = s1.trunc().max(s2.trunc()) as i64;
    let mut count = 0i64;
    for d in start..=end {
        let dow = serial_to_parts(d as f64).weekday;
        if weekend.contains(&dow) || hol.contains(&d) {
            continue;
        }
        count += 1;
    }
    FormulaValue::Number(if s1.trunc() <= s2.trunc() {
        count as f64
    } else {
        -count as f64
    })
}

#[cfg(test)]
mod tests {
    use crate::evaluator::{CellAccessor, EvalContext, Evaluator};
    use crate::functions::BuiltinRegistry;
    use crate::parse::parse_formula;
    use crate::value::{FormulaError, FormulaValue};
    use std::collections::HashMap;

    struct Empty;
    impl CellAccessor for Empty {
        fn get_cell_value(&self, _r: &str) -> FormulaValue {
            FormulaValue::Blank
        }
        fn get_range_values(&self, _s: &str, _e: &str) -> Vec<Vec<FormulaValue>> {
            vec![vec![FormulaValue::Blank]]
        }
        fn resolve_name(&self, _n: &str) -> Option<FormulaValue> {
            None
        }
        fn resolve_name_ref(&self, _n: &str) -> Option<String> {
            None
        }
    }

    fn ev(src: &str) -> FormulaValue {
        let acc = Empty;
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
    fn n(src: &str) -> f64 {
        match ev(src) {
            FormulaValue::Number(x) => x,
            other => panic!("expected number from {src}, got {other:?}"),
        }
    }
    fn t(src: &str) -> String {
        match ev(src) {
            FormulaValue::Text(s) => s,
            other => panic!("expected text from {src}, got {other:?}"),
        }
    }
    fn near(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "{a} vs {b}");
    }

    #[test]
    fn base_conversion() {
        assert_eq!(n("=BIN2DEC(\"1100100\")"), 100.0);
        assert_eq!(n("=BIN2DEC(\"1111111111\")"), -1.0);
        assert_eq!(t("=DEC2BIN(100)"), "1100100");
        assert_eq!(t("=DEC2BIN(-1)"), "1111111111");
        assert_eq!(t("=DEC2BIN(9,4)"), "1001");
        assert_eq!(t("=DEC2HEX(255)"), "FF");
        assert_eq!(n("=HEX2DEC(\"FF\")"), 255.0);
        assert_eq!(n("=HEX2DEC(\"FFFFFFFFFF\")"), -1.0);
        assert_eq!(t("=DEC2OCT(8)"), "10");
        assert_eq!(n("=OCT2DEC(\"10\")"), 8.0);
        assert_eq!(t("=BIN2HEX(\"11111011\")"), "FB");
        assert_eq!(t("=HEX2BIN(\"F\",8)"), "00001111");
        assert_eq!(t("=OCT2HEX(\"100\")"), "40");
        assert_eq!(ev("=DEC2BIN(600)"), FormulaValue::Error(FormulaError::Num));
    }

    #[test]
    fn bitwise() {
        assert_eq!(n("=BITAND(13,25)"), 9.0);
        assert_eq!(n("=BITOR(23,10)"), 31.0);
        assert_eq!(n("=BITXOR(5,3)"), 6.0);
        assert_eq!(n("=BITLSHIFT(4,2)"), 16.0);
        assert_eq!(n("=BITRSHIFT(13,2)"), 3.0);
        assert_eq!(n("=BITLSHIFT(4,-2)"), 1.0);
        assert_eq!(ev("=BITAND(-1,1)"), FormulaValue::Error(FormulaError::Num));
    }

    #[test]
    fn delta_gestep_erf() {
        assert_eq!(n("=DELTA(5,4)"), 0.0);
        assert_eq!(n("=DELTA(5,5)"), 1.0);
        assert_eq!(n("=DELTA(0)"), 1.0);
        assert_eq!(n("=GESTEP(5,4)"), 1.0);
        assert_eq!(n("=GESTEP(3,4)"), 0.0);
        near(n("=ERF(1)"), 0.842700793, 1e-6);
        near(n("=ERFC(1)"), 0.157299207, 1e-6);
        near(n("=ERF(0,1)"), 0.842700793, 1e-6);
        near(n("=ERF.PRECISE(1)"), 0.842700793, 1e-6);
    }

    #[test]
    fn bessel() {
        near(n("=BESSELJ(1.9,2)"), 0.329925829, 1e-5);
        near(n("=BESSELI(1.5,1)"), 0.981666428, 1e-5);
        near(n("=BESSELK(1.5,1)"), 0.277387804, 1e-4);
        near(n("=BESSELY(2.5,1)"), 0.145918138, 1e-4);
    }

    #[test]
    fn convert_units() {
        near(n("=CONVERT(1,\"lbm\",\"kg\")"), 0.45359237, 1e-8);
        near(n("=CONVERT(1,\"mi\",\"km\")"), 1.609344, 1e-6);
        near(n("=CONVERT(100,\"C\",\"F\")"), 212.0, 1e-9);
        near(n("=CONVERT(32,\"F\",\"C\")"), 0.0, 1e-9);
        assert_eq!(n("=CONVERT(1,\"hr\",\"sec\")"), 3600.0);
        assert_eq!(
            ev("=CONVERT(1,\"kg\",\"m\")"),
            FormulaValue::Error(FormulaError::Na)
        );
    }

    #[test]
    fn complex_numbers() {
        assert_eq!(t("=COMPLEX(3,4)"), "3+4i");
        assert_eq!(t("=COMPLEX(0,1)"), "i");
        assert_eq!(t("=COMPLEX(3,-4,\"j\")"), "3-4j");
        assert_eq!(n("=IMREAL(\"3+4i\")"), 3.0);
        assert_eq!(n("=IMAGINARY(\"3+4i\")"), 4.0);
        assert_eq!(n("=IMAGINARY(\"i\")"), 1.0);
        assert_eq!(n("=IMABS(\"3+4i\")"), 5.0);
        near(
            n("=IMARGUMENT(\"1+1i\")"),
            std::f64::consts::FRAC_PI_4,
            1e-9,
        );
        assert_eq!(t("=IMSUM(\"1+2i\",\"3+4i\")"), "4+6i");
        assert_eq!(t("=IMSUB(\"5+6i\",\"1+2i\")"), "4+4i");
        assert_eq!(t("=IMPRODUCT(\"1+2i\",\"3+4i\")"), "-5+10i");
        assert_eq!(t("=IMDIV(\"-5+10i\",\"3+4i\")"), "1+2i");
        assert_eq!(t("=IMCONJUGATE(\"3+4i\")"), "3-4i");
        assert_eq!(t("=IMEXP(\"0+0i\")"), "1");
    }

    #[test]
    fn dates() {
        // 2024-01-15 = 45306, 2024-03-15 = 45366
        assert_eq!(n("=DAYS(45366,45306)"), 60.0);
        assert_eq!(n("=DAYS360(45306,45366)"), 60.0);
        near(n("=YEARFRAC(45306,45366,0)"), 60.0 / 360.0, 1e-6);
        near(n("=YEARFRAC(45306,45366,3)"), 60.0 / 365.0, 1e-6);
        assert_eq!(n("=ISOWEEKNUM(45306)"), 3.0);
        near(n("=TIMEVALUE(\"12:00:00\")"), 0.5, 1e-9);
        near(n("=TIMEVALUE(\"6:00 PM\")"), 0.75, 1e-9);
        assert_eq!(n("=WORKDAY(45306,5)"), 45313.0);
        // 类型断言存在即可
        let _ = n("=WEEKNUM(45306)");
        let _ = n("=WORKDAY.INTL(45306,5,1)");
        let _ = n("=NETWORKDAYS.INTL(45306,45366)");
    }

    #[test]
    fn count_is_335() {
        let mut names: Vec<String> = BuiltinRegistry::new().names();
        names.sort();
        assert_eq!(
            names.len(),
            335,
            "expected 335 functions after eng additions"
        );
        // 确认无 HashMap 静默覆盖：唯一键数 = 总数
        let uniq: HashMap<_, _> = names.iter().map(|s| (s.clone(), ())).collect();
        assert_eq!(uniq.len(), 335);
    }
}
