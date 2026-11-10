//! hex 网格的几何、行文本格式化与图元生成。
//!
//! DESIGN: 05-render-and-hexview.md §3.3、26-visual-design.md §3.2

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};

/// 字节显示进制。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Radix {
    /// 十六进制：每字节 2 个字符（`00`..`FF`）。
    #[default]
    Hex,
    /// 十进制：每字节 3 个字符，右对齐（`  0`..`255`）。
    Dec,
    /// 八进制：每字节 3 个字符，右对齐（`  0`..`377`）。
    Oct,
}

impl Radix {
    /// 一个字节在本进制下实际使用的字符数（不含列内补位）。
    #[inline]
    pub const fn text_width(self) -> usize {
        match self {
            Self::Hex => 2,
            Self::Dec | Self::Oct => 3,
        }
    }
}

/// 每个字节占用的固定字符列数。
///
/// 三种进制都取 3：Hex 是 2 个数字 + 1 个补位空格，Dec/Oct 恰好 3 位。
/// 切换进制时列位置不变，文字不会左右跳动。
pub const BYTE_COL_W: usize = 3;

/// 单行格式化参数（每行字节数、分组宽度、进制）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowFormat {
    /// 每行字节数；0 表示不输出任何内容。
    pub bpr: usize,
    /// 分组字节数；0 表示不插入组间空格。
    pub group: usize,
    /// 字节显示进制。
    pub radix: Radix,
}

impl RowFormat {
    /// 构造一个十六进制的行格式。
    #[inline]
    pub const fn new(bpr: usize, group: usize) -> Self {
        Self {
            bpr,
            group,
            radix: Radix::Hex,
        }
    }

    /// 替换显示进制。
    #[inline]
    pub const fn with_radix(mut self, radix: Radix) -> Self {
        self.radix = radix;
        self
    }
}

impl Default for RowFormat {
    fn default() -> Self {
        Self::new(16, 1)
    }
}

/// 把一行字节格式化成（hex/dec/oct 列，ASCII 列）。
///
/// 每个字节固定占 [`BYTE_COL_W`] 个字符：Hex 左对齐 2 位再补 1 个空格，
/// Dec/Oct 右对齐 3 位；每组之间额外插入 1 个空格。短行按空槽补齐到 `bpr`，
/// 因此任意一行的文本列宽都相同，ASCII 列不会随最后一行左移。
///
/// ASCII 列不补位：可打印 ASCII（0x20..=0x7E）原样输出，其余字节显示为 `.`。
pub fn format_row(bytes: &[u8], fmt: &RowFormat) -> (String, String) {
    let bpr = fmt.bpr;
    if bpr == 0 {
        return (String::new(), String::new());
    }

    let groups = if fmt.group == 0 {
        1
    } else {
        bpr.div_ceil(fmt.group)
    };
    let capacity = bpr
        .saturating_mul(BYTE_COL_W)
        .saturating_add(groups.saturating_sub(1));
    let mut hex = String::with_capacity(capacity);
    let mut ascii = String::with_capacity(bpr);

    for i in 0..bpr {
        if i > 0 && fmt.group > 0 && i % fmt.group == 0 {
            hex.push(' ');
        }
        match bytes.get(i) {
            Some(&b) => {
                push_byte(&mut hex, b, fmt.radix);
                ascii.push(if (0x20..=0x7e).contains(&b) {
                    char::from(b)
                } else {
                    '.'
                });
            }
            // 空槽与 Hex 的 2 位 + 1 个补位空格等宽，保证整行宽度不变。
            None => hex.push_str("   "),
        }
    }
    (hex, ascii)
}

/// 把单个字节按进制写入定宽 3 字符的槽位。
fn push_byte(out: &mut String, b: u8, radix: Radix) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    match radix {
        Radix::Hex => {
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
            out.push(' ');
        }
        Radix::Dec => {
            if b >= 100 {
                out.push(char::from(b'0' + b / 100));
            } else {
                out.push(' ');
            }
            if b >= 10 {
                out.push(char::from(b'0' + (b / 10) % 10));
            } else {
                out.push(' ');
            }
            out.push(char::from(b'0' + b % 10));
        }
        Radix::Oct => {
            if b >= 64 {
                out.push(char::from(b'0' + b / 64));
            } else {
                out.push(' ');
            }
            if b >= 8 {
                out.push(char::from(b'0' + (b / 8) % 8));
            } else {
                out.push(' ');
            }
            out.push(char::from(b'0' + b % 8));
        }
    }
}

