//! T1.2b 的**基尺**：平坦 `Vec<Piece>` 在 piece 数增长时的定位/编辑成本。
//!
//! 这不是"性能测试通过"，而是把"为什么需要 B-tree"量化出来：
//! 每次 `spans` 都要从第一个 piece 线性扫到目标偏移，编辑要 `Vec::splice`。

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use geode_buffer::piece::{Piece, PieceTable};
use geode_buffer::tree::PieceTree;
use std::hint::black_box;

/// 在 64 KiB 源上制造 `n` 个碎片：每隔一段做一次 1 字节覆盖。
fn fragmented(n: usize) -> PieceTable {
    let mut t = PieceTable::from_source(64 * 1024);
    for i in 0..n {
        let at = (i as u64 * 37) % t.len();
        t.overwrite(at, &[0xEE]).unwrap();
    }
    t
}

fn tree_of(n: usize) -> PieceTree {
    let pieces: Vec<Piece> = (0..n)
        .map(|i| Piece::Literal(vec![i as u8].into()))
        .collect();
    PieceTree::from_pieces(pieces, 0)
}

/// 先在 64 KiB 源上做 1 万次伪随机 1 字节覆盖，再测树上的单步覆盖。
fn edited_tree(n: usize) -> PieceTree {
    let mut t = PieceTree::from_source(64 * 1024);
    for i in 0..n {
        let at = (i as u64 * 37) % t.len();
        t.overwrite(at, &[0xEE]).unwrap();
    }
    t
}

fn bench_tree_locate(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree_locate");
    for n in [2_048usize, 100_000] {
        let t = tree_of(n);
        let len = t.len();
        g.bench_function(format!("{n}_pieces_at_end"), |b| {
            b.iter(|| black_box(black_box(&t).spans(len - 64, 64).len()));
        });
        g.bench_function(format!("{n}_pieces_at_start"), |b| {
            b.iter(|| black_box(black_box(&t).spans(0, 64).len()));
        });
    }
    g.finish();
}

fn bench_locate(c: &mut Criterion) {
    let mut g = c.benchmark_group("piece_locate");
    for n in [64usize, 512, 2048] {
        let t = fragmented(n);
        let len = t.len();
        g.bench_function(format!("{n}_pieces"), |b| {
            b.iter(|| {
                let spans = black_box(&t).spans(len - 64, 64);
                black_box(spans.len())
            });
        });
    }
    g.finish();
}

fn bench_tree_edit(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree_edit");
    let tree = edited_tree(10_000);
    let at = tree.len() / 2;
    g.bench_function("10k_edits_overwrite", |b| {
        b.iter_batched_ref(
            || tree.clone(),
            |t| {
                t.overwrite(at, &[0x11, 0x22]).unwrap();
                black_box(t.len())
            },
            BatchSize::PerIteration,
        );
    });
    g.finish();
}

fn bench_edit(c: &mut Criterion) {
    let mut g = c.benchmark_group("piece_overwrite");
    for n in [64usize, 512, 2048] {
        g.bench_function(format!("{n}_pieces"), |b| {
            b.iter_batched(
                || fragmented(n),
                |mut t| {
                    let at = t.len() / 2;
                    t.overwrite(at, &[0x11, 0x22]).unwrap();
                    black_box(t.piece_count())
                },
                BatchSize::SmallInput,
            );
        });
    }
    g.finish();
}

#[allow(missing_docs)]
mod bench_entry {
    use super::*;
    criterion_group!(
        benches,
        bench_locate,
        bench_edit,
        bench_tree_locate,
        bench_tree_edit
    );
}

criterion_main!(bench_entry::benches);
