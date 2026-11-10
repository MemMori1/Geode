//! 窗口骨架：eframe 窗口 + 自绘比例尺滚动条 + 网格渲染（M2 T2.1/T2.2）。
//!
//! DESIGN: 05-render-and-hexview.md §3.3（几何）、§4.1~§4.4（分层与滚动）、
//! 16-ui-shell-theme-i18n.md §3.1（布局）、26-visual-design.md §4（色与字阶）。
//!
//! 这一版刻意**不用** `egui::ScrollArea`：它内部用 f32 累加偏移，行数到 1e7 量级
//! 后精度耗尽、拖动会"粘住"。这里把滚动位置存成 `top_row: u64`，滚动条按
//! "比例尺"画：thumb 长度 = 可见行 / 总行，位置 = top_row / 总行。

use eframe::egui;
use geode_buffer::Buffer;
use geode_core::Offset;
use geode_provider::FileProvider;
use geode_provider::page_cache::CachedProvider;

const ROW_PAD: f32 = 2.0;
const SCROLLBAR_W: f32 = 12.0;
const MIN_THUMB_H: f32 = 24.0;

/// 视图参数：每行字节数与分组宽度。
#[derive(Debug, Clone, Copy)]
struct ViewParams {
    bytes_per_row: u32,
    group: u32,
}

impl Default for ViewParams {
    fn default() -> Self {
        Self {
            bytes_per_row: 16,
            group: 2,
        }
    }
}

struct HexApp {
    buffer: Buffer,
    params: ViewParams,
    /// 顶部可见行号（u64，绝不转 f32 存储）。
    top_row: u64,
    /// 游标字节偏移。
    cursor: u64,
    path: String,
}

/// 把一行字节格式化成 hex 列与 ASCII 列（纯函数，便于单测）。
///
/// 短行补空格到满行，保证后面列不会因为最后一行短而左移。
fn format_row(bytes: &[u8], bytes_per_row: usize, group: usize) -> (String, String) {
    let mut hex = String::with_capacity(bytes_per_row * 3 + bytes_per_row / group.max(1));
    let mut ascii = String::with_capacity(bytes_per_row);
    for i in 0..bytes_per_row {
        if i > 0 && group > 0 && i % group == 0 {
            hex.push(' ');
        }
        match bytes.get(i) {
            Some(b) => {
                hex.push_str(&format!("{b:02X}"));
                ascii.push(if (0x20..=0x7e).contains(b) {
                    *b as char
                } else {
                    '.'
                });
            }
            None => hex.push_str("  "),
        }
    }
    (hex, ascii)
}

impl HexApp {
    fn new(path: &std::path::Path) -> Result<Self, String> {
        let provider = CachedProvider::new(FileProvider::open(path).map_err(|e| e.to_string())?);
        let buffer = Buffer::new(Box::new(provider));
        Ok(Self {
            buffer,
            params: ViewParams::default(),
            top_row: 0,
            cursor: 0,
            path: path.display().to_string(),
        })
    }

    fn total_rows(&self) -> u64 {
        let bpr = u64::from(self.params.bytes_per_row);
        self.buffer.len().div_ceil(bpr).max(1)
    }

    fn row_text(&self, row: u64) -> (String, String) {
        let bpr = u64::from(self.params.bytes_per_row);
        let at = row * bpr;
        let want = bpr.min(self.buffer.len().saturating_sub(at)) as usize;
        let mut bytes = vec![0u8; want];
        if want > 0 {
            let _ = self.buffer.read(Offset::new(at), &mut bytes);
        }
        format_row(&bytes, bpr as usize, self.params.group as usize)
    }
}

