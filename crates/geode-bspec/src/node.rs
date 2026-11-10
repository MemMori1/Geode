//! 解析结果的结构树。
//!
//! DESIGN: 02-dsl-spec-draft.md §7.3、05-render-and-hexview.md §4.7（`Layer::Structure`）

use crate::value::Value;

/// 结构树节点。每个节点都必须能回链到真实字节（`offset`/`size`），
/// 否则它不该出现在树里——这是 AI 生成路径上防幻觉的最后一道闸。
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// 字段名（`unknown_0x1A` 表示未命名字节）。
    pub name: String,
    /// 类型名（`u32`、`PNGHeader`、`str` …）。
    pub type_name: String,
    /// 起始偏移。
    pub offset: u64,
    /// 字节长度。
    pub size: u64,
    /// 标量值；复合类型为 `None`。
    pub value: Option<Value>,
    /// 子节点。
    pub children: Vec<Node>,
    /// 属性（`#[color]`、`#[comment]` 等），保留原始文本。
    pub attrs: Vec<(String, String)>,
    /// 该节点上的可恢复诊断（文案键，不是成品句子）。
    pub errors: Vec<String>,
    /// 路径，形如 `header.chunks[3].length`。
    pub path: String,
}

impl Node {
    /// 造一个叶子节点。
    #[must_use]
    pub fn leaf(
        name: impl Into<String>,
        type_name: impl Into<String>,
        offset: u64,
        size: u64,
        value: Option<Value>,
    ) -> Self {
        let name = name.into();
        Self {
            path: name.clone(),
            name,
            type_name: type_name.into(),
            offset,
            size,
            value,
            children: Vec::new(),
            attrs: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// 造一个复合节点（`offset`/`size` 通常覆盖全部子节点）。
    #[must_use]
    pub fn branch(
        name: impl Into<String>,
        type_name: impl Into<String>,
        offset: u64,
        size: u64,
    ) -> Self {
        Self {
            value: None,
            ..Self::leaf(name, type_name, offset, size, None)
        }
    }

    /// 追加子节点，并补全其 `path` 前缀。
    pub fn push_child(&mut self, mut child: Node) {
        let parent = if self.path.is_empty() {
            self.name.clone()
        } else {
            self.path.clone()
        };
        child.path = format!("{parent}.{}", child.name);
        for c in &mut child.children {
            c.path = format!("{}.{}", child.path, c.name);
        }
        self.children.push(child);
    }

    /// 深度优先遍历（含自身）。
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }

    /// 节点总数。
    #[must_use]
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Self::count).sum::<usize>()
    }

    /// 叶子覆盖的字节数（不重叠假设；重叠会被 `verify` 单独报）。
    #[must_use]
    pub fn covered_bytes(&self) -> u64 {
        if self.children.is_empty() {
            self.size
        } else {
            self.children.iter().map(Self::covered_bytes).sum()
        }
    }
}