/// 一个单元格的度量。全部为逻辑像素。
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    /// 单元格宽（一个 hex 字节 = 2 个 nibble 的字宽）。
    pub cell_w: f32,
    /// 行高。
    pub cell_h: f32,
    /// 网格左上角。
    pub origin: Pos2,
    /// 每组字节之间的额外间距。
    pub group_gap: f32,
    /// 每组字节数（1/2/4/8）。
    pub group: usize,
}

impl CellMetrics {
    /// 第 `col` 列、`row` 行的格子左上角。
    ///
    /// **乘法而非累加**：`origin.x + col * cell_w` 在 1e8 列后仍只有一次舍入，
    /// 而 `x += cell_w` 会累积误差（05 §3.3）。
    #[inline]
    pub fn pos(&self, col: usize, row: usize) -> Pos2 {
        let group_offset = (col / self.group.max(1)) as f32 * self.group_gap;
        Pos2::new(
            self.origin.x + col as f32 * self.cell_w + group_offset,
            self.origin.y + row as f32 * self.cell_h,
        )
    }

    /// 第 `col` 列、`row` 行的格子矩形。
    #[inline]
    pub fn rect(&self, col: usize, row: usize) -> Rect {
        Rect::from_min_size(self.pos(col, row), Vec2::new(self.cell_w, self.cell_h))
    }
}

/// 画 `cols × rows` 个格子，返回生成的图元数。
///
/// 每格 1 个填充 quad + 1 条 1px 行线 = 2 个图元；真实渲染会把 hex 文本塞进一张
/// 字形图集再出 quad，数量级与此一致（05 §4.3）。
pub fn paint_cells(painter: &Painter, metrics: &CellMetrics, cols: usize, rows: usize) -> usize {
    let even = Color32::from_rgb(0xC8, 0xD0, 0xD8);
    let odd = Color32::from_rgb(0x9A, 0xA4, 0xB0);
    let line = Color32::from_rgb(0x22, 0x28, 0x2E);
    let mut primitives = 0;
    for row in 0..rows {
        for col in 0..cols {
            let r = metrics.rect(col, row);
            let fg = if col % 2 == 0 { even } else { odd };
            painter.rect_filled(r, 1.0, fg);
            painter.hline(r.x_range(), r.bottom(), Stroke::new(1.0_f32, line));
            primitives += 2;
        }
    }
    primitives
}

/// 画 `cols × rows` 个格子，但每格走 **文本** 路径（`Painter::text`，会查字形图集）。
///
/// 这是 S3 的第二问：图元数量相同，但文本要过 galley 布局与图集查找。
/// 真实 hex 视图的 hex 列走"预排版 22 个符号 + 直接出 quad"，比这条路径更便宜；
/// 这里测的是**上界**。
pub fn paint_text_cells(
    painter: &Painter,
    metrics: &CellMetrics,
    cols: usize,
    rows: usize,
) -> usize {
    let color = Color32::from_rgb(0xC8, 0xD0, 0xD8);
    let font = FontId::monospace(12.0);
    let mut primitives = 0;
    for row in 0..rows {
        for col in 0..cols {
            let p = metrics.pos(col, row);
            painter.text(p, Align2::LEFT_TOP, "AF", font.clone(), color);
            primitives += 1;
        }
    }
    primitives
}

#[cfg(test)]
mod tests {
    use super::{BYTE_COL_W, Radix, RowFormat, format_row};

    fn row(bpr: usize, group: usize, radix: Radix) -> RowFormat {
        RowFormat { bpr, group, radix }
    }

    #[test]
    fn hex_uses_two_digits_plus_one_pad() {
        let (hex, ascii) = format_row(&[0x41, 0x42, 0x00, 0x7F], &row(8, 2, Radix::Hex));
        assert!(hex.starts_with("41 42  00 7F "), "实际 {hex:?}");
        assert_eq!(ascii, "AB..");
        assert_eq!(hex.chars().count(), 8 * BYTE_COL_W + 3, "4 组 3 个缝隙");
    }

    #[test]
    fn hex_is_uppercase() {
        let (hex, _) = format_row(&[0xab, 0xcd], &row(2, 0, Radix::Hex));
        assert_eq!(hex, "AB CD ");
    }

