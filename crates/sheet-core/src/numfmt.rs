//! Excel 数字/日期格式串引擎（编译 + 应用）。对标 cmx-megasheet 的 render/numFmt/{compile,apply}.ts。
//!
//! 「无渲染」重解读：这是**计算件**（算显示文本，非画像素），故留在 sheet-core。把格式串
//! （如 `#,##0.00;[Red](#,##0.00)`）编译成 token 流并按 `;` 分最多 4 区段（正/负/零/文本），
//! 每段抽颜色段 `[Red]`、条件段 `[>=100]`、类别（number/date/text/general）与数字子计划
//! （小数位/千分位/缩放/百分/科学/分数）。`format_with(value, fmt)` → {text, color?}。
//! 是 TEXT() 与单元格显示的**单一事实源**（对齐父项目 formatValue 单点）。

use std::sync::OnceLock;

use regex::Regex;

use crate::cell::CellValue;
use crate::date_serial::{serial_to_parts, DateParts};

/// 格式化结果：显示文本 + 可选颜色（[Red] 等负数标红）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormatResult {
    pub text: String,
    pub color: Option<String>,
}

// ── token 模型 ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum FmtToken {
    /// 数字占位 0/#/?。
    Dig(char),
    Dot,
    Comma,
    Pct,
    /// 科学计数指数符号 +/-。
    Sci(char),
    /// 归一后的日期码。
    Date(String),
    /// 字面量。
    Lit(String),
    At,
    /// 填充 *x（目标字符）。
    Fill(char),
    /// 跳宽 _x（渲染为空格）。
    Skip,
    Slash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FmtKind {
    Number,
    Date,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Condition {
    op: CondOp,
    value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CondOp {
    Ge,
    Le,
    Gt,
    Lt,
    Eq,
    Ne,
}

#[derive(Debug, Clone)]
struct FmtSection {
    tokens: Vec<FmtToken>,
    kind: FmtKind,
    color: Option<String>,
    condition: Option<Condition>,
    decimals: usize,
    int_min: usize,
    has_thousands: bool,
    /// 值除以 1000^scale（尾逗号缩放）。
    scale: u32,
    has_percent: bool,
    has_sci: bool,
    sci_exp_digits: usize,
    has_fraction: bool,
    frac_denom_digits: u32,
}

/// 编译结果：多区段 + 是否 General。
#[derive(Debug, Clone)]
pub struct CompiledFormat {
    sections: Vec<FmtSection>,
    is_general: bool,
}

impl CompiledFormat {
    /// 是否 General（空串或 "General"）。
    pub fn is_general(&self) -> bool {
        self.is_general
    }

    /// 区段数（测试/调试用）。
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

fn named_color(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "black" => Some("#000000"),
        "blue" => Some("#0000ff"),
        "cyan" => Some("#00ffff"),
        "green" => Some("#008000"),
        "magenta" => Some("#ff00ff"),
        "red" => Some("#ff0000"),
        "white" => Some("#ffffff"),
        "yellow" => Some("#ffff00"),
        _ => None,
    }
}

fn indexed_color(idx: u32) -> Option<&'static str> {
    match idx {
        1 => Some("#000000"),
        2 => Some("#ffffff"),
        3 => Some("#ff0000"),
        4 => Some("#00ff00"),
        5 => Some("#0000ff"),
        6 => Some("#ffff00"),
        7 => Some("#ff00ff"),
        8 => Some("#00ffff"),
        _ => None,
    }
}

/// 编译格式串 → CompiledFormat。空串/"General" → is_general。
pub fn compile_format(fmt: &str) -> CompiledFormat {
    let trimmed = fmt.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("general") {
        return CompiledFormat {
            sections: Vec::new(),
            is_general: true,
        };
    }
    let sections = split_sections(fmt)
        .iter()
        .map(|s| compile_section(s))
        .collect();
    CompiledFormat {
        sections,
        is_general: false,
    }
}

/// 按顶层 `;` 切段（引号 "..." 与方括号 [...] 内的 `;` 不算）。
fn split_sections(fmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut in_bracket = false;
    for ch in fmt.chars() {
        if in_str {
            cur.push(ch);
            if ch == '"' {
                in_str = false;
            }
            continue;
        }
        if in_bracket {
            cur.push(ch);
            if ch == ']' {
                in_bracket = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_str = true;
                cur.push(ch);
            }
            '[' => {
                in_bracket = true;
                cur.push(ch);
            }
            ';' => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

fn is_date_letter(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'y' | 'm' | 'd' | 'h' | 's')
}

/// 编译单区段：抽颜色/条件 → tokenize → 归一日期码 → 算 number 子计划。
fn compile_section(raw: &str) -> FmtSection {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut tokens: Vec<FmtToken> = Vec::new();
    let mut color: Option<String> = None;
    let mut condition: Option<Condition> = None;
    let mut i = 0;

    while i < n {
        let ch = chars[i];
        // 方括号：颜色 / 条件 / 经过时间
        if ch == '[' {
            if let Some(end_off) = chars[i..].iter().position(|&c| c == ']') {
                let end = i + end_off;
                let inner: String = chars[i + 1..end].iter().collect();
                match parse_bracket(&inner) {
                    Bracket::Color(c) => color = c,
                    Bracket::Cond(c) => condition = Some(c),
                    Bracket::Elapsed(code) => tokens.push(FmtToken::Date(code)),
                    Bracket::Unknown => {}
                }
                i = end + 1;
                continue;
            }
            tokens.push(FmtToken::Lit(ch.to_string()));
            i += 1;
            continue;
        }
        // 字面量 "..."
        if ch == '"' {
            let rest = &chars[i + 1..];
            let end_off = rest.iter().position(|&c| c == '"');
            let lit: String = match end_off {
                Some(e) => chars[i + 1..i + 1 + e].iter().collect(),
                None => chars[i + 1..].iter().collect(),
            };
            tokens.push(FmtToken::Lit(lit));
            i = match end_off {
                Some(e) => i + 1 + e + 1,
                None => n,
            };
            continue;
        }
        // 转义 \x
        if ch == '\\' {
            if i + 1 < n {
                tokens.push(FmtToken::Lit(chars[i + 1].to_string()));
            }
            i += 2;
            continue;
        }
        // 跳宽 _x / 填充 *x
        if ch == '_' {
            tokens.push(FmtToken::Skip);
            i += 2;
            continue;
        }
        if ch == '*' {
            tokens.push(FmtToken::Fill(chars.get(i + 1).copied().unwrap_or(' ')));
            i += 2;
            continue;
        }
        // 数字占位
        if ch == '0' || ch == '#' || ch == '?' {
            tokens.push(FmtToken::Dig(ch));
            i += 1;
            continue;
        }
        match ch {
            '.' => {
                tokens.push(FmtToken::Dot);
                i += 1;
                continue;
            }
            ',' => {
                tokens.push(FmtToken::Comma);
                i += 1;
                continue;
            }
            '%' => {
                tokens.push(FmtToken::Pct);
                i += 1;
                continue;
            }
            '/' => {
                tokens.push(FmtToken::Slash);
                i += 1;
                continue;
            }
            '@' => {
                tokens.push(FmtToken::At);
                i += 1;
                continue;
            }
            _ => {}
        }
        // 科学计数 E+/E-/e+/e-
        if (ch == 'E' || ch == 'e') && i + 1 < n && (chars[i + 1] == '+' || chars[i + 1] == '-') {
            tokens.push(FmtToken::Sci(chars[i + 1]));
            i += 2;
            continue;
        }
        // 日期字母 run
        let low = ch.to_ascii_lowercase();
        if is_date_letter(ch) {
            let mut j = i;
            while j < n && chars[j].to_ascii_lowercase() == low {
                j += 1;
            }
            tokens.push(FmtToken::Date(low.to_string().repeat(j - i)));
            i = j;
            continue;
        }
        // AM/PM 与 A/P
        let ampm = match_am_pm(&chars[i..]);
        if ampm > 0 {
            tokens.push(FmtToken::Date("am/pm".to_string()));
            i += ampm;
            continue;
        }
        // 其余字面量（¥ $ € - ( ) : 空格 中文…）
        tokens.push(FmtToken::Lit(ch.to_string()));
        i += 1;
    }

    disambiguate_minutes(&mut tokens);
    let kind = classify(&tokens);
    let plan = number_plan(&tokens);
    FmtSection {
        tokens,
        kind,
        color,
        condition,
        decimals: plan.0,
        int_min: plan.1,
        has_thousands: plan.2,
        scale: plan.3,
        has_percent: plan.4,
        has_sci: plan.5,
        sci_exp_digits: plan.6,
        has_fraction: plan.7,
        frac_denom_digits: plan.8,
    }
}

enum Bracket {
    Color(Option<String>),
    Cond(Condition),
    Elapsed(String),
    Unknown,
}

fn parse_bracket(inner: &str) -> Bracket {
    let t = inner.trim();
    if let Some(c) = named_color(t) {
        return Bracket::Color(Some(c.to_string()));
    }
    static COLOR_RE: OnceLock<Regex> = OnceLock::new();
    static COND_RE: OnceLock<Regex> = OnceLock::new();
    let color_re = COLOR_RE.get_or_init(|| Regex::new(r"(?i)^color\s*(\d+)$").unwrap());
    if let Some(caps) = color_re.captures(t) {
        let idx: u32 = caps[1].parse().unwrap_or(0);
        return Bracket::Color(indexed_color(idx).map(|s| s.to_string()));
    }
    let cond_re =
        COND_RE.get_or_init(|| Regex::new(r"^(>=|<=|<>|>|<|=)\s*(-?\d+(?:\.\d+)?)$").unwrap());
    if let Some(caps) = cond_re.captures(t) {
        let op = match &caps[1] {
            ">=" => CondOp::Ge,
            "<=" => CondOp::Le,
            "<>" => CondOp::Ne,
            ">" => CondOp::Gt,
            "<" => CondOp::Lt,
            _ => CondOp::Eq,
        };
        let value: f64 = caps[2].parse().unwrap_or(0.0);
        return Bracket::Cond(Condition { op, value });
    }
    // 经过时间 [h]/[hh]/[m]/[mm]/[s]/[ss]
    let lower = t.to_lowercase();
    if !lower.is_empty() && lower.chars().all(|c| c == 'h') {
        return Bracket::Elapsed("[h]".to_string());
    }
    if !lower.is_empty() && lower.chars().all(|c| c == 'm') {
        return Bracket::Elapsed("[m]".to_string());
    }
    if !lower.is_empty() && lower.chars().all(|c| c == 's') {
        return Bracket::Elapsed("[s]".to_string());
    }
    Bracket::Unknown
}

/// AM/PM 或 A/P（返回消费字符数，0=不匹配）。
fn match_am_pm(rest: &[char]) -> usize {
    let s: String = rest.iter().take(5).collect::<String>().to_lowercase();
    if s.starts_with("am/pm") {
        return 5;
    }
    let s3: String = rest.iter().take(3).collect::<String>().to_lowercase();
    if s3.starts_with("a/p") {
        return 3;
    }
    0
}

/// 月/分歧义：m/mm 紧跟 h/hh（前）或紧邻 s/ss（后）→ 分钟（code 改 n/nn）。
fn disambiguate_minutes(tokens: &mut [FmtToken]) {
    let date_idx: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t, FmtToken::Date(_)))
        .map(|(i, _)| i)
        .collect();
    for k in 0..date_idx.len() {
        let code = match &tokens[date_idx[k]] {
            FmtToken::Date(c) => c.clone(),
            _ => continue,
        };
        if code != "m" && code != "mm" {
            continue;
        }
        let prev_is_hour = k > 0
            && matches!(&tokens[date_idx[k - 1]], FmtToken::Date(c) if c.trim_start_matches('[').starts_with('h'));
        let next_is_sec = k + 1 < date_idx.len()
            && matches!(&tokens[date_idx[k + 1]], FmtToken::Date(c) if c.trim_start_matches('[').starts_with('s'));
        if prev_is_hour || next_is_sec {
            let new = if code == "m" { "n" } else { "nn" };
            tokens[date_idx[k]] = FmtToken::Date(new.to_string());
        }
    }
}

fn classify(tokens: &[FmtToken]) -> FmtKind {
    let mut has_at = false;
    let mut has_date = false;
    let mut has_num = false;
    for t in tokens {
        match t {
            FmtToken::At => has_at = true,
            FmtToken::Date(_) => has_date = true,
            FmtToken::Dig(_) | FmtToken::Sci(_) | FmtToken::Slash => has_num = true,
            _ => {}
        }
    }
    if has_date {
        FmtKind::Date
    } else if has_at && !has_num {
        FmtKind::Text
    } else {
        FmtKind::Number
    }
}

#[allow(clippy::type_complexity)]
fn number_plan(tokens: &[FmtToken]) -> (usize, usize, bool, u32, bool, bool, usize, bool, u32) {
    let dot_idx = tokens.iter().position(|t| matches!(t, FmtToken::Dot));
    let sci_idx = tokens.iter().position(|t| matches!(t, FmtToken::Sci(_)));
    let slash_idx = tokens.iter().position(|t| matches!(t, FmtToken::Slash));

    let mut int_min = 0usize;
    let mut decimals = 0usize;
    let mut has_thousands = false;
    let mut has_percent = false;
    let mut last_int_dig_idx: i64 = -1;

    for (i, t) in tokens.iter().enumerate() {
        if matches!(t, FmtToken::Pct) {
            has_percent = true;
        }
        if let FmtToken::Dig(c) = t {
            let before_dot = dot_idx.is_none_or(|d| i < d);
            let before_sci = sci_idx.is_none_or(|s| i < s);
            if before_dot {
                if *c == '0' {
                    int_min += 1;
                }
                last_int_dig_idx = i as i64;
            } else if before_sci {
                decimals += 1;
            }
        }
    }

    let mut scale = 0u32;
    for (i, t) in tokens.iter().enumerate() {
        if !matches!(t, FmtToken::Comma) {
            continue;
        }
        let before_dot = dot_idx.is_none_or(|d| i < d);
        if !before_dot {
            continue;
        }
        if last_int_dig_idx >= 0 && (i as i64) < last_int_dig_idx {
            has_thousands = true;
        } else if last_int_dig_idx >= 0 && (i as i64) > last_int_dig_idx {
            scale += 1;
        }
    }

    let has_sci = sci_idx.is_some();
    let mut sci_exp_digits = 0usize;
    if let Some(si) = sci_idx {
        for t in &tokens[si + 1..] {
            if matches!(t, FmtToken::Dig(_)) {
                sci_exp_digits += 1;
            }
        }
        if sci_exp_digits == 0 {
            sci_exp_digits = 2;
        }
    }

    let has_fraction = slash_idx.is_some() && sci_idx.is_none();
    let mut frac_denom_digits = 0u32;
    if let Some(sl) = slash_idx {
        if has_fraction {
            for t in &tokens[sl + 1..] {
                if matches!(t, FmtToken::Dig(_)) {
                    frac_denom_digits += 1;
                }
            }
            if frac_denom_digits == 0 {
                frac_denom_digits = 1;
            }
        }
    }

    (
        decimals,
        int_min,
        has_thousands,
        scale,
        has_percent,
        has_sci,
        sci_exp_digits,
        has_fraction,
        frac_denom_digits,
    )
}

// ── 应用 ─────────────────────────────────────────────────

const MONTHS_FULL: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS_FULL: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const WEEKDAYS_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// 值 + 编译格式 → 显示文本 + 可选颜色。
pub fn apply_format(value: &CellValue, compiled: &CompiledFormat) -> FormatResult {
    // 布尔
    if let CellValue::Bool(b) = value {
        return FormatResult {
            text: if *b { "TRUE" } else { "FALSE" }.to_string(),
            color: None,
        };
    }

    // General / 无格式
    if compiled.is_general || compiled.sections.is_empty() {
        return match value {
            CellValue::Text(s) => FormatResult {
                text: s.clone(),
                color: None,
            },
            CellValue::Number(n) => FormatResult {
                text: general_number(*n),
                color: None,
            },
            _ => FormatResult::default(),
        };
    }

    // 字符串值 → 文本段
    if let CellValue::Text(s) = value {
        let text_sec = compiled.sections.get(3).or_else(|| {
            compiled
                .sections
                .iter()
                .find(|sec| sec.kind == FmtKind::Text)
        });
        return match text_sec {
            Some(sec) => render_text(s, sec),
            None => FormatResult {
                text: s.clone(),
                color: None,
            },
        };
    }

    let num = match value {
        CellValue::Number(n) => *n,
        _ => return FormatResult::default(),
    };
    if !num.is_finite() {
        return FormatResult {
            text: num.to_string(),
            color: None,
        };
    }

    let Some(pick) = select_section(num, &compiled.sections) else {
        return FormatResult {
            text: general_number(num),
            color: None,
        };
    };
    let section = &compiled.sections[pick.index];

    match section.kind {
        FmtKind::Date => mk(render_date(num, &section.tokens), section.color.clone()),
        FmtKind::Text => mk(
            render_text(&general_number(num), section).text,
            section.color.clone(),
        ),
        FmtKind::Number => {
            let v = if pick.use_abs { num.abs() } else { num };
            let mut text = render_number(v, section);
            if pick.auto_minus && num < 0.0 {
                text = format!("-{text}");
            }
            mk(text, section.color.clone())
        }
    }
}

/// 便捷：格式串 → 结果（内部 compile）。
pub fn format_with(value: &CellValue, formatter: &str) -> FormatResult {
    apply_format(value, &compile_format(formatter))
}

fn mk(text: String, color: Option<String>) -> FormatResult {
    FormatResult { text, color }
}

struct Pick {
    index: usize,
    use_abs: bool,
    auto_minus: bool,
}

fn select_section(num: f64, sections: &[FmtSection]) -> Option<Pick> {
    let has_cond = sections.iter().any(|s| s.condition.is_some());
    if has_cond {
        for (i, s) in sections.iter().enumerate() {
            match s.condition {
                Some(c) => {
                    if match_condition(num, c) {
                        return Some(Pick {
                            index: i,
                            use_abs: num < 0.0,
                            auto_minus: false,
                        });
                    }
                }
                None => {
                    return Some(Pick {
                        index: i,
                        use_abs: false,
                        auto_minus: false,
                    });
                }
            }
        }
        // 全有条件且无一匹配 → 最后一段原样
        return sections.len().checked_sub(1).map(|i| Pick {
            index: i,
            use_abs: false,
            auto_minus: false,
        });
    }

    let n = sections.len();
    if num > 0.0 {
        return (n >= 1).then_some(Pick {
            index: 0,
            use_abs: false,
            auto_minus: false,
        });
    }
    if num < 0.0 {
        if n >= 2 {
            return Some(Pick {
                index: 1,
                use_abs: true,
                auto_minus: false,
            });
        }
        return (n >= 1).then_some(Pick {
            index: 0,
            use_abs: true,
            auto_minus: true,
        });
    }
    // === 0
    if n >= 3 {
        return Some(Pick {
            index: 2,
            use_abs: false,
            auto_minus: false,
        });
    }
    (n >= 1).then_some(Pick {
        index: 0,
        use_abs: false,
        auto_minus: false,
    })
}

fn match_condition(num: f64, cond: Condition) -> bool {
    match cond.op {
        CondOp::Ge => num >= cond.value,
        CondOp::Le => num <= cond.value,
        CondOp::Gt => num > cond.value,
        CondOp::Lt => num < cond.value,
        CondOp::Eq => num == cond.value,
        CondOp::Ne => num != cond.value,
    }
}

fn is_numeric_token(t: &FmtToken) -> bool {
    matches!(
        t,
        FmtToken::Dig(_) | FmtToken::Dot | FmtToken::Comma | FmtToken::Sci(_) | FmtToken::Slash
    )
}

fn render_number(abs: f64, section: &FmtSection) -> String {
    let mut v = abs;
    if section.scale > 0 {
        v /= 1000f64.powi(section.scale as i32);
    }
    if section.has_percent {
        v *= 100.0;
    }

    let first_num_idx = section.tokens.iter().position(is_numeric_token);
    let Some(first_num_idx) = first_num_idx else {
        return literals_only(&section.tokens);
    };

    let body = if section.has_sci {
        sci_body(v, section)
    } else if section.has_fraction {
        fraction_body(v, section)
    } else {
        plain_body(v, section)
    };

    let mut last_num_idx = first_num_idx;
    for i in (first_num_idx + 1..section.tokens.len()).rev() {
        if is_numeric_token(&section.tokens[i]) {
            last_num_idx = i;
            break;
        }
    }
    let mut out = String::new();
    for (i, t) in section.tokens.iter().enumerate() {
        if i == first_num_idx {
            out.push_str(&body);
            continue;
        }
        if is_numeric_token(t) {
            continue;
        }
        if i > first_num_idx && i < last_num_idx {
            continue; // 结构性内部字面量
        }
        match t {
            FmtToken::Pct => out.push('%'),
            FmtToken::Lit(s) => out.push_str(s),
            FmtToken::Skip => out.push(' '),
            FmtToken::Fill(c) => out.push(*c),
            _ => {}
        }
    }
    out
}

fn literals_only(tokens: &[FmtToken]) -> String {
    let mut out = String::new();
    for t in tokens {
        match t {
            FmtToken::Lit(s) => out.push_str(s),
            FmtToken::Pct => out.push('%'),
            FmtToken::Skip => out.push(' '),
            FmtToken::Fill(c) => out.push(*c),
            _ => {}
        }
    }
    out
}

fn plain_body(abs: f64, section: &FmtSection) -> String {
    let dec = section.decimals;
    let s = if dec > 0 {
        format!("{abs:.dec$}")
    } else {
        abs.round().to_string()
    };
    let (int_part, frac_part) = match s.split_once('.') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (s, String::new()),
    };
    let mut int_part = int_part;
    if int_part.len() < section.int_min {
        int_part = format!("{:0>width$}", int_part, width = section.int_min);
    }
    if section.has_thousands {
        int_part = add_thousands(&int_part);
    }
    if dec > 0 {
        format!("{int_part}.{frac_part}")
    } else {
        int_part
    }
}

