//! 公式 token 流 → AST。formula 层第二站（M3）。对标 cmx-megasheet 的 Parser.ts。
//!
//! Pratt / 优先级爬升解析器。支持中缀运算符（比较 < 连接& < 加减 < 乘除 < 幂，幂右结合）、
//! 一元 -x/+x、尾随百分号 50%→unary '%'、函数调用、单格/区域/跨表引用、整列整行、数组
//! 字面量 {1,2;3,4}、括号分组、字符串、数字、命名（TRUE/FALSE/命名区域）。零 DOM。

use crate::token::{tokenize, Token, TokenType};

/// AST 节点。对标 TS `AstNode` 判别联合。
#[derive(Debug, Clone, PartialEq)]
pub enum AstNode {
    Number(f64),
    Str(String),
    /// 单格引用（含可选 sheet 前缀）。
    Ref(String),
    /// 区域（start/end 为端点引用文本）。
    Range {
        start: String,
        end: String,
    },
    /// 数组字面量 {1,2;3,4}（行×列）。
    Array(Vec<Vec<AstNode>>),
    /// 命名 / 布尔字面（TRUE/FALSE/命名区域）。
    Name(String),
    Unary {
        op: String,
        operand: Box<AstNode>,
    },
    Binary {
        op: String,
        left: Box<AstNode>,
        right: Box<AstNode>,
    },
    Call {
        name: String,
        args: Vec<AstNode>,
    },
}

/// 语法错误（含位置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaParseError {
    pub message: String,
    pub pos: usize,
}

impl std::fmt::Display for FormulaParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (@{})", self.message, self.pos)
    }
}

impl std::error::Error for FormulaParseError {}

/// 中缀运算符优先级（越大越紧）。
fn bin_prec(op: &str) -> Option<u8> {
    match op {
        "=" | "<>" | "<" | ">" | "<=" | ">=" => Some(1),
        "&" => Some(2),
        "+" | "-" => Some(3),
        "*" | "/" => Some(4),
        "^" => Some(5),
        _ => None,
    }
}

fn is_right_assoc(op: &str) -> bool {
    op == "^"
}

