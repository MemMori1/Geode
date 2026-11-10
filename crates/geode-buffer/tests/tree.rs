//! 计数树的正确性：与 `PieceTable`（语义参考实现）逐字节对拍。
//! DESIGN: 04 §3.2、ADR-0001 / ADR-0019

use geode_buffer::piece::{Piece, PieceCursor, PieceTable, Span};
use geode_buffer::tree::PieceTree;
use geode_provider::MemoryProvider;
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

#[test]
fn tree_demotes_small_placeholders_like_the_table() {
    let src: Vec<u8> = (0..16 * 1024).map(|i| (i % 251) as u8).collect();
    let p = MemoryProvider::new("mem", src.clone());
    let mut tree = PieceTree::from_source(src.len() as u64);
    let mut table = PieceTable::from_source(src.len() as u64);
    tree.overwrite(100, &[0xAA]).unwrap();
    table.overwrite(100, &[0xAA]).unwrap();

    assert_eq!(tree.demote_small_placeholders(&p).unwrap(), 1);
    assert_eq!(table.demote_small_placeholders(&p).unwrap(), 1);

    for s in tree.spans(0, tree.len()) {
        if let Span::Source { len, .. } = s {
            assert!(len >= 4096, "树里还剩 {len} 字节的 placeholder");
        }
    }
    tree.check_invariants().unwrap();
    assert_eq!(
        bytes_of_spans(tree.spans(0, tree.len()), &src),
        bytes_of_spans(table.spans(0, table.len()), &src)
    );
}

#[test]
fn tree_defragment_is_local() {
    let src: Vec<u8> = (0..16 * 1024).map(|i| (i % 251) as u8).collect();
    let p = MemoryProvider::new("mem", src.clone());
    let mut tree = PieceTree::from_source(src.len() as u64);
    tree.defragment(1000, 200, &p).unwrap();
    tree.check_invariants().unwrap();
    assert!(tree.height() <= 2);
    assert_eq!(bytes_of_spans(tree.spans(0, tree.len()), &src), src);
    let first = &tree.spans(0, 1)[0];
    assert!(matches!(first, Span::Source { .. }));
}

#[test]
fn tree_demote_reports_short_reads() {
    struct Empty;
    impl std::fmt::Debug for Empty {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Empty")
        }
    }
    impl geode_provider::Provider for Empty {
        fn id(&self) -> geode_core::ProviderId {
            geode_core::ProviderId::new(1)
        }
        fn display_name(&self) -> String {
            "empty".into()
        }
        fn caps(&self) -> geode_provider::Caps {
            geode_provider::Caps::READ_ONLY_STABLE
        }
        fn size(&self) -> geode_provider::SizeInfo {
            geode_provider::SizeInfo::Known(0)
        }
        fn read_range(
            &self,
            _at: geode_core::Offset,
            _buf: &mut [u8],
        ) -> Result<geode_provider::ReadReport, geode_provider::ProviderError> {
            Ok(geode_provider::ReadReport {
                filled: 0,
                holes: 0,
                eof_hit: true,
            })
        }
    }
    let mut tree = PieceTree::from_source(16 * 1024);
    tree.overwrite(10, &[1]).unwrap();
    assert!(tree.demote_small_placeholders(&Empty).is_err());
}

/// 把 Span 归一化成可比较的拥有所有权的元组：`(kind, start, len, bytes, at)`。
fn span_key(s: &Span<'_>) -> (u8, u64, u64, Vec<u8>, u64) {
    match s {
        Span::Source { start, len, at } => (0, *start, *len, Vec::new(), *at),
        Span::Literal { bytes, at } => (1, 0, bytes.len() as u64, bytes.to_vec(), *at),
    }
}