fn sci_body(abs: f64, section: &FmtSection) -> String {
    let dec = section.decimals;
    let mut exp = 0i32;
    let mut mant = abs;
    if abs != 0.0 {
        exp = abs.log10().floor() as i32;
        mant = abs / 10f64.powi(exp);
        let rounded: f64 = format!("{mant:.dec$}").parse().unwrap_or(mant);
        if rounded >= 10.0 {
            mant = rounded / 10.0;
            exp += 1;
        }
    }
    let mant_str = format!("{mant:.dec$}");
    let sign = if exp >= 0 { '+' } else { '-' };
    let exp_str = format!("{:0>width$}", exp.abs(), width = section.sci_exp_digits);
    format!("{mant_str}E{sign}{exp_str}")
}

fn fraction_body(abs: f64, section: &FmtSection) -> String {
    let whole = abs.floor();
    let frac = abs - whole;
    let max_den = 10i64.pow(section.frac_denom_digits) - 1;
    let (n, d) = best_fraction(frac, max_den);
    if n == 0 {
        return format!("{}", whole as i64);
    }
    if whole == 0.0 {
        format!("{n}/{d}")
    } else {
        format!("{} {}/{}", whole as i64, n, d)
    }
}

fn best_fraction(frac: f64, max_den: i64) -> (i64, i64) {
    let mut best_n = 0i64;
    let mut best_d = 1i64;
    let mut best_err = frac.abs();
    for d in 1..=max_den {
        let n = (frac * d as f64).round() as i64;
        let err = (frac - n as f64 / d as f64).abs();
        if err < best_err {
            best_err = err;
            best_n = n;
            best_d = d;
        }
    }
    (best_n, best_d)
}

