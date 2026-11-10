//! 比例尺滚动模型（M2 T2.2）。
//!
//! DESIGN: 05-render-and-hexview.md §4.2（虚拟滚动与滚动条精度）
//!
//! 本模块是**纯数学**，刻意不依赖 `egui`：行号始终是 `u64`，比例计算用 `f64`，
//! 只有 [`ScrollModel::thumb`] 返回几何量时才转成 `f32`。这样可以单元测试，
//! 也避免 `egui::ScrollArea` 的 f32 像素累加在 1.6e7 px 后失效的问题。

/// 滚动条 thumb 的最小高度（逻辑像素）。
///
/// 05 §4.2 写的是 20px，视觉规范（26 §4.1 的 4dp 栅格）取 24px；这里采用 24px，
/// 与既有窗口实现保持一致，也满足最小指针目标。
pub const MIN_THUMB_H: f32 = 24.0;

/// 比例尺滚动状态：顶部行号 + 总行数 + 可见行数。
///
/// `top_row` 的合法范围是 `0..=total_rows.saturating_sub(1)`；保留"最后一行也能
/// 滚到视口顶部"的语义（与旧窗口实现一致）。`visible_rows` 只参与 thumb 高度，
/// 不改变滚动范围。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollModel {
    /// 顶部可见行号（0 起）。
    pub top_row: u64,
    /// 总行数；0 表示空文档。
    pub total_rows: u64,
    /// 视口内可见行数，用于计算 thumb 高度。
    pub visible_rows: u64,
}

impl ScrollModel {
    /// 从顶部开始构造滚动模型。
    #[inline]
    pub const fn new(total_rows: u64, visible_rows: u64) -> Self {
        Self {
            top_row: 0,
            total_rows,
            visible_rows,
        }
    }

    /// 允许的最大顶部行号：`total_rows - 1`（空文档为 0）。
    #[inline]
    pub const fn max_top(&self) -> u64 {
        self.total_rows.saturating_sub(1)
    }

    /// 按行滚动；正数向下、负数向上，越界自动夹紧。
    ///
    /// 用 `i64::unsigned_abs` 拆成正负两支，`i64::MIN` 与 `u64::MAX` 都不会溢出。
    pub fn scroll_by(&mut self, delta: i64) {
        if delta >= 0 {
            self.top_row = self
                .top_row
                .saturating_add(delta as u64)
                .min(self.max_top());
        } else {
            self.top_row = self
                .top_row
                .saturating_sub(delta.unsigned_abs())
                .min(self.max_top());
        }
    }

    /// 按比例设置顶部行号（滚动条点击/拖拽）。
    ///
    /// `fraction` 先夹到 0..=1；`NaN` 视为 0。行号 = `round(fraction * total_rows)`，
    /// 再夹到 [`ScrollModel::max_top`]，与 05 §4.2 的 `row = round(progress * total)`
    /// 一致。
    pub fn set_fraction(&mut self, fraction: f64) {
        let fraction = if fraction.is_nan() {
            0.0
        } else {
            fraction.clamp(0.0, 1.0)
        };
        let target = (fraction * self.total_rows as f64).round();
        // `f64 as u64` 在 Rust 中对超界值饱和，随后再夹到 max_top。
        self.top_row = (target as u64).min(self.max_top());
    }

    /// 当前顶部行号占总行数的比例（05 §4.2：`row / total_rows`）。
    ///
    /// 空文档返回 0；`top_row` 被外部写越界时按 `max_top` 截断。
    pub fn fraction(&self) -> f64 {
        if self.total_rows == 0 {
            return 0.0;
        }
        self.top_row.min(self.max_top()) as f64 / self.total_rows as f64
    }

    /// 计算 thumb 的 `(y 偏移, 高度)`，均相对轨道顶部，单位逻辑像素。
    ///
    /// 高度 = `track_h * visible_rows / total_rows`，不小于 [`MIN_THUMB_H`]，
    /// 也不超过轨道高度；`visible_rows >= total_rows`（或空文档）时铺满轨道。
    /// 所有行号运算保持整数，比例用 f64 一次算完，最后才转 f32。
    pub fn thumb(&self, track_h: f32) -> (f32, f32) {
        let track_h = if track_h.is_finite() {
            track_h.max(0.0)
        } else {
            0.0
        };
        if track_h <= 0.0 {
            return (0.0, 0.0);
        }

        let thumb_h = if self.total_rows == 0 || self.visible_rows >= self.total_rows {
            track_h
        } else {
            let h = (f64::from(track_h) * self.visible_rows as f64 / self.total_rows as f64) as f32;
            h.max(MIN_THUMB_H).min(track_h)
        };
        let y = ((f64::from(track_h) - f64::from(thumb_h)) * self.fraction()) as f32;
        (y, thumb_h)
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_THUMB_H, ScrollModel};