    #[test]
    fn decimal_is_three_wide_right_aligned() {
        let (hex, _) = format_row(&[0, 5, 42, 255], &row(4, 0, Radix::Dec));
        assert_eq!(hex.chars().count(), 12);
        let cells: Vec<&str> = (0..4).map(|i| &hex[i * 3..i * 3 + 3]).collect();
        assert_eq!(cells, ["  0", "  5", " 42", "255"]);
    }

    #[test]
    fn octal_is_three_wide_right_aligned() {
        let (hex, _) = format_row(&[0, 8, 64, 255], &row(4, 0, Radix::Oct));
        let cells: Vec<&str> = (0..4).map(|i| &hex[i * 3..i * 3 + 3]).collect();
        assert_eq!(cells, ["  0", " 10", "100", "377"]);
    }

    #[test]
    fn radix_switch_keeps_column_width() {
        let data: Vec<u8> = (0..16).map(|i| (i * 17) as u8).collect();
        let hex = format_row(&data, &row(16, 2, Radix::Hex)).0;
        let dec = format_row(&data, &row(16, 2, Radix::Dec)).0;
        let oct = format_row(&data, &row(16, 2, Radix::Oct)).0;
        assert_eq!(hex.chars().count(), 16 * BYTE_COL_W + 7);
        assert_eq!(dec.chars().count(), hex.chars().count());
        assert_eq!(oct.chars().count(), hex.chars().count());
    }

    #[test]
    fn group_boundary_inserts_exactly_one_extra_space() {
        let data = [0u8; 8];
        let ungrouped = format_row(&data, &row(8, 0, Radix::Hex)).0;
        let grouped = format_row(&data, &row(8, 4, Radix::Hex)).0;
        assert_eq!(grouped.chars().count(), ungrouped.chars().count() + 1);
        // 第 4 个字节前应有额外的组间空格。
        assert_eq!(grouped.chars().nth(12), Some(' '));
        assert_eq!(grouped.chars().nth(13), Some('0'));
    }

    #[test]
    fn group_one_separates_every_byte() {
        let (hex, _) = format_row(&[0x00, 0x11, 0x22, 0x33], &row(4, 1, Radix::Hex));
        assert_eq!(hex, "00  11  22  33 ");
        assert_eq!(hex.chars().count(), 4 * BYTE_COL_W + 3);
    }

    #[test]
    fn short_row_pads_to_full_width() {
        let full = [0u8; 16];
        let short = [1u8, 2, 3];
        for radix in [Radix::Hex, Radix::Dec, Radix::Oct] {
            let (a, _) = format_row(&full, &row(16, 4, radix));
            let (b, ascii) = format_row(&short, &row(16, 4, radix));
            assert_eq!(a.chars().count(), b.chars().count(), "{radix:?} 宽度漂移");
            assert_eq!(ascii.chars().count(), 3);
        }
    }

    #[test]
    fn empty_row_is_all_padding() {
        let (hex, ascii) = format_row(&[], &row(4, 1, Radix::Hex));
        assert_eq!(ascii, "");
        assert_eq!(hex.chars().count(), 4 * BYTE_COL_W + 3);
        assert!(hex.trim().is_empty());
    }

    #[test]
    fn zero_bpr_returns_empty_strings() {
        let (hex, ascii) = format_row(&[1, 2, 3], &row(0, 2, Radix::Dec));
        assert!(hex.is_empty());
        assert!(ascii.is_empty());
    }

    #[test]
    fn group_zero_disables_separators() {
        let (hex, _) = format_row(&[1, 2, 3, 4], &row(4, 0, Radix::Hex));
        assert_eq!(hex.chars().count(), 4 * BYTE_COL_W);
        assert_eq!(hex, "01 02 03 04 ");
    }

    #[test]
    fn extra_bytes_are_ignored() {
        let (hex, ascii) = format_row(&[0x41, 0x42, 0x43, 0x44], &row(2, 1, Radix::Hex));
        assert!(hex.starts_with("41  42 "), "实际 {hex:?}");
        assert_eq!(ascii, "AB");
    }

    #[test]
    fn ascii_keeps_printable_and_dots_the_rest() {
        let (_, ascii) = format_row(&[0x1f, 0x20, 0x7e, 0x7f], &row(4, 0, Radix::Hex));
        assert_eq!(ascii, ". ~.");
    }
}
