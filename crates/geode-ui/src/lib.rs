//! 界面层。
//!
//! DESIGN: 05-render-and-hexview.md §4.1~§4.4、26-visual-design.md §3、18 §10.3 S3
//!
//! **M0 只做 S3 spike**：证明"一屏 ~5000 个 quad 用纯 `egui::Painter` 自绘"能否
//! 在 16.6 ms 预算内完成（含 tessellation）。这里刻意不引入 wgpu/winit，也不建窗口——
//! spike 要回答的是**几何 + 图元装配的成本**，不是 GPU 提交成本。
//!
//! 关键约束（05 §3.3 / 26 §3.2）：列位一律 `x = base + n * cell_w` **乘法**，
//! 绝不做浮点累加；否则 1.6e7 像素后会出现可见漂移。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod composite;
pub mod grid;
pub mod scroll;
pub mod selection;
pub mod structure;

pub use grid::{CellMetrics, paint_cells, paint_text_cells};