struct Parser {
    tokens: Vec<Token>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.i]
    }

    fn next(&mut self) -> Token {
        let t = self.tokens[self.i].clone();
        self.i += 1;
        t
    }

    fn expect(&mut self, ty: TokenType) -> Result<Token, FormulaParseError> {
        let t = self.peek();
        if t.ty != ty {
            return Err(FormulaParseError {
                message: format!("期望 {:?}，遇到 '{}'", ty, disp(t)),
                pos: t.pos,
            });
        }
        Ok(self.next())
    }

    fn parse(&mut self) -> Result<AstNode, FormulaParseError> {
        if self.peek().ty == TokenType::Eof {
            return Err(FormulaParseError {
                message: "空公式".to_string(),
                pos: 0,
            });
        }
        let node = self.parse_expr(0)?;
        if self.peek().ty != TokenType::Eof {
            let t = self.peek();
            return Err(FormulaParseError {
                message: format!("多余的记号 '{}'", disp(t)),
                pos: t.pos,
            });
        }
        Ok(node)
    }

    /// 优先级爬升：解析 ≥ min_prec 的中缀表达式。
    fn parse_expr(&mut self, min_prec: u8) -> Result<AstNode, FormulaParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let t = self.peek();
            if t.ty != TokenType::Op {
                break;
            }
            let op = t.text.clone();
            let prec = match bin_prec(&op) {
                Some(p) if p >= min_prec => p,
                _ => break,
            };
            self.next();
            let next_min = if is_right_assoc(&op) { prec } else { prec + 1 };
            let right = self.parse_expr(next_min)?;
            left = AstNode::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// 一元前缀（- +）+ 后缀百分号。
    fn parse_unary(&mut self) -> Result<AstNode, FormulaParseError> {
        let t = self.peek();
        if t.ty == TokenType::Op && (t.text == "-" || t.text == "+") {
            let op = t.text.clone();
            self.next();
            let operand = self.parse_unary()?;
            let node = if op == "-" {
                AstNode::Unary {
                    op: "-".to_string(),
                    operand: Box::new(operand),
                }
            } else {
                operand
            };
            return Ok(self.parse_postfix(node));
        }
        let primary = self.parse_primary()?;
        Ok(self.parse_postfix(primary))
    }

    /// 后缀百分号：x% → unary '%'。
    fn parse_postfix(&mut self, node: AstNode) -> AstNode {
        let mut cur = node;
        while self.peek().ty == TokenType::Percent {
            self.next();
            cur = AstNode::Unary {
                op: "%".to_string(),
                operand: Box::new(cur),
            };
        }
        cur
    }

    fn parse_primary(&mut self) -> Result<AstNode, FormulaParseError> {
        let t = self.peek().clone();
        match t.ty {
            TokenType::Number => {
                self.next();
                Ok(AstNode::Number(t.value_num.unwrap_or(0.0)))
            }
            TokenType::Str => {
                self.next();
                Ok(AstNode::Str(t.text))
            }
            TokenType::LParen => {
                self.next();
                let inner = self.parse_expr(0)?;
                self.expect(TokenType::RParen)?;
                Ok(inner)
            }
            TokenType::LBrace => self.parse_array(),
            TokenType::Ref => self.parse_ref_or_range(),
            TokenType::Range => self.parse_whole_range(),
            TokenType::Ident => self.parse_ident(),
            _ => Err(FormulaParseError {
                message: format!("意外的记号 '{}'", disp(&t)),
                pos: t.pos,
            }),
        }
    }

    /// 引用或区域：ref [':' ref] → range，否则 ref。
    fn parse_ref_or_range(&mut self) -> Result<AstNode, FormulaParseError> {
        let first = self.next();
        if self.peek().ty == TokenType::Colon {
            self.next();
            let second = self.expect(TokenType::Ref)?;
            Ok(AstNode::Range {
                start: first.text,
                end: second.text,
            })
        } else {
            Ok(AstNode::Ref(first.text))
        }
    }

    /// 整列/整行区域 token（A:C / Sheet1!2:5）→ range 节点。
    fn parse_whole_range(&mut self) -> Result<AstNode, FormulaParseError> {
        let t = self.next();
        let text = t.text;
        let (prefix, body) = match text.find('!') {
            Some(b) => (&text[..b + 1], &text[b + 1..]),
            None => ("", text.as_str()),
        };
        let colon = body.find(':').unwrap_or(0);
        let a = &body[..colon];
        let b = &body[colon + 1..];
        Ok(AstNode::Range {
            start: format!("{prefix}{a}"),
            end: format!("{prefix}{b}"),
        })
    }

    /// 数组字面量 {1,2;3,4}：逗号分列、分号分行。
    fn parse_array(&mut self) -> Result<AstNode, FormulaParseError> {
        self.expect(TokenType::LBrace)?;
        let mut rows: Vec<Vec<AstNode>> = Vec::new();
        let mut row: Vec<AstNode> = vec![self.parse_expr(0)?];
        while self.peek().ty == TokenType::Comma || self.peek().ty == TokenType::Semicolon {
            let sep = self.next().ty;
            if sep == TokenType::Semicolon {
                rows.push(std::mem::take(&mut row));
            }
            row.push(self.parse_expr(0)?);
        }
        rows.push(row);
        self.expect(TokenType::RBrace)?;
        Ok(AstNode::Array(rows))
    }

    /// 标识符：函数调用 name(...) 或命名/布尔字面。
    fn parse_ident(&mut self) -> Result<AstNode, FormulaParseError> {
        let id = self.next();
        if self.peek().ty == TokenType::LParen {
            self.next();
            let mut args: Vec<AstNode> = Vec::new();
            if self.peek().ty != TokenType::RParen {
                args.push(self.parse_expr(0)?);
                while self.peek().ty == TokenType::Comma {
                    self.next();
                    args.push(self.parse_expr(0)?);
                }
            }
            self.expect(TokenType::RParen)?;
            Ok(AstNode::Call {
                name: id.text.to_uppercase(),
                args,
            })
        } else {
            Ok(AstNode::Name(id.text))
        }
    }
}

fn disp(t: &Token) -> String {
    if t.text.is_empty() {
        format!("{:?}", t.ty)
    } else {
        t.text.clone()
    }
}

