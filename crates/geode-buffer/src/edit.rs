//! 编辑会话：覆盖/插入/删除 + 撤销 + patch 收集。DESIGN: 04 §4.2、§4.5、§4.6
//!
//! 三条语义（04 §4.2）：
//! 1. **默认 Overwrite**；越过 EOF 视为追加。
//! 2. `insert`/`remove` 必须显式调用——绝不会因为"多打一个字节"悄悄改变文件长度。
//! 3. 覆盖模式下按 Delete **只移动游标**，要清零必须调 `fill`（显式意图）。
//!
//! 撤销小步记录**逆操作**（原字节），大步记录编辑前的整表快照（COW，见
//! [`SNAPSHOT_MIN_BYTES`]）——前者在小编辑上省内存，后者避免 1 MiB 以上的操作
//! 产生同样大的逆操作副本。

use crate::piece::{EditError, Piece, PieceTable, Span};
use geode_core::{ByteRange, Offset};
use geode_provider::Provider;
use std::collections::VecDeque;
use std::sync::Arc;

/// nibble 编辑的分组窗口：同一字节内的两次半字节输入若间隔 ≤400 ms，合并为一次编辑。
pub const NIBBLE_GROUP_MS: u128 = 400;

/// 大操作撤销快照的阈值：一次操作影响的字节数达到 1 MiB 时，撤销栈改存
/// 编辑前的 [`PieceTable`] 快照（`Undo::Snapshot`），不再逐字节记逆操作。
pub const SNAPSHOT_MIN_BYTES: u64 = 1 << 20;

/// 撤销内存预算：`min(512 MiB, max(64 MiB, 2% × 文件长度))`（04 §4.6）。
#[must_use]
pub fn default_undo_limit(len: u64) -> u64 {
    const MIN: u64 = 64 << 20;
    const MAX: u64 = 512 << 20;
    (len / 50).clamp(MIN, MAX)
}

/// 一次可序列化的编辑操作，也是 journal（`.geojournal`）的载荷单元。
///
/// 编码为定长大端：`tag u8 │ at u64 │ len u32 │ data[len]`。`tag` 的取值与
/// `docs/23 §3.8` 的 `kind` 对齐：`0x01 Overwrite`、`0x02 Insert`、`0x03 Remove`。
/// `Fill` 是覆盖的语义特化（同一字节重复），journal 目前没有独立 kind，这里使用
/// 扩展值 `0x07`，落盘前可由 T1.8 规范化为 `Overwrite`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOp {
    /// 等长替换：从 `at` 起写入 `data`；越过 EOF 时追加。
    Overwrite {
        /// 起始偏移。
        at: u64,
        /// 新字节。
        data: Vec<u8>,
    },
    /// 插入：在 `at` 前插入 `data`（长度增加）。
    Insert {
        /// 插入点。
        at: u64,
        /// 新字节。
        data: Vec<u8>,
    },
    /// 删除 `[at, at+len)`（长度减少）。
    Remove {
        /// 起始偏移。
        at: u64,
        /// 删除长度。
        len: u64,
    },
    /// 显式填充：从 `at` 起把 `data.len()` 个字节写成 `data[0]`。
    Fill {
        /// 起始偏移。
        at: u64,
        /// 填充字节（所有元素必须相同；回放只取首字节）。
        data: Vec<u8>,
    },
}

impl EditOp {
    /// journal 记录帧里的 `kind` 字节（`docs/23 §3.8`）。
    #[must_use]
    pub fn tag(&self) -> u8 {
        match self {
            Self::Overwrite { .. } => 0x01,
            Self::Insert { .. } => 0x02,
            Self::Remove { .. } => 0x03,
            Self::Fill { .. } => 0x07,
        }
    }

