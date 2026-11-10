//! `save_1gb`：1 GiB 全量保存的吞吐（00 §7：>500 MB/s）。
//!
//! DESIGN: 21 §5.2、18 §10.2 P4、00 §7
//!
//! 目的文件放在 `std::env::temp_dir()` 下的独立路径（带 pid，避免并发/残留），
//! 内容由 [`PatternSource`] 现场生成——不占 1 GiB 内存，也避免把"造数据"的
//! 时间算进保存吞吐。每轮都是"替换已有目标"的真实路径（含 `.bak` 硬链接）。
//! 结束后（含 panic）由 [`TempDest`] 的 `Drop` 清理目标与 `.bak`。
//!
//! `cargo bench -p geode-bench --bench save -- --test` 走 8 MiB 冒烟路径：
//! 只验证管线与清理，不产生吞吐数字；完整 1 GiB 用普通 `cargo bench`。

#![allow(missing_docs)] // criterion 宏生成的 item 无语义可写

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use geode_buffer::save::save_provider_atomic;
use geode_core::{ByteRange, Offset, ProviderId};
use geode_provider::{Caps, Provider, ProviderError, ReadReport, SizeInfo};
use std::hint::black_box;
use std::path::PathBuf;

/// 保存规模：1 GiB（与 00 §7 的预算行一致）。
const SAVE_LEN: u64 = 1 << 30;

/// `--test` 冒烟模式只写 8 MiB：验证管线可跑通，不在磁盘紧张的 CI 上强求 1 GiB。
const SMOKE_LEN: u64 = 8 << 20;

/// 本次运行的保存规模：criterion `--test`（空跑）缩小到 [`SMOKE_LEN`]。
fn bench_len() -> u64 {
    if std::env::args().any(|a| a == "--test") {
        SMOKE_LEN
    } else {
        SAVE_LEN
    }
}

/// 现场生成字节的数据源：只填一块内存缓冲，不持有 1 GiB 数据。
#[derive(Debug)]
struct PatternSource {
    len: u64,
}

impl Provider for PatternSource {
    fn id(&self) -> ProviderId {
        ProviderId::new(0x5341_5645) // "SAVE"
    }

    fn display_name(&self) -> String {
        "bench: 1 GiB 生成源".into()
    }

    fn caps(&self) -> Caps {
        Caps::READ_ONLY_STABLE
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.len)
    }

    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        let start = at.get();
        if start >= self.len || buf.is_empty() {
            return Ok(ReadReport {
                filled: 0,
                holes: 0,
                eof_hit: true,
            });
        }
        let remaining = self.len - start;
        let n = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        // 固定图案：内容本身不重要，只要求是真实的 1 GiB 写入。
        buf[..n].fill(0xA5);
        Ok(ReadReport {
            filled: n as u64,
            holes: 0,
            eof_hit: start + n as u64 >= self.len,
        })
    }

    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        None
    }
}

/// 临时目标文件：Drop 时删除目标与 `.bak`，保证 bench 不留垃圾。
struct TempDest {
    dest: PathBuf,
}

impl TempDest {
    fn new() -> Self {
        let dest =
            std::env::temp_dir().join(format!("geode-bench-save-1gb-{}.bin", std::process::id()));
        // pid 复用时可能残留上一次的文件，先清掉，确保测量从确定状态开始。
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(dest.with_extension("bak"));
        Self { dest }
    }
}

impl Drop for TempDest {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.dest);
        let _ = std::fs::remove_file(self.dest.with_extension("bak"));
    }
}

fn bench_save_1gb(c: &mut Criterion) {
    let len = bench_len();
    let src = PatternSource { len };
    let dest = TempDest::new();

    // 预热同时让目标存在：计时的是"替换已有文件"路径（会建 .bak 硬链接）。
    save_provider_atomic(&src, &dest.dest).expect("预热保存失败");

    let mut group = c.benchmark_group("save_1gb");
    // 每轮 1 GiB，默认 100 样本会写上百 GiB：10 个样本已足够看吞吐。
    group.sample_size(10);
    group.throughput(Throughput::Bytes(len));
    group.bench_function("save_provider_atomic", |b| {
        b.iter(|| {
            let stats = save_provider_atomic(&src, &dest.dest).expect("保存失败");
            black_box(stats);
        });
    });
    group.finish();
}

criterion_group!(benches, bench_save_1gb);
criterion_main!(benches);
