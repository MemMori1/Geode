//! 编辑会话：覆盖/插入/删除 + 撤销 + patch 收集。DESIGN: 04 §4.2、§4.5、§4.6
//!
//! 三条语义（04 §4.2）：
//! 1. **默认 Overwrite**；越过 EOF 视为追加。
//! 2. `insert`/`remove` 必须显式调用——绝不会因为"多打一个字节"悄悄改变文件长度。
//! 3. 覆盖模式下按 Delete **只移动游标**，要清零必须调 `fill`（显式意图）。
//!
//! 撤销记录的是**逆操作**（原字节），不是"把文件回滚到某个版本"——后者在 100 GB
//! 文件上等于每次编辑复制一份。

use crate::piece::{EditError, Piece, PieceTable, Span};
use geode_core::{ByteRange, Offset};
use geode_provider::Provider;
use std::collections::VecDeque;
use std::sync::Arc;

/// nibble 编辑的分组窗口：同一字节内的两次半字节输入若间隔 ≤400 ms，合并为一次编辑。
pub const NIBBLE_GROUP_MS: u128 = 400;

/// 撤销内存预算：`min(512 MiB, max(64 MiB, 2% × 文件长度))`（04 §4.6）。
#[must_use]
pub fn default_undo_limit(len: u64) -> u64 {
    const MIN: u64 = 64 << 20;
    const MAX: u64 = 512 << 20;
    (len / 50).clamp(MIN, MAX)
}

fn undo_size(op: &Undo) -> u64 {
    match op {
        Undo::Replaced { old, .. } => old.len() as u64 + 24,
        Undo::Inserted { .. } => 24,
        Undo::Removed { bytes, .. } => bytes.len() as u64 + 24,
    }
}

/// patch 归并的最大间隙（04 §4.5：16 = 一行内的典型编辑跨度）。
pub const PATCH_MERGE_GAP: u64 = 16;

/// 一次编辑的类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// 覆盖（不改长度）。
    Overwrite,
    /// 插入（长度增加）。
    Insert,
    /// 删除（长度减少）。
    Remove,
    /// 显式填充。
    Fill,
}

/// 一个 patch：原区间 + 旧字节 + 新字节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    /// 原文件坐标（插入时为插入点）。
    pub orig: ByteRange,
    /// 编辑前的字节（插入时为空）。
    pub old: Vec<u8>,
    /// 编辑后的字节（删除时为空）。
    pub new: Vec<u8>,
    /// 类型。
    pub kind: EditKind,
}

impl Patch {
    fn cur_len(&self) -> u64 {
        self.new.len() as u64
    }
}

/// 撤销记录。
#[derive(Debug)]
enum Undo {
    /// 覆盖：把 `old` 写回 `at`，并删掉当初越 EOF 追加的 `added` 字节。
    Replaced { at: u64, old: Vec<u8>, added: u64 },
    /// 插入：删掉 `at` 起的 `len` 字节。
    Inserted { at: u64, len: u64 },
    /// 删除：把 `bytes` 插回 `at`。
    Removed { at: u64, bytes: Vec<u8> },
}

/// 编辑会话。
pub struct Editor {
    table: PieceTable,
    src: Arc<dyn Provider>,
    undo: VecDeque<Undo>,
    undo_bytes: u64,
    undo_limit: u64,
    dropped_steps: u64,
    patches: Vec<Patch>,
    /// 最近一次 nibble 编辑：(字节偏移, 已写入的高半字节, 发生时刻)。
    nibble: Option<(u64, u8, std::time::Instant)>,
    clock: Box<dyn Fn() -> std::time::Instant + Send>,
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor")
            .field("len", &self.table.len())
            .field("undo", &self.undo.len())
            .field("patches", &self.patches.len())
            .finish()
    }
}

impl Editor {
    /// 用一张表和一个数据源建会话。
    #[must_use]
    pub fn new(table: PieceTable, src: Arc<dyn Provider>) -> Self {
        let undo_limit = default_undo_limit(table.len());
        Self {
            table,
            src,
            undo: VecDeque::new(),
            undo_bytes: 0,
            undo_limit,
            dropped_steps: 0,
            patches: Vec::new(),
            nibble: None,
            clock: Box::new(std::time::Instant::now),
        }
    }

