//! A1 公式词法分析器。formula 层第一站（M3）。对标 cmx-megasheet 的 Tokenizer.ts。
//!
//! 把公式源串（不含前导 '='）切成 token 流：数字、字符串、单元格/区域引用、跨表引用
//! （Sheet!A1）、整列/整行区域（A:A / 1:1）、函数名/标识符、运算符、括号、逗号、分号、
//! 冒号、百分号、花括号。纯逻辑、零 DOM。Parser 消费此 token 流。

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Number,
    Str,
    /// 单元格引用 A1 / $A$1 / Sheet1!A1。
    Ref,
    /// 整列/整行区域 A:A / 1:1（一次性匹配，避免 A: 被拆散）。
    Range,
    /// 函数名或命名（SUM、TRUE、myName）。
    Ident,
    /// 运算符 + - * / ^ & = <> < > <= >=。
    Op,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semicolon,
    Colon,
    Percent,
    Eof,
}

/// 词法记号。`value_num` 承载 number 值；`text` 承载源文本片段。
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub ty: TokenType,
    pub text: String,
    pub pos: usize,
    /// number token 的数值。
    pub value_num: Option<f64>,
}

impl Token {
    fn new(ty: TokenType, text: impl Into<String>, pos: usize) -> Self {
        Token {
            ty,
            text: text.into(),
            pos,
            value_num: None,
        }
    }
}

/// 词法错误（含位置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaLexError {
    pub message: String,
    pub pos: usize,
}

impl std::fmt::Display for FormulaLexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (@{})", self.message, self.pos)
    }
}

impl std::error::Error for FormulaLexError {}

// 单元格引用：可选 sheet 前缀 + 可选 $ + 列字母 + 可选 $ + 行数字。锚定到片段起始。
fn ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:('[^']+'|[A-Za-z_\x{4e00}-\x{9fa5}][A-Za-z0-9_\x{4e00}-\x{9fa5}]*)!)?(\$?[A-Za-z]{1,3})(\$?[0-9]+)").unwrap()
    })
}

// 整列区域：A:A / $A:$C（可带 sheet 前缀），后不接字母数字。
fn whole_col_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:('[^']+'|[A-Za-z_\x{4e00}-\x{9fa5}][A-Za-z0-9_\x{4e00}-\x{9fa5}]*)!)?(\$?[A-Za-z]{1,3}):(\$?[A-Za-z]{1,3})(?:[^A-Za-z0-9]|$)").unwrap()
    })
}

// 整行区域：1:1 / 2:5（可带 sheet 前缀），后不接数字。
fn whole_row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:('[^']+'|[A-Za-z_\x{4e00}-\x{9fa5}][A-Za-z0-9_\x{4e00}-\x{9fa5}]*)!)?(\$?[0-9]+):(\$?[0-9]+)(?:[^0-9]|$)").unwrap()
    })
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c >= '\u{4e00}'
}

fn is_ident_part(c: char) -> bool {
    is_ident_start(c) || is_digit(c) || c == '.'
}

const OP_CHARS: &[char] = &['+', '-', '*', '/', '^', '&', '=', '<', '>'];

