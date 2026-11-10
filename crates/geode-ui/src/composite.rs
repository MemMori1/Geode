//! 高亮层合成（M2 T2.4）。
//!
//! DESIGN: 05-render-and-hexview.md §4.7、26-visual-design.md §4.9
//!
//! 合成规则（与 05 §4.7 对齐）：
//! 1. 从最高 z 向下找到第一个**不透明**（`a == 255`）层作为底色；
//! 2. 比它更低的层不做 alpha 混合，只把底色沿 HSL 的 L 轴压暗，
//!    每层按层序差贡献 `0.05 × gap × opacity`，累计不超过 `0.20`——保留色相、可感知层次；
//! 3. 前景取最高 z 层的颜色（若最高层就是底色，则按底色明暗取黑/白兜底）；
//! 4. 前景与底色对比度不足 [`MIN_CONTRAST`] 时，沿 L 轴二分推到刚好达标；
//! 5. 装饰通道按层映射：Search → 下划线、Yara → 左带、Diff → 顶边、Ai → 斜纹。
//!
//! 本模块**与主题无关**：不查表、不读全局状态，所有颜色都由调用方传入。

use geode_core::{ByteRange, Offset};

/// 最低可读对比度（WCAG AA 正文要求，05 §3.3 的 `min_contrast` 默认值）。
pub const MIN_CONTRAST: f32 = 4.5;

/// 10 个高亮层，声明顺序即 z 序（低 → 高，05 §4.7 / 00 §5.3）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// 值类型分区（u8/u16/… 的交替底色）。
    #[default]
    Value,
    /// 搜索命中。
    Search,
    /// BinSpec 结构区间。
    Structure,
    /// 熵段 / 字符串密度 / 坏区等机器侧标注。
    Analysis,
    /// 选区。
    Selection,
    /// 书签。
    Bookmark,
    /// 差异。
    Diff,
    /// YARA 命中。
    Yara,
    /// AI 推断结果。
    Ai,
    /// 光标。
    Cursor,
}

impl Layer {
    /// 该层的 z 序号，`Value` 为 0，`Cursor` 为 9。
    #[inline]
    pub const fn z(self) -> u8 {
        match self {
            Self::Value => 0,
            Self::Search => 1,
            Self::Structure => 2,
            Self::Analysis => 3,
            Self::Selection => 4,
            Self::Bookmark => 5,
            Self::Diff => 6,
            Self::Yara => 7,
            Self::Ai => 8,
            Self::Cursor => 9,
        }
    }

    /// 该层对应的非色相冗余装饰（色盲可辨通道，05 §4.7 步骤 4）。
    ///
    /// 没有专属装饰的层返回 [`Decoration::None`]。
    #[inline]
    pub const fn decoration(self) -> Decoration {
        match self {
            Self::Search => Decoration::Underline,
            Self::Yara => Decoration::LeftBar,
            Self::Diff => Decoration::TopEdge,
            Self::Ai => Decoration::Hatch,
            _ => Decoration::None,
        }
    }
}

/// 8 位 sRGB + alpha 颜色。
///
/// `a == 255` 表示不透明；合成算法只把不透明层当底色，其余 alpha 视作
/// 该层的 opacity（05 §3.3 的 `layers[L].opacity`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// 红通道。
    pub r: u8,
    /// 绿通道。
    pub g: u8,
    /// 蓝通道。
    pub b: u8,
    /// 不透明度：255 = 完全不透明，0 = 完全透明。
    pub a: u8,
}

impl Rgba {
    /// 全透明黑。
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    /// 不透明黑（对比度计算的暗端锚点）。
    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    /// 不透明白（对比度计算的亮端锚点）。
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    /// 构造任意 RGBA。
    #[inline]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// 构造不透明颜色。
    #[inline]
    pub const fn opaque(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// 是否完全不透明。
    #[inline]
    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }
}

/// WCAG 2.x 相对亮度（sRGB 先线性化，再按 0.2126/0.7152/0.0722 加权）。
pub trait RelativeLuminance {
    /// 返回 `[0, 1]` 的相对亮度。
    fn relative_luminance(self) -> f32;
}

