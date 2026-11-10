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

#[derive(Debug)]
enum Kind {
    Leaf(Vec<Piece>),
    Inner(Vec<Node>),
}

#[derive(Debug)]
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

/// 拼接两棵子树，保持"每节点 ≤64 子项"。高度不同时沿较高树的边界下降。
fn concat(a: Option<Node>, b: Option<Node>) -> Option<Node> {
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
        if let Some(joined) = concat(Some(last), Some(b)) {
            kids.push(joined);
        }
        pack_nodes(kids)
    } else {
        let Kind::Inner(mut kids) = b.kind else {
            unreachable!("较高节点必然是内部节点")
        };
        let first = kids.remove(0);
        if let Some(joined) = concat(Some(a), Some(first)) {
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
#[derive(Debug, Default)]
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
            Some(e) => Err(e),
            None => Ok(demoted),
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
        if let Some(e) = err {
            return Err(e);
        }
        self.set(concat(concat(a, Some(mid)), c));
        Ok(())
    }
}
