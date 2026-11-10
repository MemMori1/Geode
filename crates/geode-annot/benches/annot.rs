//! `AnnotationIndex` 的 criterion 基准：10 万条标注下的 `query_point` 与 `insert`。
//!
//! 运行：`cargo bench -p geode-annot --bench annot`
//!
//! 数据是确定性的伪随机布局：起点约每 4 字节一个、长度 1..=8、10 层轮转、
//! priority 0..=15。查询点在 400 KiB 范围内轮转，避免固定点命中缓存。

#![allow(missing_docs)]

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use geode_annot::{Annotation, AnnotationIndex, AnnotationKind, Layer};
use geode_core::{ByteRange, Offset};
use std::hint::black_box;
use std::time::Duration;

/// 基础索引规模。
const COUNT: usize = 100_000;
/// 基础索引覆盖的字节跨度。
const SPAN: u64 = 400_000;
/// 插入基准每轮追加的条数。
const INSERT_BATCH: u64 = 1_000;

/// SplitMix64 的简化版，保证基准数据可复现。
fn next_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn build_index(count: usize) -> AnnotationIndex {
    let mut index = AnnotationIndex::with_capacity(count);
    let mut state = 0x5EED_1234_ABCD_0001u64;
    for i in 0..count {
        let start = (i as u64) * 4 + next_u64(&mut state) % 3;
        let len = 1 + next_u64(&mut state) % 8;
        let layer = Layer::ALL[(next_u64(&mut state) as usize) % Layer::COUNT];
        let priority = (next_u64(&mut state) % 16) as u8;
        let range =
            ByteRange::from_len(Offset::new(start), len).expect("基准数据保证 start + len 不溢出");
        index.insert(Annotation::new(
            range,
            layer,
            priority,
            AnnotationKind::Marker,
        ));
    }
    index
}

fn bench_query_point(c: &mut Criterion) {
    let index = build_index(COUNT);
    let mut state = 0xC0FF_EE00_1234_5678u64;
    c.bench_function("query_point/100k", |b| {
        b.iter(|| {
            let at = next_u64(&mut state) % SPAN;
            black_box(index.query_point(Offset::new(black_box(at))))
        });
    });
}

fn bench_insert(c: &mut Criterion) {
    let base = build_index(COUNT);
    // 预生成插入点，避免把伪随机数生成算进 insert 的测得时间。
    let starts: Vec<u64> = (0..INSERT_BATCH)
        .map(|k| (k * 2_654_435_761) % SPAN)
        .collect();
    c.bench_function("insert/100k+1k", |b| {
        b.iter_batched(
            || base.clone(),
            |mut index| {
                for &start in &starts {
                    let range = ByteRange::from_len(Offset::new(start), 3).expect("固定区间不溢出");
                    index.insert(Annotation::new(
                        range,
                        Layer::Bookmark,
                        (start % 16) as u8,
                        AnnotationKind::Marker,
                    ));
                }
                black_box(index.len())
            },
            BatchSize::LargeInput,
        );
    });
}

criterion_group! {
    name = annot;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4));
    targets = bench_query_point, bench_insert
}
criterion_main!(annot);
