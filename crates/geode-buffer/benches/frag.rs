//! S1 spike：placeholder 最小尺寸与碎片化（只做实验，不改库代码）。
//!
//! 测四组策略在 **4 GiB 虚拟源 + 8 个 64 KiB 改动簇 + 1 万次混合编辑** 后的
//! piece 数、描述子内存（32 B/piece）、相对 04 §3.3 理论上界的放大倍数与耗时；
//! 另跑一个 1 MiB / 10 万次 1 字节编辑的病态场景。
//!
//! # 为什么 bench 里有一份模型
//!
//! 库里的 `PieceTable::coalesce` 是内建的（R3 无法关闭），`demote_small_placeholders`
//! 的阈值也硬编码为 4 KiB。为了比较 "(a) 不合并" 和 "(c) 1 KiB" 这两组库当前 API
//! 表达不出来的策略，本文件用 `FragTable` 复刻 `PieceTable` 的 split / replace /
//! coalesce 语义，只把平坦 `Vec` 换成 treap 以便把病态场景跑完。
//!
//! 这是**近似**，不是对库的直接测量：
//! - R1 降级只把 `Source` 改记成等长 `Literal`，不真读 Provider 字节（对 piece 数、
//!   描述子内存无影响；literal 字节内存会低估）；
//! - 模型不复制 literal 字节，耗时只反映结构操作，不是 `PieceTable` 的 memcpy 成本。
//!
//! 为避免"模型只是自说自话"，`report_main` 会用真实实现做对拍：
//! (b) 用 `PieceTable`（R3 内建）、(d) 用 `PieceTable + demote_small_placeholders`
//! （零字节 Provider）、(b) 再用 `PieceTree`（R3 已搬进树）对拍。piece 数不一致会打印
//! `MISMATCH`。

use criterion::{Criterion, criterion_group, criterion_main};
use geode_buffer::hydrate::MIN_PLACEHOLDER;
use geode_buffer::piece::{MAX_LITERAL, PieceTable};
use geode_buffer::tree::PieceTree;
use geode_core::{Offset, ProviderId};
use geode_provider::{Caps, Provider, ProviderError, ReadReport, SizeInfo};
use std::hint::black_box;
use std::time::{Duration, Instant};

/// 虚拟源长度：4 GiB，不分配内存。
const SOURCE_LEN: u64 = 4 << 30;
/// 主场景编辑次数。
const EDITS: usize = 10_000;
/// 改动簇数量。
const CLUSTERS: usize = 8;
/// 每个簇的半径（簇跨度 128 KiB）。
const CLUSTER_RADIUS: u64 = 64 * 1024;
/// `Piece` 描述子按 32 B 计（04 §3.3）。
const DESCRIPTOR_BYTES: u64 = 32;
/// 病态场景源长度：1 MiB。
const PATHO_LEN: u64 = 1 << 20;
/// 病态场景编辑次数。
const PATHO_EDITS: usize = 100_000;

/// 伪随机数（xorshift64*），避免为 spike 增加依赖。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        assert!(seed != 0, "xorshift 种子不能为 0");
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "below(0) 无意义");
        self.next_u64() % n
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpKind {
    Overwrite,
    Insert,
    Remove,
}

#[derive(Clone, Copy, Debug)]
struct Op {
    kind: OpKind,
    at: u64,
    len: u64,
}

impl Op {
    /// 这次编辑"碰过"的字节数：写入 + 删除都算，用于 04 §3.3 的改动字节项。
    fn touched(self) -> u64 {
        self.len
    }
}

fn clustered_ops(n: usize, seed: u64, source_len: u64) -> Vec<Op> {
    let mut rng = Rng::new(seed);
    let centers: Vec<u64> = (0..CLUSTERS)
        .map(|i| source_len * (i as u64 + 1) / (CLUSTERS as u64 + 1))
        .collect();
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        let center = centers[(rng.below(CLUSTERS as u64)) as usize];
        let lo = center.saturating_sub(CLUSTER_RADIUS);
        let hi = (center + CLUSTER_RADIUS).min(source_len - 1);
        let at = lo + rng.below(hi - lo);
        let kind = match rng.below(100) {
            0..=69 => OpKind::Overwrite,
            70..=89 => OpKind::Insert,
            _ => OpKind::Remove,
        };
        let len = 1 + rng.below(64);
        ops.push(Op { kind, at, len });
    }
    ops
}

