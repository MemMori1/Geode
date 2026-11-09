//! `ByteRange` 的属性测试：区间代数在**任意**输入下都必须自洽。
//!
//! DESIGN: 00 §4.2、04 §3.3、18 §10.2 P5
//!
//! 为什么用属性测试而不是用例：区间是全局契约类型，错一个边界就是"读少一个字节"
//! 或"越界 panic"，而这两个 bug 在用例里几乎抓不到（只有特定长度才复现）。

use geode_core::{ByteRange, Offset};
use proptest::prelude::*;

fn any_range() -> impl Strategy<Value = ByteRange> {
    (0u64..u32::MAX as u64, 0u64..u32::MAX as u64).prop_map(|(a, b)| {
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        ByteRange::new(Offset::new(s), Offset::new(e)).expect("s <= e")
    })
}

proptest! {
    /// 长度恒等于 `end - start`，且空区间只在 `start == end` 时成立。
    #[test]
    fn len_matches_bounds(r in any_range()) {
        prop_assert_eq!(r.len(), r.end().get() - r.start().get());
        prop_assert_eq!(r.is_empty(), r.start() == r.end());
    }

    /// 交集要么不存在，要么同时落在两个区间内。
    #[test]
    fn intersection_is_inside_both(a in any_range(), b in any_range()) {
        if let Some(i) = a.intersection(b) {
            prop_assert!(i.start() >= a.start() && i.end() <= a.end());
            prop_assert!(i.start() >= b.start() && i.end() <= b.end());
            prop_assert!(a.overlaps(b));
        } else {
            prop_assert!(!a.overlaps(b));
        }
    }

    /// 包络一定包含两个区间，且长度不小于任一者。
    #[test]
    fn envelope_covers_both(a in any_range(), b in any_range()) {
        let e = a.envelope(b);
        prop_assert!(e.start() <= a.start() && e.end() >= a.end());
        prop_assert!(e.start() <= b.start() && e.end() >= b.end());
        prop_assert!(e.len() >= a.len() && e.len() >= b.len());
    }

    /// 裁剪：结果长度不超过上限，且不越过原区间。
    #[test]
    fn clip_never_exceeds_limit(r in any_range(), limit in 0u64..u32::MAX as u64) {
        let c = r.clip(limit);
        prop_assert!(c.end().get() <= limit);
        prop_assert_eq!(c.start().get(), r.start().get().min(limit));
        prop_assert!(c.len() <= r.len());
        prop_assert_eq!(c.len(), r.end().get().min(limit) - r.start().get().min(limit));
    }

    /// `contains` 与逐字节遍历一致（小样本上真遍历一遍）。
    #[test]
    fn contains_matches_iteration(start in 0u64..1000, len in 0u64..64) {
        let r = ByteRange::from_len(Offset::new(start), len).unwrap();
        let seen: Vec<u64> = r.iter_offsets().map(|o| o.get()).collect();
        prop_assert_eq!(seen.len() as u64, len);
        for probe in 0u64..(start + len + 4) {
            let expect = probe >= start && probe < start + len;
            prop_assert_eq!(r.contains(Offset::new(probe)), expect);
        }
    }
}
