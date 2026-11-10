//! `edit_random` / `undo_all`：piece table 随机编辑与全量撤销。
//!
//! DESIGN: 21 §5.2（B3）、18 §10.2 P4、00 §7、M1 验收②
//!
//! 两个 bench 共用同一份**固定种子**的编辑计划：64 MiB 内存源上 10,000 步
//! 覆盖/插入/删除混合编辑。计划在计时外生成，计时只包含 `Editor` 的编辑与
//! `undo_all` 调用本身；字节相等检查同样放在计时之外。

#![allow(missing_docs)] // criterion 宏生成的 item 无语义可写

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use geode_buffer::edit::Editor;
use geode_buffer::piece::PieceTable;
use geode_provider::{MemoryProvider, Provider};
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 内存源大小：64 MiB（与 21 §5.2 B3 的输入规模一致）。
const SOURCE_LEN: usize = 64 << 20;
/// 随机编辑步数（M1 验收②）。
const EDITS: usize = 10_000;
/// 固定种子：保证每次跑的编辑序列完全一致，数字可复现。
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
/// 每步载荷上限：overwrite/insert 的 1~64 字节。
const MAX_PAYLOAD: u64 = 64;

/// 极简 xorshift64：只用于生成可复现的编辑计划，不是密码学随机数。
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// `[0, n)`；调用方保证 `n > 0`。
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 56) as u8
    }
}

/// 预生成的一步编辑。
enum EditOp {
    Overwrite { at: u64, data: Vec<u8> },
    Insert { at: u64, data: Vec<u8> },
    Remove { at: u64, len: u64 },
}

/// 用影子长度生成 10,000 步混合编辑；位置与长度在计划阶段就固定。
fn edit_plan(initial_len: u64) -> Vec<EditOp> {
    let mut rng = XorShift64::new(SEED);
    let mut shadow_len = initial_len;
    let mut plan = Vec::with_capacity(EDITS);

    for _ in 0..EDITS {
        // 50% 覆盖 / 25% 插入 / 25% 删除；空文档只能插入。
        let roll = rng.below(100);
        let kind = if shadow_len == 0 {
            1
        } else if roll < 50 {
            0
        } else if roll < 75 {
            1
        } else {
            2
        };

        match kind {
            0 => {
                let at = rng.below(shadow_len + 1);
                let len = 1 + rng.below(MAX_PAYLOAD);
                let data = (0..len).map(|_| rng.byte()).collect();
                // 覆盖越过 EOF 等价于追加，影子长度随之增长。
                shadow_len = shadow_len.max(at + len);
                plan.push(EditOp::Overwrite { at, data });
            }
            1 => {
                let at = rng.below(shadow_len + 1);
                let len = 1 + rng.below(MAX_PAYLOAD);
                let data = (0..len).map(|_| rng.byte()).collect();
                shadow_len += len;
                plan.push(EditOp::Insert { at, data });
            }
            _ => {
                // 删除至少 1 字节：位置落在 [0, len-1]。
                let at = rng.below(shadow_len);
                let max = (shadow_len - at).min(MAX_PAYLOAD);
                let len = 1 + rng.below(max);
                shadow_len -= len;
                plan.push(EditOp::Remove { at, len });
            }
        }
    }

    plan
}