fn uniform_byte_ops(n: usize, source_len: u64, seed: u64) -> Vec<Op> {
    let mut rng = Rng::new(seed);
    (0..n)
        .map(|_| Op {
            kind: OpKind::Overwrite,
            at: rng.below(source_len),
            len: 1,
        })
        .collect()
}

fn strided_byte_ops(n: usize, source_len: u64, stride: u64, start: u64) -> Vec<Op> {
    (0..n)
        .map(|i| Op {
            kind: OpKind::Overwrite,
            at: (start + stride * i as u64) % source_len,
            len: 1,
        })
        .collect()
}

/// 一组策略参数。
#[derive(Clone, Copy, Debug)]
struct Policy {
    id: &'static str,
    name: &'static str,
    /// `None` = 无最小尺寸（不做 R1）；`Some(n)` = 小于 n 的 Source 降级。
    min_placeholder: Option<u64>,
    /// 是否执行 R3 相邻合并。
    merge: bool,
}

const POLICIES: [Policy; 4] = [
    Policy {
        id: "a_none",
        name: "(a) 无下限/不合并",
        min_placeholder: None,
        merge: false,
    },
    Policy {
        id: "b_merge",
        name: "(b) 仅合并",
        min_placeholder: None,
        merge: true,
    },
    Policy {
        id: "c_1k",
        name: "(c) 1 KiB+合并",
        min_placeholder: Some(1024),
        merge: true,
    },
    Policy {
        id: "d_4k",
        name: "(d) 4 KiB+合并",
        min_placeholder: Some(MIN_PLACEHOLDER),
        merge: true,
    },
];

#[derive(Clone, Copy, Debug)]
enum FragKind {
    Source { start: u64 },
    Literal,
}

#[derive(Clone, Copy, Debug)]
struct Frag {
    kind: FragKind,
    len: u64,
}

impl Frag {
    fn source(start: u64, len: u64) -> Self {
        Self {
            kind: FragKind::Source { start },
            len,
        }
    }

    fn literal(len: u64) -> Self {
        Self {
            kind: FragKind::Literal,
            len,
        }
    }
}

/// 把相邻两片按 04 §3.3 的 R3 规则尝试合并；不可合并返回 `None`。
fn merge_frags(a: Frag, b: Frag) -> Option<Frag> {
    match (a.kind, b.kind) {
        (FragKind::Source { start }, FragKind::Source { start: s2 }) if start + a.len == s2 => {
            Some(Frag::source(start, a.len + b.len))
        }
        (FragKind::Literal, FragKind::Literal) if a.len + b.len <= MAX_LITERAL as u64 => {
            Some(Frag::literal(a.len + b.len))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 策略模型：treap（按字节数分裂/合并），语义对齐 PieceTable。
// ---------------------------------------------------------------------------

struct Node {
    frag: Frag,
    prio: u64,
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
    bytes: u64,
    count: usize,
}

impl Node {
    fn new(frag: Frag, rng: &mut Rng) -> Self {
        Self {
            frag,
            prio: rng.next_u64(),
            left: None,
            right: None,
            bytes: frag.len,
            count: 1,
        }
    }

    fn update(&mut self) {
        self.bytes = self.frag.len
            + self.left.as_ref().map_or(0, |n| n.bytes)
            + self.right.as_ref().map_or(0, |n| n.bytes);
        self.count = 1
            + self.left.as_ref().map_or(0, |n| n.count)
            + self.right.as_ref().map_or(0, |n| n.count);
    }
}

fn node_bytes(n: &Option<Box<Node>>) -> u64 {
    n.as_ref().map_or(0, |n| n.bytes)
}

fn node_count(n: &Option<Box<Node>>) -> usize {
    n.as_ref().map_or(0, |n| n.count)
}

fn merge_trees(a: Option<Box<Node>>, b: Option<Box<Node>>) -> Option<Box<Node>> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut a), Some(mut b)) => {
            if a.prio > b.prio {
                a.right = merge_trees(a.right.take(), Some(b));
                a.update();
                Some(a)
            } else {
                b.left = merge_trees(Some(a), b.left.take());
                b.update();
                Some(b)
            }
        }
    }
}