    fn model(total: u64, visible: u64) -> ScrollModel {
        ScrollModel::new(total, visible)
    }

    #[test]
    fn starts_at_top() {
        let m = model(1000, 60);
        assert_eq!(m.top_row, 0);
        assert_eq!(m.fraction(), 0.0);
    }

    #[test]
    fn scroll_by_clamps_both_ends() {
        let mut m = model(100, 10);
        m.scroll_by(-5);
        assert_eq!(m.top_row, 0);
        m.scroll_by(1000);
        assert_eq!(m.top_row, 99);
        m.scroll_by(-1000);
        assert_eq!(m.top_row, 0);
    }

    #[test]
    fn scroll_by_handles_i64_extremes() {
        let mut m = model(100, 10);
        m.top_row = 50;
        m.scroll_by(i64::MAX);
        assert_eq!(m.top_row, 99);
        m.scroll_by(i64::MIN);
        assert_eq!(m.top_row, 0);
    }

    #[test]
    fn fraction_is_row_over_total() {
        let mut m = model(100, 10);
        m.top_row = 50;
        assert!((m.fraction() - 0.5).abs() < 1e-12);
        m.top_row = 99;
        assert!((m.fraction() - 0.99).abs() < 1e-12);
    }

    #[test]
    fn fraction_round_trip() {
        let mut m = model(100, 10);
        for top in [0u64, 1, 37, 99] {
            m.top_row = top;
            let frac = m.fraction();
            let mut n = model(100, 10);
            n.set_fraction(frac);
            assert_eq!(n.top_row, top, "比例往返失败：{top}");
        }
    }

    #[test]
    fn set_fraction_clamps_and_handles_nan() {
        let mut m = model(100, 10);
        m.set_fraction(-1.0);
        assert_eq!(m.top_row, 0);
        m.set_fraction(2.0);
        assert_eq!(m.top_row, 99);
        m.set_fraction(0.25);
        assert_eq!(m.top_row, 25);
        m.set_fraction(f64::NAN);
        assert_eq!(m.top_row, 0);
        m.set_fraction(f64::INFINITY);
        assert_eq!(m.top_row, 99);
    }

    #[test]
    fn thumb_has_minimum_height() {
        let m = model(1_000_000, 10);
        let (y, h) = m.thumb(400.0);
        assert!((h - MIN_THUMB_H).abs() < f32::EPSILON);
        assert!((y - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn thumb_reaches_track_bottom_at_last_row() {
        let mut m = model(1_000_000, 10);
        m.top_row = m.max_top();
        let track = 400.0;
        let (y, h) = m.thumb(track);
        assert!((y + h - track).abs() < 0.01, "y={y} h={h}");
    }

    #[test]
    fn thumb_fills_track_when_all_rows_visible() {
        let (y, h) = model(10, 20).thumb(400.0);
        assert_eq!((y, h), (0.0, 400.0));
    }

    #[test]
    fn total_zero_is_a_valid_empty_document() {
        let mut m = model(0, 60);
        assert_eq!(m.max_top(), 0);
        assert_eq!(m.fraction(), 0.0);
        m.scroll_by(100);
        assert_eq!(m.top_row, 0);
        m.set_fraction(0.7);
        assert_eq!(m.top_row, 0);
        assert_eq!(m.thumb(400.0), (0.0, 400.0));
    }

    #[test]
    fn total_one_never_scrolls() {
        let mut m = model(1, 60);
        assert_eq!(m.max_top(), 0);
        m.scroll_by(10);
        assert_eq!(m.top_row, 0);
        m.set_fraction(1.0);
        assert_eq!(m.top_row, 0);
        assert_eq!(m.thumb(400.0), (0.0, 400.0));
    }

    #[test]
    fn u64_max_does_not_overflow() {
        let mut m = model(u64::MAX, 60);
        m.scroll_by(i64::MAX);
        assert_eq!(m.top_row, i64::MAX as u64);
        m.scroll_by(i64::MAX);
        assert_eq!(m.top_row, u64::MAX - 1);
        m.scroll_by(i64::MIN);
        assert_eq!(m.top_row, u64::MAX - 1 - (1u64 << 63));
        m.scroll_by(i64::MIN);
        assert_eq!(m.top_row, 0);
        m.set_fraction(1.0);
        assert_eq!(m.top_row, u64::MAX - 1);

        let (y, h) = m.thumb(1000.0);
        assert!(y.is_finite() && h.is_finite());
        assert!((MIN_THUMB_H..=1000.0).contains(&h));
        assert!(y >= 0.0 && y + h <= 1000.01);
    }

    #[test]
    fn zero_or_negative_track_returns_zero_geometry() {
        let m = model(100, 10);
        assert_eq!(m.thumb(0.0), (0.0, 0.0));
        assert_eq!(m.thumb(-10.0), (0.0, 0.0));
        assert_eq!(m.thumb(f32::NAN), (0.0, 0.0));
    }
}
