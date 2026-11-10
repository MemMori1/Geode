//! 光标与选区模型（M2 T2.3）。
//!
//! DESIGN: 05-render-and-hexview.md §4.5（§3.4 在现行文档中是行缓冲）、25 §4
//!
//! 本模块是**纯逻辑**：不依赖 `egui`，不持有缓冲，不读数据。所有坐标都是
//! `u64`/`u32`，任意运算都走 checked / saturating 路径，32 位平台也不会因
//! `usize` 截断而溢出（05 §4.1）。
//!
//! 坐标约定：
//! - [`Pos::row`] 是**字节行号**，[`Pos::col`] 是行内**字节列**（0 起）；
//! - 线性选区是半开区间 `[min, max)`——`anchor == focus` 即空选区；
//! - 矩形选区按行列**闭区间**解释，但 `anchor == focus` 同样视为空（零面积）。

use geode_core::{ByteRange, Offset};

/// 网格中的字节位置：行号 + 行内列号。
///
/// 派生 `Ord` 的字段顺序保证 `(row, col)` 字典序 == 固定 `bytes_per_row` 下的
/// 字节偏移序，因此线性选区不需要额外参数就能判断方向。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pos {
    /// 字节行号（`offset / bytes_per_row`）。
    pub row: u64,
    /// 行内字节列（`offset % bytes_per_row`），0 起。
    pub col: u32,
}

impl Pos {
    /// 构造一个位置。
    #[inline]
    pub const fn new(row: u64, col: u32) -> Self {
        Self { row, col }
    }

    /// 把全局字节偏移换算成位置；`bytes_per_row == 0` 时退化为 `(0, 0)`。
    #[inline]
    pub const fn from_offset(offset: u64, bytes_per_row: u32) -> Self {
        if bytes_per_row == 0 {
            return Self { row: 0, col: 0 };
        }
        let bpr = bytes_per_row as u64;
        Self {
            row: offset / bpr,
            col: (offset % bpr) as u32,
        }
    }

    /// 换算成全局字节偏移；乘法溢出返回 `None`。
    ///
    /// `col` 不做夹紧：调用方若持有越界列，应先用
    /// [`Pos::offset_clamped`] 或自行钳制。
    #[inline]
    pub fn offset(self, bytes_per_row: u32) -> Option<u64> {
        if bytes_per_row == 0 {
            return None;
        }
        self.row
            .checked_mul(bytes_per_row as u64)?
            .checked_add(self.col as u64)
    }

    /// 换算成全局字节偏移，并把 `col` 夹到 `0..bytes_per_row`。
    ///
    /// 用于容错：命中测试 / 反序列化可能送来越界列，夹紧后语义仍落在本行内。
    #[inline]
    pub fn offset_clamped(self, bytes_per_row: u32) -> Option<u64> {
        if bytes_per_row == 0 {
            return None;
        }
        let col = self.col.min(bytes_per_row - 1);
        self.row
            .checked_mul(bytes_per_row as u64)?
            .checked_add(col as u64)
    }
}

/// 选区形状。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SelectionKind {
    /// 线性选区：从 anchor 到 focus 的连续字节区间。
    #[default]
    Linear,
    /// 矩形（块）选区：行列范围内的所有字节，可跨行切成多段。
    Rect,
}

/// 锚点—焦点选区，保留拖拽方向。
///
/// `anchor` 是按下鼠标时的固定端，`focus` 是跟随指针的移动端；复制 / 高亮时
/// 一律用 [`Selection::normalized`] 取无序端点（05 §4.5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Selection {
    /// 固定端。
    pub anchor: Pos,
    /// 移动端。
    pub focus: Pos,
    /// 选区形状。
    pub kind: SelectionKind,
}

impl Selection {
    /// 构造选区；不校验顺序，方向由 `anchor`/`focus` 的先后自然表达。
    #[inline]
    pub const fn new(anchor: Pos, focus: Pos, kind: SelectionKind) -> Self {
        Self {
            anchor,
            focus,
            kind,
        }
    }

    /// 返回无序端点 `(较前, 较后)`。
    ///
    /// - 线性：按字节偏移序（等价于 `(row, col)` 字典序）；
    /// - 矩形：`(min_row, min_col)` 与 `(max_row, max_col)`，与拖拽方向无关。
    #[inline]
    pub fn normalized(&self) -> (Pos, Pos) {
        match self.kind {
            SelectionKind::Linear => {
                if self.anchor <= self.focus {
                    (self.anchor, self.focus)
                } else {
                    (self.focus, self.anchor)
                }
            }
            SelectionKind::Rect => {
                let row0 = self.anchor.row.min(self.focus.row);
                let row1 = self.anchor.row.max(self.focus.row);
                let col0 = self.anchor.col.min(self.focus.col);
                let col1 = self.anchor.col.max(self.focus.col);
                (Pos::new(row0, col0), Pos::new(row1, col1))
            }
        }
    }