    /// 当前长度。
    #[must_use]
    pub fn len(&self) -> u64 {
        self.table.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// 只读访问底层表。
    #[must_use]
    pub fn table(&self) -> &PieceTable {
        &self.table
    }

    /// 已收集的 patch（按原坐标升序、已归并）。
    #[must_use]
    pub fn patches(&self) -> &[Patch] {
        &self.patches
    }

    /// 可以撤销几步。
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// 覆盖 `at` 起的字节；越过 EOF 时追加。
    pub fn overwrite(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        if at > self.table.len() {
            return Err(EditError::OutOfRange {
                at,
                len: self.table.len(),
            });
        }
        let old = self.read_span(at, data.len() as u64);
        let available = self.table.len().saturating_sub(at).min(data.len() as u64);
        let added = data.len() as u64 - available;
        self.table.overwrite(at, data)?;
        self.push_undo(Undo::Replaced {
            at,
            old: old.clone(),
            added,
        });
        self.push_patch(at, old, data.to_vec(), EditKind::Overwrite);
        self.nibble = None;
        Ok(())
    }

    /// 在 `at` 插入。
    pub fn insert(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        self.table.insert(at, data)?;
        self.push_undo(Undo::Inserted {
            at,
            len: data.len() as u64,
        });
        self.push_patch(at, Vec::new(), data.to_vec(), EditKind::Insert);
        self.nibble = None;
        Ok(())
    }

    /// 删除 `[at, at+len)`。
    pub fn remove(&mut self, at: u64, len: u64) -> Result<(), EditError> {
        let bytes = self.read_span(at, len);
        self.table.remove(at, len)?;
        self.push_undo(Undo::Removed {
            at,
            bytes: bytes.clone(),
        });
        self.push_patch(at, bytes, Vec::new(), EditKind::Remove);
        self.nibble = None;
        Ok(())
    }

    /// 显式填充（覆盖模式的 Delete 不会调到这）。
    pub fn fill(&mut self, at: u64, len: u64, byte: u8) -> Result<(), EditError> {
        let n = usize::try_from(len).unwrap_or(usize::MAX);
        self.overwrite(at, &vec![byte; n]).map(|()| {
            if let Some(p) = self.patches.last_mut() {
                p.kind = EditKind::Fill;
            }
        })
    }

    /// 半字节编辑：`high` 为真改高 4 位，否则低 4 位。
    ///
    /// 同一字节的两次 nibble 编辑若间隔 ≤400 ms，合并成一次字节级编辑（撤销一次回到原字节）。
    pub fn nibble(&mut self, at: u64, high: bool, value: u8) -> Result<(), EditError> {
        let cur = self.read_span(at, 1);
        let old = *cur.first().unwrap_or(&0);
        let now = (self.clock)();
        let merged = matches!(self.nibble, Some((p, _, t)) if p == at && now.duration_since(t).as_millis() <= NIBBLE_GROUP_MS);
        let new = if high {
            (value << 4) | (old & 0x0F)
        } else {
            (old & 0xF0) | (value & 0x0F)
        };
        if merged {
            // 复用上一步撤销记录与 patch，不再新增。
            self.table.overwrite(at, &[new])?;
            debug_assert!(
                matches!(self.undo.back(), Some(Undo::Replaced { at: u, .. }) if *u == at)
            );
            if let Some(p) = self.patches.last_mut() {
                p.new = vec![new];
            }
        } else {
            self.table.overwrite(at, &[new])?;
            self.push_undo(Undo::Replaced {
                at,
                old: cur,
                added: 0,
            });
            self.push_patch(at, vec![old], vec![new], EditKind::Overwrite);
        }
        self.nibble = Some((at, new, now));
        Ok(())
    }

    fn push_undo(&mut self, op: Undo) {
        self.undo_bytes += undo_size(&op);
        self.undo.push_back(op);
        while self.undo_bytes > self.undo_limit && self.undo.len() > 1 {
            if let Some(old) = self.undo.pop_front() {
                self.undo_bytes = self.undo_bytes.saturating_sub(undo_size(&old));
                self.dropped_steps += 1;
            }
        }
    }

    /// 当前撤销记录占用的估算字节数。
    #[must_use]
    pub fn undo_bytes(&self) -> u64 {
        self.undo_bytes
    }

    /// 撤销预算（字节）。
    #[must_use]
    pub fn undo_limit(&self) -> u64 {
        self.undo_limit
    }

    /// 因超出预算被丢弃的最早撤销步数。
    #[must_use]
    pub fn dropped_steps(&self) -> u64 {
        self.dropped_steps
    }

    /// 覆盖撤销预算（测试与"大文件省内存"模式用）。
    pub fn set_undo_limit(&mut self, limit: u64) {
        self.undo_limit = limit.max(1);
    }

    /// 撤销一步。
    pub fn undo(&mut self) -> bool {
        let Some(op) = self.undo.pop_back() else {
            return false;
        };
        let ok = match op {
            Undo::Replaced { at, old, added } => {
                let wrote = self.table.overwrite(at, &old).is_ok();
                let trimmed = if added == 0 {
                    true
                } else {
                    self.table.remove(at + old.len() as u64, added).is_ok()
                };
                wrote && trimmed
            }
            Undo::Inserted { at, len } => self.table.remove(at, len).is_ok(),
            Undo::Removed { at, bytes } => self.table.insert(at, &bytes).is_ok(),
        };
        self.nibble = None;
        ok
    }

    /// 全部撤销；返回实际撤销步数。
    pub fn undo_all(&mut self) -> usize {
        let mut n = 0;
        while self.undo() {
            n += 1;
        }
        n
    }

    /// 读一段（跨 piece）。
    #[must_use]
    pub fn read(&self, at: u64, len: u64) -> Vec<u8> {
        self.read_span(at, len)
    }

    fn read_span(&self, at: u64, len: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        if len == 0 {
            return out;
        }
        for s in self.table.spans(at, len) {
            match s {
                Span::Source { start, len, .. } => {
                    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
                    let _ = self.src.read_range(Offset::new(start), &mut buf);
                    out.extend_from_slice(&buf);
                }
                Span::Literal { bytes, .. } => out.extend_from_slice(bytes),
            }
        }
        out.truncate(usize::try_from(len).unwrap_or(usize::MAX));
        out
    }

    fn push_patch(&mut self, at: u64, old: Vec<u8>, new: Vec<u8>, kind: EditKind) {
        let orig_len = old.len() as u64;
        let overwrite_like = |k: EditKind| matches!(k, EditKind::Overwrite | EditKind::Fill);
        let compatible = self.patches.last().is_some_and(|last| {
            let last_end = last.orig.end().get();
            // 只归并"等长替换"：插入/删除会改变后续坐标，归并后既不是合法的
            // 连续区间替换，也无法用 orig 坐标重放（见 04 §4.5 的 seq 语义）。
            // 另外必须严格向后（at >= last_end），否则重叠编辑会被拼成重复字节。
            at >= last_end
                && at - last_end <= PATCH_MERGE_GAP
                && overwrite_like(kind)
                && overwrite_like(last.kind)
        });
        if compatible {
            let last_end = self.patches.last().expect("刚判过非空").orig.end().get();
            let gap = at.saturating_sub(last_end);
            // 归并要把中间未改动的字节一并带上，否则 patch 不再是"连续区间替换"。
            let filler = if gap > 0 {
                self.read_span(last_end, gap)
            } else {
                Vec::new()
            };
            let last = self.patches.last_mut().expect("刚判过非空");
            last.old.extend_from_slice(&filler);
            last.new.extend_from_slice(&filler);
            last.old.extend_from_slice(&old);
            last.new.extend_from_slice(&new);
            let start = last.orig.start();
            last.orig = ByteRange::from_len(start, last.old.len() as u64)
                .expect("patch 起点 + 长度必然可表示");
            return;
        }
        self.patches.push(Patch {
            orig: ByteRange::from_len(Offset::new(at), orig_len)
                .expect("编辑起点 + 长度必然可表示"),
            old,
            new,
            kind,
        });
    }

    /// 供测试注入时钟（nibble 分组用）。
    pub fn set_clock(&mut self, f: impl Fn() -> std::time::Instant + Send + 'static) {
        self.clock = Box::new(f);
    }

    /// 当前 patch 覆盖的总字节数（诊断用）。
    #[must_use]
    pub fn patch_bytes(&self) -> u64 {
        self.patches.iter().map(|p| p.cur_len()).sum()
    }

    /// 底层 piece 数（诊断用）。
    #[must_use]
    pub fn piece_count(&self) -> usize {
        self.table.piece_count()
    }

    /// piece 是否还是 Source（诊断用）。
    #[must_use]
    pub fn is_source_piece(&self, i: usize) -> bool {
        matches!(
            self.table.spans(0, self.table.len()).get(i),
            Some(Span::Source { .. })
        )
    }

    /// 把字面量片段拼成完整字节（**仅测试与小文件用**）。
    #[must_use]
    pub fn materialize(&self) -> Vec<u8> {
        self.read_span(0, self.table.len())
    }
}

/// `Piece::Literal` 便捷构造（测试用）。
#[must_use]
pub fn literal(bytes: &[u8]) -> Piece {
    Piece::Literal(bytes.to_vec().into())
}
