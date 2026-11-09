//! Piece table 的语义回归。DESIGN: 04 §3.2/§4.2，ADR-0001
//!
//! 判据只有一条：**任何编辑序列之后，`spans` 拼出来的字节必须与朴素 `Vec<u8>`
//! 逐字节相等**。索引层可以换、可以优化，这条不能变。

use geode_buffer::piece::{EditError, PieceTable, Span};
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
