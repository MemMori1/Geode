//! 计数树（T1.2b 的索引层雏形）。DESIGN: 04 §3.2、ADR-0001
//!
//! 目标只有一个：**按字节偏移定位不再线性扫描**。
//!
//! 结构：叶子装 `Piece`（≤64 个），内部节点装子节点（≤64 个），每个节点缓存
//! `bytes`（子树总字节数）。定位与 `spans` 都按 `bytes` 下降，复杂度 O(高度)，而
//! 高度由"超 64 就分裂"维持。
//!
//! **与文档的偏差（必须记住）**：这不是严格 B-tree——允许内部节点的子节点高度不一致，
//! 也允许节点欠载。换来的是 `split`/`join` 不需要 borrow/merge 那一套，先让 O(log n)
//! 的读路径跑起来；等 S1 的碎片化结论出来再决定是否收紧。ADR-0019 记了这笔账。

use crate::hydrate::{HydrateError, MIN_PLACEHOLDER};
use crate::piece::{EditError, MAX_LITERAL, Piece, Span};
use geode_provider::Provider;

/// 每个节点的最大子项数（叶：piece 数；内部节点：子节点数）。
pub const MAX_KIDS: usize = 64;

#[derive(Clone, Debug)]
enum Kind {
    Leaf(Vec<Piece>),
    Inner(Vec<Node>),
}

#[derive(Clone, Debug)]
struct Node {
    kind: Kind,
    bytes: u64,
    /// 叶为 0；内部节点为 `1 + max(子节点高度)`。
    height: u16,
}

