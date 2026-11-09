//! hex 网格的几何与图元生成。
//!
//! DESIGN: 05-render-and-hexview.md §3.3、26-visual-design.md §3.2

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};

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
