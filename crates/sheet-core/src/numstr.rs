//! 数值 → 字符串，对齐 JS `Number.prototype.toString()` 的常见输出。
//!
//! 用于单元格显示文本、TSV、公式文本拼接的兜底路径（正式数字格式串在 RS-M7 的
//! numfmt 模块）。关键差异点：整数值不带 `.0`（`42` 而非 `42.0`），与 cmx-megasheet
//! 的 `String(number)` 一致，保住跨引擎 parity。

/// f64 → 字符串（整数无小数点；其余走 Rust 最短往返 Display）。
pub fn num_to_string(n: f64) -> String {
    if n == 0.0 {
        // 归一 -0 → "0"（对齐 JS）
        return "0".to_string();
    }
    if !n.is_finite() {
        // JS: Infinity/-Infinity/NaN。表格里一般不落这些值，兜底给可读串。
        if n.is_nan() {
            return "NaN".to_string();
        }
        return if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    // Rust 的 f64 Display 已是最短往返；整数值天然无 ".0"。
    // 与 JS 的差异仅在极大/极小指数记法，RS-M7 numfmt 再精修。
    format!("{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_have_no_decimal() {
        assert_eq!(num_to_string(42.0), "42");
        assert_eq!(num_to_string(-7.0), "-7");
        assert_eq!(num_to_string(0.0), "0");
        assert_eq!(num_to_string(-0.0), "0");
        assert_eq!(num_to_string(1000000.0), "1000000");
    }

    #[test]
    fn fractions_kept() {
        assert_eq!(num_to_string(42.5), "42.5");
        assert_eq!(num_to_string(0.1), "0.1");
        assert_eq!(num_to_string(1.23456), "1.23456");
    }
}
