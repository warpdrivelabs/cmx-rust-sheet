//! 区域排序（M11）。纯数据层：按主/次关键字给区域行排序。对标 cmx-megasheet 的 sort.ts。
//!
//! 类型感知：数字 < 文本、日期序列按数字、空值末位（升降都排后，对齐 Excel）。保持整行随动。
//! 本模块只算「行的新顺序」(permutation)，不改 sheet——命令层据此搬数据 + 快照撤销。零 DOM。

use std::cmp::Ordering;

use crate::cell::CellValue;

/// 排序关键字。
#[derive(Debug, Clone, Copy)]
pub struct SortKey {
    /// 关键字列（0-based 绝对列）。
    pub col: u32,
    /// 升序（默认 true）。
    pub ascending: bool,
}

impl SortKey {
    pub fn new(col: u32, ascending: bool) -> Self {
        SortKey { col, ascending }
    }
}

fn is_empty(v: Option<&CellValue>) -> bool {
    match v {
        None => true,
        Some(CellValue::Text(s)) => s.is_empty(),
        _ => false,
    }
}

/// 一行的排序输入：(绝对行号, 各关键字列的值)。
pub type SortRow = (u32, Vec<Option<CellValue>>);

/// 计算区域行的排序后新顺序（返回原行号的排列，稳定排序）。
/// rows[i] = (绝对行号, 各关键字列的值)，values[k] 对应 keys[k]。
pub fn compute_sort_order(rows: &[SortRow], keys: &[SortKey]) -> Vec<u32> {
    let mut indexed: Vec<(usize, &SortRow)> = rows.iter().enumerate().collect();
    indexed.sort_by(|(ia, a), (ib, b)| {
        for (k, key) in keys.iter().enumerate() {
            let av = a.1.get(k).and_then(|o| o.as_ref());
            let bv = b.1.get(k).and_then(|o| o.as_ref());
            let a_empty = is_empty(av);
            let b_empty = is_empty(bv);
            // 空值恒排后（不受升降影响）
            if a_empty && !b_empty {
                return Ordering::Greater;
            }
            if !a_empty && b_empty {
                return Ordering::Less;
            }
            if a_empty && b_empty {
                continue;
            }
            let cmp = compare_cell_values(av, bv);
            if cmp != Ordering::Equal {
                return if key.ascending { cmp } else { cmp.reverse() };
            }
        }
        ia.cmp(ib) // 稳定
    });
    indexed.iter().map(|(_, (row, _))| *row).collect()
}

/// 类型感知比较：空末位；数字 < 文本；布尔按 FALSE<TRUE 转字符串混排。返回 Ordering。
pub fn compare_cell_values(a: Option<&CellValue>, b: Option<&CellValue>) -> Ordering {
    let a_empty = is_empty(a);
    let b_empty = is_empty(b);
    if a_empty && b_empty {
        return Ordering::Equal;
    }
    if a_empty {
        return Ordering::Greater;
    }
    if b_empty {
        return Ordering::Less;
    }
    let a = a.unwrap();
    let b = b.unwrap();
    match (a, b) {
        (CellValue::Number(x), CellValue::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (CellValue::Number(_), _) => Ordering::Less, // 数字在文本前
        (_, CellValue::Number(_)) => Ordering::Greater,
        _ => {
            let sa = to_sort_text(a);
            let sb = to_sort_text(b);
            sa.cmp(&sb)
        }
    }
}

fn to_sort_text(v: &CellValue) -> String {
    match v {
        CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        cv => cv.to_text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(x: f64) -> Option<CellValue> {
        Some(CellValue::Number(x))
    }
    fn t(s: &str) -> Option<CellValue> {
        Some(CellValue::Text(s.into()))
    }

    #[test]
    fn type_aware_compare() {
        assert_eq!(
            compare_cell_values(n(1.0).as_ref(), n(2.0).as_ref()),
            Ordering::Less
        );
        assert_eq!(
            compare_cell_values(n(5.0).as_ref(), t("a").as_ref()),
            Ordering::Less
        );
        assert_eq!(
            compare_cell_values(t("a").as_ref(), t("b").as_ref()),
            Ordering::Less
        );
        assert_eq!(
            compare_cell_values(None, n(5.0).as_ref()),
            Ordering::Greater
        );
    }

    #[test]
    fn single_key_ascending() {
        let rows = vec![(0u32, vec![n(3.0)]), (1, vec![n(1.0)]), (2, vec![n(2.0)])];
        assert_eq!(
            compute_sort_order(&rows, &[SortKey::new(0, true)]),
            vec![1, 2, 0]
        );
    }

    #[test]
    fn descending_empty_stays_last() {
        let rows = vec![(0u32, vec![n(3.0)]), (1, vec![None]), (2, vec![n(5.0)])];
        assert_eq!(
            compute_sort_order(&rows, &[SortKey::new(0, false)]),
            vec![2, 0, 1]
        );
    }

    #[test]
    fn multi_key() {
        let rows = vec![
            (0u32, vec![t("A"), n(2.0)]),
            (1, vec![t("A"), n(1.0)]),
            (2, vec![t("B"), n(9.0)]),
        ];
        let keys = [SortKey::new(0, true), SortKey::new(1, false)];
        assert_eq!(compute_sort_order(&rows, &keys), vec![0, 1, 2]);
    }
}