    /// 是否为空选区。
    ///
    /// 两种形状都以 `anchor == focus` 表示"只有光标、没有选中字节"：线性是零长
    /// 半开区间，矩形是零面积块。单格矩形请用线性选区表达，避免与空选区混淆。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// 是否包含某个位置。
    ///
    /// 线性按半开区间 `[min, max)`，矩形按行列闭区间；空选区不包含任何位置。
    #[inline]
    pub fn contains(&self, pos: Pos) -> bool {
        if self.is_empty() {
            return false;
        }
        let (lo, hi) = self.normalized();
        match self.kind {
            SelectionKind::Linear => lo <= pos && pos < hi,
            SelectionKind::Rect => {
                lo.row <= pos.row && pos.row <= hi.row && lo.col <= pos.col && pos.col <= hi.col
            }
        }
    }

    /// 把选区展开成若干半开字节区间。
    ///
    /// - 线性：至多一段连续区间（`min..max`）；
    /// - 矩形：逐行切段，每行一段；行起点越过 `len` 即停止；
    /// - 所有区间都裁剪到 `[0, len)`，并保证互不重叠、按偏移升序；
    /// - `bytes_per_row == 0`、`len == 0` 或空选区返回空 `Vec`。
    ///
    /// 注意：超大矩形会返回与行数同数量的区间；调用方应先与可见行求交，
    /// 不要对全文件选区直接调用（05 §4.5 "超长选区裁剪"）。
    pub fn byte_ranges(&self, bytes_per_row: u32, len: u64) -> Vec<ByteRange> {
        if bytes_per_row == 0 || len == 0 || self.is_empty() {
            return Vec::new();
        }
        match self.kind {
            SelectionKind::Linear => self.linear_ranges(bytes_per_row, len),
            SelectionKind::Rect => self.rect_ranges(bytes_per_row, len),
        }
    }

    fn linear_ranges(&self, bytes_per_row: u32, len: u64) -> Vec<ByteRange> {
        let (lo, hi) = self.normalized();
        let start = lo
            .offset_clamped(bytes_per_row)
            .unwrap_or(u64::MAX)
            .min(len);
        let end = hi
            .offset_clamped(bytes_per_row)
            .unwrap_or(u64::MAX)
            .min(len);
        if start >= end {
            return Vec::new();
        }
        ByteRange::new(Offset::new(start), Offset::new(end))
            .map(|r| vec![r])
            .unwrap_or_default()
    }

    fn rect_ranges(&self, bytes_per_row: u32, len: u64) -> Vec<ByteRange> {
        let (lo, hi) = self.normalized();
        let bpr = bytes_per_row as u64;
        // 越界列夹到本行最后一列；这比"整段丢弃"更符合"边界夹紧"的容错约定。
        let min_col = lo.col.min(bytes_per_row - 1) as u64;
        let max_col = hi.col.min(bytes_per_row - 1) as u64;
        let last_row = (len - 1) / bpr;
        let last_row = hi.row.min(last_row);
        if lo.row > last_row {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut row = lo.row;
        while let Some(row_start) = row.checked_mul(bpr) {
            let start = row_start.saturating_add(min_col);
            if start >= len {
                break;
            }
            let end = row_start.saturating_add(max_col).saturating_add(1).min(len);
            if start < end {
                if let Some(range) = ByteRange::new(Offset::new(start), Offset::new(end)) {
                    out.push(range);
                }
            }
            if row == last_row {
                break;
            }
            let Some(next) = row.checked_add(1) else {
                break;
            };
            row = next;
        }
        out
    }

    /// 移动焦点（Shift+单击 / 拖拽），锚点不动。
    #[inline]
    pub fn extend_to(&mut self, pos: Pos) {
        self.focus = pos;
    }

    /// 折叠到某个位置：锚点与焦点同时归位，选区变为空。
    #[inline]
    pub fn collapse_to(&mut self, pos: Pos) {
        self.anchor = pos;
        self.focus = pos;
    }
}

/// 光标位置 + 半字节选择。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// 光标所在的字节位置。
    pub pos: Pos,
    /// `Some(true)` = 高半字节，`Some(false)` = 低半字节，`None` = 整字节
    /// （ASCII 列恒为 `None`，见 05 §4.5）。
    pub nibble: Option<bool>,
}