/// 词法分析：源串 → token 数组（末尾含 eof）。前导 '=' 自动剥。
pub fn tokenize(input: &str) -> Result<Vec<Token>, FormulaLexError> {
    let stripped = input.strip_prefix('=').unwrap_or(input);
    // 以字符向量索引，pos 用字符下标（与 TS 一致：TS 也按 UTF-16 code unit，但公式中
    // CJK 仅出现在 sheet 名/命名内，位置仅用于报错，字符下标已足够精确）。
    let chars: Vec<char> = stripped.chars().collect();
    let n = chars.len();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0;

    // 缓存「从字符 i 起的剩余子串」以喂正则；正则匹配长度按字符数回转。
    let rest_from = |i: usize| -> String { chars[i..].iter().collect() };

    while i < n {
        let c = chars[i];

        // 空白
        if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
            i += 1;
            continue;
        }

        // 字符串 "..."（内部 "" 转义为一个 "）
        if c == '"' {
            let mut j = i + 1;
            let mut s = String::new();
            let mut closed = false;
            while j < n {
                if chars[j] == '"' {
                    if j + 1 < n && chars[j + 1] == '"' {
                        s.push('"');
                        j += 2;
                        continue;
                    }
                    closed = true;
                    j += 1;
                    break;
                }
                s.push(chars[j]);
                j += 1;
            }
            if !closed {
                return Err(FormulaLexError {
                    message: "未闭合的字符串".to_string(),
                    pos: i,
                });
            }
            let text: String = chars[i..j].iter().collect();
            let mut tok = Token::new(TokenType::Str, text, i);
            tok.text = s; // string token 的 text 存已解转义内容（对齐 TS value）
            tokens.push(tok);
            i = j;
            continue;
        }

        // 整行区域（1:1 / 2:5）——须先于数字分支
        {
            let rest = rest_from(i);
            if let Some(caps) = whole_row_re().captures(&rest) {
                let text = matched_range_text(&caps);
                let clen = text.chars().count();
                tokens.push(Token::new(TokenType::Range, text, i));
                i += clen;
                continue;
            }
        }

        // 数字（含小数、科学计数法）
        if is_digit(c) || (c == '.' && i + 1 < n && is_digit(chars[i + 1])) {
            let mut j = i;
            while j < n && is_digit(chars[j]) {
                j += 1;
            }
            if j < n && chars[j] == '.' {
                j += 1;
                while j < n && is_digit(chars[j]) {
                    j += 1;
                }
            }
            if j < n && (chars[j] == 'e' || chars[j] == 'E') {
                let mut k = j + 1;
                if k < n && (chars[k] == '+' || chars[k] == '-') {
                    k += 1;
                }
                if k < n && is_digit(chars[k]) {
                    j = k;
                    while j < n && is_digit(chars[j]) {
                        j += 1;
                    }
                }
            }
            let text: String = chars[i..j].iter().collect();
            let mut tok = Token::new(TokenType::Number, text.clone(), i);
            tok.value_num = text.parse::<f64>().ok();
            tokens.push(tok);
            i = j;
            continue;
        }

        // 整列区域（A:A / $A:$C）——先于普通 ref 与 ident
        {
            let rest = rest_from(i);
            if let Some(caps) = whole_col_re().captures(&rest) {
                let text = matched_range_text(&caps);
                let clen = text.chars().count();
                tokens.push(Token::new(TokenType::Range, text, i));
                i += clen;
                continue;
            }
        }

        // 引用（Sheet!A1 / A1 / $A$1）——先于 ident
        if let Some(caps) = ref_re().captures(&rest_from(i)) {
            let full = caps.get(0).unwrap().as_str();
            let text = full.to_string();
            let clen = text.chars().count();
            // 消歧：紧跟 '(' 的「像引用的标识符」实为函数调用（LOG10(...)）。
            let next_is_paren = i + clen < n && chars[i + clen] == '(';
            // 消歧：紧跟标识符字母/下划线者，是更长的标识符（DEC2BIN、BIN2HEX 等函数名
            // 中段含数字，ref_re 会先贪配出 DEC2 段）——不当引用，落到下方 ident 分支整取。
            let next_is_ident =
                i + clen < n && (chars[i + clen].is_ascii_alphabetic() || chars[i + clen] == '_');
            if !next_is_paren && !next_is_ident {
                tokens.push(Token::new(TokenType::Ref, text, i));
                i += clen;
                continue;
            }
        }

        // 标识符 / 函数名
        if is_ident_start(c) {
            let mut j = i + 1;
            while j < n && is_ident_part(chars[j]) {
                j += 1;
            }
            let text: String = chars[i..j].iter().collect();
            tokens.push(Token::new(TokenType::Ident, text, i));
            i = j;
            continue;
        }

        // 括号 / 标点
        let single = match c {
            '(' => Some(TokenType::LParen),
            ')' => Some(TokenType::RParen),
            '{' => Some(TokenType::LBrace),
            '}' => Some(TokenType::RBrace),
            ',' => Some(TokenType::Comma),
            ';' => Some(TokenType::Semicolon),
            ':' => Some(TokenType::Colon),
            '%' => Some(TokenType::Percent),
            _ => None,
        };
        if let Some(ty) = single {
            tokens.push(Token::new(ty, c.to_string(), i));
            i += 1;
            continue;
        }

        // 运算符（含双字符 <> <= >=）
        if OP_CHARS.contains(&c) {
            let mut text = c.to_string();
            if i + 1 < n {
                let c2 = chars[i + 1];
                if (c == '<' && (c2 == '>' || c2 == '=')) || (c == '>' && c2 == '=') {
                    text.push(c2);
                }
            }
            let clen = text.chars().count();
            tokens.push(Token::new(TokenType::Op, text, i));
            i += clen;
            continue;
        }

        return Err(FormulaLexError {
            message: format!("无法识别的字符 '{c}'"),
            pos: i,
        });
    }

    tokens.push(Token::new(TokenType::Eof, "", n));
    Ok(tokens)
}