/// 按文档偏移 `at` 把树切成 `[0, at)` 与 `[at, len)`。
fn split_tree(root: Option<Box<Node>>, at: u64) -> (Option<Box<Node>>, Option<Box<Node>>) {
    match root {
        None => (None, None),
        Some(mut n) => {
            let left_bytes = node_bytes(&n.left);
            if at <= left_bytes {
                let (l, r) = split_tree(n.left.take(), at);
                n.left = r;
                n.update();
                (l, Some(n))
            } else if at >= left_bytes + n.frag.len {
                let (l, r) = split_tree(n.right.take(), at - left_bytes - n.frag.len);
                n.right = l;
                n.update();
                (Some(n), r)
            } else {
                // 切在 n 内部：把 n 拆成两片，左树/右树各自带上残余。
                let off = at - left_bytes;
                let left_node = Node {
                    frag: shrink_left(n.frag, off),
                    prio: n.prio,
                    left: n.left.take(),
                    right: None,
                    bytes: 0,
                    count: 1,
                };
                let right_node = Node {
                    frag: shrink_right(n.frag, off),
                    prio: n.prio.wrapping_add(1),
                    left: None,
                    right: n.right.take(),
                    bytes: 0,
                    count: 1,
                };
                (finish_node(left_node), finish_node(right_node))
            }
        }
    }
}

fn shrink_left(frag: Frag, off: u64) -> Frag {
    match frag.kind {
        FragKind::Source { start } => Frag::source(start, off),
        FragKind::Literal => Frag::literal(off),
    }
}

fn shrink_right(frag: Frag, off: u64) -> Frag {
    let len = frag.len - off;
    match frag.kind {
        FragKind::Source { start } => Frag::source(start + off, len),
        FragKind::Literal => Frag::literal(len),
    }
}

fn finish_node(mut n: Node) -> Option<Box<Node>> {
    n.update();
    Some(Box::new(n))
}

fn pop_last(mut root: Option<Box<Node>>) -> (Option<Box<Node>>, Option<Box<Node>>) {
    match root.take() {
        None => (None, None),
        Some(mut n) => match n.right.take() {
            None => {
                let left = n.left.take();
                n.update();
                (left, Some(n))
            }
            Some(r) => {
                let (rest, last) = pop_last(Some(r));
                n.right = rest;
                n.update();
                (Some(n), last)
            }
        },
    }
}

fn pop_first(mut root: Option<Box<Node>>) -> (Option<Box<Node>>, Option<Box<Node>>) {
    match root.take() {
        None => (None, None),
        Some(mut n) => match n.left.take() {
            None => {
                let right = n.right.take();
                n.update();
                (right, Some(n))
            }
            Some(l) => {
                let (rest, first) = pop_first(Some(l));
                n.left = rest;
                n.update();
                (Some(n), first)
            }
        },
    }
}

/// `at` 是否真的落在两片之间（而不是某片内部）。
///
/// 这个检查是必要的：合并一旦跨过 `at`，`at` 就落到新片内部；若再次
/// split 再合并，会无限循环。
fn is_boundary(n: &Option<Box<Node>>, at: u64) -> bool {
    match n {
        None => false,
        Some(n) => {
            let left_bytes = node_bytes(&n.left);
            if at < left_bytes {
                is_boundary(&n.left, at)
            } else if at == left_bytes {
                true
            } else if at < left_bytes + n.frag.len {
                false
            } else if at == left_bytes + n.frag.len {
                true
            } else {
                is_boundary(&n.right, at - left_bytes - n.frag.len)
            }
        }
    }
}