fn cursor_bytes(mut cursor: geode_buffer::tree::TreeCursor<'_>, src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(s) = cursor.next_span() {
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
fn tree_cursor_starts_at_leaf_boundaries_and_eof() {
    let expected: Vec<u8> = (0..200usize)
        .flat_map(|i| std::iter::repeat_n(i as u8, (i % 7) + 1))
        .collect();
    let mut pieces = Vec::new();
    let mut pos = 0usize;
    for i in 0..200usize {
        let n = (i % 7) + 1;
        pieces.push(Piece::Literal(expected[pos..pos + n].to_vec().into()));
        pos += n;
    }
    let tree = PieceTree::from_pieces(pieces, 0);
    assert_eq!(tree.height(), 1);
    // 4 个叶子：0..64、64..128、128..192、192..200。
    for at in [0u64, 1, 63, 64, 65, 127, 128, 129, 191, 192, 199, 200] {
        let cursor = tree.cursor(at);
        assert_eq!(cursor.offset(), at);
        assert_eq!(cursor.remaining(), tree.len() - at);
        assert_eq!(cursor_bytes(cursor, &[]), expected[at as usize..].to_vec());
    }
    let mut at_eof = tree.cursor(tree.len() + 100);
    assert_eq!(at_eof.offset(), tree.len());
    assert_eq!(at_eof.remaining(), 0);
    assert!(at_eof.next_span().is_none());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(250))]

    /// 树游标与平坦 `PieceCursor` 逐 span 对拍：起点随机，编辑长度随机。
    #[test]
    fn tree_cursor_matches_piece_cursor(
        ops in prop::collection::vec(
            prop_oneof![
                (0usize..512, prop::collection::vec(any::<u8>(), 0..24)).prop_map(|(a, d)| (0u8, a, d)),
                (0usize..512, prop::collection::vec(any::<u8>(), 0..24)).prop_map(|(a, d)| (1u8, a, d)),
                (0usize..512, 0usize..24).prop_map(|(a, n)| (2u8, a, vec![0u8; n])),
            ],
            1..80
        ),
        start_ratio in 0u64..=100,
    ) {
        let src: Vec<u8> = (0..600usize).map(|i| (i as u8).wrapping_mul(13)).collect();
        let mut tree = PieceTree::from_source(src.len() as u64);
        let mut table = PieceTable::from_source(src.len() as u64);
        for (kind, at, data) in ops {
            let at = at.min(tree.len() as usize) as u64;
            match kind {
                0 => {
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
        }
        tree.check_invariants().map_err(|e| TestCaseError::fail(e.join("; ")))?;
        let at = start_ratio * tree.len() / 100;
        let mut tc = tree.cursor(at);
        let mut pc = PieceCursor::new(&table, at);
        let mut consumed = 0u64;
        loop {
            match (tc.next_span(), pc.next_span()) {
                (None, None) => break,
                (Some(a), Some(b)) => {
                    prop_assert_eq!(span_key(&a), span_key(&b));
                    consumed += match &a {
                        Span::Source { len, .. } => *len,
                        Span::Literal { bytes, .. } => bytes.len() as u64,
                    };
                }
                (a, b) => prop_assert!(false, "span 数不一致：tree={a:?} table={b:?}"),
            }
        }
        prop_assert_eq!(tc.offset(), at + consumed);
        prop_assert_eq!(tc.remaining(), 0);
        prop_assert_eq!(consumed, table.len() - at);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// 500 例随机编辑后结构不变量成立，且与 `PieceTable` 逐字节一致。
    #[test]
    fn tree_invariants_after_random_edits(
        ops in prop::collection::vec(
            prop_oneof![
                (0usize..400, prop::collection::vec(any::<u8>(), 0..20)).prop_map(|(a, d)| (0u8, a, d)),
                (0usize..400, prop::collection::vec(any::<u8>(), 0..20)).prop_map(|(a, d)| (1u8, a, d)),
                (0usize..400, 0usize..20).prop_map(|(a, n)| (2u8, a, vec![0u8; n])),
            ],
            1..60
        )
    ) {
        let src: Vec<u8> = (0..512usize).map(|i| (i as u8).wrapping_mul(29)).collect();
        let mut tree = PieceTree::from_source(src.len() as u64);
        let mut table = PieceTable::from_source(src.len() as u64);
        for (kind, at, data) in ops {
            let at = at.min(tree.len() as usize) as u64;
            match kind {
                0 => {
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
            tree.check_invariants()
                .map_err(|e| TestCaseError::fail(format!("编辑后不变量失败：{}", e.join("; "))))?;
            let got = bytes_of_spans(tree.spans(0, tree.len()), &src);
            let want = bytes_of_spans(table.spans(0, table.len()), &src);
            prop_assert_eq!(got, want);
        }
    }
}