    /// 编码为 journal 载荷（不含记录帧的 `seq`/`len`/CRC）。
    ///
    /// `at` 是 u64，原样编码；单条操作的数据长度超过 `u32::MAX` 时 panic——
    /// 合法编辑不可能达到 4 GiB，需要可恢复的失败用 [`Self::try_to_bytes`]。
    ///
    /// # Panics
    ///
    /// 当 `data.len()` 或删除长度超过 `u32::MAX` 时 panic。
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.try_to_bytes()
            .expect("单条编辑操作不可能超过 u32::MAX 字节")
    }

    /// [`Self::to_bytes`] 的可失败版本：长度超出 u32 编码宽度时返回 `Err`。
    pub fn try_to_bytes(&self) -> Result<Vec<u8>, EditOpError> {
        let (tag, at, len, data): (u8, u64, u64, &[u8]) = match self {
            Self::Overwrite { at, data } | Self::Fill { at, data } => {
                (self.tag(), *at, data.len() as u64, data)
            }
            Self::Insert { at, data } => (self.tag(), *at, data.len() as u64, data),
            Self::Remove { at, len } => (self.tag(), *at, *len, &[]),
        };
        if len > u64::from(u32::MAX) {
            return Err(EditOpError::TooLarge {
                what: "len",
                value: len,
            });
        }
        let mut out = Vec::with_capacity(13 + data.len());
        out.push(tag);
        out.extend_from_slice(&at.to_be_bytes());
        out.extend_from_slice(&(len as u32).to_be_bytes());
        out.extend_from_slice(data);
        Ok(out)
    }

    /// 从 journal 载荷解码。
    ///
    /// 任何截断、未知 tag、声明长度与实际不符、尾随字节都返回 `Err`，**绝不 panic**。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EditOpError> {
        const HEADER: usize = 13;
        if bytes.len() < HEADER {
            return Err(EditOpError::Truncated {
                need: HEADER,
                got: bytes.len(),
            });
        }
        let tag = bytes[0];
        let at = u64::from_be_bytes([
            bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        ]);
        let len = u32::from_be_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]);
        match tag {
            0x01 | 0x02 | 0x07 => {
                let data_len = usize::try_from(len).map_err(|_| EditOpError::TooLarge {
                    what: "len",
                    value: u64::from(len),
                })?;
                let need = HEADER.checked_add(data_len).ok_or(EditOpError::TooLarge {
                    what: "len",
                    value: u64::from(len),
                })?;
                if bytes.len() < need {
                    return Err(EditOpError::Truncated {
                        need,
                        got: bytes.len(),
                    });
                }
                if bytes.len() > need {
                    return Err(EditOpError::TrailingBytes {
                        extra: bytes.len() - need,
                    });
                }
                let data = bytes[HEADER..].to_vec();
                Ok(match tag {
                    0x01 => Self::Overwrite { at, data },
                    0x02 => Self::Insert { at, data },
                    _ => Self::Fill { at, data },
                })
            }
            0x03 => {
                if bytes.len() > HEADER {
                    return Err(EditOpError::TrailingBytes {
                        extra: bytes.len() - HEADER,
                    });
                }
                Ok(Self::Remove {
                    at,
                    len: u64::from(len),
                })
            }
            other => Err(EditOpError::UnknownTag(other)),
        }
    }
}

/// [`EditOp`] 解码/编码失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOpError {
    /// 输入短于声明的长度。
    Truncated {
        /// 需要的最少字节数。
        need: usize,
        /// 实际字节数。
        got: usize,
    },
    /// 头部 `len` 之外还有多余字节。
    TrailingBytes {
        /// 多余字节数。
        extra: usize,
    },
    /// 未知的 `tag`。
    UnknownTag(u8),
    /// 字段超出编码宽度（`len` 为 u32）。
    TooLarge {
        /// 字段名。
        what: &'static str,
        /// 实际值。
        value: u64,
    },
}

impl std::fmt::Display for EditOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { need, got } => write!(f, "载荷截断：需要 {need} 字节，只有 {got}"),
            Self::TrailingBytes { extra } => write!(f, "载荷尾部多出 {extra} 字节"),
            Self::UnknownTag(tag) => write!(f, "未知 EditOp tag 0x{tag:02X}"),
            Self::TooLarge { what, value } => {
                write!(f, "字段 {what} 的值 {value} 超出 u32 编码范围")
            }
        }
    }
}

impl std::error::Error for EditOpError {}

/// 快照的内存估算：literal 字节（快照与活动表共享 `Arc`，这里保守按全量计）
/// + piece 描述子 + `Arc` 头开销。
fn table_mem(table: &PieceTable) -> u64 {
    let literal: u64 = table
        .pieces
        .iter()
        .map(|p| match p {
            Piece::Literal(b) => b.len() as u64,
            Piece::Source { .. } => 0,
        })
        .sum();
    literal
        + (table.pieces.len() as u64) * (std::mem::size_of::<Piece>() as u64 + 8)
        + std::mem::size_of::<PieceTable>() as u64
}

/// 克隆一张 piece 表，**不读源数据**：`pieces` 整体克隆，`Piece::Literal` 的
/// `Arc<[u8]>` 只增加引用计数（真正的 COW）。
///
/// `PieceTable` 的 `len`/`source_len` 是私有字段，无法在 `edit` 模块里直接构造；
/// 这里先用公开原语建出一张长度/源长度都正确的空壳，再把 `pieces`（`pub(crate)`）
/// 换成克隆出的描述子。`S < L`（纯内存表或大段插入）时先补一段零字节把长度撑到
/// `L`，随后整段丢弃——峰值只多 `L - S` 字节，不复制既有 literal。
fn clone_table(table: &PieceTable) -> PieceTable {
    let mut out = PieceTable::from_source(table.source_len());
    if table.len() < table.source_len() {
        out.remove(table.len(), table.source_len() - table.len())
            .expect("快照克隆：截断到文档长度必然合法");
    } else if table.len() > table.source_len() {
        let pad = usize::try_from(table.len() - table.source_len())
            .expect("快照克隆：文档长度在 64 位平台必然可表示");
        out.insert(table.source_len(), &vec![0u8; pad])
            .expect("快照克隆：在源长度处追加必然合法");
    }
    out.pieces = table.pieces.clone();
    debug_assert_eq!(out.len(), table.len());
    debug_assert_eq!(out.source_len(), table.source_len());
    out
}