impl Cursor {
    /// 在某个位置构造光标，默认选中整字节。
    #[inline]
    pub const fn new(pos: Pos) -> Self {
        Self { pos, nibble: None }
    }

    /// 在某个全局偏移构造光标。
    #[inline]
    pub fn at_offset(offset: u64, bytes_per_row: u32) -> Self {
        Self::new(Pos::from_offset(offset, bytes_per_row))
    }

    /// 按**字节**移动光标并夹紧到 `[0, len - 1]`。
    ///
    /// 水平移动传 `±1`，垂直移动传 `±bytes_per_row`（05 §4.5）；两者都只是
    /// 平铺字节偏移上的增量。`len == 0` 或 `bytes_per_row == 0` 时光标归零，
    /// 不 panic。半字节状态保持不变——调用方切到 ASCII 列时应自行清空。
    pub fn move_by(&mut self, delta_bytes: i64, bytes_per_row: u32, len: u64) {
        if len == 0 || bytes_per_row == 0 {
            self.pos = Pos::default();
            self.nibble = None;
            return;
        }
        let max = len - 1;
        let current = self
            .pos
            .offset_clamped(bytes_per_row)
            .unwrap_or(max)
            .min(max);
        let target = if delta_bytes >= 0 {
            current.saturating_add(delta_bytes as u64)
        } else {
            current.saturating_sub(delta_bytes.unsigned_abs())
        }
        .min(max);
        self.pos = Pos::from_offset(target, bytes_per_row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offsets(ranges: &[ByteRange]) -> Vec<(u64, u64)> {
        ranges
            .iter()
            .map(|r| (r.start().get(), r.end().get()))
            .collect()
    }

    #[test]
    fn linear_normalizes_in_byte_order() {
        let sel = Selection::new(Pos::new(2, 5), Pos::new(0, 1), SelectionKind::Linear);
        assert_eq!(sel.normalized(), (Pos::new(0, 1), Pos::new(2, 5)));
        assert!(!sel.is_empty());
        assert_eq!(offsets(&sel.byte_ranges(16, 100)), vec![(1, 37)]);
    }

    #[test]
    fn linear_reverse_drag_keeps_direction_but_clips_to_min_max() {
        let mut sel = Selection::new(Pos::new(4, 3), Pos::new(4, 3), SelectionKind::Linear);
        assert!(sel.is_empty());
        assert!(sel.byte_ranges(16, 100).is_empty());

        sel.extend_to(Pos::new(1, 2)); // 向左上反向拖拽
        assert_eq!(sel.anchor, Pos::new(4, 3));
        assert_eq!(sel.focus, Pos::new(1, 2));
        assert_eq!(offsets(&sel.byte_ranges(16, 100)), vec![(18, 67)]);
        assert!(sel.contains(Pos::new(2, 0)));
        assert!(!sel.contains(Pos::new(4, 3))); // 半开：focus 本身不含
    }

    #[test]
    fn rect_selection_splits_per_row() {
        let sel = Selection::new(Pos::new(1, 3), Pos::new(3, 5), SelectionKind::Rect);
        assert_eq!(
            offsets(&sel.byte_ranges(16, 1000)),
            vec![(19, 22), (35, 38), (51, 54)]
        );
        assert!(sel.contains(Pos::new(2, 4)));
        assert!(!sel.contains(Pos::new(2, 6)));
    }

    #[test]
    fn rect_reverse_drag_normalizes_columns_and_rows() {
        let sel = Selection::new(Pos::new(3, 5), Pos::new(1, 3), SelectionKind::Rect);
        assert_eq!(sel.normalized(), (Pos::new(1, 3), Pos::new(3, 5)));
        assert_eq!(
            offsets(&sel.byte_ranges(16, 1000)),
            vec![(19, 22), (35, 38), (51, 54)]
        );
    }

    #[test]
    fn rect_skips_rows_past_eof() {
        let sel = Selection::new(Pos::new(1, 2), Pos::new(3, 4), SelectionKind::Rect);
        assert_eq!(offsets(&sel.byte_ranges(16, 40)), vec![(18, 21), (34, 37)]);
    }

    #[test]
    fn rect_clips_last_partial_row_and_column_overflow() {
        // 列号超出本行宽度时夹到最后一列，不做跨行扩张。
        let overflow = Selection::new(Pos::new(0, 14), Pos::new(0, 99), SelectionKind::Rect);
        assert_eq!(offsets(&overflow.byte_ranges(16, 20)), vec![(14, 16)]);

        // 最后一行不足 16 字节：只保留真实存在的字节。
        let partial = Selection::new(Pos::new(0, 2), Pos::new(2, 9), SelectionKind::Rect);
        assert_eq!(
            offsets(&partial.byte_ranges(16, 20)),
            vec![(2, 10), (18, 20)]
        );
    }

    #[test]
    fn empty_and_degenerate_selections_are_safe() {
        let empty = Selection::new(Pos::new(7, 1), Pos::new(7, 1), SelectionKind::Rect);
        assert!(empty.is_empty());
        assert!(empty.byte_ranges(16, 100).is_empty());
        assert!(empty.byte_ranges(0, 100).is_empty());
        assert!(!empty.contains(Pos::new(7, 1)));
        let normal = Selection::new(Pos::new(0, 0), Pos::new(0, 1), SelectionKind::Rect);
        assert!(normal.byte_ranges(16, 0).is_empty());
    }

    #[test]
    fn collapse_to_clears_selection() {
        let mut sel = Selection::new(Pos::new(1, 1), Pos::new(5, 5), SelectionKind::Linear);
        sel.collapse_to(Pos::new(3, 2));
        assert_eq!(sel.anchor, Pos::new(3, 2));
        assert_eq!(sel.focus, Pos::new(3, 2));
        assert!(sel.is_empty());
    }

    #[test]
    fn cursor_moves_by_byte_and_clamps_at_zero() {
        let mut cur = Cursor::new(Pos::new(1, 0));
        cur.move_by(-1, 16, 100);
        assert_eq!(cur.pos, Pos::new(0, 15));
        cur.move_by(-100, 16, 100);
        assert_eq!(cur.pos, Pos::new(0, 0));
        cur.move_by(-1, 16, 100);
        assert_eq!(cur.pos, Pos::new(0, 0));
    }

    #[test]
    fn cursor_vertical_move_crosses_rows_and_clamps_at_eof() {
        let mut cur = Cursor::new(Pos::new(1, 15));
        cur.move_by(1, 16, 100);
        assert_eq!(cur.pos, Pos::new(2, 0));
        cur.move_by(-16, 16, 100);
        assert_eq!(cur.pos, Pos::new(1, 0));
        cur.move_by(i64::MAX, 16, 100);
        assert_eq!(cur.pos, Pos::new(6, 3)); // len=100 → 最后一字节 99
    }

    #[test]
    fn cursor_handles_zero_len_zero_bpr_and_overflowing_rows() {
        let mut cur = Cursor::new(Pos::new(u64::MAX, 7));
        cur.move_by(1, 16, 100);
        assert_eq!(cur.pos, Pos::new(6, 3));

        cur.move_by(1, 0, 100);
        assert_eq!(cur.pos, Pos::default());
        assert_eq!(cur.nibble, None);

        cur.move_by(1, 16, 0);
        assert_eq!(cur.pos, Pos::default());
    }

    #[test]
    fn pos_offset_roundtrip_and_overflow() {
        for off in [0, 1, 15, 16, 17, 999] {
            let pos = Pos::from_offset(off, 16);
            assert_eq!(pos.offset(16), Some(off));
        }
        assert_eq!(Pos::new(u64::MAX, 1).offset(16), None);
        assert_eq!(Pos::from_offset(5, 0), Pos::default());
    }

    /// 确定性伪随机数（xorshift64*），用于不引入额外依赖的属性测试。
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

        fn below(&mut self, bound: u64) -> u64 {
            if bound == 0 { 0 } else { self.next() % bound }
        }
    }

    #[test]
    fn property_byte_ranges_are_disjoint_and_clipped() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..4_000 {
            let bpr = (rng.below(64) + 1) as u32;
            let len = rng.below(20_000);
            let max_row = len / bpr as u64 + 3;
            let a = Pos::new(rng.below(max_row + 1), rng.below(bpr as u64 + 3) as u32);
            let b = Pos::new(rng.below(max_row + 1), rng.below(bpr as u64 + 3) as u32);
            let kind = if rng.next() & 1 == 0 {
                SelectionKind::Linear
            } else {
                SelectionKind::Rect
            };
            let ranges = Selection::new(a, b, kind).byte_ranges(bpr, len);

            let mut cursor = 0;
            let mut total = 0_u64;
            for r in &ranges {
                assert!(r.start().get() <= r.end().get(), "区间必须有序");
                assert!(r.end().get() <= len, "区间必须裁到 len 内");
                assert!(r.start().get() >= cursor, "区间不得重叠且须升序");
                cursor = r.end().get();
                total = total.saturating_add(r.len());
            }
            assert!(total <= len, "选中总长不得超过文件长度");
        }
    }
}
