//! 单元格样式对象 + 命名样式表 + 优先级级联解析。
//!
//! 对标 cmx-megasheet 的 Style.ts。值语义中性：不含 dark/light 视图色。
//! 级联优先级（高→低）：单元格 > 行默认 > 列默认 > sheet 默认。
//!
//! serde：字段用 `#[serde(rename)]` 对齐 TS 的 camelCase 键 + `skip_serializing_if`
//! 保稀疏，使中性快照与 cmx-megasheet 字节同构（RS-M4 parity 基石）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 水平对齐（M18 补 fill/justify/centerContinuous）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HAlign {
    Left,
    Center,
    Right,
    Fill,
    Justify,
    CenterContinuous,
}

/// 垂直对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

/// 边框线型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BorderLineStyle {
    None,
    Thin,
    Medium,
    Thick,
    Dashed,
    Dotted,
    Double,
}

/// 单条边框（线型 + 颜色）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorderEdge {
    pub style: BorderLineStyle,
    pub color: String,
}

/// 四边 + 两对角边框。全部可选，稀疏。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Borders {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub top: Option<BorderEdge>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bottom: Option<BorderEdge>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub left: Option<BorderEdge>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub right: Option<BorderEdge>,
    /// M18 对角线：diagonalUp = 左下→右上。
    #[serde(
        rename = "diagonalUp",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub diagonal_up: Option<BorderEdge>,
    #[serde(
        rename = "diagonalDown",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub diagonal_down: Option<BorderEdge>,
}

impl Borders {
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.bottom.is_none()
            && self.left.is_none()
            && self.right.is_none()
            && self.diagonal_up.is_none()
            && self.diagonal_down.is_none()
    }
}

/// M18 图案填充类型（18 种 Excel patternType）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PatternType {
    Solid,
    Gray75,
    Gray50,
    Gray25,
    Gray125,
    Gray0625,
    Horizontal,
    Vertical,
    Down,
    Up,
    Grid,
    Trellis,
    LightHorizontal,
    LightVertical,
    LightDown,
    LightUp,
    LightGrid,
    LightTrellis,
}

/// 渐变停止点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    pub pos: f64,
    pub color: String,
}

/// M18 结构化填充（判别联合，对齐 TS CellFill）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CellFill {
    Solid {
        color: String,
    },
    Pattern {
        pattern: PatternType,
        #[serde(rename = "fgColor")]
        fg_color: String,
        #[serde(rename = "bgColor")]
        bg_color: String,
    },
    Gradient {
        kind: GradientKind,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        degree: Option<f64>,
        stops: Vec<GradientStop>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GradientKind {
    Linear,
    Path,
}

/// 单元格类型（M12）：目前仅 checkbox。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CellType {
    Checkbox,
}

/// 样式属性集。全部可选——稀疏存储，只记设过的键。对齐 TS StyleProps。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Style {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub italic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub underline: Option<bool>,
    #[serde(rename = "fontSize", skip_serializing_if = "Option::is_none", default)]
    pub font_size: Option<f64>,
    #[serde(
        rename = "fontFamily",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub font_family: Option<String>,
    #[serde(rename = "hAlign", skip_serializing_if = "Option::is_none", default)]
    pub h_align: Option<HAlign>,
    #[serde(rename = "vAlign", skip_serializing_if = "Option::is_none", default)]
    pub v_align: Option<VAlign>,
    #[serde(rename = "foreColor", skip_serializing_if = "Option::is_none", default)]
    pub fore_color: Option<String>,
    #[serde(rename = "backColor", skip_serializing_if = "Option::is_none", default)]
    pub back_color: Option<String>,
    /// 数字/日期格式串（Excel formatter 语义）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub formatter: Option<String>,
    #[serde(rename = "wordWrap", skip_serializing_if = "Option::is_none", default)]
    pub word_wrap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub borders: Option<Borders>,
    /// 引用命名样式名；解析时先展开命名样式再叠加本对象其余键。
    #[serde(rename = "styleName", skip_serializing_if = "Option::is_none", default)]
    pub style_name: Option<String>,
    #[serde(rename = "cellType", skip_serializing_if = "Option::is_none", default)]
    pub cell_type: Option<CellType>,
    // ── M18 样式细项 ──────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub strikethrough: Option<bool>,
    #[serde(
        rename = "textRotation",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub text_rotation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub indent: Option<f64>,
    #[serde(
        rename = "shrinkToFit",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub shrink_to_fit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fill: Option<CellFill>,
    // ── M20 保护 ──────────────────────────────────────
    /// 单元格锁定（Excel 语义：缺省=锁定；仅当 sheet 被保护时生效）。显式 false = 解锁。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub locked: Option<bool>,
}

