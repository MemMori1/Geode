//! Piece table 的语义回归。DESIGN: 04 §3.2/§4.2，ADR-0001
//!
//! 判据只有一条：**任何编辑序列之后，`spans` 拼出来的字节必须与朴素 `Vec<u8>`
//! 逐字节相等**。索引层可以换、可以优化，这条不能变。

use geode_buffer::piece::{EditError, PieceCursor, PieceTable, Span};
use proptest::prelude::*;

fn materialize(t: &PieceTable, src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(t.len() as usize);
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

#[test]
fn opening_a_file_is_one_piece() {
    let t = PieceTable::from_source(1 << 30);
    assert_eq!(t.piece_count(), 1);
    assert_eq!(t.len(), 1 << 30);
}

#[test]
fn overwrite_inside_source_keeps_spans_honest() {
    let src: Vec<u8> = (0..64u8).collect();
    let mut t = PieceTable::from_source(src.len() as u64);
    t.overwrite(10, b"XYZW").unwrap();
    let got = materialize(&t, &src);
    assert_eq!(got.len(), src.len());
    assert_eq!(&got[10..14], b"XYZW");
    assert_eq!(&got[..10], &src[..10]);
    assert_eq!(&got[14..], &src[14..]);
}

#[test]
fn overwrite_across_eof_extends() {
    let src = b"abc".to_vec();
    let mut t = PieceTable::from_source(3);
    t.overwrite(1, b"XYZ").unwrap();
    assert_eq!(t.len(), 4);
    assert_eq!(materialize(&t, &src), b"aXYZ".to_vec());
}

#[test]
fn insert_and_remove_roundtrip() {
    let src = b"0123456789".to_vec();
    let mut t = PieceTable::from_source(10);
    t.insert(5, b"---").unwrap();
    assert_eq!(t.len(), 13);
    assert_eq!(materialize(&t, &src), b"01234---56789".to_vec());
    t.remove(5, 3).unwrap();
    assert_eq!(materialize(&t, &src), src);
}

#[test]
fn remove_everything_leaves_empty_table() {
    let mut t = PieceTable::from_source(100);
    t.remove(0, 100).unwrap();
    assert!(t.is_empty());
    assert_eq!(t.piece_count(), 0);
    assert_eq!(materialize(&t, &[]), Vec::<u8>::new());
}

#[test]
fn out_of_range_is_an_error_not_a_panic() {
    let mut t = PieceTable::from_source(10);
    assert_eq!(
        t.overwrite(11, b"x"),
        Err(EditError::OutOfRange { at: 11, len: 10 })
    );
    assert!(t.remove(5, 10).is_err());
    assert!(t.insert(11, b"x").is_err());
}

#[test]
fn adjacent_literals_coalesce() {
    let mut t = PieceTable::from_bytes(b"");
    for i in 0..100u8 {
        t.insert(t.len(), &[i]).unwrap();
    }
    // 100 次追加必须合并成一个 literal piece，而不是 100 个。
    assert_eq!(t.piece_count(), 1);
}

#[test]
fn removing_an_insert_brings_source_back_together() {
    let src: Vec<u8> = (0..64u8).collect();
    let mut t = PieceTable::from_source(64);
    t.insert(32, b"garbage").unwrap();
    assert_eq!(t.piece_count(), 3);
    t.remove(32, 7).unwrap();
    // 两侧 Source 在源上连续 ⇒ 必须重新合并回 1 个 piece。
    assert_eq!(t.piece_count(), 1);
    assert_eq!(materialize(&t, &src), src);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// 随机编辑序列与 `Vec<u8>` oracle 对拍。
    #[test]
    fn random_edits_match_vec_oracle(
        ops in prop::collection::vec(
            prop_oneof![
                (0usize..64, prop::collection::vec(any::<u8>(), 0..8)).prop_map(|(at, d)| (0u8, at, d)),
                (0usize..64, prop::collection::vec(any::<u8>(), 0..8)).prop_map(|(at, d)| (1u8, at, d)),
                (0usize..64, 0usize..8).prop_map(|(at, n)| (2u8, at, vec![0u8; n])),
            ],
            1..40
        )
    ) {
        let mut t = PieceTable::from_bytes(b"0123456789abcdef");
        let mut oracle = b"0123456789abcdef".to_vec();
        for (kind, at, data) in ops {
            let at = at.min(oracle.len());
            match kind {
                0 => {
                    // overwrite（可越过 EOF）
                    let end = at + data.len();
                    oracle.resize(oracle.len().max(end), 0);
                    oracle[at..end].copy_from_slice(&data);
                    t.overwrite(at as u64, &data).unwrap();
                }
                1 => {
                    oracle.splice(at..at, data.iter().copied());
                    t.insert(at as u64, &data).unwrap();
                }
                _ => {
                    let n = data.len().min(oracle.len() - at);
                    oracle.drain(at..at + n);
                    t.remove(at as u64, n as u64).unwrap();
                }
            }
            prop_assert_eq!(t.len(), oracle.len() as u64);
            prop_assert_eq!(materialize(&t, &[]), oracle.clone());
        }
    }
}

#[test]
fn cursor_spans_cover_exactly_the_table() {
    let src: Vec<u8> = (0..200u8).collect();
    let mut t = PieceTable::from_source(200);
    t.overwrite(10, b"AAAA").unwrap();
    t.insert(100, b"BBBBBB").unwrap();
    t.remove(50, 20).unwrap();
    let want = materialize(&t, &src);
    let mut got = Vec::new();
    let mut c = PieceCursor::new(&t, 0);
    while let Some(s) = c.next_span() {
        match s {
            Span::Source { start, len, .. } => {
                got.extend_from_slice(&src[start as usize..(start + len) as usize])
            }
            Span::Literal { bytes, .. } => got.extend_from_slice(bytes),
        }
    }
    assert_eq!(got, want);
    assert_eq!(got.len() as u64, t.len());
}

#[test]
fn cursor_starting_mid_piece() {
    let t = PieceTable::from_source(10);
    let mut c = PieceCursor::new(&t, 4);
    match c.next_span().unwrap() {
        Span::Source { start, len, .. } => {
            assert_eq!((start, len), (4, 6));
        }
        _ => panic!("应当是 Source"),
    }
    assert!(c.next_span().is_none());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// 任意编辑序列之后，结构不变量都必须成立（空 piece / 未合并的相邻片 / 总长不符）。
    #[test]
    fn invariants_hold_after_random_edits(
        ops in prop::collection::vec(
            prop_oneof![
                (0usize..512, prop::collection::vec(any::<u8>(), 0..16)).prop_map(|(a, d)| (0u8, a, d)),
                (0usize..512, prop::collection::vec(any::<u8>(), 0..16)).prop_map(|(a, d)| (1u8, a, d)),
                (0usize..512, 0usize..16).prop_map(|(a, n)| (2u8, a, vec![0u8; n])),
            ],
            1..60
        )
    ) {
        let mut t = PieceTable::from_source(4096);
        for (kind, at, data) in ops {
            let at = at.min(t.len() as usize) as u64;
            match kind {
                0 => t.overwrite(at, &data).unwrap(),
                1 => t.insert(at, &data).unwrap(),
                _ => {
                    let n = (data.len() as u64).min(t.len() - at);
                    t.remove(at, n).unwrap();
                }
            }
            if let Err(e) = t.check_invariants() {
                prop_assert!(false, "不变量被破坏：{e}");
            }
        }
    }
}