/// 在边界 `at` 处尝试合并左右两片；返回 (新树, 是否合并)。
fn try_merge_at(root: Option<Box<Node>>, at: u64, rng: &mut Rng) -> (Option<Box<Node>>, bool) {
    let total = node_bytes(&root);
    if total == 0 {
        return (root, false);
    }
    if at != 0 && at != total && !is_boundary(&root, at) {
        return (root, false);
    }
    if at == 0 {
        let (rest, first) = pop_first(root);
        let (rest, second) = pop_first(rest);
        return match (first, second) {
            (Some(first), Some(second)) => match merge_frags(first.frag, second.frag) {
                Some(frag) => {
                    let merged = Box::new(Node::new(frag, rng));
                    (merge_trees(rest, Some(merged)), true)
                }
                None => (
                    merge_trees(merge_trees(rest, Some(first)), Some(second)),
                    false,
                ),
            },
            (Some(first), None) => (merge_trees(rest, Some(first)), false),
            _ => (rest, false),
        };
    }
    if at >= total {
        let (rest, last) = pop_last(root);
        let (rest, prev) = pop_last(rest);
        return match (prev, last) {
            (Some(prev), Some(last)) => match merge_frags(prev.frag, last.frag) {
                Some(frag) => {
                    let merged = Box::new(Node::new(frag, rng));
                    (merge_trees(rest, Some(merged)), true)
                }
                None => (
                    merge_trees(merge_trees(rest, Some(prev)), Some(last)),
                    false,
                ),
            },
            (Some(prev), None) => (merge_trees(rest, Some(prev)), false),
            _ => (rest, false),
        };
    }
    let (a, b) = split_tree(root, at);
    let (a, left) = pop_last(a);
    let (b, right) = pop_first(b);
    match (left, right) {
        (Some(left), Some(right)) => match merge_frags(left.frag, right.frag) {
            Some(frag) => {
                let merged = Box::new(Node::new(frag, rng));
                (merge_trees(merge_trees(a, Some(merged)), b), true)
            }
            None => (
                merge_trees(merge_trees(a, Some(left)), merge_trees(Some(right), b)),
                false,
            ),
        },
        (left, right) => (
            merge_trees(a, merge_trees(left, merge_trees(right, b))),
            false,
        ),
    }
}

/// 把边界 `at` 两侧的小 Source 降级为等长 Literal（R1 的近似）。
fn demote_at(root: Option<Box<Node>>, at: u64, min: u64, rng: &mut Rng) -> Option<Box<Node>> {
    let total = node_bytes(&root);
    if total == 0 {
        return root;
    }
    let (mut a, mut b) = if at == 0 {
        (None, root)
    } else if at >= total {
        (root, None)
    } else {
        split_tree(root, at)
    };
    let (a2, left) = pop_last(a.take());
    a = a2;
    let (b2, right) = pop_first(b.take());
    b = b2;
    let left_len = left.as_ref().map_or(0, |n| n.frag.len);
    let right_len = right.as_ref().map_or(0, |n| n.frag.len);
    let left = left.map(|mut n| {
        if matches!(n.frag.kind, FragKind::Source { .. }) && n.frag.len < min {
            n.frag = Frag::literal(n.frag.len);
        }
        n
    });
    let right = right.map(|mut n| {
        if matches!(n.frag.kind, FragKind::Source { .. }) && n.frag.len < min {
            n.frag = Frag::literal(n.frag.len);
        }
        n
    });
    let mut root = merge_trees(merge_trees(a, left), merge_trees(right, b));
    // 真实的 `demote_small_placeholders` 末尾会全局 coalesce。降级后的 literal
    // 可能和**远侧**邻居合并（编辑边界在中间隔不住），所以要把该片两侧的
    // 边界都再合并一遍，否则模型会多留碎片。
    for boundary in [at.saturating_sub(left_len), at, at + right_len] {
        loop {
            let (next, merged) = try_merge_at(root.take(), boundary, rng);
            root = next;
            if !merged {
                break;
            }
        }
    }
    root
}

/// 策略模型。
struct FragTable {
    root: Option<Box<Node>>,
    len: u64,
    policy: Policy,
    rng: Rng,
}

