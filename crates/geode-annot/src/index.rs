//! 标注区间索引：分块平坦数组 + 块级 `max_end` 隐式线段树。
//!
//! DESIGN: 00 §5.3 / 05 §4.7、05 §3.5（否决 intervaltree 与"二分 + 回扫"的理由）
//!
//! # 为什么不直接用 `Vec` + 二分回扫
//!
//! 一个长区间会让"从右往左扫到 `max_end <= at` 为止"退化到 O(n)：例如 `[0, 10^6)`
//! 压在最左边，右边十万条短区间全都不覆盖查询点，回扫仍要逐个检查。本实现把
//! 有序数组切成固定大小的块（块内仍连续），块级 `max_end` 放进隐式线段树：
//! 查询沿树剪掉整块，只下钻可能含命中的块，因此最坏只与命中块数相关。
//!
//! # 不变量
//!
//! 1. 块按 `start` 全局有序，块内按 `(start, id)` 升序；
//! 2. 每个块非空，`max_end` 等于块内所有 `end` 的最大值；
//! 3. `slot[id]` 与排序结构中的 `id` 一一对应；删除后槽位为 `None`，id 不复用；
//! 4. `max_end` 线段树是块 `max_end` 的逐点上包络。
//!
//! `query_point` / `query` 返回 `(Layer::z, priority, id)` 升序的 id 列表；
//! `deepest_at` 返回同一全序的最后一个。

use crate::annotation::{Annotation, AnnotationId};
use crate::layer::Layer;
use geode_core::{ByteRange, Offset};

/// 点参数：同时接受 [`Offset`] 与裸 `u64`，方便 UI 层直接传光标偏移。
pub trait PointArg {
    /// 转成 [`Offset`]。
    fn into_offset(self) -> Offset;
}

impl PointArg for Offset {
    #[inline]
    fn into_offset(self) -> Offset {
        self
    }
}

impl PointArg for &Offset {
    #[inline]
    fn into_offset(self) -> Offset {
        *self
    }
}

impl PointArg for u64 {
    #[inline]
    fn into_offset(self) -> Offset {
        Offset::new(self)
    }
}

/// 目标块大小；块满到 `MAX_BLOCK` 时二分。
const TARGET_BLOCK: usize = 64;
/// 块上限。留一倍余量，让插入在两次二分之间摊销。
const MAX_BLOCK: usize = TARGET_BLOCK * 2;
/// 低于半块时尝试与邻居合并，避免删除把块打碎。
const MIN_BLOCK: usize = TARGET_BLOCK / 2;

/// 排序结构里的轻量条目：只复制查询热路径需要的字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    id: AnnotationId,
    start: u64,
    end: u64,
    z: u8,
    priority: u8,
}

impl Entry {
    fn from_annotation(ann: &Annotation) -> Self {
        Self {
            id: ann.id,
            start: ann.range.start().get(),
            end: ann.range.end().get(),
            z: ann.layer.z(),
            priority: ann.priority,
        }
    }

    /// 视觉覆盖顺序：低 z 先画，同层低 priority 先画，再按 id。
    fn sort_key(self) -> (u8, u8, AnnotationId) {
        (self.z, self.priority, self.id)
    }

    #[inline]
    fn covers_point(self, at: u64) -> bool {
        self.start <= at && at < self.end
    }

    #[inline]
    fn overlaps(self, range: ByteRange) -> bool {
        // 空区间（start == end）不占任何字节，永不与查询相交。
        self.start < self.end && self.start < range.end().get() && range.start().get() < self.end
    }
}

/// `AnnotationId` 到槽位下标：id 从 1 开始，槽位从 0 开始。
fn slot_index(id: AnnotationId) -> Option<usize> {
    id.0.checked_sub(1)
        .and_then(|value| usize::try_from(value).ok())
}

/// 一个有序块。
#[derive(Clone, Debug, Default)]
struct Block {
    entries: Vec<Entry>,
    /// 块内所有 `end` 的最大值；空块只在构造过程中短暂出现。
    max_end: u64,
}

