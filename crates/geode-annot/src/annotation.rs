//! 标注值、稳定 id 与负载类型。
//!
//! DESIGN: 00 §5.3 / 05 §4.7
//!
//! [`Annotation`] 是索引里唯一被存储的实体；[`AnnotationIndex`](crate::AnnotationIndex)
//! 只把 `(id, range, layer, priority)` 复制进排序结构，负载留在 id 槽位里，查询返回
//! [`AnnotationId`] 后再按需取用。这样排序热路径不会搬运 `String`。

use crate::layer::Layer;
use core::fmt;
use geode_core::ByteRange;

/// 标注的稳定 id。
///
/// `0` 保留给"未分配"占位；[`AnnotationIndex::insert`](crate::AnnotationIndex::insert)
/// 会覆盖为从 1 开始的新 id。id 永不复用，删除后旧 id 永久失效（查询返回空、`get`
/// 返回 `None`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnnotationId(pub u64);

impl AnnotationId {
    /// 未分配占位 id。
    pub const UNASSIGNED: Self = Self(0);

    /// 是否为未分配占位。
    #[inline]
    pub const fn is_unassigned(self) -> bool {
        self.0 == 0
    }

    /// 取出裸值。
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for AnnotationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ann#{}", self.0)
    }
}

/// sRGB 颜色（0..=255，alpha 0..=255）。
///
/// 不依赖 `egui::Color32`：`geode-annot` 位于 L0，不能引入 GUI 类型。渲染层负责
/// 把它转成 `Color32`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// 红色通道。
    pub r: u8,
    /// 绿色通道。
    pub g: u8,
    /// 蓝色通道。
    pub b: u8,
    /// alpha 通道（255 = 不透明）。
    pub a: u8,
}

impl Rgba {
    /// 构造 RGBA。
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// 构造不透明 RGB。
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

/// 标注负载：语义层要画什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationKind {
    /// 纯文本标注（tooltip、Inspector 提示等）。
    Text(String),
    /// 前景/背景样式；`None` 表示沿用该层或主题的默认值。
    Style {
        /// 前景色。
        fg: Option<Rgba>,
        /// 背景色。
        bg: Option<Rgba>,
    },
    /// 无负载的标记（书签、断点、装饰锚点等）。
    Marker,
}

impl AnnotationKind {
    /// 是否是无负载标记。
    pub const fn is_marker(&self) -> bool {
        matches!(self, Self::Marker)
    }
}

/// 一条标注。
///
/// `id` 由索引在插入时分配；通过 [`Annotation::new`] 构造时是
/// [`AnnotationId::UNASSIGNED`]，插入后索引会把它改成新 id。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    /// 字节区间（半开）。
    pub range: ByteRange,
    /// 图层。
    pub layer: Layer,
    /// 同层覆盖顺序，越大越靠上。
    pub priority: u8,
    /// 稳定 id；插入前是占位 0。
    pub id: AnnotationId,
    /// 负载。
    pub payload: AnnotationKind,
}

impl Annotation {
    /// 构造一条未分配 id 的标注；`insert` 时由索引赋 id。
    pub fn new(range: ByteRange, layer: Layer, priority: u8, payload: AnnotationKind) -> Self {
        Self {
            range,
            layer,
            priority,
            id: AnnotationId::UNASSIGNED,
            payload,
        }
    }

    /// 构造一条显式指定 id 的标注。
    ///
    /// 仅用于反序列化/调试等"id 已知"的场景；`AnnotationIndex::insert` 仍会分配新 id
    /// 并覆盖此字段，以保证索引内 id 唯一。
    pub fn with_id(
        id: AnnotationId,
        range: ByteRange,
        layer: Layer,
        priority: u8,
        payload: AnnotationKind,
    ) -> Self {
        Self {
            range,
            layer,
            priority,
            id,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::Offset;

    #[test]
    fn new_starts_unassigned() {
        let ann = Annotation::new(
            ByteRange::from_len(Offset::new(1), 2).unwrap(),
            Layer::Search,
            7,
            AnnotationKind::Text("hit".into()),
        );
        assert_eq!(ann.id, AnnotationId::UNASSIGNED);
        assert!(ann.id.is_unassigned());
        assert!(!ann.payload.is_marker());
    }

    #[test]
    fn rgba_constructors_are_opaque_by_default() {
        assert_eq!(Rgba::rgb(1, 2, 3), Rgba::new(1, 2, 3, 255));
        assert_eq!(Rgba::new(1, 2, 3, 4).a, 4);
    }
}
