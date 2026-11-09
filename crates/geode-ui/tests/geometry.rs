//! 网格几何的回归测试：**乘法定位**是硬约束（05 §3.3、26 §3.2）。
//!
//! 这里锁的是"第 n 列的位置只由 n 决定"，而不是"x += cell_w 累加出来的结果"。

use egui::{Pos2, Vec2};
use geode_ui::CellMetrics;

fn metrics() -> CellMetrics {
    CellMetrics {
        cell_w: 9.2,
        cell_h: 18.0,
        origin: Pos2::ZERO,
        group_gap: 6.0,
        group: 4,
    }
}

#[test]
fn position_is_multiplicative_not_accumulated() {
    let m = metrics();
    // 逐列累加（错误做法）与乘法（正确做法）在第 100 万列会出现可见差异。
    let mut acc = 0.0f32;
    for _ in 0..1_000_000 {
        acc += m.cell_w;
    }
    let by_mul = m.pos(1_000_000, 0).x;
    assert!(
        (by_mul - acc).abs() > 0.5,
        "如果两者相等，说明这个测试失去意义：{by_mul} vs {acc}"
    );
    // 乘法的结果必须严格等于 f32 的一次乘法。
    assert_eq!(
        by_mul,
        1_000_000.0f32 * m.cell_w + (1_000_000 / 4) as f32 * m.group_gap
    );
}

#[test]
fn row_and_group_offsets() {
    let m = metrics();
    assert_eq!(m.pos(0, 3).y, 54.0);
    // 第 4 列进入下一组，多一个 group_gap。
    let g0 = m.pos(3, 0).x;
    let g1 = m.pos(4, 0).x;
    assert!((g1 - g0 - m.cell_w - m.group_gap).abs() < 1e-4);
    assert_eq!(
        m.rect(2, 5),
        egui::Rect::from_min_size(m.pos(2, 5), Vec2::new(m.cell_w, m.cell_h))
    );
}