impl Block {
    fn new(entries: Vec<Entry>) -> Self {
        debug_assert!(!entries.is_empty());
        let mut block = Self {
            entries,
            max_end: 0,
        };
        block.recompute_max_end();
        block
    }

    fn recompute_max_end(&mut self) {
        self.max_end = self.entries.iter().map(|e| e.end).max().unwrap_or(0);
    }

    fn first_start(&self) -> Option<u64> {
        self.entries.first().map(|e| e.start)
    }

    fn last_start(&self) -> Option<u64> {
        self.entries.last().map(|e| e.start)
    }
}

/// 标注区间索引。
///
/// 插入返回从 1 开始单调递增的 [`AnnotationId`]；删除后 id 不复用。索引不持有
/// `Arc`，负载直接存在 id 槽位，查询只返回 id，渲染层按需 [`get`](Self::get)。
#[derive(Clone, Debug)]
pub struct AnnotationIndex {
    /// 按 `start` 全局有序的块序列。
    blocks: Vec<Block>,
    /// 块级 `max_end` 的隐式线段树（`tree[1]` 是根）。
    max_tree: Vec<u64>,
    /// `max_tree` 的叶子数（2 的幂）。
    tree_size: usize,
    /// 稳定 id → 标注；已删除为 `None`。
    slots: Vec<Option<Annotation>>,
    /// 下一个分配的 id（从 1 开始）。
    next_id: u64,
    /// 当前标注数。
    len: usize,
}

