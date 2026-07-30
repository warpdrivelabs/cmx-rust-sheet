//! A1 地址 ↔ 行列索引（0-based）互转，以及地址/区域字符串解析。
//!
//! 整个引擎的坐标基元层：core / formula / io 全部依赖它。行为对齐 cmx-megasheet
//! 的 address.ts（colToLabel/labelToCol/parseAddr/parseRange…），使中性快照的 A1
//! 语义在两个引擎间完全一致。
//!
//! 约定：
//!  - 列索引、行索引均 0-based（A→0, B→1；第 1 行→0）。
//!  - 列标签为大写字母序列（A..Z, AA..），bijective base-26。
//!  - A1 引用文本里行号是 1-based（"A1" → {row:0,col:0}）。

/// 单元格坐标（0-based）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellCoord {
    pub row: u32,
    pub col: u32,
}

/// 矩形区域坐标（0-based，闭区间 [r1..r2] × [c1..c2]，已归一化 r1≤r2, c1≤c2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeCoord {
    pub r1: u32,
    pub c1: u32,
    pub r2: u32,
    pub c2: u32,
}

/// 列索引（0-based）→ 列标签（"A", "Z", "AA"…）。
/// bijective base-26：idx 0→A, 25→Z, 26→AA。
pub fn col_to_label(index: u32) -> String {
    // 0-based → 1-based 计数
    let mut n = index as u64 + 1;
    let mut s = Vec::new();
    while n > 0 {
        let r = ((n - 1) % 26) as u8;
        s.push(b'A' + r);
        n = (n - 1) / 26;
    }
    if s.is_empty() {
        return "A".to_string();
    }
    s.reverse();
    // s 只含 ASCII 大写字母，from_utf8 不会失败
    String::from_utf8(s).unwrap()
}

/// 列标签 → 列索引（0-based）。大小写不敏感。
/// 非法标签（空 / 含非字母）返回 None（TS 版返回 -1）。
pub fn label_to_col(label: &str) -> Option<u32> {
    if label.is_empty() || !label.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut n: u64 = 0;
    for b in label.bytes() {
        let up = b.to_ascii_uppercase();
        n = n * 26 + (up - b'A' + 1) as u64;
    }
    Some((n - 1) as u32)
}

/// 解析单格地址 "A1" → CellCoord（0-based）。大小写不敏感、trim。
/// 非法输入返回 None。不处理 `$` 绝对引用符号（公式层负责剥离）。
pub fn parse_addr(addr: &str) -> Option<CellCoord> {
    let s = addr.trim();
    // 拆成前缀字母段 + 后缀数字段
    let split = s.bytes().position(|b| b.is_ascii_digit())?;
    if split == 0 {
        return None; // 无字母前缀
    }
    let (letters, digits) = s.split_at(split);
    if !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let col = label_to_col(letters)?;
    let row1: u64 = digits.parse().ok()?;
    if row1 < 1 {
        return None; // 行号 1-based，0 非法
    }
    Some(CellCoord {
        row: (row1 - 1) as u32,
        col,
    })
}

/// 组装单格地址 (row,col) → "A1"（0-based 入，1-based 行号出）。
pub fn format_addr(row: u32, col: u32) -> String {
    format!("{}{}", col_to_label(col), row + 1)
}

/// 解析区域字符串 → 归一化 RangeCoord。
/// 支持 "A1:C3"（区域）与 "A1"（单格，退化为 1×1）。端点顺序任意。非法返回 None。
pub fn parse_range(range: &str) -> Option<RangeCoord> {
    let raw = range.trim();
    if raw.is_empty() {
        return None;
    }
    let mut parts = raw.splitn(2, ':');
    let first = parts.next()?;
    let second = parts.next();
    let a = parse_addr(first)?;
    let b = match second {
        Some(s) => parse_addr(s)?,
        None => a,
    };
    Some(RangeCoord {
        r1: a.row.min(b.row),
        c1: a.col.min(b.col),
        r2: a.row.max(b.row),
        c2: a.col.max(b.col),
    })
}

