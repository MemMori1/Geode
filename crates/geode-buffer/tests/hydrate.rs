//! R1 降级与局部 defrag（T1.3）。DESIGN: 04 §3.3、§4.4

use geode_buffer::hydrate::{HydrateError, MIN_PLACEHOLDER};
use geode_buffer::piece::{Piece, PieceTable, Span};
use geode_provider::{MemoryProvider, Provider};

fn materialize(t: &PieceTable, src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for s in t.spans(0, t.len()) {
        match s {
            Span::Source { start, len, .. } => {
                out.extend_from_slice(&src[start as usize..(start + len) as usize]);
            }
            Span::Literal { bytes, .. } => out.extend_from_slice(bytes),
        }
    }
    out
}

fn source() -> Vec<u8> {
    (0..16 * 1024).map(|i| (i % 251) as u8).collect()
}

#[test]
fn demotes_fragments_below_4k_and_keeps_bytes() {
    let src = source();
    let p = MemoryProvider::new("mem", src.clone());
    let mut t = PieceTable::from_source(src.len() as u64);
    // 在偏移 100 覆盖 1 字节：左边 100 字节、右边 16283 字节。
    t.overwrite(100, &[0xAA]).unwrap();
    assert_eq!(t.piece_count(), 3);

    let demoted = t.demote_small_placeholders(&p).unwrap();
    assert_eq!(demoted, 1, "只有 100 字节那片该被降级");

    for s in t.spans(0, t.len()) {
        if let Span::Source { len, .. } = s {
            assert!(len >= MIN_PLACEHOLDER, "还剩 {len} 字节的 placeholder");
        }
    }
    assert_eq!(materialize(&t, &src), {
        let mut want = src.clone();
        want[100] = 0xAA;
        want[..].to_vec()
    });
}

#[test]
fn demotion_is_idempotent() {
    let src = source();
    let p = MemoryProvider::new("mem", src.clone());
    let mut t = PieceTable::from_source(src.len() as u64);
    t.overwrite(10, &[1, 2, 3]).unwrap();
    assert_eq!(t.demote_small_placeholders(&p).unwrap(), 1);
    assert_eq!(t.demote_small_placeholders(&p).unwrap(), 0);
    assert_eq!(materialize(&t, &src).len(), src.len());
}

#[test]
fn defragment_turns_only_the_range_into_literals() {
    let src = source();
    let p = MemoryProvider::new("mem", src.clone());
    let mut t = PieceTable::from_source(src.len() as u64);
    t.defragment(1000, 200, &p).unwrap();
    assert_eq!(t.piece_count(), 3);
    let lits: usize = t
        .spans(0, t.len())
        .iter()
        .filter(|s| matches!(s, Span::Literal { .. }))
        .count();
    assert_eq!(lits, 1);
    // 范围外的字节仍指向源
    let first = &t.spans(0, 1)[0];
    assert!(matches!(first, Span::Source { .. }));
    assert_eq!(materialize(&t, &src), src);
}

#[test]
fn short_read_is_an_error_not_silent_zeros() {
    struct HalfFull(Vec<u8>);
    impl std::fmt::Debug for HalfFull {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("HalfFull")
        }
    }
    impl Provider for HalfFull {
        fn id(&self) -> geode_core::ProviderId {
            geode_core::ProviderId::new(7)
        }
        fn display_name(&self) -> String {
            "half".into()
        }
        fn caps(&self) -> geode_provider::Caps {
            geode_provider::Caps::READ_ONLY_STABLE
        }
        fn size(&self) -> geode_provider::SizeInfo {
            geode_provider::SizeInfo::Known(self.0.len() as u64)
        }
        fn read_range(
            &self,
            at: geode_core::Offset,
            buf: &mut [u8],
        ) -> Result<geode_provider::ReadReport, geode_provider::ProviderError> {
            // 只填一半，模拟"空洞/短读"
            let n = buf.len() / 2;
            buf[..n].copy_from_slice(&self.0[at.get() as usize..at.get() as usize + n]);
            Ok(geode_provider::ReadReport {
                filled: n as u64,
                holes: 0,
                eof_hit: false,
            })
        }
    }

    let src = source();
    let p = HalfFull(src.clone());
    let mut t = PieceTable::from_source(src.len() as u64);
    t.overwrite(10, &[9]).unwrap();
    match t.demote_small_placeholders(&p) {
        Err(HydrateError::ShortRead { want, got }) => {
            assert_eq!(want, 10);
            assert_eq!(got, 5);
        }
        other => panic!("期望 ShortRead，得到 {other:?}"),
    }
}

#[test]
fn pieces_are_still_consistent_after_demotion() {
    let src = source();
    let p = MemoryProvider::new("mem", src.clone());
    let mut t = PieceTable::from_source(src.len() as u64);
    for i in 0..20u64 {
        t.overwrite(i * 137, &[0x5A]).unwrap();
    }
    let demoted = t.demote_small_placeholders(&p).unwrap();
    assert!(demoted > 0);
    assert!(t.check_invariants().is_ok(), "降级后结构不变量被破坏");
    assert_eq!(materialize(&t, &src).len(), src.len());
}

#[test]
fn no_zero_length_piece_is_created() {
    let src = source();
    let p = MemoryProvider::new("mem", src.clone());
    let mut t = PieceTable::from_source(src.len() as u64);
    t.overwrite(0, &[1]).unwrap(); // 左边 0 字节，不应产生空片
    t.demote_small_placeholders(&p).unwrap();
    assert!(t.spans(0, t.len()).iter().all(|s| match s {
        Span::Source { len, .. } => *len > 0,
        Span::Literal { bytes, .. } => !bytes.is_empty(),
    }));
    let _ = Piece::Source { start: 0, len: 1 };
}