impl Style {
    /// 判断样式是否无任何有效键（空 borders 视作无）。
    pub fn is_empty(&self) -> bool {
        self.bold.is_none()
            && self.italic.is_none()
            && self.underline.is_none()
            && self.font_size.is_none()
            && self.font_family.is_none()
            && self.h_align.is_none()
            && self.v_align.is_none()
            && self.fore_color.is_none()
            && self.back_color.is_none()
            && self.formatter.is_none()
            && self.word_wrap.is_none()
            && self.borders.as_ref().is_none_or(|b| b.is_empty())
            && self.style_name.is_none()
            && self.cell_type.is_none()
            && self.strikethrough.is_none()
            && self.text_rotation.is_none()
            && self.indent.is_none()
            && self.shrink_to_fit.is_none()
            && self.fill.is_none()
            && self.locked.is_none()
    }

    /// 用 `over` 的非 None 键覆盖 self（就地）。borders 逐边深合并。
    pub fn overlay(&mut self, over: &Style) {
        macro_rules! ov {
            ($($f:ident),+ $(,)?) => {
                $( if over.$f.is_some() { self.$f = over.$f.clone(); } )+
            };
        }
        ov!(
            bold,
            italic,
            underline,
            font_size,
            font_family,
            h_align,
            v_align,
            fore_color,
            back_color,
            formatter,
            word_wrap,
            style_name,
            cell_type,
            strikethrough,
            text_rotation,
            indent,
            shrink_to_fit,
            fill,
            locked
        );
        if over.borders.is_some() {
            self.borders = Some(merge_borders(self.borders.as_ref(), over.borders.as_ref()));
        }
    }
}

/// 浅合并两个样式：override 的非 None 键覆盖 base。borders 逐边深合并。返回新对象。
pub fn merge_style(base: Option<&Style>, over: Option<&Style>) -> Style {
    match (base, over) {
        (None, None) => Style::default(),
        (Some(b), None) => b.clone(),
        (None, Some(o)) => o.clone(),
        (Some(b), Some(o)) => {
            let mut out = b.clone();
            out.overlay(o);
            out
        }
    }
}

fn merge_borders(base: Option<&Borders>, over: Option<&Borders>) -> Borders {
    let mut out = base.cloned().unwrap_or_default();
    if let Some(o) = over {
        if o.top.is_some() {
            out.top = o.top.clone();
        }
        if o.bottom.is_some() {
            out.bottom = o.bottom.clone();
        }
        if o.left.is_some() {
            out.left = o.left.clone();
        }
        if o.right.is_some() {
            out.right = o.right.clone();
        }
        if o.diagonal_up.is_some() {
            out.diagonal_up = o.diagonal_up.clone();
        }
        if o.diagonal_down.is_some() {
            out.diagonal_down = o.diagonal_down.clone();
        }
    }
    out
}

/// 命名样式表：集中管理复用样式（对齐 ReportModel.grid.styleClasses）。
#[derive(Debug, Clone, Default)]
pub struct StyleSheet {
    named: BTreeMap<String, Style>,
}

impl StyleSheet {
    pub fn new() -> Self {
        StyleSheet::default()
    }

    /// 定义/覆盖一个命名样式。
    pub fn define(&mut self, name: &str, style: Style) {
        self.named.insert(name.to_string(), style);
    }

    /// 取命名样式（副本）；不存在返回 None。
    pub fn get(&self, name: &str) -> Option<Style> {
        self.named.get(name).cloned()
    }

    pub fn has(&self, name: &str) -> bool {
        self.named.contains_key(name)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.named.remove(name).is_some()
    }

    /// 命名列表（BTreeMap 保证有序，稳定 diff）。
    pub fn names(&self) -> Vec<String> {
        self.named.keys().cloned().collect()
    }