/// 解析公式源串 → AST（前导 '=' 自动剥离）。
pub fn parse_formula(input: &str) -> Result<AstNode, FormulaParseError> {
    let tokens = tokenize(input).map_err(|e| FormulaParseError {
        message: e.message,
        pos: e.pos,
    })?;
    let mut p = Parser { tokens, i: 0 };
    p.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_refs() {
        assert_eq!(parse_formula("42").unwrap(), AstNode::Number(42.0));
        assert_eq!(parse_formula("\"hi\"").unwrap(), AstNode::Str("hi".into()));
        assert_eq!(parse_formula("A1").unwrap(), AstNode::Ref("A1".into()));
        assert_eq!(
            parse_formula("A1:C3").unwrap(),
            AstNode::Range {
                start: "A1".into(),
                end: "C3".into()
            }
        );
        assert_eq!(
            parse_formula("Sheet1!A1:B2").unwrap(),
            AstNode::Range {
                start: "Sheet1!A1".into(),
                end: "B2".into()
            }
        );
        assert_eq!(parse_formula("TRUE").unwrap(), AstNode::Name("TRUE".into()));
    }

    #[test]
    fn precedence() {
        // 1+2*3 => 1 + (2*3)
        if let AstNode::Binary { op, right, .. } = parse_formula("1+2*3").unwrap() {
            assert_eq!(op, "+");
            assert!(matches!(*right, AstNode::Binary { ref op, .. } if op == "*"));
        } else {
            panic!("expected binary");
        }
        // 2^3^2 => 2^(3^2) 右结合
        if let AstNode::Binary { op, right, .. } = parse_formula("2^3^2").unwrap() {
            assert_eq!(op, "^");
            assert!(matches!(*right, AstNode::Binary { ref op, .. } if op == "^"));
        } else {
            panic!();
        }
        // (1+2)*3
        if let AstNode::Binary { op, left, .. } = parse_formula("(1+2)*3").unwrap() {
            assert_eq!(op, "*");
            assert!(matches!(*left, AstNode::Binary { ref op, .. } if op == "+"));
        } else {
            panic!();
        }
        // A1+1>B1 => (A1+1)>B1
        if let AstNode::Binary { op, left, .. } = parse_formula("A1+1>B1").unwrap() {
            assert_eq!(op, ">");
            assert!(matches!(*left, AstNode::Binary { ref op, .. } if op == "+"));
        } else {
            panic!();
        }
        // "a"&1+2 => "a"&(1+2)
        if let AstNode::Binary { op, right, .. } = parse_formula("\"a\"&1+2").unwrap() {
            assert_eq!(op, "&");
            assert!(matches!(*right, AstNode::Binary { ref op, .. } if op == "+"));
        } else {
            panic!();
        }
    }

    #[test]
    fn unary_percent() {
        assert_eq!(
            parse_formula("-5").unwrap(),
            AstNode::Unary {
                op: "-".into(),
                operand: Box::new(AstNode::Number(5.0))
            }
        );
        assert_eq!(parse_formula("+5").unwrap(), AstNode::Number(5.0));
        assert_eq!(
            parse_formula("50%").unwrap(),
            AstNode::Unary {
                op: "%".into(),
                operand: Box::new(AstNode::Number(50.0))
            }
        );
        // 3--2 => 3 - (-2)
        if let AstNode::Binary { op, right, .. } = parse_formula("3--2").unwrap() {
            assert_eq!(op, "-");
            assert!(matches!(*right, AstNode::Unary { ref op, .. } if op == "-"));
        } else {
            panic!();
        }
    }

    #[test]
    fn function_calls() {
        assert_eq!(
            parse_formula("NOW()").unwrap(),
            AstNode::Call {
                name: "NOW".into(),
                args: vec![]
            }
        );
        if let AstNode::Call { name, args } = parse_formula("sum(A1, B1, 3)").unwrap() {
            assert_eq!(name, "SUM");
            assert_eq!(args.len(), 3);
        } else {
            panic!();
        }
        if let AstNode::Call { name, args } = parse_formula("IF(A1>0, ROUND(B1*2,2), 0)").unwrap() {
            assert_eq!(name, "IF");
            assert!(matches!(args[0], AstNode::Binary { ref op, .. } if op == ">"));
            assert!(matches!(args[1], AstNode::Call { ref name, .. } if name == "ROUND"));
            assert_eq!(args[2], AstNode::Number(0.0));
        } else {
            panic!();
        }
        if let AstNode::Call { args, .. } = parse_formula("SUM(A1:A10)").unwrap() {
            assert_eq!(
                args[0],
                AstNode::Range {
                    start: "A1".into(),
                    end: "A10".into()
                }
            );
        } else {
            panic!();
        }
    }

    #[test]
    fn errors() {
        assert!(parse_formula("").is_err());
        assert!(parse_formula("SUM(A1").is_err());
        assert!(parse_formula("1 2").is_err());
        assert!(
            matches!(parse_formula("=1+1").unwrap(), AstNode::Binary { ref op, .. } if op == "+")
        );
    }
}
