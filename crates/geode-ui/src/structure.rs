//! 结构树的**纯模型**（不碰 egui）。
//!
//! DESIGN: 02 §7.3（`Node`）、05 §4.7（`Layer::Structure`）、25 §4（结构面板线框）、
//! 26 §3（行高与缩进栅格）。
//!
//! 为什么单独拆出来：结构树的展开态、可见行、`offset → path` 反查都是**可测的纯逻辑**，
//! 而绘制只在最后一步发生。把它塞进 `egui::Ui` 里写，就只能靠人眼看截图验证。
//!
//! 三条硬规则：
//! 1. **行是「展开态的投影」，不是缓存**。`rows()` 每次按当前 `expanded` 现算——
//!    树最多几万个节点，一次遍历是微秒级；反过来维护一份增量行表，一定会和展开态打架。
//! 2. **路径是唯一键**。`Node::path` 由 `push_child` 生成，天然唯一；不用「第几个孩子」
//!    这种下标键，否则重解析后行会错位。
//! 3. **`reveal()` 会改展开态**。从 hex 视图点一个字节跳到结构树，必须把祖先链全展开，
//!    否则"跳过去了但看不见"。

use std::collections::HashSet;

use geode_bspec::node::Node;
use geode_bspec::value::Value;

/// 一行（已经按展开态投影好，可直接交给绘制层）。
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    /// 缩进层级，根为 0。
    pub depth: u16,
    /// 节点路径（唯一键）。
    pub path: String,
    /// 字段名。
    pub name: String,
    /// 类型名。
    pub type_name: String,
    /// 起始偏移。
    pub offset: u64,
    /// 字节长度。
    pub size: u64,
    /// 标量值的显示文本；复合节点为 `None`。
    pub value: Option<String>,
    /// 是否有子节点（决定要不要画三角）。
    pub has_children: bool,
    /// 当前是否展开。
    pub expanded: bool,
    /// 节点上的诊断条数（>0 时行尾显示角标）。
    pub error_count: u32,
}

impl TreeRow {
    /// `offset` 的显示文本（`0x0000_1A2B` 形式，固定 8 位以上）。
    #[must_use]
    pub fn offset_text(&self) -> String {
        if self.offset > u32::MAX as u64 {
            format!(
                "0x{:08X}_{:08X}",
                self.offset >> 32,
                self.offset & 0xFFFF_FFFF
            )
        } else {
            format!("0x{:08X}", self.offset)
        }
    }

    /// `size` 的显示文本（带千分位不在这一层做，交给 i18n）。
    #[must_use]
    pub fn size_text(&self) -> String {
        self.size.to_string()
    }
}

/// 结构树模型：持有一棵 `Node` 树 + 展开态。
#[derive(Debug, Clone)]
pub struct StructureTree {
    root: Node,
    expanded: HashSet<String>,
    /// 是否显示 `#[hidden]` 字段（默认 `false`）。
    show_hidden: bool,
}