impl Node {
    fn bytes(&self) -> u64 {
        self.bytes
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn leaf(pieces: Vec<Piece>) -> Option<Self> {
        let bytes = pieces.iter().map(Piece::len).sum();
        if pieces.is_empty() {
            return None;
        }
        Some(Self {
            kind: Kind::Leaf(pieces),
            bytes,
            height: 0,
        })
    }

    fn inner(kids: Vec<Node>) -> Option<Self> {
        if kids.is_empty() {
            return None;
        }
        let bytes = kids.iter().map(Node::bytes).sum();
        let height = 1 + kids.iter().map(Node::height).max().unwrap_or(0);
        Some(Self {
            kind: Kind::Inner(kids),
            bytes,
            height,
        })
    }
}

/// 把一串 piece 打包成合法节点（超过 64 就分层）。
fn pack_pieces(pieces: Vec<Piece>) -> Option<Node> {
    if pieces.len() <= MAX_KIDS {
        return Node::leaf(pieces);
    }
    let leaves: Vec<Node> = pieces
        .chunks(MAX_KIDS)
        .filter_map(|c| Node::leaf(c.to_vec()))
        .collect();
    pack_nodes(leaves)
}

fn pack_nodes(nodes: Vec<Node>) -> Option<Node> {
    match nodes.len() {
        0 => return None,
        // 单子节点不再包一层“独生子”内部节点：它只会白涨树高，不增加任何分支。
        1 => return nodes.into_iter().next(),
        _ => {}
    }
    if nodes.len() <= MAX_KIDS {
        return Node::inner(nodes);
    }
    let mut groups: Vec<Node> = Vec::new();
    let mut cur: Vec<Node> = Vec::new();
    for n in nodes {
        cur.push(n);
        if cur.len() == MAX_KIDS {
            groups.push(Node::inner(std::mem::take(&mut cur)).expect("非空"));
        }
    }
    if !cur.is_empty() {
        groups.push(Node::inner(cur).expect("非空"));
    }
    pack_nodes(groups)
}

/// 最右 piece（用于 R3 边界合并检查）。
fn rightmost_piece(node: &Node) -> &Piece {
    match &node.kind {
        Kind::Leaf(pieces) => pieces.last().expect("叶子至少有一个 piece"),
        Kind::Inner(kids) => rightmost_piece(kids.last().expect("内部节点至少有一个子节点")),
    }
}

/// 最左 piece（用于 R3 边界合并检查）。
fn leftmost_piece(node: &Node) -> &Piece {
    match &node.kind {
        Kind::Leaf(pieces) => pieces.first().expect("叶子至少有一个 piece"),
        Kind::Inner(kids) => leftmost_piece(kids.first().expect("内部节点至少有一个子节点")),
    }
}

/// 两个相邻 piece 是否满足 R3 的合并条件。
fn can_merge(left: &Piece, right: &Piece) -> bool {
    match (left, right) {
        (
            Piece::Source { start, len },
            Piece::Source {
                start: right_start, ..
            },
        ) => start.saturating_add(*len) == *right_start,
        (Piece::Literal(a), Piece::Literal(b)) => a.len().saturating_add(b.len()) <= MAX_LITERAL,
        _ => false,
    }
}

/// 把 `left` 与 `right` 合成一个 piece；调用方必须先用 [`can_merge`] 保证可合。
fn merge_pieces(left: Piece, right: Piece) -> Piece {
    match (left, right) {
        (Piece::Source { start, len }, Piece::Source { len: rlen, .. }) => Piece::Source {
            start,
            len: len + rlen,
        },
        (Piece::Literal(a), Piece::Literal(b)) => {
            let mut out = Vec::with_capacity(a.len() + b.len());
            out.extend_from_slice(&a);
            out.extend_from_slice(&b);
            Piece::Literal(out.into())
        }
        _ => unreachable!("只有同类且满足条件的相邻 piece 才能合并"),
    }
}

/// 把树内所有 piece 按文档顺序展开（移动所有权）。
fn take_pieces(node: Node, out: &mut Vec<Piece>) {
    match node.kind {
        Kind::Leaf(pieces) => out.extend(pieces),
        Kind::Inner(kids) => {
            for kid in kids {
                take_pieces(kid, out);
            }
        }
    }
}

/// 顺序合并相邻可合 piece；用于整体重写（降级/局部 literal 化）之后恢复 R3。
fn coalesce_pieces(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out = Vec::with_capacity(pieces.len());
    for p in pieces {
        let merge = out.last().is_some_and(|last| can_merge(last, &p));
        if merge {
            let left = out.pop().expect("刚检查过非空");
            out.push(merge_pieces(left, p));
        } else {
            out.push(p);
        }
    }
    out
}

/// 全树重建式合并；只在本来就是 O(n) 的整体操作（降级/defragment）末尾调用。
fn coalesce_all(root: &mut Option<Node>) {
    let Some(node) = root.take() else {
        return;
    };
    let mut pieces = Vec::new();
    take_pieces(node, &mut pieces);
    *root = pack_pieces(coalesce_pieces(pieces));
}

/// 把 `node` 最右 piece 与 `other` 合并（`other` 必须是另一棵树的最左 piece）。
///
/// 只改右边界这一条路径，`node` 的总字节数增加 `other.len()`。
fn merge_rightmost_with(node: &mut Node, other: &Piece) {
    match &mut node.kind {
        Kind::Leaf(pieces) => {
            let last = pieces.last_mut().expect("叶子至少有一个 piece");
            *last = merge_pieces(last.clone(), other.clone());
            node.bytes += other.len();
        }
        Kind::Inner(kids) => {
            let last = kids.last_mut().expect("内部节点至少有一个子节点");
            merge_rightmost_with(last, other);
            node.bytes += other.len();
        }
    }
}

/// 摘掉最左 piece；若所在叶子因此变空，就顺手删掉空叶并重算路径上的 bytes/height。
fn take_leftmost(node: &mut Node) -> Piece {
    match &mut node.kind {
        Kind::Leaf(pieces) => {
            let p = pieces.remove(0);
            node.bytes -= p.len();
            p
        }
        Kind::Inner(kids) => {
            let p = take_leftmost(&mut kids[0]);
            if kids[0].bytes == 0 {
                kids.remove(0);
            }
            node.bytes -= p.len();
            node.height = if kids.is_empty() {
                0
            } else {
                1 + kids.iter().map(Node::height).max().unwrap_or(0)
            };
            p
        }
    }
}

fn take_leftmost_from(root: &mut Option<Node>) -> Piece {
    let node = root.as_mut().expect("调用方保证树非空");
    let p = take_leftmost(node);
    if node.bytes == 0 {
        *root = None;
    }
    p
}

/// 拼接两棵子树，保持"每节点 ≤64 子项"。高度不同时沿较高树的边界下降。
///
/// **R3 边界合并**：拼接前只检查 `a` 最右与 `b` 最左两个 piece；可合就在 `a` 的
/// 右边界原地合并，并摘掉 `b` 的最左 piece。合并只沿边界路径走，不做全树重扫；
/// 因为 `a`、`b` 各自内部已无相邻可合对，检查这一对就恢复了整棵树的 R3 不变量。
fn concat(a: Option<Node>, b: Option<Node>) -> Option<Node> {
    let (mut a, b) = match (a, b) {
        (None, x) | (x, None) => return x,
        (Some(a), Some(b)) => (a, b),
    };
    if can_merge(rightmost_piece(&a), leftmost_piece(&b)) {
        let first = leftmost_piece(&b).clone();
        merge_rightmost_with(&mut a, &first);
        let mut b = Some(b);
        take_leftmost_from(&mut b);
        return concat_raw(Some(a), b);
    }
    concat_raw(Some(a), Some(b))
}

/// `concat` 的结构部分；调用前边界 piece 已经处理过，递归只做节点拼接。
fn concat_raw(a: Option<Node>, b: Option<Node>) -> Option<Node> {
    let (a, b) = match (a, b) {
        (None, x) | (x, None) => return x,
        (Some(a), Some(b)) => (a, b),
    };
    if a.height == b.height {
        return match (a.kind, b.kind) {
            (Kind::Leaf(mut x), Kind::Leaf(y)) => {
                x.extend(y);
                pack_pieces(x)
            }
            (Kind::Inner(mut x), Kind::Inner(y)) => {
                x.extend(y);
                pack_nodes(x)
            }
            _ => unreachable!("同高度的节点必然同类型"),
        };
    }
    if a.height > b.height {
        let Kind::Inner(mut kids) = a.kind else {
            unreachable!("较高节点必然是内部节点")
        };
        let last = kids.pop().expect("内部节点至少有一个子节点");
        if let Some(joined) = concat_raw(Some(last), Some(b)) {
            kids.push(joined);
        }
        pack_nodes(kids)
    } else {
        let Kind::Inner(mut kids) = b.kind else {
            unreachable!("较高节点必然是内部节点")
        };
        let first = kids.remove(0);
        if let Some(joined) = concat_raw(Some(a), Some(first)) {
            kids.insert(0, joined);
        }
        pack_nodes(kids)
    }
}

/// 在 `at` 处把子树切成两半；`at` 必须在 `(0, bytes)` 之间。
fn split(node: Node, at: u64) -> (Option<Node>, Option<Node>) {
    debug_assert!(at > 0 && at < node.bytes, "split 的调用方必须先处理边界");
    match node.kind {
        Kind::Leaf(pieces) => split_leaf(pieces, at),
        Kind::Inner(kids) => {
            let mut pos = 0u64;
            let mut idx = 0usize;
            for (i, k) in kids.iter().enumerate() {
                if at < pos + k.bytes {
                    idx = i;
                    break;
                }
                pos += k.bytes;
            }
            let mut kids = kids;
            if at == pos {
                // 切点正好落在子节点边界上：不需要拆任何节点（proptest 抓到的真 bug）。
                let right = kids.split_off(idx);
                return (pack_nodes(kids), pack_nodes(right));
            }
            let mid = kids.remove(idx);
            let (l, r) = split(mid, at - pos);
            let mut left: Vec<Node> = kids.drain(..idx).collect();
            left.extend(l);
            let right: Vec<Node> = r.into_iter().chain(kids).collect();
            (pack_nodes(left), pack_nodes(right))
        }
    }
}

fn split_leaf(pieces: Vec<Piece>, at: u64) -> (Option<Node>, Option<Node>) {
    let mut left = Vec::new();
    let mut pos = 0u64;
    let mut iter = pieces.into_iter();
    while let Some(p) = iter.next() {
        let next = pos + p.len();
        if at >= next {
            left.push(p);
            pos = next;
            continue;
        }
        let off = at - pos;
        if off == 0 {
            // 切点正好落在 piece 边界上：不要切出零长 piece（树的不变量要求 piece 非空）。
            let mut right = vec![p];
            right.extend(iter);
            return (pack_pieces(left), pack_pieces(right));
        }
        let (l, r) = split_piece(p, off);
        left.push(l);
        let mut right = vec![r];
        right.extend(iter);
        return (pack_pieces(left), pack_pieces(right));
    }
    (pack_pieces(left), None)
}

fn split_piece(p: Piece, off: u64) -> (Piece, Piece) {
    match p {
        Piece::Source { start, len } => (
            Piece::Source { start, len: off },
            Piece::Source {
                start: start + off,
                len: len - off,
            },
        ),
        Piece::Literal(b) => {
            let s = usize::try_from(off).unwrap_or(0);
            (
                Piece::Literal(b[..s].to_vec().into()),
                Piece::Literal(b[s..].to_vec().into()),
            )
        }
    }
}

fn collect_spans<'a>(node: &'a Node, from: u64, end: u64, base: u64, out: &mut Vec<Span<'a>>) {
    if base >= end {
        return;
    }
    match &node.kind {
        Kind::Leaf(pieces) => {
            let mut pos = 0u64;
            for p in pieces {
                let next = pos + p.len();
                if base + pos >= end {
                    break;
                }
                if next <= from {
                    pos = next;
                    continue;
                }
                let skip = from.saturating_sub(pos);
                let at = base + pos + skip;
                out.push(match p {
                    Piece::Source { start, .. } => Span::Source {
                        start: start + skip,
                        len: p.len() - skip,
                        at,
                    },
                    Piece::Literal(b) => Span::Literal {
                        bytes: &b[usize::try_from(skip).unwrap_or(0)..],
                        at,
                    },
                });
                pos = next;
            }
        }
        Kind::Inner(kids) => {
            let mut pos = 0u64;
            for k in kids {
                if base + pos >= end {
                    break;
                }
                if pos + k.bytes <= from {
                    pos += k.bytes;
                    continue;
                }
                collect_spans(k, from.saturating_sub(pos), end, base + pos, out);
                pos += k.bytes;
            }
        }
    }
}