fn undo_size(op: &Undo) -> u64 {
    match op {
        Undo::Replaced { old, .. } => old.len() as u64 + 24,
        Undo::Inserted { .. } => 24,
        Undo::Removed { bytes, .. } => bytes.len() as u64 + 24,
        Undo::Snapshot(table) => table_mem(table),
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
    /// 大操作：编辑前的整表快照（结构共享的 COW 副本），撤销即回到该表。
    Snapshot(Arc<PieceTable>),
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
    clock: Box<dyn Fn() -> std::time::Instant + Send + Sync>,
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

    /// 撤销栈里当前有多少步是整表快照（大操作 COW）。
    #[must_use]
    pub fn undo_snapshot_count(&self) -> usize {
        self.undo
            .iter()
            .filter(|op| matches!(op, Undo::Snapshot(_)))
            .count()
    }

    /// 依次回放一组编辑操作，等价于按顺序调用 [`Self::overwrite`]/[`Self::insert`]/
    /// [`Self::remove`]/[`Self::fill`]。任何一步失败即返回 `Err`，此前成功的步骤
    /// 保留（与 journal 的"已写入即生效"语义一致）。
    pub fn replay(&mut self, ops: &[EditOp]) -> Result<(), EditError> {
        for op in ops {
            match op {
                EditOp::Overwrite { at, data } => self.overwrite(*at, data)?,
                EditOp::Insert { at, data } => self.insert(*at, data)?,
                EditOp::Remove { at, len } => self.remove(*at, *len)?,
                EditOp::Fill { at, data } => {
                    if let Some(&byte) = data.first() {
                        self.fill(*at, data.len() as u64, byte)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// 覆盖 `at` 起的字节；越过 EOF 时追加。
    pub fn overwrite(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        if at > self.table.len() {
            return Err(EditError::OutOfRange {
                at,
                len: self.table.len(),
            });
        }
        let snapshot =
            (data.len() as u64 >= SNAPSHOT_MIN_BYTES).then(|| Arc::new(clone_table(&self.table)));
        let old = self.read_span(at, data.len() as u64);
        let available = self.table.len().saturating_sub(at).min(data.len() as u64);
        let added = data.len() as u64 - available;
        self.table.overwrite(at, data)?;
        match snapshot {
            Some(table) => self.push_undo(Undo::Snapshot(table)),
            None => self.push_undo(Undo::Replaced {
                at,
                old: old.clone(),
                added,
            }),
        }
        self.push_patch(at, old, data.to_vec(), EditKind::Overwrite);
        self.nibble = None;
        Ok(())
    }

    /// 在 `at` 插入。
    pub fn insert(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        if at > self.table.len() {
            return Err(EditError::OutOfRange {
                at,
                len: self.table.len(),
            });
        }
        let snapshot =
            (data.len() as u64 >= SNAPSHOT_MIN_BYTES).then(|| Arc::new(clone_table(&self.table)));
        self.table.insert(at, data)?;
        match snapshot {
            Some(table) => self.push_undo(Undo::Snapshot(table)),
            None => self.push_undo(Undo::Inserted {
                at,
                len: data.len() as u64,
            }),
        }
        self.push_patch(at, Vec::new(), data.to_vec(), EditKind::Insert);
        self.nibble = None;
        Ok(())
    }

    /// 删除 `[at, at+len)`。
    pub fn remove(&mut self, at: u64, len: u64) -> Result<(), EditError> {
        let snapshot = (len >= SNAPSHOT_MIN_BYTES).then(|| Arc::new(clone_table(&self.table)));
        let bytes = self.read_span(at, len);
        self.table.remove(at, len)?;
        match snapshot {
            Some(table) => self.push_undo(Undo::Snapshot(table)),
            None => self.push_undo(Undo::Removed {
                at,
                bytes: bytes.clone(),
            }),
        }
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
        let size = undo_size(&op);
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
            Undo::Snapshot(table) => {
                // 栈上的快照通常独占；若仍被别处引用（例如调用方拿着旧表）就再
                // 克隆一份描述子，绝不复制 literal 字节。
                self.table = match Arc::try_unwrap(table) {
                    Ok(table) => table,
                    Err(shared) => clone_table(&shared),
                };
                true
            }
        };
        self.undo_bytes = self.undo_bytes.saturating_sub(size);
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
    pub fn set_clock(&mut self, f: impl Fn() -> std::time::Instant + Send + Sync + 'static) {
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
