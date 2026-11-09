//! `read_64k`：随机偏移 64 KiB 读的延迟分布。
//!
//! DESIGN: 21 §5.2、18 §10.2 P4

#![allow(missing_docs)] // criterion 宏生成的 item 无语义可写

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use geode_buffer::Buffer;
use geode_core::Offset;
use geode_provider::MemoryProvider;

fn bench_read(c: &mut Criterion) {
    const LEN: usize = 64 << 20; // 64 MiB
    let data: Vec<u8> = (0..LEN).map(|i| (i * 31 % 251) as u8).collect();
    let buffer = Buffer::new(Box::new(MemoryProvider::new("bench", data)));

    let mut group = c.benchmark_group("read_64k");
    group.throughput(Throughput::Bytes(64 << 10));
    group.bench_function("sequential", |b| {
        let mut out = vec![0u8; 64 << 10];
        b.iter(|| buffer.read(Offset::new(0), &mut out).expect("读"))
    });
    group.finish();
}

criterion_group!(benches, bench_read);
criterion_main!(benches);