/// 从整列/整行捕获里拼回不含哨兵字符的区域文本（前缀! + a:b）。
fn matched_range_text(caps: &regex::Captures) -> String {
    let prefix = caps
        .get(1)
        .map(|m| format!("{}!", m.as_str()))
        .unwrap_or_default();
    let a = caps.get(2).unwrap().as_str();
    let b = caps.get(3).unwrap().as_str();
    format!("{prefix}{a}:{b}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(input: &str) -> Vec<TokenType> {
        tokenize(input)
            .unwrap()
            .into_iter()
            .filter(|t| t.ty != TokenType::Eof)
            .map(|t| t.ty)
            .collect()
    }
    fn texts(input: &str) -> Vec<String> {
        tokenize(input)
            .unwrap()
            .into_iter()
            .filter(|t| t.ty != TokenType::Eof)
            .map(|t| t.text)
            .collect()
    }

    #[test]
    fn numbers() {
        assert_eq!(tokenize("42").unwrap()[0].value_num, Some(42.0));
        assert_eq!(tokenize("3.25").unwrap()[0].value_num, Some(3.25));
        assert_eq!(tokenize(".5").unwrap()[0].value_num, Some(0.5));
        assert_eq!(tokenize("1.5e3").unwrap()[0].value_num, Some(1500.0));
        assert_eq!(tokenize("2E-2").unwrap()[0].value_num, Some(0.02));
    }

    #[test]
    fn strings() {
        let t = &tokenize("\"hello\"").unwrap()[0];
        assert_eq!(t.ty, TokenType::Str);
        assert_eq!(t.text, "hello");
        let t2 = &tokenize("\"a\"\"b\"").unwrap()[0];
        assert_eq!(t2.text, "a\"b");
        assert!(tokenize("\"oops").is_err());
    }

    #[test]
    fn references() {
        assert_eq!(tokenize("A1").unwrap()[0].ty, TokenType::Ref);
        assert_eq!(tokenize("A1").unwrap()[0].text, "A1");
        assert_eq!(tokenize("$A$1").unwrap()[0].text, "$A$1");
        assert_eq!(tokenize("$B2").unwrap()[0].text, "$B2");
        assert_eq!(tokenize("Sheet1!A1").unwrap()[0].text, "Sheet1!A1");
        assert_eq!(tokenize("'资产 表'!B2").unwrap()[0].text, "'资产 表'!B2");
        assert_eq!(
            types("A1:C3"),
            vec![TokenType::Ref, TokenType::Colon, TokenType::Ref]
        );
    }

    #[test]
    fn idents_not_refs() {
        assert_eq!(tokenize("SUM").unwrap()[0].ty, TokenType::Ident);
        assert_eq!(tokenize("TRUE").unwrap()[0].ty, TokenType::Ident);
    }

    #[test]
    fn operators_punct() {
        assert_eq!(
            texts("1+2-3*4/5^6&7"),
            vec!["1", "+", "2", "-", "3", "*", "4", "/", "5", "^", "6", "&", "7"]
        );
        assert_eq!(texts("a<>b"), vec!["a", "<>", "b"]);
        assert_eq!(texts("a<=b"), vec!["a", "<=", "b"]);
        assert_eq!(texts("a>=b"), vec!["a", ">=", "b"]);
        assert_eq!(texts("a=b"), vec!["a", "=", "b"]);
        assert_eq!(
            types("SUM(A1,B1)"),
            vec![
                TokenType::Ident,
                TokenType::LParen,
                TokenType::Ref,
                TokenType::Comma,
                TokenType::Ref,
                TokenType::RParen
            ]
        );
        assert_eq!(types("50%"), vec![TokenType::Number, TokenType::Percent]);
    }

    #[test]
    fn full_formulas() {
        assert_eq!(texts("=SUM(A1:A3)"), vec!["SUM", "(", "A1", ":", "A3", ")"]);
        assert_eq!(
            types("IF(A1>0, ROUND(B1*2, 2), 0)"),
            vec![
                TokenType::Ident,
                TokenType::LParen,
                TokenType::Ref,
                TokenType::Op,
                TokenType::Number,
                TokenType::Comma,
                TokenType::Ident,
                TokenType::LParen,
                TokenType::Ref,
                TokenType::Op,
                TokenType::Number,
                TokenType::Comma,
                TokenType::Number,
                TokenType::RParen,
                TokenType::Comma,
                TokenType::Number,
                TokenType::RParen,
            ]
        );
        assert_eq!(tokenize("1").unwrap().last().unwrap().ty, TokenType::Eof);
        assert_eq!(tokenize("").unwrap().last().unwrap().ty, TokenType::Eof);
        assert_eq!(texts("  A1  +  B1 "), vec!["A1", "+", "B1"]);
    }

    #[test]
    fn whole_col_row_ranges() {
        assert_eq!(tokenize("A:A").unwrap()[0].ty, TokenType::Range);
        assert_eq!(tokenize("A:A").unwrap()[0].text, "A:A");
        assert_eq!(tokenize("1:1").unwrap()[0].text, "1:1");
        assert_eq!(tokenize("Sheet1!2:5").unwrap()[0].text, "Sheet1!2:5");
        // 整列后接内容不吞
        assert_eq!(
            types("SUM(A:A)"),
            vec![
                TokenType::Ident,
                TokenType::LParen,
                TokenType::Range,
                TokenType::RParen
            ]
        );
    }
}

#[cfg(test)]
mod digit_mid_name_tests {
    use super::*;
    #[test]
    fn func_name_with_mid_digit_is_one_ident() {
        // DEC2BIN/BIN2HEX 中段含数字，曾被 ref_re 贪配出 DEC2 段致解析失败。
        for name in ["DEC2BIN", "BIN2HEX", "HEX2DEC", "OCT2BIN"] {
            let toks = tokenize(&format!("{name}(100)")).unwrap();
            assert_eq!(toks[0].ty, TokenType::Ident, "{name} 应为单一 Ident");
            assert_eq!(toks[0].text, name);
            assert_eq!(toks[1].ty, TokenType::LParen);
        }
        // 真单元格引用后接函数不受影响
        let t = tokenize("A1+SUM(B1:B2)").unwrap();
        assert_eq!(t[0].ty, TokenType::Ref);
        assert_eq!(t[0].text, "A1");
    }
}