fn walk_pieces_mut(node: &mut Node, f: &mut impl FnMut(&mut Piece)) {
    match &mut node.kind {
        Kind::Leaf(pieces) => pieces.iter_mut().for_each(f),
        Kind::Inner(kids) => kids.iter_mut().for_each(|k| walk_pieces_mut(k, f)),
    }
}

/// 计数树本体。
#[derive(Clone, Debug, Default)]
pub struct PieceTree {
    root: Option<Node>,
    len: u64,
    source_len: u64,
}

impl PieceTree {
    /// 以整段源区间建树（O(1)，只有一个叶子 piece）。
    #[must_use]
    pub fn from_source(len: u64) -> Self {
        let root = if len == 0 {
            None
        } else {
            Node::leaf(vec![Piece::Source { start: 0, len }])
        };
        Self {
            root,
            len,
            source_len: len,
        }
    }

    /// 从任意 piece 列表建树（测试与 hydrate 用）。
    #[must_use]
    pub fn from_pieces(pieces: Vec<Piece>, source_len: u64) -> Self {
        let len = pieces.iter().map(Piece::len).sum();
        Self {
            root: pack_pieces(pieces),
            len,
            source_len,
        }
    }

    /// 文档长度。
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 源长度。
    #[must_use]
    pub fn source_len(&self) -> u64 {
        self.source_len
    }

