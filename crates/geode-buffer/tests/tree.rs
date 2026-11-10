//! 计数树的正确性：与 `PieceTable`（语义参考实现）逐字节对拍。
//! DESIGN: 04 §3.2、ADR-0001 / ADR-0019

use geode_buffer::piece::{Piece, PieceTable, Span};
use geode_buffer::tree::PieceTree;
use proptest::prelude::*;

fn bytes_of_spans(spans: Vec<Span<'_>>, src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for s in spans {
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
fn tree_from_source_is_one_piece() {
    let t = PieceTree::from_source(10 << 20);
    assert_eq!(t.piece_count(), 1);
    assert_eq!(t.height(), 0);
    assert_eq!(t.len(), 10 << 20);
}

#[test]
fn spans_are_global_and_clipped() {
    let src: Vec<u8> = (0..128u8).collect();
    let t = PieceTree::from_source(128);
    let got = bytes_of_spans(t.spans(120, 64), &src);
    assert_eq!(got, src[120..].to_vec());
    assert!(t.spans(200, 10).is_empty());
}

#[test]
fn big_literal_table_stays_shallow() {
    // 10 万个 1 字节 piece：如果还是平坦数组，定位要扫 10 万次；树高应当很小。
    let pieces: Vec<Piece> = (0..100_000usize)
        .map(|i| Piece::Literal(vec![i as u8].into()))
        .collect();
    let t = PieceTree::from_pieces(pieces, 0);
    assert_eq!(t.piece_count(), 100_000);
    assert!(t.height() <= 6, "树高 {} 太大", t.height());
    let got = bytes_of_spans(t.spans(t.len() - 4, 4), &[]);
    assert_eq!(got.len(), 4);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]

    /// 随机编辑序列：树与语义参考实现必须逐字节一致。
    #[test]
    fn tree_matches_piece_table(
        ops in prop::collection::vec(
            prop_oneof![
                (0usize..256, prop::collection::vec(any::<u8>(), 0..12)).prop_map(|(a, d)| (0u8, a, d)),
                (0usize..256, prop::collection::vec(any::<u8>(), 0..12)).prop_map(|(a, d)| (1u8, a, d)),
                (0usize..256, 0usize..12).prop_map(|(a, n)| (2u8, a, vec![0u8; n])),
            ],
            1..50
        )
    ) {
        let src: Vec<u8> = (0..300usize).map(|i| (i as u8).wrapping_mul(7)).collect();
        let mut tree = PieceTree::from_source(src.len() as u64);
        let mut table = PieceTable::from_source(src.len() as u64);
        for (kind, at, data) in ops {
            let at = at.min(tree.len() as usize) as u64;
            match kind {
                0 => {
                    // 越过 EOF 的覆盖：两边都要扩展
                    tree.overwrite(at, &data).unwrap();
                    table.overwrite(at, &data).unwrap();
                }
                1 => {
                    tree.insert(at, &data).unwrap();
                    table.insert(at, &data).unwrap();
                }
                _ => {
                    let n = (data.len() as u64).min(tree.len() - at);
                    tree.remove(at, n).unwrap();
                    table.remove(at, n).unwrap();
                }
            }
            prop_assert_eq!(tree.len(), table.len());
            let a = bytes_of_spans(tree.spans(0, tree.len()), &src);
            let b = bytes_of_spans(table.spans(0, table.len()), &src);
            prop_assert_eq!(a, b);
        }
    }
}
