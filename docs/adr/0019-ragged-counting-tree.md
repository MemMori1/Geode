# 0019 索引层先用"松散计数树"，不一步做到严格 B-tree

## 状态

已采纳（2026-11-10）。**临时性决策**：等 S1（placeholder 4 KiB 碎片化）有结论后重评。

## 背景

T1.2 要求"计数型 B-tree：叶 64 piece、fanout 64、split/join"（04 §3.2、ADR-0001）。
基线实测（`docs/spikes/T1.2b-baseline.md`）：平坦 `Vec<Piece>` 在 2,048 piece 时定位
1.11 µs、编辑 28.2 µs；外推到 10 万 piece 是 54 µs / 1.4 ms，一帧 68 次定位约 3.7 ms，
**过不了 05 §5.2 的滚动 p99 <25 ms**。

严格 B-tree 的 `split`/`join` 需要处理借位（borrow）与合并（merge），在 Rust 里还要
绕开 `&mut` 别名；一次写对不难，写错却**不会崩**——只会让某些偏移读出错误字节。

## 决策

先实现 `PieceTree`：叶子 ≤64 piece、内部节点 ≤64 子节点、每节点缓存 `bytes`，
`split`/`concat` 只做"超限就分裂"，**允许内部节点子节点高度不一致、允许节点欠载**。

## 后果

好处：

- 定位已经是 O(高度)：10 万 piece 时树高 ≤6、定位 **0.48 µs**，且与偏移位置无关；
- `split`/`concat` 不需要 borrow/merge，代码量小、可被 150 例随机编辑对拍钉住；
- 调用方接口（`spans`/`overwrite`/`insert`/`remove`）与最终 B-tree 一致，换实现不改上层。

代价：

- 节点可能不"胖"，最坏情况下高度高于 log₆₄ n；
- 相邻 piece 的合并规则还留在语义层，树内没有做——piece 数会多于理论值；
- **`PieceCursor` 还没改成沿树下降**，现在仍走平坦表。

何时应重新考虑：S1 测出 1 万次随机编辑后 piece 数超预算，或 M1 的 `edit_random`
基准显示树内 piece 数成为瓶颈——那时把"分裂即打包"换成真正的 B-tree merge/borrow。

## 备选方案与否决理由

| 方案 | 否决理由 |
|---|---|
| implicit treap | ADR-0001 已否决（缓存局部性差、每节点随机优先级 8 B 开销）；本次不再翻案 |
| 一步写严格 B-tree | 工期风险：没有 S1 的碎片数上界，无法判断该不该为 merge/borrow 付复杂度 |
| 继续用平坦 `Vec` | 已被基线否掉：10 万 piece 一帧定位 3.7 ms |