    /// 树高（叶为 0；空树返回 0）。
    #[must_use]
    pub fn height(&self) -> u16 {
        self.root.as_ref().map_or(0, Node::height)
    }

    /// piece 总数（诊断用）。
    #[must_use]
    pub fn piece_count(&self) -> usize {
        fn walk(n: &Node) -> usize {
            match &n.kind {
                Kind::Leaf(p) => p.len(),
                Kind::Inner(k) => k.iter().map(walk).sum(),
            }
        }
        self.root.as_ref().map_or(0, walk)
    }

    /// 结构不变量自检。
    ///
    /// 返回**全部**发现的错误而不是遇到第一处就停：随机编辑测试里一次拿到完整清单，
    /// 比只看到一条更有用。检查项：
    /// - piece 非空；
    /// - 不存在可合并的相邻 piece（Source 源上连续 / Literal 合并后 ≤64 KiB）；
    /// - 根节点 `bytes` == `len`；
    /// - 每个节点的 `bytes` 等于子节点（叶为 piece）之和；
    /// - 叶 height 为 0，内部节点 height 为 `1 + max(子节点 height)`。
    pub fn check_invariants(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        match &self.root {
            Some(root) => {
                check_node_invariants(root, &mut errors);
                if root.bytes != self.len {
                    errors.push(format!(
                        "根节点 bytes {} != 树长度 {}",
                        root.bytes, self.len
                    ));
                }
                let mut prev = None;
                let mut pos = 0u64;
                check_adjacent_invariants(root, &mut prev, &mut pos, &mut errors);
            }
            None => {
                if self.len != 0 {
                    errors.push(format!("空树长度 {} != 0", self.len));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// 从 `at` 开始沿树下降的顺序游标。
    ///
    /// `at` 超过文档长度时按文档末尾处理（游标立即为空）；否则起点可以落在某个
    /// piece 中间，第一段会从 `at` 对应的位置切开。语义与
    /// [`crate::piece::PieceCursor`] 一致，但定位是 O(高度)，且跨叶不会回头重扫前缀。
    #[must_use]
    pub fn cursor(&self, at: u64) -> TreeCursor<'_> {
        TreeCursor::new(self, at)
    }

    /// 读 `[at, at+len)` 的分段视图。**O(高度 + 段数)**。
    #[must_use]
    pub fn spans(&self, at: u64, len: u64) -> Vec<Span<'_>> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        if at >= self.len {
            return Vec::new();
        }
        let end = at.saturating_add(len).min(self.len);
        let mut all = Vec::new();
        collect_spans(root, at, end, 0, &mut all);
        let mut out = Vec::new();
        for s in all {
            let (s_at, s_len) = match &s {
                Span::Source { at, len, .. } => (*at, *len),
                Span::Literal { bytes, at } => (*at, bytes.len() as u64),
            };
            if s_at >= end {
                break;
            }
            let take = (end - s_at).min(s_len);
            if take == 0 {
                continue;
            }
            out.push(match s {
                Span::Source { start, at, .. } => Span::Source {
                    start,
                    len: take,
                    at,
                },
                Span::Literal { bytes, at } => Span::Literal {
                    bytes: &bytes[..usize::try_from(take).unwrap_or(0)],
                    at,
                },
            });
        }
        out
    }

    /// 覆盖写（可越过 EOF 扩展）。
    pub fn overwrite(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        if at > self.len {
            return Err(EditError::OutOfRange { at, len: self.len });
        }
        let (a, b) = self.take_split(at);
        let (_, c) = split_n(b, data.len() as u64);
        let mid = pack_pieces(chunks(data));
        self.set(concat(concat(a, mid), c));
        Ok(())
    }

    /// 插入。
    pub fn insert(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        if at > self.len {
            return Err(EditError::OutOfRange { at, len: self.len });
        }
        let (a, b) = self.take_split(at);
        self.set(concat(concat(a, pack_pieces(chunks(data))), b));
        Ok(())
    }

    /// 删除。
    pub fn remove(&mut self, at: u64, len: u64) -> Result<(), EditError> {
        let end = at.saturating_add(len);
        if end > self.len {
            return Err(EditError::OutOfRange {
                at: end,
                len: self.len,
            });
        }
        let (a, b) = self.take_split(at);
        let (_, c) = split_n(b, len);
        self.set(concat(a, c));
        Ok(())
    }

    fn take_split(&mut self, at: u64) -> (Option<Node>, Option<Node>) {
        let root = self.root.take();
        let Some(root) = root else {
            return (None, None);
        };
        if at == 0 {
            return (None, Some(root));
        }
        if at >= self.len {
            return (Some(root), None);
        }
        let (a, b) = split(root, at);
        (a, b)
    }

    fn set(&mut self, root: Option<Node>) {
        self.len = root.as_ref().map_or(0, Node::bytes);
        self.root = root;
    }
}

/// 树游标的一层祖先：从 `node` 的第 `child` 个子节点下来，该子节点从 `child_base` 开始。
#[derive(Debug)]
struct Frame<'a> {
    node: &'a Node,
    child: usize,
    /// 当前子节点在文档中的绝对起始偏移。
    child_base: u64,
}