fn add_thousands(int_digits: &str) -> String {
    let neg = int_digits.starts_with('-');
    let digits = int_digits.trim_start_matches('-');
    let bytes: Vec<char> = digits.chars().collect();
    let len = bytes.len();
    let mut out = String::new();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

fn render_date(serial: f64, tokens: &[FmtToken]) -> String {
    let p = serial_to_parts(serial);
    let is12h = tokens
        .iter()
        .any(|t| matches!(t, FmtToken::Date(c) if c == "am/pm"));
    let mut out = String::new();
    for t in tokens {
        match t {
            FmtToken::Date(code) => out.push_str(&render_date_token(code, serial, &p, is12h)),
            FmtToken::Lit(s) => out.push_str(s),
            FmtToken::Skip => out.push(' '),
            FmtToken::Fill(c) => out.push(*c),
            FmtToken::Slash => out.push('/'),
            FmtToken::Comma => out.push(','),
            FmtToken::Dot => out.push('.'),
            _ => {}
        }
    }
    out
}

fn render_date_token(code: &str, serial: f64, p: &DateParts, is12h: bool) -> String {
    match code {
        "yyyy" => format!("{:04}", p.year),
        "yy" => format!("{:02}", p.year.rem_euclid(100)),
        "mmmm" => MONTHS_FULL[(p.month - 1) as usize].to_string(),
        "mmm" => MONTHS_ABBR[(p.month - 1) as usize].to_string(),
        "mm" => format!("{:02}", p.month),
        "m" => p.month.to_string(),
        "dddd" => WEEKDAYS_FULL[p.weekday as usize].to_string(),
        "ddd" => WEEKDAYS_ABBR[p.weekday as usize].to_string(),
        "dd" => format!("{:02}", p.day),
        "d" => p.day.to_string(),
        "hh" => format!("{:02}", hour12(p.hours, is12h)),
        "h" => hour12(p.hours, is12h).to_string(),
        "nn" => format!("{:02}", p.minutes),
        "n" => p.minutes.to_string(),
        "ss" => format!("{:02}", p.seconds),
        "s" => p.seconds.to_string(),
        "am/pm" => if p.hours < 12 { "AM" } else { "PM" }.to_string(),
        "[h]" => ((serial * 24.0).floor() as i64).to_string(),
        "[m]" => ((serial * 24.0 * 60.0).floor() as i64).to_string(),
        "[s]" => ((serial * 24.0 * 60.0 * 60.0).floor() as i64).to_string(),
        _ => String::new(),
    }
}

fn hour12(h: u32, is12h: bool) -> u32 {
    if !is12h {
        return h;
    }
    let r = h % 12;
    if r == 0 {
        12
    } else {
        r
    }
}

fn render_text(str_val: &str, section: &FmtSection) -> FormatResult {
    let mut out = String::new();
    for t in &section.tokens {
        match t {
            FmtToken::At => out.push_str(str_val),
            FmtToken::Lit(s) => out.push_str(s),
            FmtToken::Skip => out.push(' '),
            FmtToken::Fill(c) => out.push(*c),
            _ => {}
        }
    }
    mk(out, section.color.clone())
}

/// 通用数字最短表示（整值无小数），对齐 JS Number.toString / 父项目 generalNumber。
fn general_number(n: f64) -> String {
    crate::numstr::num_to_string(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date_serial::{date_to_serial, parts_to_serial};

    fn t(v: CellValue, fmt: &str) -> String {
        format_with(&v, fmt).text
    }
    fn c(v: CellValue, fmt: &str) -> Option<String> {
        format_with(&v, fmt).color
    }
    fn tn(n: f64, fmt: &str) -> String {
        t(CellValue::Number(n), fmt)
    }

    #[test]
    fn compile_general() {
        assert!(compile_format("").is_general());
        assert!(compile_format("General").is_general());
        assert!(!compile_format("#,##0").is_general());
    }

    #[test]
    fn compile_sections_split() {
        let c = compile_format("#,##0;[Red](#,##0);\"-\";@");
        assert_eq!(c.section_count(), 4);
        assert_eq!(c.sections[1].color.as_deref(), Some("#ff0000"));
    }

    #[test]
    fn compile_kinds() {
        assert_eq!(compile_format("#,##0.00").sections[0].kind, FmtKind::Number);
        assert_eq!(compile_format("yyyy-mm-dd").sections[0].kind, FmtKind::Date);
        assert_eq!(compile_format("@\"后缀\"").sections[0].kind, FmtKind::Text);
    }

    #[test]
    fn compile_number_plan() {
        let s = &compile_format("#,##0.00").sections[0];
        assert_eq!(s.decimals, 2);
        assert!(s.has_thousands);
        assert_eq!(compile_format("#,##0,").sections[0].scale, 1);
        assert_eq!(compile_format("#,##0,,").sections[0].scale, 2);
        assert!(compile_format("0.0%").sections[0].has_percent);
    }

    #[test]
    fn compile_condition() {
        let s = &compile_format("[>=100]0").sections[0];
        assert_eq!(
            s.condition,
            Some(Condition {
                op: CondOp::Ge,
                value: 100.0
            })
        );
    }

    #[test]
    fn compile_minute_disambiguation() {
        let s = &compile_format("hh:mm:ss").sections[0];
        let codes: Vec<String> = s
            .tokens
            .iter()
            .filter_map(|t| {
                if let FmtToken::Date(c) = t {
                    Some(c.clone())
                } else {
                    None
                }
            })
            .collect();
        assert!(codes.contains(&"nn".to_string()));
        assert!(!codes.contains(&"mm".to_string()));
    }

    #[test]
    #[allow(clippy::approx_constant)] // 3.14159 是格式化输入样本，非 PI 常量用途
    fn apply_number_basic() {
        assert_eq!(tn(3.14159, "0.00"), "3.14");
        assert_eq!(tn(5.0, "0.0"), "5.0");
        assert_eq!(tn(1234567.0, "#,##0"), "1,234,567");
        assert_eq!(tn(1234.5, "#,##0.00"), "1,234.50");
        assert_eq!(tn(0.1234, "0.00%"), "12.34%");
        assert_eq!(tn(0.5, "0%"), "50%");
        assert_eq!(tn(1234.5, "¥#,##0.00"), "¥1,234.50");
    }

    #[test]
    fn apply_negative_sections() {
        assert_eq!(tn(-1234.0, "#,##0;(#,##0)"), "(1,234)");
        assert_eq!(tn(1234.0, "#,##0;(#,##0)"), "1,234");
        assert_eq!(tn(-42.0, "0.0"), "-42.0");
    }

    #[test]
    fn apply_int_min_padding() {
        assert_eq!(tn(7.0, "000"), "007");
        assert_eq!(tn(42.5, "000.0"), "042.5");
    }

    #[test]
    fn apply_scaling() {
        assert_eq!(tn(1234567.0, "#,##0,"), "1,235");
        assert_eq!(tn(12000.0, "#,##0,\"K\""), "12K");
        assert_eq!(tn(2_500_000.0, "#,##0,,\"M\""), "3M");
    }

    #[test]
    fn apply_scientific() {
        assert_eq!(tn(0.5, "0.00E+00"), "5.00E-01");
        assert_eq!(tn(12345.0, "0.00E+00"), "1.23E+04");
        assert_eq!(tn(0.0, "0.00E+00"), "0.00E+00");
    }

    #[test]
    fn apply_fraction() {
        assert_eq!(tn(1.25, "# ?/?"), "1 1/4");
        assert_eq!(tn(0.5, "# ?/?"), "1/2");
        assert_eq!(tn(2.0, "# ?/?"), "2");
    }

    #[test]
    fn apply_zero_section() {
        assert_eq!(tn(0.0, "0.0;-0.0;\"—\""), "—");
    }

    #[test]
    fn apply_color_sections() {
        assert_eq!(
            c(CellValue::Number(-1234.0), "#,##0.00;[Red](#,##0.00)").as_deref(),
            Some("#ff0000")
        );
        assert_eq!(tn(-1234.0, "#,##0.00;[Red](#,##0.00)"), "(1,234.00)");
        assert_eq!(
            c(CellValue::Number(1234.0), "#,##0.00;[Red](#,##0.00)"),
            None
        );
        assert_eq!(
            c(CellValue::Number(1.0), "[Blue]0").as_deref(),
            Some("#0000ff")
        );
        assert_eq!(
            c(CellValue::Number(1.0), "[Color5]0").as_deref(),
            Some("#0000ff")
        );
    }

    #[test]
    fn apply_conditional_sections() {
        let f = "[>=100]\"大\";[<0]\"负\";0";
        assert_eq!(tn(150.0, f), "大");
        assert_eq!(tn(-5.0, f), "负");
        assert_eq!(tn(42.0, f), "42");
    }

    #[test]
    fn apply_text_sections() {
        assert_eq!(t(CellValue::Text("world".into()), "@\"!\""), "world!");
        assert_eq!(t(CellValue::Text("abc".into()), "\"[\" @ \"]\""), "[ abc ]");
        assert_eq!(t(CellValue::Text("hello".into()), ""), "hello");
    }

    #[test]
    fn apply_dates() {
        let d = date_to_serial(2024, 1, 15);
        assert_eq!(tn(d, "yyyy-mm-dd"), "2024-01-15");
        assert_eq!(tn(date_to_serial(2024, 3, 9), "m/d/yy"), "3/9/24");
        assert_eq!(tn(d, "yyyy年m月d日"), "2024年1月15日");
        assert_eq!(tn(d, "mmm d, yyyy"), "Jan 15, 2024");
        assert_eq!(tn(d, "mmmm"), "January");
        assert_eq!(tn(d, "ddd"), "Mon");
        assert_eq!(tn(d, "dddd"), "Monday");
    }

    #[test]
    fn apply_time() {
        let s = parts_to_serial(2024, 6, 15, 13, 45, 30);
        assert_eq!(tn(s, "yyyy-mm-dd hh:mm:ss"), "2024-06-15 13:45:30");
        assert_eq!(tn(s, "h:mm AM/PM"), "1:45 PM");
        assert_eq!(
            tn(parts_to_serial(2024, 6, 15, 0, 5, 0), "h:mm AM/PM"),
            "12:05 AM"
        );
        assert_eq!(tn(1.5, "[h]:mm"), "36:00");
    }

    #[test]
    fn apply_legacy_values() {
        assert_eq!(t(CellValue::Text(String::new()), ""), "");
        assert_eq!(t(CellValue::Bool(true), ""), "TRUE");
        assert_eq!(t(CellValue::Bool(false), ""), "FALSE");
        assert_eq!(tn(42.0, ""), "42");
        assert_eq!(tn(3.5, ""), "3.5");
    }
}
