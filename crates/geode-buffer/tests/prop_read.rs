//! `Buffer::read` 的属性测试：**任意**内容 + 任意区间，读出的字节必须与影子 `Vec<u8>` 一致。
//!
//! DESIGN: 04 §4.1（读路径）、18 §10.2 P5
//!
//! M1 会把 `read` 的内部换成跨 piece 拼接。这个测试是那次改写的**唯一护栏**：
//! 只要"读出来的字节和一段连续内存相同"这条不变量还在，换实现就是安全的。

use geode_buffer::Buffer;
use geode_core::Offset;
use geode_provider::MemoryProvider;
use proptest::prelude::*;

proptest! {
    #[test]
    fn read_matches_shadow(
        content in prop::collection::vec(any::<u8>(), 0..4096),
        at in 0u64..8192,
        want in 0usize..256,
    ) {
        let shadow = content.clone();
        let len = shadow.len() as u64;
        let buf_provider = MemoryProvider::new("prop", content);
        let buffer = Buffer::new(Box::new(buf_provider));

        let mut out = vec![0u8; want];
        let report = buffer.read(Offset::new(at), &mut out).expect("内存源不会失败");

        // 期望：从 min(at, len) 开始，最多读到 EOF。
        let start = at.min(len) as usize;
        let n = want.min(shadow.len().saturating_sub(start));
        prop_assert_eq!(report.filled, n as u64);
        // 06 §3.1 / §4.2：`eof_hit` 表示"这次是短读"（撞到文件末尾），
        // 而不是"读到的最后一个字节恰好是文件最后一个字节"。
        // 恰好读到 EOF 时不算命中——否则调用方会误以为还有下一块。
        prop_assert_eq!(report.eof_hit, at >= len || n < want);
        prop_assert_eq!(&out[..n], &shadow[start..start + n]);
        // 超出写入部分必须保持调用方原值（0），不得被"顺手填零"。
        prop_assert!(out[n..].iter().all(|b| *b == 0));
    }

    #[test]
    fn read_range_is_clipped_not_panicking(
        content in prop::collection::vec(any::<u8>(), 0..2048),
        start in 0u64..4096,
        len in 0u64..4096,
    ) {
        let shadow = content.clone();
        let buffer = Buffer::new(Box::new(MemoryProvider::new("prop", content)));
        let got = buffer.read_range(geode_core::ByteRange::from_len(Offset::new(start), len).unwrap())
            .expect("越界只裁剪，不报错（00 §5.1 铁律）");
        let s = (start as usize).min(shadow.len());
        let e = ((start + len) as usize).min(shadow.len());
        prop_assert_eq!(got, shadow[s..e].to_vec());
    }
}