/// 沿计数树下降的顺序游标。
///
/// 从构造时的偏移开始，按文档顺序逐 piece 吐出 [`Span`]；起点可以落在 piece 中间。
/// 内部维护一条根到当前叶的栈，跨叶时向上回溯再下降到下一棵子树——每个树边在
/// 一次完整遍历中最多被上下各走一次，因此 `next_span` 均摊 O(1)。
#[derive(Debug)]
pub struct TreeCursor<'a> {
    /// 根到当前叶的祖先路径。
    path: Vec<Frame<'a>>,
    /// 当前叶；遍历结束时为 `None`。
    leaf: Option<&'a Node>,
    /// 当前叶内下一个要吐的 piece 下标。
    piece: usize,
    /// 当前 piece 内已消费的字节数（起点落在 piece 中间时非 0）。
    skip: u64,
    /// 下一个字节的全局偏移。
    at: u64,
    /// 尚未吐出的字节数。
    remaining: u64,
}

impl<'a> TreeCursor<'a> {
    fn new(tree: &'a PieceTree, at: u64) -> Self {
        let at = at.min(tree.len);
        let remaining = tree.len - at;
        let mut cursor = Self {
            path: Vec::new(),
            leaf: None,
            piece: 0,
            skip: 0,
            at,
            remaining,
        };
        if let Some(root) = &tree.root {
            if remaining > 0 {
                cursor.descend(root, 0, at);
            }
        }
        cursor
    }

