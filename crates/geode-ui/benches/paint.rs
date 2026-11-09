//! S3 spike：一屏 ~5000 个 quad 的 `Painter` 自绘 + tessellation 成本。
//!
//! DESIGN: 18 §10.3 S3「纯 Painter 5000 quad/帧」、05 §5.2
//!
//! 判定标准（写死在输出里）：**整帧 < 16.6 ms**（60 fps 预算），目标 < 8 ms 留出
//! 布局/命中的余量。这个 bench 不含 GPU 提交，只测几何生成 + 三角化。

#![allow(missing_docs)]

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use egui::{Pos2, Rect, Vec2};
use geode_ui::{CellMetrics, paint_cells, paint_text_cells};

/// 造一帧：`cols × rows` 个格子，返回（图元数）。
fn frame(ctx: &egui::Context, cols: usize, rows: usize) -> usize {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0))),
        ..Default::default()
    };
    let mut primitives = 0;
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let m = CellMetrics {
                cell_w: 9.2,
                cell_h: 18.0,
                origin: ui.max_rect().min,
                group_gap: 6.0,
                group: 4,
            };
            primitives = paint_cells(ui.painter(), &m, cols, rows);
        });
    });
    let _meshes = ctx.tessellate(out.shapes, out.pixels_per_point);
    primitives
}

/// 文本路径的一帧（16 列 × 68 行 = 1088 格，一屏的典型值）。
fn frame_text(ctx: &egui::Context, cols: usize, rows: usize) -> usize {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0))),
        ..Default::default()
    };
    let mut primitives = 0;
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let m = CellMetrics {
                cell_w: 9.2,
                cell_h: 18.0,
                origin: ui.max_rect().min,
                group_gap: 6.0,
                group: 4,
            };
            primitives = paint_text_cells(ui.painter(), &m, cols, rows);
        });
    });
    let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
    primitives
}

fn bench_paint(c: &mut Criterion) {
    let ctx = egui::Context::default();
    let mut group = c.benchmark_group("painter_quads");
    // 16 列 × 68 行 ≈ 一屏（05 §5.2 的满屏口径）；64 列 × 120 行 ≈ 7680 格 ≈ 15360 图元。
    for (cols, rows) in [(16usize, 68usize), (32, 90), (64, 120)] {
        let prims = frame(&ctx, cols, rows);
        group.bench_with_input(
            BenchmarkId::new(format!("{cols}x{rows}"), prims),
            &prims,
            |b, _| b.iter(|| frame(&ctx, cols, rows)),
        );
    }
    group.bench_function("text_16x68", |b| b.iter(|| frame_text(&ctx, 16, 68)));
    group.finish();
}

criterion_group!(benches, bench_paint);
criterion_main!(benches);