impl StructureTree {
    /// 用根节点建树。默认展开根，收起全部后代（大结构体全展开会滚不完）。
    #[must_use]
    pub fn new(root: Node) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.path.clone());
        Self {
            root,
            expanded,
            show_hidden: false,
        }
    }

    /// 根节点。
    #[must_use]
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// 根节点路径。
    #[must_use]
    pub fn root_path(&self) -> &str {
        &self.root.path
    }

    /// 是否显示 `#[hidden]` 字段。
    #[must_use]
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// 设置是否显示 `#[hidden]` 字段。
    pub fn set_show_hidden(&mut self, on: bool) {
        self.show_hidden = on;
    }

    /// 某路径是否展开。
    #[must_use]
    pub fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }

    /// 展开/收起某路径。返回切换后的状态；路径不存在返回 `None`。
    pub fn toggle(&mut self, path: &str) -> Option<bool> {
        if !self.contains(path) {
            return None;
        }
        if self.expanded.remove(path) {
            Some(false)
        } else {
            self.expanded.insert(path.to_string());
            Some(true)
        }
    }

    /// 显式设置展开态。路径不存在返回 `false`。
    pub fn set_expanded(&mut self, path: &str, on: bool) -> bool {
        if !self.contains(path) {
            return false;
        }
        if on {
            self.expanded.insert(path.to_string());
        } else {
            self.expanded.remove(path);
        }
        true
    }

    /// 全部展开（含根）。
    pub fn expand_all(&mut self) {
        for_each_path(&self.root, &mut |p| {
            self.expanded.insert(p.to_string());
        });
    }

    /// 全部收起（保留根展开，否则整棵树只剩一行）。
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.expanded.insert(self.root.path.clone());
    }

    /// 把 `offset` 所在的**最深**节点路径找出来，并把祖先链全部展开。
    ///
    /// 用于 hex → 结构树的双向联动（05 §4.8）。没有任何节点覆盖该偏移时返回 `None`。
    pub fn reveal(&mut self, offset: u64) -> Option<String> {
        let mut best: Option<(u16, String)> = None;
        collect_deepest(&self.root, offset, 0, &mut best);
        let (_, path) = best?;
        // 展开从根到该节点的全部祖先。
        let mut prefix = String::new();
        for (i, seg) in path.split('.').enumerate() {
            if i == 0 {
                prefix.push_str(seg);
            } else {
                prefix.push('.');
                prefix.push_str(seg);
            }
            self.expanded.insert(prefix.clone());
        }
        Some(path)
    }

    /// 只会返回**当前可见**的行（父节点收起时其子树不出现）。
    #[must_use]
    pub fn rows(&self) -> Vec<TreeRow> {
        let mut out = Vec::new();
        self.push_rows(&self.root, 0, &mut out);
        out
    }

    /// 可见行里某路径的下标。
    #[must_use]
    pub fn row_index(&self, path: &str) -> Option<usize> {
        self.rows().iter().position(|r| r.path == path)
    }

    /// 路径是否存在（含根）。
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        find(&self.root, path).is_some()
    }

    /// 按路径取节点。
    #[must_use]
    pub fn node(&self, path: &str) -> Option<&Node> {
        find(&self.root, path)
    }

    fn push_rows(&self, node: &Node, depth: u16, out: &mut Vec<TreeRow>) {
        if !self.show_hidden && is_hidden(node) && depth > 0 {
            return;
        }
        let has_children = !node.children.is_empty();
        let expanded = self.expanded.contains(&node.path);
        out.push(TreeRow {
            depth,
            path: node.path.clone(),
            name: node.name.clone(),
            type_name: node.type_name.clone(),
            offset: node.offset,
            size: node.size,
            value: node.value.as_ref().map(value_text),
            has_children,
            expanded,
            error_count: u32::try_from(node.errors.len()).unwrap_or(u32::MAX),
        });
        if has_children && expanded {
            for c in &node.children {
                self.push_rows(c, depth + 1, out);
            }
        }
    }
}

fn is_hidden(node: &Node) -> bool {
    node.attrs.iter().any(|(k, _)| k == "hidden")
}

fn find<'a>(node: &'a Node, path: &str) -> Option<&'a Node> {
    if node.path == path {
        return Some(node);
    }
    node.children.iter().find_map(|c| find(c, path))
}

fn for_each_path(node: &Node, f: &mut impl FnMut(&str)) {
    f(&node.path);
    for c in &node.children {
        for_each_path(c, f);
    }
}

fn collect_deepest(node: &Node, offset: u64, depth: u16, best: &mut Option<(u16, String)>) {
    let end = node.offset.saturating_add(node.size);
    if offset < node.offset || offset >= end {
        return;
    }
    let better = match best {
        Some((d, _)) => depth >= *d,
        None => true,
    };
    if better {
        *best = Some((depth, node.path.clone()));
    }
    for c in &node.children {
        collect_deepest(c, offset, depth + 1, best);
    }
}

/// 标量值的显示文本。长字节串截断——结构树是"看形状"的地方，不是 dump 的地方。
#[must_use]
pub fn value_text(v: &Value) -> String {
    match v {
        Value::UInt(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => {
            if f.is_nan() {
                "NaN".to_string()
            } else if f.is_infinite() {
                if *f > 0.0 { "inf" } else { "-inf" }.to_string()
            } else {
                f.to_string()
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => {
            let mut t = String::with_capacity(s.len() + 2);
            t.push('"');
            for ch in s.chars().take(64) {
                match ch {
                    '\n' => t.push_str("\\n"),
                    '\r' => t.push_str("\\r"),
                    '\t' => t.push_str("\\t"),
                    c => t.push(c),
                }
            }
            if s.chars().count() > 64 {
                t.push('…');
            }
            t.push('"');
            t
        }
        Value::Bytes(b) => {
            let shown: Vec<String> = b.iter().take(12).map(|x| format!("{x:02X}")).collect();
            if b.len() > 12 {
                format!("[{} … {} B]", shown.join(" "), b.len())
            } else {
                format!("[{}]", shown.join(" "))
            }
        }
        Value::None => "—".to_string(),
    }
}