    /// 沿 `bytes` 从 `node` 下降到包含全局偏移 `at` 的叶子；`base` 是 `node` 的起点。
    fn descend(&mut self, mut node: &'a Node, mut base: u64, at: u64) {
        loop {
            match &node.kind {
                Kind::Inner(kids) => {
                    let mut idx = 0;
                    let mut child_base = base;
                    for (i, kid) in kids.iter().enumerate() {
                        if at < child_base + kid.bytes {
                            idx = i;
                            break;
                        }
                        child_base += kid.bytes;
                    }
                    debug_assert!(idx < kids.len(), "偏移必然落在某个子节点内");
                    self.path.push(Frame {
                        node,
                        child: idx,
                        child_base,
                    });
                    let next = &kids[idx];
                    node = next;
                    base = child_base;
                }
                Kind::Leaf(pieces) => {
                    let mut pos = base;
                    for (i, p) in pieces.iter().enumerate() {
                        if at < pos + p.len() {
                            self.leaf = Some(node);
                            self.piece = i;
                            self.skip = at - pos;
                            return;
                        }
                        pos += p.len();
                    }
                    unreachable!("偏移必然落在叶子内的某个 piece");
                }
            }
        }
    }

    /// 当前叶吐完后，沿祖先栈找下一棵子树并下降到它的最左叶。
    fn advance_leaf(&mut self) {
        self.leaf = None;
        while let Some(frame) = self.path.pop() {
            let node = frame.node;
            let Kind::Inner(kids) = &node.kind else {
                continue;
            };
            let next = frame.child + 1;
            if next < kids.len() {
                let child_base = frame.child_base + kids[frame.child].bytes;
                self.path.push(Frame {
                    node,
                    child: next,
                    child_base,
                });
                self.descend(&kids[next], child_base, child_base);
                return;
            }
        }
    }

    /// 下一个段落；到末尾返回 `None`。
    pub fn next_span(&mut self) -> Option<Span<'a>> {
        if self.remaining == 0 {
            return None;
        }
        let leaf = self.leaf?;
        let Kind::Leaf(pieces) = &leaf.kind else {
            unreachable!("叶节点必然是 Leaf");
        };
        let skip = self.skip;
        let p = &pieces[self.piece];
        let out = match p {
            Piece::Source { start, .. } => Span::Source {
                start: start + skip,
                len: p.len() - skip,
                at: self.at,
            },
            Piece::Literal(b) => Span::Literal {
                bytes: &b[usize::try_from(skip).unwrap_or(0)..],
                at: self.at,
            },
        };
        let take = p.len() - skip;
        self.at += take;
        self.remaining -= take;
        self.piece += 1;
        self.skip = 0;
        if self.piece >= pieces.len() {
            self.advance_leaf();
        }
        Some(out)
    }

    /// 尚未吐出的字节数。
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// 下一个要吐的字节在文档中的偏移（遍历结束后等于文档长度）。
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.at
    }
}