    /// 展开单个样式：若含 style_name，先取命名样式为底，叠加本对象其余键；
    /// 否则原样返回副本。剥去 style_name 键（已展开）。
    pub fn expand(&self, style: Option<&Style>) -> Style {
        let Some(style) = style else {
            return Style::default();
        };
        match &style.style_name {
            None => {
                let mut s = style.clone();
                s.style_name = None;
                s
            }
            Some(name) => {
                let base = self.get(name).unwrap_or_default();
                let mut rest = style.clone();
                rest.style_name = None;
                let mut out = base;
                out.overlay(&rest);
                out
            }
        }
    }

    /// 序列化命名样式表（供 snapshot）。
    pub fn to_map(&self) -> BTreeMap<String, Style> {
        self.named.clone()
    }

    /// 从 map 恢复。
    pub fn from_map(map: BTreeMap<String, Style>) -> Self {
        StyleSheet { named: map }
    }
}

/// 级联解析：按优先级从低到高依次合并，命名样式在各层内先展开。
/// 传入顺序应为 [sheet_default, col_default, row_default, cell]（低→高）。None 层跳过。
pub fn resolve_style(sheet: &StyleSheet, layers: &[Option<&Style>]) -> Style {
    let mut acc = Style::default();
    for layer in layers {
        if layer.is_none() {
            continue;
        }
        let expanded = sheet.expand(*layer);
        acc.overlay(&expanded);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(style: BorderLineStyle, color: &str) -> BorderEdge {
        BorderEdge {
            style,
            color: color.to_string(),
        }
    }

    #[test]
    fn is_empty_variants() {
        assert!(Style::default().is_empty());
        assert!(Style {
            borders: Some(Borders::default()),
            ..Default::default()
        }
        .is_empty());
        assert!(!Style {
            bold: Some(true),
            ..Default::default()
        }
        .is_empty());
        assert!(!Style {
            borders: Some(Borders {
                top: Some(edge(BorderLineStyle::Thin, "#000")),
                ..Default::default()
            }),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn merge_scalar_override_wins() {
        let r = merge_style(
            Some(&Style {
                bold: Some(true),
                font_size: Some(11.0),
                ..Default::default()
            }),
            Some(&Style {
                font_size: Some(14.0),
                ..Default::default()
            }),
        );
        assert_eq!(
            r,
            Style {
                bold: Some(true),
                font_size: Some(14.0),
                ..Default::default()
            }
        );
    }

    #[test]
    fn merge_skips_none() {
        // Rust 无 undefined：None 即「未设」，自然跳过。
        let r = merge_style(
            Some(&Style {
                bold: Some(true),
                ..Default::default()
            }),
            Some(&Style {
                italic: Some(true),
                ..Default::default()
            }),
        );
        assert_eq!(
            r,
            Style {
                bold: Some(true),
                italic: Some(true),
                ..Default::default()
            }
        );
    }

    #[test]
    fn merge_borders_per_edge() {
        let base = Style {
            borders: Some(Borders {
                top: Some(edge(BorderLineStyle::Thin, "#111")),
                left: Some(edge(BorderLineStyle::Thin, "#111")),
                ..Default::default()
            }),
            ..Default::default()
        };
        let over = Style {
            borders: Some(Borders {
                top: Some(edge(BorderLineStyle::Thick, "#f00")),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = merge_style(Some(&base), Some(&over));
        let b = r.borders.unwrap();
        assert_eq!(b.top, Some(edge(BorderLineStyle::Thick, "#f00")));
        assert_eq!(b.left, Some(edge(BorderLineStyle::Thin, "#111")));
    }

    #[test]
    fn merge_undefined_operands() {
        assert_eq!(
            merge_style(
                None,
                Some(&Style {
                    bold: Some(true),
                    ..Default::default()
                })
            ),
            Style {
                bold: Some(true),
                ..Default::default()
            }
        );
        assert_eq!(
            merge_style(
                Some(&Style {
                    bold: Some(true),
                    ..Default::default()
                }),
                None
            ),
            Style {
                bold: Some(true),
                ..Default::default()
            }
        );
        assert_eq!(merge_style(None, None), Style::default());
    }

    #[test]
    fn stylesheet_crud() {
        let mut ss = StyleSheet::new();
        ss.define(
            "h1",
            Style {
                bold: Some(true),
                font_size: Some(16.0),
                ..Default::default()
            },
        );
        assert!(ss.has("h1"));
        assert_eq!(
            ss.get("h1"),
            Some(Style {
                bold: Some(true),
                font_size: Some(16.0),
                ..Default::default()
            })
        );
        assert_eq!(ss.names(), vec!["h1".to_string()]);
        assert!(ss.remove("h1"));
        assert!(!ss.has("h1"));
    }

    #[test]
    fn expand_inlines_and_drops_name() {
        let mut ss = StyleSheet::new();
        ss.define(
            "title",
            Style {
                bold: Some(true),
                h_align: Some(HAlign::Center),
                ..Default::default()
            },
        );
        let r = ss.expand(Some(&Style {
            style_name: Some("title".into()),
            fore_color: Some("#333".into()),
            ..Default::default()
        }));
        assert_eq!(
            r,
            Style {
                bold: Some(true),
                h_align: Some(HAlign::Center),
                fore_color: Some("#333".into()),
                ..Default::default()
            }
        );
        assert!(r.style_name.is_none());
    }

    #[test]
    fn expand_cell_key_overrides_named() {
        let mut ss = StyleSheet::new();
        ss.define(
            "title",
            Style {
                bold: Some(true),
                h_align: Some(HAlign::Center),
                ..Default::default()
            },
        );
        let r = ss.expand(Some(&Style {
            style_name: Some("title".into()),
            h_align: Some(HAlign::Right),
            ..Default::default()
        }));
        assert_eq!(r.h_align, Some(HAlign::Right));
    }

    #[test]
    fn expand_without_name() {
        let ss = StyleSheet::new();
        assert_eq!(
            ss.expand(Some(&Style {
                bold: Some(true),
                ..Default::default()
            })),
            Style {
                bold: Some(true),
                ..Default::default()
            }
        );
        assert_eq!(ss.expand(None), Style::default());
    }

    #[test]
    fn stylesheet_serialize_round_trip() {
        let mut ss = StyleSheet::new();
        ss.define(
            "a",
            Style {
                bold: Some(true),
                ..Default::default()
            },
        );
        ss.define(
            "b",
            Style {
                italic: Some(true),
                ..Default::default()
            },
        );
        let restored = StyleSheet::from_map(ss.to_map());
        assert_eq!(
            restored.get("a"),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
        assert_eq!(
            restored.get("b"),
            Some(Style {
                italic: Some(true),
                ..Default::default()
            })
        );
    }

    #[test]
    fn resolve_cascade_low_to_high() {
        let ss = StyleSheet::new();
        let sheet_default = Style {
            font_family: Some("Arial".into()),
            font_size: Some(11.0),
            ..Default::default()
        };
        let col_default = Style {
            font_size: Some(12.0),
            ..Default::default()
        };
        let row_default = Style {
            bold: Some(true),
            ..Default::default()
        };
        let cell = Style {
            font_size: Some(14.0),
            fore_color: Some("#333".into()),
            ..Default::default()
        };
        let r = resolve_style(
            &ss,
            &[
                Some(&sheet_default),
                Some(&col_default),
                Some(&row_default),
                Some(&cell),
            ],
        );
        assert_eq!(
            r,
            Style {
                font_family: Some("Arial".into()),
                font_size: Some(14.0),
                bold: Some(true),
                fore_color: Some("#333".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn resolve_expands_named_in_layers() {
        let mut ss = StyleSheet::new();
        ss.define(
            "emph",
            Style {
                bold: Some(true),
                fore_color: Some("#c00".into()),
                ..Default::default()
            },
        );
        let r = resolve_style(
            &ss,
            &[
                None,
                Some(&Style {
                    style_name: Some("emph".into()),
                    ..Default::default()
                }),
                None,
                Some(&Style {
                    fore_color: Some("#00c".into()),
                    ..Default::default()
                }),
            ],
        );
        assert_eq!(
            r,
            Style {
                bold: Some(true),
                fore_color: Some("#00c".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn resolve_empty_layers() {
        assert_eq!(resolve_style(&StyleSheet::new(), &[]), Style::default());
    }

    #[test]
    fn serde_renames_to_camel_case() {
        // 验证 snapshot 字段名与 TS 对齐（RS-M4 parity 前哨）。
        let s = Style {
            font_size: Some(12.0),
            fore_color: Some("#333".into()),
            bold: Some(true),
            ..Default::default()
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"fontSize\":12"), "got {j}");
        assert!(j.contains("\"foreColor\":\"#333\""), "got {j}");
        assert!(j.contains("\"bold\":true"), "got {j}");
        // 稀疏：未设字段不出现
        assert!(!j.contains("italic"), "got {j}");
    }
}