impl eframe::App for HexApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let total_rows = self.total_rows();
        let bpr = self.params.bytes_per_row;
        let cell_h = ctx.fonts(|f| f.row_height(&egui::FontId::monospace(13.0))) + ROW_PAD;
        let visible_rows = ((ctx.screen_rect().height() - 96.0) / cell_h)
            .floor()
            .max(1.0) as u64;

        // ---- 键盘与滚轮（先处理输入，再画）----
        let mut delta_rows = 0i64;
        ctx.input(|i| {
            for ev in &i.events {
                match ev {
                    egui::Event::MouseWheel { delta, .. } => {
                        delta_rows -= (delta.y / cell_h).round() as i64;
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => match key {
                        egui::Key::ArrowDown => delta_rows += 1,
                        egui::Key::ArrowUp => delta_rows -= 1,
                        egui::Key::PageDown => delta_rows += visible_rows as i64,
                        egui::Key::PageUp => delta_rows -= visible_rows as i64,
                        egui::Key::Home => delta_rows = -(total_rows as i64),
                        egui::Key::End => delta_rows = total_rows as i64,
                        egui::Key::ArrowRight => {
                            self.cursor =
                                (self.cursor + 1).min(self.buffer.len().saturating_sub(1));
                        }
                        egui::Key::ArrowLeft => {
                            self.cursor = self.cursor.saturating_sub(1);
                        }
                        _ => {
                            let _ = modifiers;
                        }
                    },
                    _ => {}
                }
            }
        });
        let new_top = (self.top_row as i64 + delta_rows).clamp(0, total_rows as i64) as u64;
        self.top_row = new_top.min(total_rows.saturating_sub(1));

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Geode").strong());
                ui.separator();
                ui.label(&self.path);
                ui.separator();
                ui.label("每行");
                for bpr_opt in [8u32, 16, 32, 64] {
                    if ui
                        .selectable_label(self.params.bytes_per_row == bpr_opt, bpr_opt.to_string())
                        .clicked()
                    {
                        self.params.bytes_per_row = bpr_opt;
                    }
                }
                ui.separator();
                ui.label("分组");
                for g in [1u32, 2, 4, 8] {
                    if ui
                        .selectable_label(self.params.group == g, g.to_string())
                        .clicked()
                    {
                        self.params.group = g;
                    }
                }
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("偏移 0x{:X}", self.cursor));
                ui.separator();
                ui.label(format!("长度 {}", self.buffer.len()));
                ui.separator();
                ui.label(format!("行 {}/{}", self.top_row + 1, total_rows));
                ui.separator();
                ui.label(format!("bpr {} / group {}", bpr, self.params.group));
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            let painter = ui.painter_at(rect);
            let mono = egui::FontId::monospace(13.0);
            let char_w = ctx.fonts(|f| f.glyph_width(&mono, '0'));
            let row_h = cell_h;

            // 偏移列宽：10 个 hex 位 + 2 空格
            let offset_w = char_w * 10.0 + char_w;
            let ascii_x = rect.left() + offset_w + char_w * (f32::from(bpr as u16) * 3.0 + 4.0);

            let first = self.top_row;
            let last = (self.top_row + visible_rows + 1).min(total_rows);
            for row in first..last {
                let y = rect.top() + (row - first) as f32 * row_h;
                let (hex, ascii) = self.row_text(row);
                painter.text(
                    egui::pos2(rect.left(), y),
                    egui::Align2::LEFT_TOP,
                    format!("{:010X}", row * u64::from(bpr)),
                    mono.clone(),
                    ui.visuals().weak_text_color(),
                );
                painter.text(
                    egui::pos2(rect.left() + offset_w, y),
                    egui::Align2::LEFT_TOP,
                    hex,
                    mono.clone(),
                    ui.visuals().text_color(),
                );
                painter.text(
                    egui::pos2(ascii_x, y),
                    egui::Align2::LEFT_TOP,
                    ascii,
                    mono.clone(),
                    ui.visuals().weak_text_color(),
                );
            }

            // 游标高亮（色块 + 下划线，不叠 alpha）
            let cur_row = self.cursor / u64::from(bpr);
            if cur_row >= first && cur_row < last {
                let col = (self.cursor % u64::from(bpr)) as f32;
                let group_gap = (col / self.params.group as f32).floor() * char_w;
                let x = rect.left() + offset_w + col * char_w * 3.0 + group_gap;
                let y = rect.top() + (cur_row - first) as f32 * row_h;
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(char_w * 2.0, row_h)),
                    2.0,
                    ui.visuals().selection.bg_fill,
                );
                painter.text(
                    egui::pos2(x, y),
                    egui::Align2::LEFT_TOP,
                    format!("{:02X}", self.byte_at(self.cursor)),
                    mono.clone(),
                    ui.visuals().strong_text_color(),
                );
            }

            // ---- 比例尺滚动条 ----
            let track = egui::Rect::from_min_max(
                egui::pos2(rect.right() - SCROLLBAR_W, rect.top()),
                egui::pos2(rect.right(), rect.bottom()),
            );
            let resp = ui.interact(
                track,
                ui.id().with("scrollbar"),
                egui::Sense::click_and_drag(),
            );
            let frac = self.top_row as f32 / total_rows as f32;
            let visible_frac = (visible_rows as f32 / total_rows as f32).min(1.0);
            let thumb_h = (track.height() * visible_frac).max(MIN_THUMB_H);
            let thumb_y = track.top() + (track.height() - thumb_h) * frac;
            let thumb = egui::Rect::from_min_size(
                egui::pos2(track.left() + 2.0, thumb_y),
                egui::vec2(SCROLLBAR_W - 4.0, thumb_h),
            );
            painter.rect_filled(thumb, 3.0, ui.visuals().widgets.active.bg_fill);
            if resp.dragged() || resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let t = ((p.y - track.top()) / track.height()).clamp(0.0, 1.0);
                    self.top_row = (t as f64 * total_rows as f64) as u64;
                    self.top_row = self.top_row.min(total_rows.saturating_sub(1));
                }
            }

            // 点击设置游标
            if ui.input(|i| i.pointer.primary_clicked()) {
                if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                    if rect.contains(p) && p.x < ascii_x {
                        let row = first + ((p.y - rect.top()) / row_h).floor().max(0.0) as u64;
                        let rel = (p.x - rect.left() - offset_w).max(0.0);
                        let col = (rel / (char_w * 3.0)).floor().max(0.0) as u64;
                        let at = row * u64::from(bpr) + col.min(u64::from(bpr) - 1);
                        self.cursor = at.min(self.buffer.len().saturating_sub(1));
                    }
                }
            }
        });

        ctx.request_repaint();
    }
}