fn check_node_invariants(node: &Node, errors: &mut Vec<String>) {
    match &node.kind {
        Kind::Leaf(pieces) => {
            if pieces.is_empty() {
                errors.push("空叶子节点".into());
            }
            let sum: u64 = pieces.iter().map(Piece::len).sum();
            if node.bytes != sum {
                errors.push(format!("叶子 bytes {} != piece 总长 {sum}", node.bytes));
            }
            if node.height != 0 {
                errors.push(format!("叶子 height {} != 0", node.height));
            }
            for (i, p) in pieces.iter().enumerate() {
                if p.is_empty() {
                    errors.push(format!("第 {i} 个 piece 长度为 0"));
                }
            }
        }
        Kind::Inner(kids) => {
            if kids.is_empty() {
                errors.push("空内部节点".into());
            }
            let sum: u64 = kids.iter().map(Node::bytes).sum();
            if node.bytes != sum {
                errors.push(format!("内部节点 bytes {} != 子节点之和 {sum}", node.bytes));
            }
            let want_height = 1 + kids.iter().map(Node::height).max().unwrap_or(0);
            if node.height != want_height {
                errors.push(format!(
                    "内部节点 height {} != 1 + max(子节点 height) = {want_height}",
                    node.height
                ));
            }
            for kid in kids {
                check_node_invariants(kid, errors);
            }
        }
    }
}

/// 按文档顺序检查相邻 piece；`pos` 是当前 piece 的全局起始偏移（仅用于报错）。
fn check_adjacent_invariants<'a>(
    node: &'a Node,
    prev: &mut Option<&'a Piece>,
    pos: &mut u64,
    errors: &mut Vec<String>,
) {
    match &node.kind {
        Kind::Leaf(pieces) => {
            for p in pieces {
                if let Some(q) = prev {
                    if can_merge(q, p) {
                        errors.push(format!("偏移 {pos} 处相邻 piece 可合并却未合并"));
                    }
                }
                *prev = Some(p);
                *pos += p.len();
            }
        }
        Kind::Inner(kids) => {
            for kid in kids {
                check_adjacent_invariants(kid, prev, pos, errors);
            }
        }
    }
}

fn split_n(node: Option<Node>, n: u64) -> (Option<Node>, Option<Node>) {
    let Some(node) = node else {
        return (None, None);
    };
    if n == 0 {
        return (None, Some(node));
    }
    if n >= node.bytes {
        return (Some(node), None);
    }
    split(node, n)
}

fn chunks(data: &[u8]) -> Vec<Piece> {
    data.chunks(MAX_LITERAL)
        .map(|c| Piece::Literal(c.to_vec().into()))
        .collect()
}

impl PieceTree {
    /// **R1**：把所有 `< 4 KiB` 的 placeholder 读成 literal（04 §3.3）。
    ///
    /// 字节长度不变，因此节点的 `bytes` 计数不需要重算。
    pub fn demote_small_placeholders(&mut self, src: &dyn Provider) -> Result<usize, HydrateError> {
        let mut demoted = 0usize;
        let mut err = None;
        if let Some(root) = &mut self.root {
            walk_pieces_mut(root, &mut |p| {
                if err.is_some() {
                    return;
                }
                if let Piece::Source { start, len } = *p {
                    if len < MIN_PLACEHOLDER {
                        match crate::hydrate::read_exact(src, start, len) {
                            Ok(buf) => {
                                *p = Piece::Literal(buf.into());
                                demoted += 1;
                            }
                            Err(e) => err = Some(e),
                        }
                    }
                }
            });
        }
        match err {
            Some(e) => {
                // 已转换的部分可能和新邻居形成可合对；即使返回错误也恢复 R3。
                coalesce_all(&mut self.root);
                Err(e)
            }
            None => {
                if demoted > 0 {
                    coalesce_all(&mut self.root);
                }
                Ok(demoted)
            }
        }
    }

    /// 局部 literal 化（04 §4.4 的 `defragment`）。
    pub fn defragment(
        &mut self,
        at: u64,
        len: u64,
        src: &dyn Provider,
    ) -> Result<(), HydrateError> {
        if at.saturating_add(len) > self.len {
            return Err(HydrateError::Edit(EditError::OutOfRange {
                at,
                len: self.len,
            }));
        }
        if len == 0 {
            return Ok(());
        }
        let (a, rest) = self.take_split(at);
        let (mid, c) = split_n(rest, len);
        let Some(mut mid) = mid else { return Ok(()) };
        let mut err = None;
        walk_pieces_mut(&mut mid, &mut |p| {
            if err.is_some() {
                return;
            }
            if let Piece::Source { start, len } = *p {
                match crate::hydrate::read_exact(src, start, len) {
                    Ok(buf) => *p = Piece::Literal(buf.into()),
                    Err(e) => err = Some(e),
                }
            }
        });
        // literal 化只发生在 mid 内部；先把 mid 内部新产生的可合邻对收掉，
        // 再让 concat 只处理 a/mid 与 mid/c 两个边界。
        let mut mid = Some(mid);
        coalesce_all(&mut mid);
        if let Some(e) = err {
            // 读源失败时把已经拆开的树按原字节拼回去，避免把文档留在半截状态。
            self.set(concat(concat(a, mid), c));
            return Err(e);
        }
        self.set(concat(concat(a, mid), c));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn collect_pieces<'a>(node: &'a Node, out: &mut Vec<&'a Piece>) {
        match &node.kind {
            Kind::Leaf(pieces) => out.extend(pieces),
            Kind::Inner(kids) => {
                for kid in kids {
                    collect_pieces(kid, out);
                }
            }
        }
    }