/// 造 64 MiB 内存源；返回原文副本（撤销后逐字节比对用）与共享 Provider。
fn source() -> (Vec<u8>, Arc<dyn Provider>) {
    let mut data = vec![0u8; SOURCE_LEN];
    let mut x = 0x0BAD_F00D_DEAD_BEEFu64;
    for b in &mut data {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    let provider: Arc<dyn Provider> =
        Arc::new(MemoryProvider::new("geode-bench-edit", data.clone()));
    (data, provider)
}

/// 建一个指向 64 MiB 源、空撤销历史的编辑会话。
fn new_editor(src: &Arc<dyn Provider>) -> Editor {
    Editor::new(PieceTable::from_source(SOURCE_LEN as u64), Arc::clone(src))
}

fn apply(editor: &mut Editor, op: &EditOp) {
    let result = match op {
        EditOp::Overwrite { at, data } => editor.overwrite(*at, data),
        EditOp::Insert { at, data } => editor.insert(*at, data),
        EditOp::Remove { at, len } => editor.remove(*at, *len),
    };
    result.expect("编辑计划由同一影子长度生成，必然合法");
}

/// 逐条执行计划，记录每一步耗时（含 `Instant::now` 的固有开销）。
fn run_timed(editor: &mut Editor, plan: &[EditOp]) -> Vec<Duration> {
    let mut durations = Vec::with_capacity(plan.len());
    for op in plan {
        let started = Instant::now();
        apply(editor, op);
        durations.push(started.elapsed());
        black_box(editor.len());
    }
    durations
}

/// 最近秩 p99：`ceil(0.99 × n)` 名（1 起）。
fn p99(durations: &mut [Duration]) -> Duration {
    durations.sort_unstable();
    let rank = durations.len().saturating_mul(99).div_ceil(100).max(1);
    durations[rank - 1]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

/// 一次完整 10,000 步编辑：返回总耗时与单步 p99。
fn measure_edits(src: &Arc<dyn Provider>, plan: &[EditOp]) -> (Duration, Duration) {
    let mut editor = new_editor(src);
    let started = Instant::now();
    let mut durations = run_timed(&mut editor, plan);
    let elapsed = started.elapsed();
    let step_p99 = p99(&mut durations);
    black_box(editor.len());
    (elapsed, step_p99)
}

fn bench_edit_random(c: &mut Criterion) {
    let (_, src) = source();
    let plan = edit_plan(SOURCE_LEN as u64);

    // 摘要运行：真实数字只统计编辑本身，计划生成与建表不计入。
    let (elapsed, step_p99) = measure_edits(&src, &plan);
    eprintln!(
        "edit_random/64MiB: {EDITS} 步总耗时 {:.3} ms，单步 p99 {:.1} µs（{:.3} ms）",
        ms(elapsed),
        us(step_p99),
        ms(step_p99)
    );

    let mut group = c.benchmark_group("edit_random");
    group.sample_size(10);
    group.throughput(Throughput::Elements(EDITS as u64));
    group.bench_function("64MiB", |b| {
        b.iter_custom(|iters| {
            let mut measured = Duration::ZERO;
            for _ in 0..iters {
                // 建会话在计时外；每轮编辑都会改表，无法复用。
                let mut editor = new_editor(&src);
                let started = Instant::now();
                let durations = run_timed(&mut editor, &plan);
                measured += started.elapsed();
                black_box(editor.len());
                // p99 的排序与 Vec 释放都不计入 criterion 的 measured 时间。
                black_box(&durations);
            }
            measured
        });
    });
    group.finish();
}

/// 一次 `undo_all`：返回总耗时、撤销步数、字节是否与原文相等。
fn measure_undo_all(
    src: &Arc<dyn Provider>,
    plan: &[EditOp],
    original: &[u8],
) -> (Duration, usize, bool) {
    let mut editor = new_editor(src);
    for op in plan {
        apply(&mut editor, op);
    }
    let started = Instant::now();
    let undone = editor.undo_all();
    let elapsed = started.elapsed();
    let equal = editor.materialize() == original;
    (elapsed, undone, equal)
}

/// 诊断用：逐步 `undo()` 的 p99（覆盖 00 §7 "撤销单步 <1ms" 的观察口径）。
fn measure_undo_steps(
    src: &Arc<dyn Provider>,
    plan: &[EditOp],
    original: &[u8],
) -> (Duration, bool) {
    let mut editor = new_editor(src);
    for op in plan {
        apply(&mut editor, op);
    }
    let mut durations = Vec::with_capacity(EDITS);
    for _ in 0..EDITS {
        let started = Instant::now();
        let ok = editor.undo();
        durations.push(started.elapsed());
        assert!(ok, "撤销步数不应提前耗尽");
    }
    let equal = editor.materialize() == original;
    (p99(&mut durations), equal)
}

fn bench_undo_all(c: &mut Criterion) {
    let (original, src) = source();
    let plan = edit_plan(SOURCE_LEN as u64);

    // 摘要运行：10,000 次编辑在计时外，只量 undo_all；相等检查也在计时外。
    let (elapsed, undone, equal) = measure_undo_all(&src, &plan, &original);
    let (step_p99, equal_after_steps) = measure_undo_steps(&src, &plan, &original);
    assert_eq!(undone, EDITS, "撤销预算不应丢弃任何一步");
    assert!(equal, "undo_all 后必须与原文逐字节相等");
    assert!(equal_after_steps, "逐步撤销后必须与原文逐字节相等");
    eprintln!(
        "undo_all/10000: {EDITS} 步撤销总耗时 {:.3} ms（{:.1} µs/步），逐步撤销 p99 {:.1} µs，字节相等={equal}",
        ms(elapsed),
        us(elapsed) / EDITS as f64,
        us(step_p99)
    );

    let mut group = c.benchmark_group("undo_all");
    group.sample_size(10);
    group.throughput(Throughput::Elements(EDITS as u64));
    group.bench_function("10000", |b| {
        b.iter_custom(|iters| {
            let mut measured = Duration::ZERO;
            for _ in 0..iters {
                // 前置 10,000 次编辑属于 setup，不计时。
                let mut editor = new_editor(&src);
                for op in &plan {
                    apply(&mut editor, op);
                }
                let started = Instant::now();
                let undone = editor.undo_all();
                measured += started.elapsed();
                black_box(undone);
                // 相等检查在计时外，且每轮都断言。
                assert_eq!(undone, EDITS, "撤销预算不应丢弃任何一步");
                assert!(
                    editor.materialize() == original,
                    "undo_all 后必须与原文逐字节相等"
                );
            }
            measured
        });
    });
    group.finish();
}

criterion_group!(benches, bench_edit_random, bench_undo_all);
criterion_main!(benches);