impl HexApp {
    fn byte_at(&self, at: u64) -> u8 {
        let mut b = [0u8; 1];
        let _ = self.buffer.read(Offset::new(at), &mut b);
        b[0]
    }
}

/// 打开一个文件并运行窗口（阻塞到用户关闭）。
pub fn run(path: &std::path::Path) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Geode"),
        ..Default::default()
    };
    eframe::run_native(
        "Geode",
        options,
        Box::new(move |_cc| Ok(Box::new(HexApp::new(path)?))),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::format_row;

    #[test]
    fn row_hex_groups_and_ascii() {
        let (hex, ascii) = format_row(&[0x41, 0x42, 0x00, 0x7f], 8, 2);
        assert!(hex.starts_with("4142 007F"), "实际 {hex:?}");
        assert_eq!(ascii, "AB..");
        // 短行必须补齐到与满行一样宽：列宽不随最后一行变化
        let (full, _) = format_row(&[0u8; 8], 8, 2);
        assert_eq!(hex.chars().count(), full.chars().count());
    }

    #[test]
    fn empty_row_is_all_padding() {
        let (hex, ascii) = format_row(&[], 4, 1);
        assert_eq!(ascii, "");
        let (full, _) = format_row(&[0u8; 4], 4, 1);
        assert_eq!(hex.chars().count(), full.chars().count());
        assert!(hex.trim().is_empty());
    }
}
