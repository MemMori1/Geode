//! 标注图层与固定 z 序。
//!
//! DESIGN: 00 §5.3 / 05 §4.7
//!
//! **枚举声明顺序就是 z 序**（低 → 高）。渲染合成、命中排序、主题面板都依赖这一点，
//! 因此禁止重排变体；新增图层只能追加，并同步修改 [`Layer::COUNT`] 与 10 个变体的
//! 契约（16 §4 的主题必须逐一提供颜色）。

use core::fmt;

/// 标注图层，声明顺序即 z 序（低 → 高）。
///
/// 顺序：`Value < Search < Structure < Analysis < Selection < Bookmark < Diff <
/// Yara < Ai < Cursor`，共 10 层。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Layer {
    /// 值类型分区（u8/u16/… 的交替底色）。
    Value = 0,
    /// 搜索命中。
    Search = 1,
    /// BinSpec 结构区间。
    Structure = 2,
    /// 熵段 / 字符串 / 坏区 / 可视化器 hover 等机器侧标注。
    Analysis = 3,
    /// 选区。
    Selection = 4,
    /// 书签。
    Bookmark = 5,
    /// diff 差异。
    Diff = 6,
    /// YARA 命中。
    Yara = 7,
    /// AI 推断结果与置信度。
    Ai = 8,
    /// 游标。
    Cursor = 9,
}

impl Layer {
    /// 图层总数（10）。
    pub const COUNT: usize = 10;

    /// 按 z 序排列的全部图层，供主题面板与测试遍历。
    pub const ALL: [Self; Self::COUNT] = [
        Self::Value,
        Self::Search,
        Self::Structure,
        Self::Analysis,
        Self::Selection,
        Self::Bookmark,
        Self::Diff,
        Self::Yara,
        Self::Ai,
        Self::Cursor,
    ];

    /// z 序（0 = 最底层，9 = 最顶层）。
    #[inline]
    pub const fn z(self) -> u8 {
        self as u8
    }

    /// 稳定的英文名（主题键、日志与调试输出用）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Search => "search",
            Self::Structure => "structure",
            Self::Analysis => "analysis",
            Self::Selection => "selection",
            Self::Bookmark => "bookmark",
            Self::Diff => "diff",
            Self::Yara => "yara",
            Self::Ai => "ai",
            Self::Cursor => "cursor",
        }
    }

    /// 从 z 值还原图层；越界返回 `None`。
    pub const fn from_z(z: u8) -> Option<Self> {
        match z {
            0 => Some(Self::Value),
            1 => Some(Self::Search),
            2 => Some(Self::Structure),
            3 => Some(Self::Analysis),
            4 => Some(Self::Selection),
            5 => Some(Self::Bookmark),
            6 => Some(Self::Diff),
            7 => Some(Self::Yara),
            8 => Some(Self::Ai),
            9 => Some(Self::Cursor),
            _ => None,
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_in_declaration_order() {
        for (i, layer) in Layer::ALL.into_iter().enumerate() {
            assert_eq!(layer.z() as usize, i);
            assert_eq!(Layer::from_z(layer.z()), Some(layer));
            assert_eq!(Layer::ALL[layer.z() as usize], layer);
        }
        assert_eq!(Layer::ALL.len(), Layer::COUNT);
        assert_eq!(Layer::from_z(10), None);
    }

    #[test]
    fn z_order_matches_contract() {
        let order = [
            Layer::Value,
            Layer::Search,
            Layer::Structure,
            Layer::Analysis,
            Layer::Selection,
            Layer::Bookmark,
            Layer::Diff,
            Layer::Yara,
            Layer::Ai,
            Layer::Cursor,
        ];
        assert_eq!(Layer::ALL, order);
        assert!(Layer::Value < Layer::Search);
        assert!(Layer::Ai < Layer::Cursor);
        assert_eq!(order.map(Layer::z), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}