impl RelativeLuminance for Rgba {
    fn relative_luminance(self) -> f32 {
        fn linearize(channel: u8) -> f32 {
            let c = channel as f32 / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linearize(self.r) + 0.7152 * linearize(self.g) + 0.0722 * linearize(self.b)
    }
}

/// WCAG 对比度 `(L_亮 + 0.05) / (L_暗 + 0.05)`，范围 `1.0..=21.0`。
///
/// 只比较 RGB；alpha 不参与（WCAG 公式针对不透明前景/背景）。
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f32 {
    let la = a.relative_luminance();
    let lb = b.relative_luminance();
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    // 浮点误差可能让纯黑/纯白略超 21；按 WCAG 定义域夹紧，保证 1.0..=21.0。
    ((hi + 0.05) / (lo + 0.05)).clamp(1.0, 21.0)
}

/// 一条命中某个字节区间的高亮。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Highlight {
    /// 命中的字节区间（半开）。
    pub range: ByteRange,
    /// 所属层，决定 z 序。
    pub layer: Layer,
    /// 层色：作为底色参与压暗；最高层时也作为前景候选色。
    pub color: Rgba,
    /// 同层内覆盖顺序，越大越靠上（05 §4.7 "同层按 priority 升序"）。
    pub priority: u8,
}

/// 非色相冗余装饰通道。
///
/// 一个单元格最多返回一个主装饰；同格多种装饰共存属于渲染层的叠加职责
/// （各装饰占固定像素位置，互不覆盖，05 §4.7 步骤 4）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Decoration {
    /// 无装饰。
    #[default]
    None,
    /// 1px 下划线（Search）。
    Underline,
    /// 3px 左带（Yara）。
    LeftBar,
    /// 1px 顶边（Diff）。
    TopEdge,
    /// 斜纹（Ai 低置信）。
    Hatch,
}

/// 单个字节的合成结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Composite {
    /// 底色（始终不透明）。
    pub bg: Rgba,
    /// 前景文字色（已保证与 `bg` 的对比度 ≥ [`MIN_CONTRAST`]）。
    pub fg: Rgba,
    /// 装饰通道。
    pub decoration: Decoration,
}

/// 合成某个偏移处的高亮；无主题底时用中性黑兜底。
///
/// 主题相关的调用方应使用 [`composite_at_with_base`] 传入自己的 `bg.base`，
/// 这样"无任何不透明层"时会压暗真实主题底而不是黑。
pub fn composite_at(offset: u64, layers: &[Highlight]) -> Composite {
    composite_at_with_base(offset, layers, Rgba::BLACK)
}

/// 与 [`composite_at`] 相同，但允许调用方传入主题默认底色。
///
/// `fallback_bg` 的 alpha 会被强制为 255：它代表已经开始绘制的不透明底层。
pub fn composite_at_with_base(offset: u64, layers: &[Highlight], fallback_bg: Rgba) -> Composite {
    let at = Offset::new(offset);
    let hits: Vec<(usize, &Highlight)> = layers
        .iter()
        .enumerate()
        .filter(|(_, h)| h.range.contains(at))
        .collect();

    // 1. 最高 z 的不透明层定底色；同层 priority 大者优先，再按输入顺序稳定。
    let mut base: Option<(u8, u8, usize)> = None;
    for &(index, h) in &hits {
        if !h.color.is_opaque() {
            continue;
        }
        let rank = (h.layer.z(), h.priority, index);
        if base.is_none_or(|current| rank > current) {
            base = Some(rank);
        }
    }
    let base_color = base
        .and_then(|(_, _, index)| layers.get(index))
        .map_or(fallback_bg, |h| h.color);
    let base_color = Rgba {
        a: 255,
        ..base_color
    };

    // 2. 底色之下的层只压 L，不混色：每层按层序差贡献 0.05 × opacity，累计上限 0.20。
    let base_z = base.map(|(z, _, _)| z);
    let mut darken = 0.0_f32;
    for &(index, h) in &hits {
        let rank = (h.layer.z(), h.priority, index);
        let below = base.is_none_or(|current| rank < current);
        let gap = base_z.map(|z| z.saturating_sub(h.layer.z())).unwrap_or(1);
        if below && gap > 0 && h.color.a > 0 {
            darken += 0.05 * gap as f32 * (h.color.a as f32 / 255.0);
            darken = darken.min(0.20);
        }
    }
    let bg = if darken > 0.0 {
        let (h, s, l) = rgb_to_hsl(base_color);
        hsl_to_rgb(h, s, (l - darken).max(0.0))
    } else {
        base_color
    };

    // 3. 前景：最高 z 层（透明度 > 0）的颜色；若最高的就是底色本身，
    //    则按底色明暗在黑/白之间取对比度更大的一侧作为兜底。
    let mut top: Option<(u8, u8, usize)> = None;
    for &(index, h) in &hits {
        if h.color.a == 0 {
            continue;
        }
        let rank = (h.layer.z(), h.priority, index);
        if top.is_none_or(|current| rank > current) {
            top = Some(rank);
        }
    }
    let fg_candidate = match top {
        Some(rank) if base.is_none_or(|b| rank > b) => {
            let (_, _, index) = rank;
            Rgba {
                a: 255,
                ..layers[index].color
            }
        }
        _ => default_fg(bg),
    };
    let fg = ensure_contrast(fg_candidate, bg, MIN_CONTRAST);

    // 4. 装饰：命中的层里 z 最高的装饰通道。
    let mut decoration = Decoration::None;
    let mut decoration_rank: Option<(u8, u8, usize)> = None;
    for &(index, h) in &hits {
        let candidate = h.layer.decoration();
        if candidate == Decoration::None {
            continue;
        }
        let rank = (h.layer.z(), h.priority, index);
        if decoration_rank.is_none_or(|current| rank > current) {
            decoration = candidate;
            decoration_rank = Some(rank);
        }
    }

    Composite { bg, fg, decoration }
}

