//! R1 降级与局部 defrag（T1.3）。DESIGN: 04 §3.3、§4.4

use geode_buffer::hydrate::{EnsureFilled, HydrateError, MIN_PLACEHOLDER, ensure_filled};
use geode_buffer::piece::{Piece, PieceTable, Span};
use geode_buffer::tree::PieceTree;
use geode_provider::{
    CacheStats, CachedProvider, FileProvider, MemoryProvider, PAGE_SIZE, Provider,
};
use std::path::PathBuf;

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

// ---- T1.3 lazy 填页：ensure_filled 只处理请求区间 ----

/// 稀疏临时文件：退出时删除。
struct SparseFile {
    path: PathBuf,
}

impl SparseFile {
    fn new(len: u64) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "geode-hydrate-sparse-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            len
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(len).unwrap();
        Self { path }
    }
}

impl Drop for SparseFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn four_gib_sparse_open_reads_nothing_then_fills_one_page() {
    const FOUR_GIB: u64 = 4 << 30;
    const AT: u64 = 1 << 20;
    const LEN: u64 = 4096;

    let file = SparseFile::new(FOUR_GIB);
    let cached = CachedProvider::new(FileProvider::open(&file.path).unwrap());
    assert_eq!(
        cached.stats(),
        CacheStats::default(),
        "打开 4 GiB 稀疏源不得读一个字节"
    );

    let mut tree = PieceTree::from_source(FOUR_GIB);
    let before = cached.stats();
    let demoted = ensure_filled(&mut tree, AT, LEN, &cached).unwrap();
    let after = cached.stats();

    assert_eq!(demoted, 1);
    assert!(
        after.reads - before.reads <= 2,
        "填 4 KiB 最多碰 2 页，实际 {} 页",
        after.reads - before.reads
    );
    assert_eq!(after.bytes_cached, PAGE_SIZE as u64);
    assert_eq!(
        (after.reads, after.hits, after.misses),
        (1, 0, 1),
        "首次填页应恰好一次底层读、一次 miss"
    );

    // 只有请求区间变成 literal，前后仍是 placeholder。
    let spans = tree.spans(0, tree.len());
    assert_eq!(spans.len(), 3);
    assert!(matches!(
        &spans[0],
        Span::Source { start: 0, len, at: 0 } if *len == AT
    ));
    match &spans[1] {
        Span::Literal { bytes, at } => {
            assert_eq!(*at, AT);
            assert_eq!(bytes.len(), LEN as usize);
            assert!(bytes.iter().all(|b| *b == 0), "稀疏文件这里应是 0");
        }
        other => panic!("请求区间应已降级为 literal，得到 {other:?}"),
    }
    assert!(matches!(
        &spans[2],
        Span::Source { start, len, at }
            if *start == AT + LEN && *len == FOUR_GIB - AT - LEN && *at == AT + LEN
    ));
    assert!(tree.check_invariants().is_ok());

    // 幂等：再填同一区间完全不碰源，也不产生新的页请求。
    let stats_before_second = cached.stats();
    assert_eq!(tree.ensure_filled(AT, LEN, &cached).unwrap(), 0);
    assert_eq!(cached.stats(), stats_before_second);
}

#[test]
fn ensure_filled_piece_table_demotes_only_the_requested_range() {
    let src: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let cached = CachedProvider::new(MemoryProvider::new("ensure-table", src.clone()));
    let mut t = PieceTable::from_source(src.len() as u64);

    let before = cached.stats();
    let demoted = t.ensure_filled(1 << 20, 4096, &cached).unwrap();
    let after = cached.stats();

    assert_eq!(demoted, 1);
    assert!(after.reads - before.reads <= 2);
    assert_eq!(after.bytes_cached, PAGE_SIZE as u64);

    let spans = t.spans(0, t.len());
    assert_eq!(spans.len(), 3);
    assert!(matches!(
        &spans[0],
        Span::Source { start: 0, len, at: 0 } if *len == 1 << 20
    ));
    assert!(matches!(
        &spans[1],
        Span::Literal { bytes, at } if *at == 1 << 20 && bytes.len() == 4096
    ));
    assert!(matches!(
        &spans[2],
        Span::Source { start, .. } if *start == (1 << 20) + 4096
    ));
    assert_eq!(materialize(&t, &src), src);
    assert!(t.check_invariants().is_ok());
}

#[test]
fn ensure_filled_clips_out_of_range_and_ignores_empty() {
    let src = source();
    let p = MemoryProvider::new("ensure-clip", src.clone());
    let mut t = PieceTable::from_source(src.len() as u64);

    assert_eq!(t.ensure_filled(t.len() + 10, 4096, &p).unwrap(), 0);
    assert_eq!(t.ensure_filled(0, 0, &p).unwrap(), 0);
    let before = t.piece_count();
    let demoted = t.ensure_filled(t.len() - 16, 4096, &p).unwrap();
    assert_eq!(demoted, 1, "尾部区间应按文档长度裁剪后降级");
    assert_eq!(t.piece_count(), before + 1);
    assert_eq!(materialize(&t, &src), src);
}