impl FragTable {
    fn new(policy: Policy, source_len: u64) -> Self {
        let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
        let root = if source_len == 0 {
            None
        } else {
            Some(Box::new(Node::new(Frag::source(0, source_len), &mut rng)))
        };
        Self {
            root,
            len: source_len,
            policy,
            rng,
        }
    }

    fn apply(&mut self, op: Op) {
        let at = op.at.min(self.len);
        match op.kind {
            OpKind::Overwrite => {
                let end = (at + op.len).min(self.len);
                let (a, rest) = split_tree(self.root.take(), at);
                let (_, c) = split_tree(rest, end - at);
                let mid = Some(Box::new(Node::new(Frag::literal(op.len), &mut self.rng)));
                self.root = merge_trees(merge_trees(a, mid), c);
                self.len = self.len - (end - at) + op.len;
                self.post_edit(at, op.len);
            }
            OpKind::Insert => {
                let (a, b) = split_tree(self.root.take(), at);
                let mid = Some(Box::new(Node::new(Frag::literal(op.len), &mut self.rng)));
                self.root = merge_trees(merge_trees(a, mid), b);
                self.len += op.len;
                self.post_edit(at, op.len);
            }
            OpKind::Remove => {
                let end = (at + op.len).min(self.len);
                let (a, rest) = split_tree(self.root.take(), at);
                let (_, c) = split_tree(rest, end - at);
                self.root = merge_trees(a, c);
                self.len -= end - at;
                self.post_edit(at, 0);
            }
        }
    }

    fn post_edit(&mut self, at: u64, span: u64) {
        let end = at + span;
        if self.policy.merge {
            self.coalesce_at(at);
            self.coalesce_at(end);
        }
        if let Some(min) = self.policy.min_placeholder {
            self.root = demote_at(self.root.take(), at, min, &mut self.rng);
            self.root = demote_at(self.root.take(), end, min, &mut self.rng);
        }
        if self.policy.merge {
            self.coalesce_at(at);
            self.coalesce_at(end);
        }
    }

    fn coalesce_at(&mut self, at: u64) {
        loop {
            let (root, merged) = try_merge_at(self.root.take(), at, &mut self.rng);
            self.root = root;
            if !merged {
                break;
            }
        }
    }

    fn piece_count(&self) -> usize {
        node_count(&self.root)
    }

    fn real_count(&self) -> usize {
        fn walk(n: &Option<Box<Node>>) -> usize {
            match n {
                None => 0,
                Some(n) => 1 + walk(&n.left) + walk(&n.right),
            }
        }
        walk(&self.root)
    }

    fn zero_len_count(&self) -> usize {
        fn walk(n: &Option<Box<Node>>) -> usize {
            match n {
                None => 0,
                Some(n) => usize::from(n.frag.len == 0) + walk(&n.left) + walk(&n.right),
            }
        }
        walk(&self.root)
    }

    fn dump_around(&self, at: u64, radius: u64) {
        fn walk(n: &Option<Box<Node>>, pos: &mut u64, lo: u64, hi: u64, out: &mut Vec<String>) {
            if let Some(n) = n {
                walk(&n.left, pos, lo, hi, out);
                let start = *pos;
                let end = start + n.frag.len;
                if end > lo && start < hi {
                    out.push(format!(
                        "[{start},{end}) {:?} len={}",
                        n.frag.kind, n.frag.len
                    ));
                }
                *pos = end;
                walk(&n.right, pos, lo, hi, out);
            }
        }
        let mut out = Vec::new();
        let mut pos = 0;
        walk(
            &self.root,
            &mut pos,
            at.saturating_sub(radius),
            at + radius,
            &mut out,
        );
        println!("  model 附近: {}", out.join(" | "));
    }

    fn literal_bytes(&self) -> u64 {
        fn walk(n: &Option<Box<Node>>) -> u64 {
            match n {
                None => 0,
                Some(n) => {
                    let own = if matches!(n.frag.kind, FragKind::Literal) {
                        n.frag.len
                    } else {
                        0
                    };
                    own + walk(&n.left) + walk(&n.right)
                }
            }
        }
        walk(&self.root)
    }
}

