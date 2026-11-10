//! 标注区间索引：整个 hex 视图的**唯一高亮数据源**。
//!
//! DESIGN: 00 §5.3 / 05 §4.7
//!
//! 本 crate 是 L0 叶子之上的纯 CPU 层：只依赖 `geode-core`，不碰 GUI、Provider 或
//! Buffer。所有视觉高亮（值分区、搜索命中、结构区间、选区、书签、diff、YARA、
//! AI 推断、游标）都先变成 [`Annotation`]，再由 [`AnnotationIndex`] 按区间查询。
//!
//! # 数据结构
//!
//! 按 `start` 排序的**分块平坦数组**：每块至多 128 条，块内保持 `(start, id)` 有序；
//! 每块缓存 `max_end`，块索引之上是一棵隐式线段树（`max_end` 前缀最大值的分块版本）。
//! 查询时先用 `start` 二分定位上界，再沿 `max_end` 剪枝下钻，只扫描可能命中的块。
//! 这避免了两条被文档否决的路径：
//!
//! - `intervaltree` crate：指针追逐、构建慢 3~5 倍；
//! - "二分 + 线性回扫"：最坏 O(n)，点击会卡。
//!
//! 插入与删除均摊 O(块大小 + 块数)（块大小固定，实际接近 O(√n) 以内），
//! 点查询与区间查询为 O(log n + k)，`k` 是命中标注数。
//!
//! # 排序语义
//!
//! 查询结果按 `(Layer::z, priority, id)` 升序返回；`id` 单调递增，因此同层同优先级
//! 时后插入者排在后面（渲染时压在上面）。[`AnnotationIndex::deepest_at`] 取这个
//! 全序的最后一个，即 z 最高、同层 priority 最高、再同 id 最大的标注。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod annotation;
pub mod index;
pub mod layer;

pub use annotation::{Annotation, AnnotationId, AnnotationKind, Rgba};
pub use index::{AnnotationIndex, PointArg};
pub use layer::Layer;
