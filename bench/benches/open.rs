//! `open_4gb`：4 GiB 稀疏文件的打开耗时（M0① 的守门 bench）。
//!
//! DESIGN: 21 §5.2、18 §10.2 P4

#![allow(missing_docs)] // criterion 宏生成的 item 无语义可写

use criterion::{Criterion, criterion_group, criterion_main};
use geode_buffer::Buffer;
use geode_provider::FileProvider;
use std::path::PathBuf;

/// 造一个 4 GiB 稀疏文件（不占磁盘，只占 inode）。
fn sparse_4gb() -> PathBuf {
    let path = std::env::temp_dir().join(format!("geode-bench-open-{}.bin", std::process::id()));
    let f = std::fs::File::create(&path).expect("创建临时文件");
    f.set_len(4 << 30).expect("设置稀疏长度");
    path
}

fn bench_open(c: &mut Criterion) {
    let path = sparse_4gb();
    c.bench_function("open_4gb", |b| {
        b.iter(|| {
            let p = FileProvider::open(&path).expect("打开");
            let buf = Buffer::new(Box::new(p));
            assert_eq!(buf.len(), 4 << 30);
            // 只读 16 字节：证明"打开不整读"，但不把读成本算进 open。
            let mut head = [0u8; 16];
            let _ = buf.read(geode_core::Offset::new(0), &mut head);
        })
    });
    let _ = std::fs::remove_file(&path);
}

criterion_group!(benches, bench_open);
criterion_main!(benches);