// ---------------------------------------------------------------------------
// 真实实现对拍与统计
// ---------------------------------------------------------------------------

/// 零字节 Provider：只为 `demote_small_placeholders` 提供"读得满"的源。
#[derive(Debug)]
struct ZeroProvider {
    len: u64,
}

impl Provider for ZeroProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(0x5A45_524F)
    }

    fn display_name(&self) -> String {
        "零字节虚拟源".to_string()
    }

    fn caps(&self) -> Caps {
        Caps::READ_ONLY_STABLE
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.len)
    }

    fn read_range(&self, _at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        buf.fill(0);
        Ok(ReadReport {
            filled: buf.len() as u64,
            holes: 0,
            eof_hit: false,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Stats {
    pieces: usize,
    literal_bytes: u64,
    elapsed: Duration,
}

impl Stats {
    fn descriptor_bytes(self) -> u64 {
        self.pieces as u64 * DESCRIPTOR_BYTES
    }
}

fn run_model(policy: Policy, ops: &[Op], source_len: u64) -> Stats {
    let mut table = FragTable::new(policy, source_len);
    let start = Instant::now();
    for &op in ops {
        table.apply(op);
    }
    let elapsed = start.elapsed();
    Stats {
        pieces: table.piece_count(),
        literal_bytes: table.literal_bytes(),
        elapsed,
    }
}

fn run_piece_table(ops: &[Op], source_len: u64, demote: bool) -> Stats {
    let mut table = PieceTable::from_source(source_len);
    let src = ZeroProvider { len: source_len };
    let data = [0xA5u8; 64];
    let start = Instant::now();
    for &op in ops {
        let at = op.at.min(table.len());
        match op.kind {
            OpKind::Overwrite => {
                table
                    .overwrite(at, &data[..usize::try_from(op.len).unwrap()])
                    .expect("overwrite 合法");
            }
            OpKind::Insert => {
                table
                    .insert(at, &data[..usize::try_from(op.len).unwrap_or(0)])
                    .expect("insert 合法");
            }
            OpKind::Remove => {
                let end = (at + op.len).min(table.len());
                table.remove(at, end - at).expect("remove 合法");
            }
        }
        if demote {
            table
                .demote_small_placeholders(&src)
                .expect("零字节 Provider 不短读");
        }
    }
    let elapsed = start.elapsed();
    table.check_invariants().expect("PieceTable 不变量");
    Stats {
        pieces: table.piece_count(),
        literal_bytes: table.literal_bytes().len() as u64,
        elapsed,
    }
}

fn run_piece_tree(ops: &[Op], source_len: u64) -> Stats {
    let mut tree = PieceTree::from_source(source_len);
    let data = [0xA5u8; 64];
    let start = Instant::now();
    for &op in ops {
        let at = op.at.min(tree.len());
        match op.kind {
            OpKind::Overwrite => {
                tree.overwrite(at, &data[..usize::try_from(op.len).unwrap()])
                    .expect("overwrite 合法");
            }
            OpKind::Insert => {
                tree.insert(at, &data[..usize::try_from(op.len).unwrap_or(0)])
                    .expect("insert 合法");
            }
            OpKind::Remove => {
                let end = (at + op.len).min(tree.len());
                tree.remove(at, end - at).expect("remove 合法");
            }
        }
    }
    let elapsed = start.elapsed();
    Stats {
        // split 现在不会在 piece 边界切出零长片，两者口径一致。
        pieces: tree.piece_count(),
        literal_bytes: 0,
        elapsed,
    }
}

fn print_row(label: &str, stats: Stats, theory: f64) {
    let desc = stats.descriptor_bytes() as f64 / 1024.0;
    let ratio = if theory > 0.0 {
        stats.pieces as f64 / theory
    } else {
        f64::NAN
    };
    println!(
        "{label:<22} pieces={:>7}  desc={:>8.1} KiB  x_theory={:>6.1}  model={:>8.2} ms  literal={:>8.1} KiB",
        stats.pieces,
        desc,
        ratio,
        stats.elapsed.as_secs_f64() * 1000.0,
        stats.literal_bytes as f64 / 1024.0
    );
}

fn debug_compare(ops: &[Op], source_len: u64) {
    let policy = POLICIES[1];
    let mut model = FragTable::new(policy, source_len);
    let mut tree = PieceTree::from_source(source_len);
    let data = [0xA5u8; 64];
    for (i, &op) in ops.iter().enumerate() {
        let before_model = model.piece_count();
        let before_tree = tree.piece_count();
        model.apply(op);
        let at = op.at.min(tree.len());
        match op.kind {
            OpKind::Overwrite => {
                tree.overwrite(at, &data[..usize::try_from(op.len).unwrap()])
                    .unwrap();
            }
            OpKind::Insert => {
                tree.insert(at, &data[..usize::try_from(op.len).unwrap()])
                    .unwrap();
            }
            OpKind::Remove => {
                let end = (at + op.len).min(tree.len());
                tree.remove(at, end - at).unwrap();
            }
        }
        let tree_pieces = tree.piece_count();
        if model.piece_count() != tree_pieces || model.len != tree.len() {
            println!(
                "DEBUG 首个不一致 @ op {i}: {op:?}\n  before model={before_model} tree={before_tree}\n  model pieces={} real={} len={} zero={}\n  tree  pieces={} len={}",
                model.piece_count(),
                model.real_count(),
                model.len,
                model.zero_len_count(),
                tree_pieces,
                tree.len()
            );
            model.dump_around(op.at, 200);
            println!(
                "  tree 附近: {:?}",
                tree.spans(op.at.saturating_sub(200), 400)
            );
            return;
        }
    }
    println!("DEBUG 前 {} 次编辑 piece 数/长度逐次一致", ops.len());
}

fn report_main() {
    let ops = clustered_ops(EDITS, 0x5EED_1234, SOURCE_LEN);
    if std::env::var_os("FRAG_DEBUG").is_some() {
        debug_compare(&ops, SOURCE_LEN);
        std::process::exit(0);
    }
    let changed: u64 = ops.iter().map(|op| op.touched()).sum();
    let theory = CLUSTERS as f64 + changed as f64 / MIN_PLACEHOLDER as f64;
    println!("\n=== S1 主场景：4 GiB 源 / {EDITS} 次编辑 / {CLUSTERS} 簇 x 64 KiB ===");
    println!(
        "改动字节 = {changed} ({} KiB)，理论上界 = C + 改动字节/4KiB = {theory:.1} 片，描述子理论 ≈ {:.1} KiB",
        changed / 1024,
        theory * DESCRIPTOR_BYTES as f64 / 1024.0
    );

    let mut model = Vec::new();
    for policy in POLICIES {
        let stats = run_model(policy, &ops, SOURCE_LEN);
        print_row(policy.name, stats, theory);
        model.push(stats);
    }

    println!("-- 对拍（piece 数）--");
    let pt = run_piece_table(&ops, SOURCE_LEN, false);
    println!(
        "PieceTable(R3)          pieces={:>7}  vs 模型(b)={:>7}  {}",
        pt.pieces,
        model[1].pieces,
        if pt.pieces == model[1].pieces {
            "OK"
        } else {
            "MISMATCH"
        }
    );
    let pt4 = run_piece_table(&ops, SOURCE_LEN, true);
    println!(
        "PieceTable(R1=4K)       pieces={:>7}  vs 模型(d)={:>7}  {}  [{} ms]",
        pt4.pieces,
        model[3].pieces,
        if pt4.pieces == model[3].pieces {
            "OK"
        } else {
            "MISMATCH"
        },
        pt4.elapsed.as_millis()
    );
    let tree = run_piece_tree(&ops, SOURCE_LEN);
    println!(
        "PieceTree(R3)           pieces={:>7}  vs 模型(b)={:>7}  {}  [{} ms]",
        tree.pieces,
        model[1].pieces,
        if tree.pieces == model[1].pieces {
            "OK"
        } else {
            "MISMATCH"
        },
        tree.elapsed.as_millis()
    );
    println!(
        "真实 PieceTable 耗时（仅供参考，flat Vec 的 O(piece^2) 不是本 spike 的主题）：\
         仅合并 {} ms，4 KiB {} ms",
        pt.elapsed.as_millis(),
        pt4.elapsed.as_millis()
    );
}

fn report_pathological() {
    println!("\n=== S1 病态场景：1 MiB 源 / {PATHO_EDITS} 次 1 字节覆盖 ===");
    let scenarios: [(&str, Vec<Op>); 3] = [
        (
            "全文件随机",
            uniform_byte_ops(PATHO_EDITS, PATHO_LEN, 0xBAD5_EED1),
        ),
        (
            "地址连续 0x1000",
            strided_byte_ops(PATHO_EDITS, PATHO_LEN, 1, 0x1000),
        ),
        (
            "隔字节 0x1000",
            strided_byte_ops(PATHO_EDITS, PATHO_LEN, 2, 0x1000),
        ),
    ];
    let no_merge = Policy {
        id: "patho_a",
        name: "(a) 无下限/不合并",
        min_placeholder: None,
        merge: false,
    };
    let merge_only = Policy {
        id: "patho_b",
        name: "(b) 仅合并",
        min_placeholder: None,
        merge: true,
    };
    println!(
        "{:<16} {:>10} {:>10} {:>10} {:>10}",
        "编辑模式", "不合并", "仅合并", "4KiB+合并", "不合并MiB"
    );
    for (name, ops) in scenarios {
        let a = run_model(no_merge, &ops, PATHO_LEN);
        let b = run_model(merge_only, &ops, PATHO_LEN);
        let d = run_model(POLICIES[3], &ops, PATHO_LEN);
        println!(
            "{name:<16} {:>10} {:>10} {:>10} {:>10.2}",
            a.pieces,
            b.pieces,
            d.pieces,
            a.descriptor_bytes() as f64 / (1024.0 * 1024.0)
        );
    }
    println!(
        "结构上限：10 万次编辑、每次最多 +2 片 ⇒ 200,001 片（{:.1} MiB 描述子）；50 万片需要约 25 万次。",
        200_001.0 * DESCRIPTOR_BYTES as f64 / (1024.0 * 1024.0)
    );
    let long = strided_byte_ops(250_000, PATHO_LEN, 2, 0x1000);
    let long_stats = run_model(no_merge, &long, PATHO_LEN);
    println!(
        "外推校验：25 万次隔字节覆盖 ⇒ {} 片（{:.1} MiB 描述子）",
        long_stats.pieces,
        long_stats.descriptor_bytes() as f64 / (1024.0 * 1024.0)
    );
    let full = strided_byte_ops(500_000, PATHO_LEN, 2, 0);
    let full_d = run_model(POLICIES[3], &full, PATHO_LEN);
    println!(
        "同一场景补满 50 万次编辑（覆盖整个 1 MiB，min=4KiB+R3）⇒ {} 片（文档说 1 片）",
        full_d.pieces
    );

    // 低规模对拍：PieceTree 的 R3 与模型(b) 应逐片一致。
    let small = uniform_byte_ops(20_000, PATHO_LEN, 0xBAD5_EED1);
    let b = run_model(merge_only, &small, PATHO_LEN);
    let tree = run_piece_tree(&small, PATHO_LEN);
    println!(
        "对拍（2 万次随机）：模型(b)={} 片，PieceTree={} 片，{}",
        b.pieces,
        tree.pieces,
        if b.pieces == tree.pieces {
            "OK"
        } else {
            "MISMATCH"
        }
    );
}

fn bench_models(c: &mut Criterion) {
    report_main();
    report_pathological();

    let ops = clustered_ops(EDITS, 0x5EED_1234, SOURCE_LEN);
    let mut group = c.benchmark_group("S1_frag_model");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(300));
    for policy in POLICIES {
        group.bench_function(policy.id, |b| {
            b.iter(|| {
                let stats = run_model(policy, &ops, SOURCE_LEN);
                black_box(stats.pieces)
            });
        });
    }
    group.finish();
}

#[allow(missing_docs)]
mod bench_entry {
    use super::*;

    criterion_group!(benches, bench_models);
}

criterion_main!(bench_entry::benches);