    fn literal_arc(p: &Piece) -> &Arc<[u8]> {
        match p {
            Piece::Literal(b) => b,
            Piece::Source { .. } => panic!("期望 Literal"),
        }
    }

    /// R3 合并只发生在拼接边界：远处/中间的 Literal 不复制，piece 数不增长而 bytes 增长。
    #[test]
    fn tree_coalesce_is_local() {
        let far: Arc<[u8]> = Arc::from(&b"far-away"[..]);
        let middle: Arc<[u8]> = Arc::from(&b"middle"[..]);
        let tail: Arc<[u8]> = Arc::from(&b"0123456789"[..]);
        let mut pieces = vec![
            Piece::Literal(far.clone()),
            Piece::Source { start: 0, len: 100 },
            Piece::Literal(middle.clone()),
            Piece::Source {
                start: 200,
                len: 100,
            },
        ];
        // 彼此不连续的 Source piece，避免 from_pieces 的测试夹具本身违反 R3。
        for i in 0..125u64 {
            pieces.push(Piece::Source {
                start: 1000 + i * 2,
                len: 1,
            });
        }
        pieces.push(Piece::Literal(tail.clone()));
        let mut t = PieceTree::from_pieces(pieces, 2000);
        assert_eq!(t.piece_count(), 130);
        assert_eq!(t.height(), 1);
        let bytes_before = t.len();

        let far_before = far.clone();
        let middle_before = middle.clone();
        let tail_before = tail.clone();

        // 在文件末尾插入 1 字节：只应在右边界与最后的 10 字节 Literal 合并。
        t.insert(t.len(), &[0xAA]).unwrap();

        let mut all = Vec::new();
        collect_pieces(t.root.as_ref().unwrap(), &mut all);
        assert_eq!(all.len(), 130, "边界合并后 piece 数不应增长");
        assert_eq!(t.piece_count(), 130);
        assert_eq!(t.len(), bytes_before + 1);
        assert_eq!(t.height(), 1);

        // Arc 指针不变 = 没有把远处 Literal 复制/重排。
        assert!(Arc::ptr_eq(literal_arc(all[0]), &far_before));
        assert!(Arc::ptr_eq(literal_arc(all[2]), &middle_before));
        match all.last().unwrap() {
            Piece::Literal(b) => {
                assert_eq!(b.len(), 11);
                assert_ne!(Arc::as_ptr(b), Arc::as_ptr(&tail_before));
            }
            Piece::Source { .. } => panic!("末尾应是合并后的 Literal"),
        }
        t.check_invariants().unwrap();
    }

    /// 自检确实能抓到三类破坏：空 piece、可合并邻对、节点计数/高度错误。
    #[test]
    fn check_invariants_detects_corruption() {
        let empty = PieceTree::from_pieces(vec![Piece::Literal(Arc::from(&b""[..]))], 0);
        let errs = empty.check_invariants().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("长度为 0")), "{errs:?}");

        let mergeable = PieceTree::from_pieces(
            vec![
                Piece::Literal(Arc::from(&b"a"[..])),
                Piece::Literal(Arc::from(&b"b"[..])),
            ],
            0,
        );
        let errs = mergeable.check_invariants().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("可合并")), "{errs:?}");

        let mut bad = PieceTree::from_source(10);
        bad.root.as_mut().unwrap().bytes = 9;
        bad.root.as_mut().unwrap().height = 3;
        let errs = bad.check_invariants().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("bytes")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("height")), "{errs:?}");
    }
}