impl Default for AnnotationIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnotationIndex {
    /// 创建空索引。
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            max_tree: vec![0, 0],
            tree_size: 1,
            slots: Vec::new(),
            next_id: 1,
            len: 0,
        }
    }

    /// 创建空索引并预留 `capacity` 条的槽位与排序条目空间。
    pub fn with_capacity(capacity: usize) -> Self {
        let mut index = Self::new();
        index.slots.reserve(capacity);
        index.blocks.reserve(capacity / TARGET_BLOCK + 1);
        index
    }

    /// 当前标注数（含空区间标注）。
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 是否没有标注。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 按 id 取标注；已删除或从未分配返回 `None`。
    #[inline]
    pub fn get(&self, id: AnnotationId) -> Option<&Annotation> {
        slot_index(id)
            .and_then(|i| self.slots.get(i))
            .and_then(Option::as_ref)
    }

    /// 遍历所有仍存活的标注（顺序为 id 升序）。
    pub fn iter(&self) -> impl Iterator<Item = &Annotation> + '_ {
        self.slots.iter().flatten()
    }

    /// 插入一条标注，返回索引分配的稳定 id。
    ///
    /// 传入的 `ann.id` 会被覆盖；这样调用方不需要自己维护 id 空间。[`Annotation::new`]
    /// 构造的占位 id 0 会被替换为从 1 开始的新 id。
    pub fn insert(&mut self, mut ann: Annotation) -> AnnotationId {
        let id = AnnotationId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("AnnotationId 空间耗尽（u64 上限）");
        ann.id = id;

        let entry = Entry::from_annotation(&ann);
        if self.blocks.is_empty() {
            self.blocks.push(Block::default());
        }
        let block_idx = self.insertion_block(entry.start);
        let pos = self.blocks[block_idx]
            .entries
            .partition_point(|e| (e.start, e.id) < (entry.start, entry.id));
        self.blocks[block_idx].entries.insert(pos, entry);
        self.blocks[block_idx].recompute_max_end();

        if self.blocks[block_idx].entries.len() > MAX_BLOCK {
            self.split_block(block_idx);
        } else {
            self.update_tree(block_idx);
        }

        debug_assert_eq!(self.slots.len() as u64, id.0 - 1);
        self.slots.push(Some(ann));
        self.len += 1;
        id
    }

    /// 便捷插入：等价于 [`Annotation::new`] + [`insert`](Self::insert)。
    pub fn insert_new(
        &mut self,
        range: ByteRange,
        layer: Layer,
        priority: u8,
        payload: crate::AnnotationKind,
    ) -> AnnotationId {
        self.insert(Annotation::new(range, layer, priority, payload))
    }

    /// 删除一条标注并返回它；id 不存在或已删除返回 `None`。
    pub fn remove(&mut self, id: AnnotationId) -> Option<Annotation> {
        let slot = slot_index(id)?;
        let ann = self.slots.get_mut(slot)?.take()?;
        let start = ann.range.start().get();
        if !self.remove_entry(id, start) {
            // 不变量被破坏时宁可把槽位放回去，也不静默丢数据。
            self.slots[slot] = Some(ann);
            debug_assert!(false, "slot 中存在标注但排序结构里找不到 id {id}");
            return None;
        }
        self.len -= 1;
        Some(ann)
    }

    /// 清空一个图层的所有标注，返回删除条数。
    pub fn clear_layer(&mut self, layer: Layer) -> usize {
        let mut removed = 0;
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|ann| ann.layer == layer) {
                *slot = None;
                removed += 1;
            }
        }
        if removed > 0 {
            self.len -= removed;
            self.rebuild_from_slots();
        }
        removed
    }

    /// 清空全部标注；已分配的 id 仍不复用。
    ///
    /// 槽位数组保留为等长的 `None` 洞，这样 `id - 1` 的下标关系不会被破坏，后续
    /// 插入继续拿到更大的 id。
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.rebuild_tree();
        self.slots.fill(None);
        self.len = 0;
    }

    /// 查询覆盖 `at` 的全部标注，按 `(layer.z, priority, id)` 升序。
    pub fn query_point(&self, at: impl PointArg) -> Vec<AnnotationId> {
        let mut out = Vec::new();
        self.query_point_into(at, &mut out);
        out
    }

    /// [`query_point`](Self::query_point) 的复用缓冲版本；会先清空 `out`。
    pub fn query_point_into(&self, at: impl PointArg, out: &mut Vec<AnnotationId>) {
        out.clear();
        let mut hits = Vec::new();
        self.for_each_covering_point(at.into_offset().get(), |entry| hits.push(entry));
        hits.sort_unstable_by_key(|entry| entry.sort_key());
        out.extend(hits.into_iter().map(|entry| entry.id));
    }

    /// 查询与 `range` 有交集的全部标注，按 `(layer.z, priority, id)` 升序。
    ///
    /// 半开语义：`[0, 10)` 与 `[10, 20)` 不相交；空查询区间返回空。
    pub fn query(&self, range: ByteRange) -> Vec<AnnotationId> {
        let mut out = Vec::new();
        self.query_into(range, &mut out);
        out
    }

    /// [`query`](Self::query) 的复用缓冲版本；会先清空 `out`。
    pub fn query_into(&self, range: ByteRange, out: &mut Vec<AnnotationId>) {
        out.clear();
        let mut hits = Vec::new();
        self.for_each_covering_range(range, |entry| hits.push(entry));
        hits.sort_unstable_by_key(|entry| entry.sort_key());
        out.extend(hits.into_iter().map(|entry| entry.id));
    }

    /// 返回覆盖 `at` 的"最深"标注：z 最高，同层 priority 最高，再 id 最大。
    ///
    /// 无命中返回 `None`。这是点击命中（05 §4.8）与光标高亮的入口。
    pub fn deepest_at(&self, at: impl PointArg) -> Option<AnnotationId> {
        let mut best: Option<Entry> = None;
        self.for_each_covering_point(at.into_offset().get(), |entry| {
            if best.is_none_or(|current| entry.sort_key() > current.sort_key()) {
                best = Some(entry);
            }
        });
        best.map(|entry| entry.id)
    }

    // ── 排序结构的内部操作 ──────────────────────────────────────────────

    /// 找到新条目应插入的块：最后一个首 `start <= start` 的块；空索引取块 0。
    ///
    /// 相同 `start` 可能横跨多个块（二分点恰好落在相同 start 中间），取最后一个
    /// 首元素 `<= start` 的块可保证新 id（全局最大）插在相同 start 的最右侧。
    fn insertion_block(&self, start: u64) -> usize {
        if self.blocks.is_empty() {
            return 0;
        }
        let after = self
            .blocks
            .partition_point(|block| block.first_start().is_some_and(|first| first <= start));
        if after == 0 { 0 } else { after - 1 }
    }

    /// 删除排序结构里的条目；成功返回 `true`。
    fn remove_entry(&mut self, id: AnnotationId, start: u64) -> bool {
        // 第一个"最后 start >= start"的块；相同 start 可能跨块，向后继续找。
        let mut block_idx = self
            .blocks
            .partition_point(|block| block.last_start().is_some_and(|last| last < start));
        while block_idx < self.blocks.len() {
            let Some(first) = self.blocks[block_idx].first_start() else {
                break;
            };
            if first > start {
                break;
            }
            if let Some(pos) = self.blocks[block_idx]
                .entries
                .iter()
                .position(|entry| entry.id == id)
            {
                self.blocks[block_idx].entries.remove(pos);
                if self.blocks[block_idx].entries.is_empty() {
                    self.blocks.remove(block_idx);
                } else {
                    self.blocks[block_idx].recompute_max_end();
                    self.maybe_merge(block_idx);
                }
                self.rebuild_tree();
                return true;
            }
            block_idx += 1;
        }
        false
    }

    /// 块过小时与右邻居（优先）或左邻居合并。
    fn maybe_merge(&mut self, block_idx: usize) {
        if self.blocks[block_idx].entries.len() >= MIN_BLOCK || self.blocks.len() < 2 {
            return;
        }
        if block_idx + 1 < self.blocks.len()
            && self.blocks[block_idx].entries.len() + self.blocks[block_idx + 1].entries.len()
                <= MAX_BLOCK
        {
            let right = self.blocks.remove(block_idx + 1);
            self.blocks[block_idx].entries.extend(right.entries);
            self.blocks[block_idx].recompute_max_end();
            return;
        }
        if block_idx > 0
            && self.blocks[block_idx - 1].entries.len() + self.blocks[block_idx].entries.len()
                <= MAX_BLOCK
        {
            let current = self.blocks.remove(block_idx);
            self.blocks[block_idx - 1].entries.extend(current.entries);
            self.blocks[block_idx - 1].recompute_max_end();
        }
    }

    /// 块超过上限时二分，重建块树。
    fn split_block(&mut self, block_idx: usize) {
        let split_at = self.blocks[block_idx].entries.len() / 2;
        let right_entries = self.blocks[block_idx].entries.split_off(split_at);
        self.blocks[block_idx].recompute_max_end();
        let right = Block::new(right_entries);
        self.blocks.insert(block_idx + 1, right);
        self.rebuild_tree();
    }

    /// 清空图层后从槽位重建整个排序结构。
    fn rebuild_from_slots(&mut self) {
        let mut entries: Vec<Entry> = self
            .slots
            .iter()
            .flatten()
            .map(Entry::from_annotation)
            .collect();
        entries.sort_unstable_by_key(|entry| (entry.start, entry.id));
        self.blocks = entries
            .chunks(TARGET_BLOCK)
            .map(|chunk| Block::new(chunk.to_vec()))
            .collect();
        self.rebuild_tree();
    }

    // ── 块级 max_end 线段树 ─────────────────────────────────────────────

    /// 重建块树。块结构变化（二分/合并/删除空块/整层清理）时调用。
    fn rebuild_tree(&mut self) {
        let mut size = 1usize;
        while size < self.blocks.len() {
            size <<= 1;
        }
        let mut tree = vec![0u64; size * 2];
        for (i, block) in self.blocks.iter().enumerate() {
            tree[size + i] = block.max_end;
        }
        for node in (1..size).rev() {
            tree[node] = tree[node * 2].max(tree[node * 2 + 1]);
        }
        self.tree_size = size;
        self.max_tree = tree;
    }

    /// 单块 `max_end` 变化后沿树更新到根。
    fn update_tree(&mut self, block_idx: usize) {
        let mut node = self.tree_size + block_idx;
        self.max_tree[node] = self.blocks[block_idx].max_end;
        node /= 2;
        while node > 0 {
            self.max_tree[node] = self.max_tree[node * 2].max(self.max_tree[node * 2 + 1]);
            node /= 2;
        }
    }

    /// 收集 `[0, hi_block]` 中 `max_end > threshold` 的块下标。
    ///
    /// 递归沿块树下钻：整块 `max_end <= threshold` 直接剪掉；叶子即候选块。
    fn collect_blocks(
        &self,
        node: usize,
        node_lo: usize,
        node_hi: usize,
        hi_block: usize,
        threshold: u64,
        out: &mut Vec<usize>,
    ) {
        if node_lo > hi_block || self.max_tree[node] <= threshold {
            return;
        }
        if node_hi - node_lo == 1 {
            if node_lo < self.blocks.len() {
                out.push(node_lo);
            }
            return;
        }
        let mid = node_lo + (node_hi - node_lo) / 2;
        self.collect_blocks(node * 2, node_lo, mid, hi_block, threshold, out);
        self.collect_blocks(node * 2 + 1, mid, node_hi, hi_block, threshold, out);
    }

    /// 最后一个首 `start <= at` 的块。
    fn hi_block_for_point(&self, at: u64) -> Option<usize> {
        let count = self
            .blocks
            .partition_point(|block| block.first_start().is_some_and(|first| first <= at));
        count.checked_sub(1)
    }

    /// 最后一个首 `start < end` 的块（区间查询的起点上界）。
    fn hi_block_for_range(&self, end: u64) -> Option<usize> {
        let count = self
            .blocks
            .partition_point(|block| block.first_start().is_some_and(|first| first < end));
        count.checked_sub(1)
    }

    /// 遍历所有覆盖 `at` 的条目。
    fn for_each_covering_point(&self, at: u64, mut f: impl FnMut(Entry)) {
        let Some(hi_block) = self.hi_block_for_point(at) else {
            return;
        };
        let mut candidates = Vec::new();
        self.collect_blocks(1, 0, self.tree_size, hi_block, at, &mut candidates);
        for block_idx in candidates {
            for entry in &self.blocks[block_idx].entries {
                if entry.covers_point(at) {
                    f(*entry);
                }
            }
        }
    }

    /// 遍历所有与 `range` 相交的条目。
    fn for_each_covering_range(&self, range: ByteRange, mut f: impl FnMut(Entry)) {
        let start = range.start().get();
        let end = range.end().get();
        if start >= end {
            return;
        }
        let Some(hi_block) = self.hi_block_for_range(end) else {
            return;
        };
        let mut candidates = Vec::new();
        // 用 start 作阈值：块 max_end <= start 的整块都不可能和 [start, end) 相交。
        self.collect_blocks(1, 0, self.tree_size, hi_block, start, &mut candidates);
        for block_idx in candidates {
            for entry in &self.blocks[block_idx].entries {
                if entry.overlaps(range) {
                    f(*entry);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::AnnotationKind;

    fn r(start: u64, end: u64) -> ByteRange {
        ByteRange::from_len(Offset::new(start), end - start).unwrap()
    }

    #[test]
    fn empty_index_has_no_hits() {
        let mut index = AnnotationIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.query_point(Offset::new(0)).is_empty());
        assert!(index.query_point(0u64).is_empty());
        assert!(index.query(r(0, 100)).is_empty());
        assert_eq!(index.deepest_at(Offset::new(5)), None);
        assert_eq!(index.remove(AnnotationId(1)), None);
        assert_eq!(index.get(AnnotationId(1)), None);
    }

    #[test]
    fn insert_assigns_monotonic_ids_and_stores_payload() {
        let mut index = AnnotationIndex::new();
        let a = index.insert_new(r(0, 1), Layer::Value, 0, AnnotationKind::Text("a".into()));
        let b = index.insert_new(
            r(2, 3),
            Layer::Search,
            0,
            AnnotationKind::Style {
                fg: None,
                bg: Some(crate::Rgba::rgb(1, 2, 3)),
            },
        );
        assert_eq!(a, AnnotationId(1));
        assert_eq!(b, AnnotationId(2));
        assert_eq!(
            index.get(a).unwrap().payload,
            AnnotationKind::Text("a".into())
        );
        assert_eq!(index.get(a).unwrap().id, a);
        assert_eq!(index.len(), 2);
        assert_eq!(index.iter().count(), 2);
    }

    #[test]
    fn single_point_interval_is_half_open() {
        let mut index = AnnotationIndex::new();
        let id = index.insert_new(r(5, 6), Layer::Search, 0, AnnotationKind::Marker);
        assert!(index.query_point(Offset::new(4)).is_empty());
        assert_eq!(index.query_point(Offset::new(5)), vec![id]);
        assert!(index.query_point(Offset::new(6)).is_empty());
        assert!(index.query(r(5, 5)).is_empty());
        assert_eq!(index.query(r(5, 6)), vec![id]);
        assert_eq!(index.query(r(0, 100)), vec![id]);
    }

    #[test]
    fn fully_contained_annotations_both_hit() {
        let mut index = AnnotationIndex::new();
        let outer = index.insert_new(r(0, 100), Layer::Structure, 0, AnnotationKind::Marker);
        let inner = index.insert_new(r(20, 30), Layer::Selection, 0, AnnotationKind::Marker);
        let got = index.query_point(Offset::new(25));
        assert_eq!(got, vec![outer, inner]);
        assert_eq!(index.deepest_at(Offset::new(25)), Some(inner));
        // 内层结束在 30（不含），30 只剩外层。
        assert_eq!(index.query_point(Offset::new(30)), vec![outer]);
        assert_eq!(index.query(r(20, 30)), vec![outer, inner]);
    }

    #[test]
    fn adjacent_ranges_do_not_overlap() {
        let mut index = AnnotationIndex::new();
        let left = index.insert_new(r(0, 10), Layer::Search, 0, AnnotationKind::Marker);
        let right = index.insert_new(r(10, 20), Layer::Search, 0, AnnotationKind::Marker);
        assert_eq!(index.query_point(Offset::new(9)), vec![left]);
        assert_eq!(index.query_point(Offset::new(10)), vec![right]);
        assert_eq!(index.query(r(0, 20)), vec![left, right]);
        assert!(index.query(r(10, 10)).is_empty());
        assert_eq!(index.query(r(0, 10)), vec![left]);
        assert_eq!(index.query(r(10, 20)), vec![right]);
    }

    #[test]
    fn same_layer_sorted_by_priority_then_id() {
        let mut index = AnnotationIndex::new();
        let p5 = index.insert_new(r(0, 10), Layer::Search, 5, AnnotationKind::Marker);
        let p1 = index.insert_new(r(0, 10), Layer::Search, 1, AnnotationKind::Marker);
        let p3 = index.insert_new(r(0, 10), Layer::Search, 3, AnnotationKind::Marker);
        assert_eq!(index.query_point(Offset::new(0)), vec![p1, p3, p5]);
        assert_eq!(index.deepest_at(Offset::new(0)), Some(p5));

        // 同层同 priority：id 大者最后（渲染在上）。
        let tie = index.insert_new(r(0, 10), Layer::Search, 5, AnnotationKind::Marker);
        assert_eq!(index.query_point(Offset::new(0)), vec![p1, p3, p5, tie]);
        assert_eq!(index.deepest_at(Offset::new(0)), Some(tie));
    }

    #[test]
    fn layer_z_is_primary_order() {
        let mut index = AnnotationIndex::new();
        let mut ids = Vec::new();
        for layer in Layer::ALL {
            ids.push(index.insert_new(r(0, 10), layer, 0, AnnotationKind::Marker));
        }
        assert_eq!(index.query_point(Offset::new(0)), ids);
        assert_eq!(
            index.deepest_at(Offset::new(0)),
            Some(ids[Layer::COUNT - 1])
        );
    }

    #[test]
    fn delete_then_query_excludes_id() {
        let mut index = AnnotationIndex::new();
        let a = index.insert_new(r(0, 10), Layer::Search, 0, AnnotationKind::Marker);
        let b = index.insert_new(r(0, 10), Layer::Structure, 0, AnnotationKind::Marker);
        let c = index.insert_new(r(0, 10), Layer::Selection, 0, AnnotationKind::Marker);
        assert_eq!(index.query_point(Offset::new(5)), vec![a, b, c]);
        let removed = index.remove(b).unwrap();
        assert_eq!(removed.id, b);
        assert_eq!(index.query_point(Offset::new(5)), vec![a, c]);
        assert_eq!(index.deepest_at(Offset::new(5)), Some(c));
        assert_eq!(index.remove(b), None);
        assert_eq!(index.get(b), None);
        assert_eq!(index.len(), 2);
        // id 不复用：下一次插入拿到 4。
        assert_eq!(
            index.insert_new(r(5, 6), Layer::Cursor, 0, AnnotationKind::Marker),
            AnnotationId(4)
        );
    }

    #[test]
    fn clear_layer_removes_only_that_layer() {
        let mut index = AnnotationIndex::new();
        let s1 = index.insert_new(r(0, 10), Layer::Search, 0, AnnotationKind::Marker);
        let _s2 = index.insert_new(r(2, 4), Layer::Search, 9, AnnotationKind::Marker);
        let v = index.insert_new(r(0, 10), Layer::Value, 0, AnnotationKind::Marker);
        assert_eq!(index.clear_layer(Layer::Search), 2);
        assert_eq!(index.query_point(Offset::new(3)), vec![v]);
        assert_eq!(index.get(s1), None);
        assert_eq!(index.clear_layer(Layer::Search), 0);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn clear_keeps_ids_monotonic_and_slots_valid() {
        let mut index = AnnotationIndex::new();
        let a = index.insert_new(r(0, 10), Layer::Search, 0, AnnotationKind::Marker);
        let b = index.insert_new(r(2, 4), Layer::Structure, 0, AnnotationKind::Marker);
        index.clear();
        assert!(index.is_empty());
        assert_eq!(index.get(a), None);
        assert_eq!(index.get(b), None);
        assert!(index.query_point(Offset::new(3)).is_empty());
        let c = index.insert_new(r(5, 6), Layer::Selection, 0, AnnotationKind::Marker);
        assert_eq!(c, AnnotationId(3));
        assert_eq!(index.query_point(Offset::new(5)), vec![c]);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn empty_annotation_never_matches() {
        let mut index = AnnotationIndex::new();
        let id = index.insert_new(r(7, 7), Layer::Bookmark, 255, AnnotationKind::Marker);
        assert!(index.query_point(Offset::new(7)).is_empty());
        assert!(index.query(r(0, 100)).is_empty());
        assert_eq!(index.deepest_at(Offset::new(7)), None);
        assert_eq!(index.remove(id).unwrap().range, r(7, 7));
    }

    #[test]
    fn same_start_can_span_many_blocks_and_delete_by_id() {
        let mut index = AnnotationIndex::new();
        let mut ids = Vec::new();
        for i in 0..500u64 {
            ids.push(index.insert_new(r(0, 1000), Layer::Search, 0, AnnotationKind::Marker));
            assert_eq!(ids[i as usize], AnnotationId(i + 1));
        }
        for id in ids.iter().step_by(2) {
            assert!(index.remove(*id).is_some());
        }
        let expected: Vec<u64> = ids.iter().skip(1).step_by(2).map(|id| id.0).collect();
        assert_eq!(
            index
                .query_point(Offset::new(500))
                .into_iter()
                .map(|id| id.0)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(index.len(), 250);
    }

    #[test]
    fn long_interval_does_not_force_scanning_all_short_ones() {
        // 回归"二分 + 回扫"退化：长区间在左端，右侧大量短区间。
        // 这里只验证正确性；性能形态由 benches/annot.rs 记录。
        let mut index = AnnotationIndex::new();
        let long = index.insert_new(r(0, 1_000_000), Layer::Structure, 0, AnnotationKind::Marker);
        for i in 0..10_000u64 {
            index.insert_new(
                r(500_000 + i * 2, 500_001 + i * 2),
                Layer::Search,
                0,
                AnnotationKind::Marker,
            );
        }
        assert_eq!(index.query_point(Offset::new(750_000)), vec![long]);
        assert!(index.query_point(Offset::new(1_500_000)).is_empty());
    }
}
