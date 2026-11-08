//! 半开字节区间。
//!
//! DESIGN: 00 §4.2、04 §3.3、05 §4.4

use crate::offset::Offset;
use core::fmt;

/// 半开区间 `[start, end)`。
///
/// 不变量：`start <= end`。构造时校验，因此所有实例都可用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ByteRange {
    start: Offset,
    end: Offset,
}

impl ByteRange {
    /// 构造区间；`start > end` 时返回 `None`。
    #[inline]
    pub fn new(start: Offset, end: Offset) -> Option<Self> {
        if start.get() <= end.get() {
            Some(Self { start, end })
        } else {
            None
        }
    }

    /// 从起点与长度构造。
    #[inline]
    pub fn from_len(start: Offset, len: u64) -> Option<Self> {
        start.checked_add(len).and_then(|end| Self::new(start, end))
    }

    /// 空区间，位于 `at`。
    #[inline]
    pub const fn empty_at(at: Offset) -> Self {
        Self { start: at, end: at }
    }

    /// 起点。
    #[inline]
    pub const fn start(self) -> Offset {
        self.start
    }

    /// 终点（不含）。
    #[inline]
    pub const fn end(self) -> Offset {
        self.end
    }

    /// 长度。
    #[inline]
    pub const fn len(self) -> u64 {
        self.end.get() - self.start.get()
    }

    /// 是否为空。
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start.get() == self.end.get()
    }

    /// 是否包含某偏移。
    #[inline]
    pub const fn contains(self, at: Offset) -> bool {
        self.start.get() <= at.get() && at.get() < self.end.get()
    }

    /// 把区间裁到 `[0, limit)` 内；完全落在外面时返回 `None`。
    ///
    /// 越界读**不报错**，而是被裁剪后返回短读——见 00 §5.1 的铁律"短读合法"。
    #[inline]
    pub fn clip(self, limit: u64) -> Option<Self> {
        let start = self.start.get().min(limit);
        let end = self.end.get().min(limit);
        Self::new(Offset::new(start), Offset::new(end))
    }

    /// 交集；无交集返回 `None`（空交集也返回 `None`）。
    #[inline]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let start = self.start.get().max(other.start.get());
        let end = self.end.get().min(other.end.get());
        if start < end {
            Self::new(Offset::new(start), Offset::new(end))
        } else {
            None
        }
    }

    /// 是否有重叠（共享至少一个字节）。
    #[inline]
    pub fn overlaps(self, other: Self) -> bool {
        self.intersection(other).is_some()
    }

    /// 是否**紧邻或重叠** `other`——用于合并相邻 patch（04 §4.5）。
    #[inline]
    pub fn adjacent_or_overlaps(self, other: Self) -> bool {
        self.start.get() <= other.end.get() && other.start.get() <= self.end.get()
    }

    /// 合并两个区间，取并集包络（不做空洞检测，故名为 envelope）。
    #[inline]
    pub fn envelope(self, other: Self) -> Self {
        let start = self.start.get().min(other.start.get());
        let end = self.end.get().max(other.end.get());
        Self {
            start: Offset::new(start),
            end: Offset::new(end),
        }
    }

    /// 迭代器：区间内的每个偏移。
    #[inline]
    pub fn iter_offsets(self) -> impl Iterator<Item = Offset> {
        (self.start.get()..self.end.get()).map(Offset::new)
    }
}

impl fmt::Display for ByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}, {}) = {} B", self.start, self.end, self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: u64, e: u64) -> ByteRange {
        ByteRange::new(Offset::new(s), Offset::new(e)).unwrap()
    }

    #[test]
    fn rejects_inverted() {
        assert!(ByteRange::new(Offset::new(5), Offset::new(4)).is_none());
        assert!(ByteRange::new(Offset::new(4), Offset::new(4)).is_some());
    }

    #[test]
    fn no_overflow_on_from_len() {
        assert!(ByteRange::from_len(Offset::new(u64::MAX), 2).is_none());
    }

    #[test]
    fn clip_truncates_instead_of_erroring() {
        assert_eq!(r(10, 20).clip(15), Some(r(10, 15)));
        assert_eq!(
            r(30, 40).clip(15),
            Some(ByteRange::empty_at(Offset::new(15)))
        );
        assert_eq!(r(30, 40).clip(15).unwrap().len(), 0);
    }

    #[test]
    fn overlaps_is_strict() {
        assert!(r(0, 10).overlaps(r(9, 20)));
        assert!(!r(0, 10).overlaps(r(10, 20)));
        assert!(r(0, 10).adjacent_or_overlaps(r(10, 20)));
    }

    #[test]
    fn intersection_and_envelope() {
        assert_eq!(r(0, 10).intersection(r(5, 20)), Some(r(5, 10)));
        assert_eq!(r(0, 10).intersection(r(10, 20)), None);
        assert_eq!(r(0, 10).envelope(r(20, 30)), r(0, 30));
    }

    #[test]
    fn iter_offsets_is_half_open() {
        let v: Vec<u64> = r(3, 6).iter_offsets().map(|o| o.get()).collect();
        assert_eq!(v, vec![3, 4, 5]);
    }
}