/// 按底色亮度选黑/白中对比度更大的那个。
fn default_fg(bg: Rgba) -> Rgba {
    if contrast_ratio(Rgba::BLACK, bg) >= contrast_ratio(Rgba::WHITE, bg) {
        Rgba::BLACK
    } else {
        Rgba::WHITE
    }
}

/// 不足 `min_contrast` 时，把 `fg` 沿 HSL 的 L 轴向黑/白推，直到刚好达标。
fn ensure_contrast(fg: Rgba, bg: Rgba, min_contrast: f32) -> Rgba {
    if contrast_ratio(fg, bg) >= min_contrast {
        return fg;
    }

    // 黑/白中必有一侧能达到 4.5:1（两式的交点约在 L=0.179），选余量更大的一侧。
    let target_black = contrast_ratio(Rgba::BLACK, bg) >= contrast_ratio(Rgba::WHITE, bg);
    let target_l = if target_black { 0.0 } else { 1.0 };
    let (h, s, l) = rgb_to_hsl(fg);

    // 极端色也不达标时（理论上不会），直接返回黑白中较好的一个，绝不返回低对比色。
    let extreme = hsl_to_rgb(h, s, target_l);
    if contrast_ratio(extreme, bg) < min_contrast {
        return default_fg(bg);
    }

    // 对比度沿 L 单调；二分找满足条件的最小位移（20 次 ≈ 1e-6 精度）。
    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    for _ in 0..20 {
        let mid = (lo + hi) * 0.5;
        let candidate = hsl_to_rgb(h, s, l + (target_l - l) * mid);
        if contrast_ratio(candidate, bg) >= min_contrast {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hsl_to_rgb(h, s, l + (target_l - l) * hi)
}

/// sRGB → HSL，返回 `(h, s, l)`，各分量都在 `[0, 1]`。
fn rgb_to_hsl(color: Rgba) -> (f32, f32, f32) {
    let r = color.r as f32 / 255.0;
    let g = color.g as f32 / 255.0;
    let b = color.b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if max - min <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

/// HSL → sRGB，`h/s/l` 都按 `[0, 1]` 输入，越界分量会被夹紧。
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Rgba {
    let h = h.rem_euclid(1.0);
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    if s <= 0.0 {
        let v = (l * 255.0).round() as u8;
        return Rgba::opaque(v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f32| -> u8 {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    Rgba::opaque(channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange::new(Offset::new(start), Offset::new(end)).expect("测试区间必须合法")
    }

    fn hl(layer: Layer, color: Rgba, priority: u8, start: u64, end: u64) -> Highlight {
        Highlight {
            range: range(start, end),
            layer,
            color,
            priority,
        }
    }

    fn hue(color: Rgba) -> f32 {
        rgb_to_hsl(color).0
    }

    fn hue_distance(a: f32, b: f32) -> f32 {
        let d = (a - b).abs();
        d.min(1.0 - d)
    }

    #[test]
    fn wcag_contrast_black_white_is_21() {
        let ratio = contrast_ratio(Rgba::BLACK, Rgba::WHITE);
        assert!((ratio - 21.0).abs() < 1e-4, "实测 {ratio}");
    }

    #[test]
    fn wcag_contrast_is_symmetric_and_identity_is_one() {
        let a = Rgba::opaque(0x12, 0x34, 0x56);
        let b = Rgba::opaque(0xab, 0xcd, 0xef);
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-6);
        assert!((contrast_ratio(a, a) - 1.0).abs() < 1e-6);
        assert!((contrast_ratio(Rgba::WHITE, Rgba::WHITE) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn relative_luminance_known_values() {
        assert!(Rgba::WHITE.relative_luminance() > 0.999);
        assert!(Rgba::BLACK.relative_luminance() < 1e-6);
        let mid = Rgba::opaque(128, 128, 128).relative_luminance();
        assert!((mid - 0.2158).abs() < 1e-3, "实测 {mid}");
    }

    #[test]
    fn layer_z_order_is_declaration_order() {
        let layers = [
            Layer::Value,
            Layer::Search,
            Layer::Structure,
            Layer::Analysis,
            Layer::Selection,
            Layer::Bookmark,
            Layer::Diff,
            Layer::Yara,
            Layer::Ai,
            Layer::Cursor,
        ];
        for (expected, layer) in layers.iter().enumerate() {
            assert_eq!(layer.z() as usize, expected);
        }
        assert_eq!(layers[0].z(), 0);
        assert_eq!(layers[9].z(), 9);
    }

    #[test]
    fn no_layers_returns_neutral_fallback() {
        let c = composite_at(0, &[]);
        assert_eq!(c.bg, Rgba::BLACK);
        assert_eq!(c.fg, Rgba::WHITE);
        assert_eq!(c.decoration, Decoration::None);
        assert!(contrast_ratio(c.fg, c.bg) >= MIN_CONTRAST);
    }

    #[test]
    fn highest_single_opaque_layer_becomes_base() {
        let color = Rgba::opaque(0x3b, 0x82, 0xf6);
        let c = composite_at(5, &[hl(Layer::Selection, color, 0, 0, 10)]);
        assert_eq!(c.bg, color);
        assert!(contrast_ratio(c.fg, c.bg) >= MIN_CONTRAST);
    }

    #[test]
    fn transparent_layer_never_becomes_base() {
        let translucent = Rgba::new(0xff, 0x00, 0x00, 100);
        let c = composite_at(0, &[hl(Layer::Search, translucent, 0, 0, 10)]);
        assert_eq!(c.bg, Rgba::BLACK);
    }

    #[test]
    fn lower_layers_darken_without_new_hue() {
        let base_color = Rgba::opaque(0x3b, 0x82, 0xf6);
        let c = composite_at(
            3,
            &[
                hl(Layer::Value, Rgba::opaque(0x60, 0xa5, 0xfa), 0, 0, 10),
                hl(Layer::Structure, Rgba::opaque(0x22, 0xd3, 0xee), 0, 0, 10),
                hl(Layer::Selection, base_color, 0, 0, 10),
            ],
        );
        assert!(
            hue_distance(hue(c.bg), hue(base_color)) < 0.002,
            "底色色相必须保持，实测 {} vs {}",
            hue(c.bg),
            hue(base_color)
        );
        assert!(c.bg.relative_luminance() < base_color.relative_luminance());
    }

    #[test]
    fn darkening_is_capped_at_020() {
        let base_color = hsl_to_rgb(0.6, 0.5, 0.9);
        let layers = vec![
            hl(Layer::Value, Rgba::opaque(1, 2, 3), 0, 0, 10),
            hl(Layer::Search, Rgba::opaque(4, 5, 6), 0, 0, 10),
            hl(Layer::Structure, Rgba::opaque(7, 8, 9), 0, 0, 10),
            hl(Layer::Analysis, Rgba::opaque(10, 11, 12), 0, 0, 10),
            hl(Layer::Selection, base_color, 0, 0, 10),
        ];
        let c = composite_at(0, &layers);
        let l = rgb_to_hsl(c.bg).2;
        assert!((l - 0.7).abs() < 0.01, "压暗应封顶 0.20，实测 L={l}");
        // 不封顶会按层序差一路压到 0.40；封顶后应停在 0.70。
        assert!(l > 0.68);
        // 只叠紧邻的一层（层序差 1）时压暗 0.05，亮度应高于封顶的四层。
        let fewer = composite_at(0, &[layers[3], layers[4]]);
        assert!(fewer.bg.relative_luminance() > c.bg.relative_luminance());
    }

    #[test]
    fn foreground_comes_from_highest_layer_when_it_has_contrast() {
        let base = Rgba::BLACK;
        let yellow = Rgba::new(0xfa, 0xcc, 0x15, 250);
        let c = composite_at(
            0,
            &[
                hl(Layer::Selection, base, 0, 0, 10),
                hl(Layer::Diff, yellow, 0, 0, 10),
            ],
        );
        assert_eq!(c.fg, Rgba::opaque(0xfa, 0xcc, 0x15));
        assert!(contrast_ratio(c.fg, c.bg) >= MIN_CONTRAST);
    }

    #[test]
    fn foreground_is_corrected_when_candidate_fails_contrast() {
        let white = Rgba::WHITE;
        let c = composite_at(
            0,
            &[
                hl(Layer::Selection, white, 0, 0, 10),
                hl(Layer::Cursor, Rgba::new(255, 255, 255, 200), 0, 0, 10),
            ],
        );
        assert_eq!(c.bg, white);
        assert!(contrast_ratio(c.fg, c.bg) >= MIN_CONTRAST - 1e-4);
        assert!(c.fg.relative_luminance() < 0.2, "浅底上必须压到深色");
    }

    #[test]
    fn decorations_map_and_highest_wins() {
        let white = Rgba::WHITE;
        assert_eq!(
            composite_at(0, &[hl(Layer::Search, white, 0, 0, 10)]).decoration,
            Decoration::Underline
        );
        assert_eq!(
            composite_at(0, &[hl(Layer::Yara, white, 0, 0, 10)]).decoration,
            Decoration::LeftBar
        );
        assert_eq!(
            composite_at(0, &[hl(Layer::Diff, white, 0, 0, 10)]).decoration,
            Decoration::TopEdge
        );
        assert_eq!(
            composite_at(0, &[hl(Layer::Ai, white, 0, 0, 10)]).decoration,
            Decoration::Hatch
        );
        let multi = composite_at(
            0,
            &[
                hl(Layer::Search, white, 0, 0, 10),
                hl(Layer::Diff, Rgba::opaque(0xf4, 0x72, 0xb6), 0, 0, 10),
                hl(Layer::Yara, Rgba::opaque(0xef, 0x44, 0x44), 3, 0, 10),
                hl(Layer::Ai, Rgba::opaque(0xc0, 0x84, 0xfc), 0, 0, 10),
            ],
        );
        assert_eq!(multi.decoration, Decoration::Hatch);
    }

    #[test]
    fn same_layer_priority_breaks_ties() {
        let low = Rgba::opaque(0xef, 0x44, 0x44);
        let high = Rgba::opaque(0x3b, 0x82, 0xf6);
        let c = composite_at(
            0,
            &[
                hl(Layer::Selection, low, 1, 0, 10),
                hl(Layer::Selection, high, 9, 0, 10),
            ],
        );
        assert_eq!(c.bg, high);
    }

    #[test]
    fn range_miss_falls_back_to_base() {
        let base = Rgba::opaque(0x11, 0x22, 0x33);
        let c = composite_at_with_base(99, &[hl(Layer::Selection, Rgba::WHITE, 0, 0, 10)], base);
        assert_eq!(c.bg, base);
        assert!(contrast_ratio(c.fg, c.bg) >= MIN_CONTRAST);
    }

    /// 确定性伪随机数（xorshift64*）：不引入额外的 proptest 依赖。
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn color(&mut self) -> Rgba {
            let v = self.next();
            Rgba::new(v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8)
        }
    }

    #[test]
    fn property_contrast_ratio_stays_within_wcag_bounds() {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        for _ in 0..4_000 {
            let a = rng.color();
            let b = rng.color();
            let ratio = contrast_ratio(a, b);
            assert!(ratio.is_finite());
            assert!(ratio >= 1.0 - 1e-4, "对比度不得小于 1: {ratio}");
            assert!(ratio <= 21.0 + 1e-3, "对比度不得大于 21: {ratio}");
            let reverse = contrast_ratio(b, a);
            assert!((ratio - reverse).abs() < 1e-4);
        }
    }

    #[test]
    fn property_composite_foreground_is_always_readable() {
        let mut rng = Rng(0x1234_5678_9ABC_DEF0);
        for _ in 0..2_000 {
            let len = rng.next() % 64 + 1;
            let start = rng.next() % 32;
            let color = rng.color();
            let layer = match rng.next() % 10 {
                0 => Layer::Value,
                1 => Layer::Search,
                2 => Layer::Structure,
                3 => Layer::Analysis,
                4 => Layer::Selection,
                5 => Layer::Bookmark,
                6 => Layer::Diff,
                7 => Layer::Yara,
                8 => Layer::Ai,
                _ => Layer::Cursor,
            };
            let highlight = hl(layer, color, rng.next() as u8, start, start + len);
            let c = composite_at(start, &[highlight]);
            assert!(
                contrast_ratio(c.fg, c.bg) >= MIN_CONTRAST - 1e-3,
                "合成后必须可读：fg={:?} bg={:?}",
                c.fg,
                c.bg
            );
        }
    }
}