/// 组装区域坐标 → 区域字符串。单格输出 "A1"，否则 "A1:C3"。输入不要求已归一。
pub fn format_range(range: RangeCoord) -> String {
    let r1 = range.r1.min(range.r2);
    let c1 = range.c1.min(range.c2);
    let r2 = range.r1.max(range.r2);
    let c2 = range.c1.max(range.c2);
    let a = format_addr(r1, c1);
    let b = format_addr(r2, c2);
    if a == b {
        a
    } else {
        format!("{a}:{b}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_label_first_26() {
        assert_eq!(col_to_label(0), "A");
        assert_eq!(col_to_label(25), "Z");
    }

    #[test]
    fn col_label_double_letter() {
        assert_eq!(col_to_label(26), "AA");
        assert_eq!(col_to_label(27), "AB");
        assert_eq!(col_to_label(51), "AZ");
        assert_eq!(col_to_label(52), "BA");
        assert_eq!(col_to_label(701), "ZZ");
        assert_eq!(col_to_label(702), "AAA");
    }

    #[test]
    fn col_label_round_trip() {
        for idx in [0, 1, 25, 26, 51, 52, 700, 701, 702, 16383] {
            assert_eq!(label_to_col(&col_to_label(idx)), Some(idx));
        }
    }

    #[test]
    fn label_to_col_case_insensitive() {
        assert_eq!(label_to_col("a"), Some(0));
        assert_eq!(label_to_col("aa"), Some(26));
        assert_eq!(label_to_col("Ab"), Some(27));
    }

    #[test]
    fn label_to_col_invalid() {
        assert_eq!(label_to_col(""), None);
        assert_eq!(label_to_col("A1"), None);
        assert_eq!(label_to_col("1"), None);
        assert_eq!(label_to_col("A B"), None);
    }

    #[test]
    fn parse_addr_basic() {
        assert_eq!(parse_addr("A1"), Some(CellCoord { row: 0, col: 0 }));
        assert_eq!(parse_addr("C3"), Some(CellCoord { row: 2, col: 2 }));
        assert_eq!(parse_addr("AA10"), Some(CellCoord { row: 9, col: 26 }));
    }

    #[test]
    fn parse_addr_trims_case() {
        assert_eq!(parse_addr("  b2 "), Some(CellCoord { row: 1, col: 1 }));
    }

    #[test]
    fn parse_addr_malformed() {
        assert_eq!(parse_addr(""), None);
        assert_eq!(parse_addr("A"), None);
        assert_eq!(parse_addr("1"), None);
        assert_eq!(parse_addr("A0"), None); // 行 0 非法（1-based）
        assert_eq!(parse_addr("$A$1"), None); // 绝对引用符不在此处理
    }

    #[test]
    fn parse_addr_round_trip() {
        for (r, c) in [(0, 0), (2, 2), (9, 26), (99, 701)] {
            let a = format_addr(r, c);
            assert_eq!(parse_addr(&a), Some(CellCoord { row: r, col: c }));
        }
    }

    #[test]
    fn parse_range_normal() {
        assert_eq!(
            parse_range("A1:C3"),
            Some(RangeCoord {
                r1: 0,
                c1: 0,
                r2: 2,
                c2: 2
            })
        );
    }

    #[test]
    fn parse_range_reversed() {
        assert_eq!(
            parse_range("C3:A1"),
            Some(RangeCoord {
                r1: 0,
                c1: 0,
                r2: 2,
                c2: 2
            })
        );
    }

    #[test]
    fn parse_range_single() {
        assert_eq!(
            parse_range("B2"),
            Some(RangeCoord {
                r1: 1,
                c1: 1,
                r2: 1,
                c2: 1
            })
        );
    }

    #[test]
    fn parse_range_malformed() {
        assert_eq!(parse_range(""), None);
        assert_eq!(parse_range("A1:"), None);
        assert_eq!(parse_range(":C3"), None);
    }

    #[test]
    fn format_range_single_no_colon() {
        assert_eq!(
            format_range(RangeCoord {
                r1: 1,
                c1: 1,
                r2: 1,
                c2: 1
            }),
            "B2"
        );
    }

    #[test]
    fn format_range_multi_normalizes() {
        assert_eq!(
            format_range(RangeCoord {
                r1: 2,
                c1: 2,
                r2: 0,
                c2: 0
            }),
            "A1:C3"
        );
    }

    #[test]
    fn format_range_round_trip() {
        for s in ["A1:C3", "B2", "A1:Z100"] {
            assert_eq!(format_range(parse_range(s).unwrap()), s);
        }
    }
}
