//! 字节直方图：256 bin 计数、top-N、熵与均匀分布 χ²。
//!
//! DESIGN: 10 §4.4。直方图是单遍多分析器最便宜的观察者（每字节一次 fetch-add），
//! 之后所有统计量都从 256 个 `u64` 算出，不再触碰原始数据。

use crate::entropy::entropy_from_counts;

/// 256 bin 字节直方图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByteHistogram {
    counts: [u64; 256],
}

impl Default for ByteHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl ByteHistogram {
    /// 全零直方图。
    pub const fn new() -> Self {
        Self {
            counts: [0u64; 256],
        }
    }

    /// 记录一个字节。
    #[inline]
    pub fn observe(&mut self, byte: u8) {
        self.counts[usize::from(byte)] += 1;
    }

    /// 记录一段字节。
    pub fn observe_slice(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.observe(byte);
        }
    }

    /// 合并另一个直方图（计数按 `u64` 饱和相加，不 panic）。
    pub fn merge(&mut self, other: &Self) {
        for (mine, theirs) in self.counts.iter_mut().zip(other.counts.iter()) {
            *mine = mine.saturating_add(*theirs);
        }
    }

    /// 指定字节的计数。
    pub fn count(&self, byte: u8) -> u64 {
        self.counts[usize::from(byte)]
    }

    /// 全部 256 个计数。
    pub fn counts(&self) -> &[u64; 256] {
        &self.counts
    }

    /// 总样本数。
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// 出现次数最高的 `n` 个字节，按 `(count 降序, byte 升序)` 排列。
    ///
    /// 计数为 0 的字节不会出现在结果里；`n` 超过实际出现的字节数时结果更短。
    pub fn top_n(&self, n: usize) -> Vec<(u8, u64)> {
        let mut items: Vec<(u8, u64)> = self
            .counts
            .iter()
            .enumerate()
            .filter(|&(_, &count)| count > 0)
            .map(|(byte, &count)| (byte as u8, count))
            .collect();
        items.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        items.truncate(n);
        items
    }

    /// 直方图的 Shannon 熵（与 [`crate::entropy::shannon`] 同口径）。
    pub fn entropy(&self) -> f64 {
        entropy_from_counts(&self.counts)
    }

    /// 与离散均匀分布（每字节期望 `total/256`）的 χ²。
    ///
    /// 空直方图返回 0.0。纯随机字节的期望约为 255（自由度 255），
    /// 单字节输入则恰好为 255。
    pub fn chi_square(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        let expected = total as f64 / 256.0;
        let chi: f64 = self
            .counts
            .iter()
            .map(|&count| {
                let diff = count as f64 - expected;
                diff * diff / expected
            })
            .sum();
        if chi.is_finite() { chi.max(0.0) } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_and_total_track_input_len() {
        let mut histogram = ByteHistogram::new();
        histogram.observe_slice(b"abracadabra");
        assert_eq!(histogram.total(), 11);
        assert_eq!(histogram.count(b'a'), 5);
        assert_eq!(histogram.count(b'b'), 2);
        assert_eq!(histogram.count(b'z'), 0);
    }

    #[test]
    fn merge_adds_counts() {
        let mut left = ByteHistogram::new();
        left.observe_slice(b"aaaa");
        let mut right = ByteHistogram::new();
        right.observe_slice(b"aa");
        left.merge(&right);
        assert_eq!(left.count(b'a'), 6);
        assert_eq!(left.total(), 6);
    }

    #[test]
    fn top_n_sorts_by_count_then_byte() {
        let mut histogram = ByteHistogram::new();
        histogram.observe_slice(b"aaabcc");
        assert_eq!(histogram.top_n(3), vec![(b'a', 3), (b'c', 2), (b'b', 1)]);
        assert_eq!(histogram.top_n(0), Vec::new());
        assert_eq!(histogram.top_n(1000).len(), 3);
    }

    #[test]
    fn chi_square_for_single_byte_is_255() {
        let mut histogram = ByteHistogram::new();
        histogram.observe(b'x');
        assert!((histogram.chi_square() - 255.0).abs() < 1e-9);
        assert_eq!(ByteHistogram::new().chi_square(), 0.0);
    }

    #[test]
    fn entropy_matches_shannon() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut histogram = ByteHistogram::new();
        histogram.observe_slice(data);
        assert!((histogram.entropy() - crate::entropy::shannon(data)).abs() < 1e-12);
    }
}
