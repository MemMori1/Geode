//! M2 T2.2 预算 bench：单行格式化 + 比例尺滚动模型。
//!
//! DESIGN: 18 §10.3（每帧 16.6 ms 预算）、05 §4.2（比例尺滚动条）
//!
//! 两个口径：
//! ① `format_row` 对 16 字节/行、100 万行随机数据中的**单行**格式化耗时；
//! ② `ScrollModel::scroll_by` + `thumb` **10 万次**的总耗时。
//! 两者都应远小于 16 ms/帧，bench 启动时会打印一次量级检查。

#![allow(missing_docs)]

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use geode_ui::grid::{Radix, RowFormat, format_row};
use geode_ui::scroll::ScrollModel;

const ROWS: usize = 1_000_000;
const BPR: usize = 16;

/// 用 xorshift 生成 100 万行随机字节（16 MiB），避免引入 rand 依赖。
fn random_data() -> Vec<u8> {
    let mut data = vec![0u8; ROWS * BPR];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for byte in &mut data {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = (state >> 32) as u8;
    }
    data
}

/// 跑一次量级检查并把结果打到 bench 日志里。
fn budget_report(data: &[u8]) {
    let fmt = RowFormat::new(BPR, 2);

    let start = Instant::now();
    for _ in 0..10_000 {
        black_box(format_row(black_box(&data[..BPR]), &fmt));
    }
    let per_row = start.elapsed() / 10_000;

    let mut model = ScrollModel::new(u64::MAX, 60);
    model.top_row = u64::MAX / 2;
    let start = Instant::now();
    for _ in 0..100_000 {
        model.scroll_by(1);
        black_box(model.thumb(800.0));
    }
    let scroll_batch = start.elapsed();

    println!(
        "预算检查: format_row 单行 {:?}（16 ms 的 {:.4}%）；\
         scroll_by+thumb 10 万次 {:?}（16 ms 的 {:.2}%）",
        per_row,
        per_row.as_secs_f64() * 100.0 / 0.016,
        scroll_batch,
        scroll_batch.as_secs_f64() * 100.0 / 0.016,
    );
}

/// 16 字节/行、100 万行随机数据上的单行格式化。
fn bench_format(c: &mut Criterion) {
    let data = random_data();
    budget_report(&data);
    let fmt = RowFormat::new(BPR, 2);

    let mut group = c.benchmark_group("format_row_16b_1m_rows");
    for radix in [Radix::Hex, Radix::Dec, Radix::Oct] {
        group.bench_function(format!("{radix:?}"), |b| {
            b.iter(|| {
                let row = black_box(&data[..BPR]);
                black_box(format_row(row, &fmt.with_radix(radix)))
            });
        });
    }
    group.finish();
}

/// `scroll_by` + `thumb` 各 10 万次。
fn bench_scroll(c: &mut Criterion) {
    let mut model = ScrollModel::new(u64::MAX, 60);
    model.top_row = u64::MAX / 2;

    c.bench_function("scroll_by_plus_thumb_100k", |b| {
        b.iter(|| {
            let mut checksum = 0u64;
            for i in 0..100_000 {
                model.scroll_by(if i % 2 == 0 { 1 } else { -1 });
                let (y, h) = model.thumb(800.0);
                checksum ^= u64::from(y.to_bits()) ^ u64::from(h.to_bits());
            }
            black_box(checksum)
        });
    });

    // 让一次完整的 10 万次调用留在总预算的量级内（失败不 panic，只打印）。
    let start = Instant::now();
    let mut m = ScrollModel::new(u64::MAX, 60);
    for _ in 0..100_000 {
        m.scroll_by(1);
        black_box(m.thumb(800.0));
    }
    println!(
        "预算检查: scroll_by+thumb 10 万次 {:?}（< 16 ms 预算：{}）",
        start.elapsed(),
        start.elapsed() < std::time::Duration::from_millis(16)
    );
}

criterion_group!(benches, bench_format, bench_scroll);
criterion_main!(benches);
