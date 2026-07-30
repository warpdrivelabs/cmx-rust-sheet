//! 数据验证引擎（M12）。纯逻辑：给一条规则 + 用户输入串，判定是否通过。
//! 对标 cmx-megasheet 的 validation.ts。
//!
//! 类型：list（候选值集合）/ whole（整数）/ decimal（小数）/ date（日期序列）/
//!       textLength（文本长度）/ custom（自定义，交调用方求值）。
//! whole/decimal/date/textLength 用 operator + formula1/formula2 比较界。零 DOM。

use crate::worksheet::{DataValidation, ValidationOperator, ValidationType};

/// 校验结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    pub ok: bool,
    /// 失败原因（error 文案或默认）。
    pub message: Option<String>,
}

impl ValidationResult {
    fn pass() -> Self {
        ValidationResult {
            ok: true,
            message: None,
        }
    }
}

fn fail(rule: &DataValidation, def: &str) -> ValidationResult {
    ValidationResult {
        ok: false,
        message: Some(rule.error.clone().unwrap_or_else(|| def.to_string())),
    }
}

/// 校验用户输入串是否满足规则。custom_eval 为自定义求值器（type==Custom 时调用）。
pub fn validate_value(
    rule: &DataValidation,
    raw: &str,
    custom_eval: Option<&dyn Fn(&str) -> bool>,
) -> ValidationResult {
    let allow_blank = rule.allow_blank != Some(false);
    if raw.is_empty() {
        return if allow_blank {
            ValidationResult::pass()
        } else {
            fail(rule, "不允许空值")
        };
    }

    match rule.validation_type {
        ValidationType::List => {
            let list = rule.list.clone().unwrap_or_default();
            if list.iter().any(|x| x == raw) {
                ValidationResult::pass()
            } else {
                fail(rule, &format!("须为列表值之一：{}", list.join("、")))
            }
        }
        ValidationType::Whole => match raw.parse::<f64>() {
            Ok(n) if n.is_finite() && n.fract() == 0.0 => {
                if compare_bound(n, rule.operator, f1(rule), f2(rule)) {
                    ValidationResult::pass()
                } else {
                    fail(rule, "整数超出允许范围")
                }
            }
            _ => fail(rule, "须为整数"),
        },
        ValidationType::Decimal => match raw.parse::<f64>() {
            Ok(n) if n.is_finite() => {
                if compare_bound(n, rule.operator, f1(rule), f2(rule)) {
                    ValidationResult::pass()
                } else {
                    fail(rule, "数字超出允许范围")
                }
            }
            _ => fail(rule, "须为数字"),
        },
        ValidationType::Date => match raw.parse::<f64>() {
            Ok(n) if n.is_finite() => {
                if compare_bound(n, rule.operator, f1(rule), f2(rule)) {
                    ValidationResult::pass()
                } else {
                    fail(rule, "日期超出允许范围")
                }
            }
            _ => fail(rule, "须为有效日期"),
        },
        ValidationType::TextLength => {
            let len = raw.chars().count() as f64;
            if compare_bound(len, rule.operator, f1(rule), f2(rule)) {
                ValidationResult::pass()
            } else {
                fail(rule, "文本长度超出允许范围")
            }
        }
        ValidationType::Custom => {
            let formula = rule.formula1.as_ref().and_then(|b| b.as_text());
            match (custom_eval, formula) {
                (Some(eval), Some(f)) => {
                    if eval(f) {
                        ValidationResult::pass()
                    } else {
                        fail(rule, "自定义验证未通过")
                    }
                }
                _ => ValidationResult::pass(),
            }
        }
    }
}

fn f1(rule: &DataValidation) -> f64 {
    rule.formula1
        .as_ref()
        .map(|b| b.as_number())
        .unwrap_or(f64::NAN)
}
fn f2(rule: &DataValidation) -> f64 {
    rule.formula2
        .as_ref()
        .map(|b| b.as_number())
        .unwrap_or(f64::NAN)
}

fn compare_bound(v: f64, op: Option<ValidationOperator>, f1: f64, f2: f64) -> bool {
    match op {
        Some(ValidationOperator::Between) => v >= f1.min(f2) && v <= f1.max(f2),
        Some(ValidationOperator::NotBetween) => v < f1.min(f2) || v > f1.max(f2),
        Some(ValidationOperator::Eq) => v == f1,
        Some(ValidationOperator::Ne) => v != f1,
        Some(ValidationOperator::Gt) => v > f1,
        Some(ValidationOperator::Lt) => v < f1,
        Some(ValidationOperator::Ge) => v >= f1,
        Some(ValidationOperator::Le) => v <= f1,
        None => true, // 无 operator = 只校验类型
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worksheet::{RegionRect, ValidationBound};

    fn rule(vt: ValidationType) -> DataValidation {
        DataValidation {
            range: RegionRect::new(0, 0, 1, 1),
            validation_type: vt,
            operator: None,
            formula1: None,
            formula2: None,
            list: None,
            allow_blank: None,
            prompt: None,
            error: None,
        }
    }

    #[test]
    fn list_accepts_candidates() {
        let mut r = rule(ValidationType::List);
        r.list = Some(vec!["A".into(), "B".into(), "C".into()]);
        assert!(validate_value(&r, "A", None).ok);
        assert!(!validate_value(&r, "X", None).ok);
    }

    #[test]
    fn whole_between() {
        let mut r = rule(ValidationType::Whole);
        r.operator = Some(ValidationOperator::Between);
        r.formula1 = Some(ValidationBound::Number(1.0));
        r.formula2 = Some(ValidationBound::Number(10.0));
        assert!(validate_value(&r, "5", None).ok);
        assert!(!validate_value(&r, "11", None).ok);
        assert!(!validate_value(&r, "5.5", None).ok); // 非整数
    }

    #[test]
    fn decimal_gt() {
        let mut r = rule(ValidationType::Decimal);
        r.operator = Some(ValidationOperator::Gt);
        r.formula1 = Some(ValidationBound::Number(0.0));
        assert!(validate_value(&r, "0.1", None).ok);
        assert!(!validate_value(&r, "-1", None).ok);
    }

    #[test]
    fn text_length_le() {
        let mut r = rule(ValidationType::TextLength);
        r.operator = Some(ValidationOperator::Le);
        r.formula1 = Some(ValidationBound::Number(3.0));
        assert!(validate_value(&r, "abc", None).ok);
        assert!(!validate_value(&r, "abcd", None).ok);
    }

    #[test]
    fn allow_blank() {
        let mut strict = rule(ValidationType::Whole);
        strict.allow_blank = Some(false);
        assert!(!validate_value(&strict, "", None).ok);
        let mut lax = rule(ValidationType::Whole);
        lax.allow_blank = Some(true);
        assert!(validate_value(&lax, "", None).ok);
    }

    #[test]
    fn custom_eval() {
        let mut r = rule(ValidationType::Custom);
        r.formula1 = Some(ValidationBound::Text("A1>0".into()));
        let yes: &dyn Fn(&str) -> bool = &|_| true;
        let no: &dyn Fn(&str) -> bool = &|_| false;
        assert!(validate_value(&r, "5", Some(yes)).ok);
        assert!(!validate_value(&r, "5", Some(no)).ok);
    }

    #[test]
    fn error_message() {
        let mut r = rule(ValidationType::Whole);
        r.operator = Some(ValidationOperator::Gt);
        r.formula1 = Some(ValidationBound::Number(100.0));
        r.error = Some("必须大于100".into());
        assert_eq!(
            validate_value(&r, "50", None).message.as_deref(),
            Some("必须大于100")
        );
    }
}
