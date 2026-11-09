//! Piece table：编辑语义的**唯一真相**。DESIGN: 04 §3.2/§4.3、ADR-0001
//!
//! 这一版是 T1.2 的**语义层**：`Piece` 模型 + Overwrite/Insert/Remove + 合并规则。
//! 索引层（计数 B-tree）还没接上——现在 `pieces` 是平坦 `Vec`，编辑 O(n)，
//! 但**语义与不变量先钉死**，后面换索引不改调用方。
//!
//! 三条硬规则（04 §3.2）：
//! 1. **Piece 长度恒 >0**——空 piece 会让 "第 i 个字节在哪" 出现歧义。
//! 2. **相邻 Source piece 若在源上连续则必须合并**——否则 1 字节的碎片会堆积。
//! 3. **Source piece 被切到 <4 KiB 时降级为 Literal**——placeholder 的最小尺寸
//!    约束（04 §4.3 R1）。**本层不做降级**：降级要把源字节读进来，而语义层手里
//!    没有 Provider；它在 T1.3 的 `hydrate` 路径里执行（见 §10 开放问题）。

use std::sync::Arc;

/// placeholder 的最小尺寸（04 §4.3 R1）。
pub const MIN_PLACEHOLDER: u64 = 4096;
/// 单个 literal piece 的上限：超过就切块，避免一次粘贴生成 100 MB 的单片。
pub const MAX_LITERAL: usize = 64 * 1024;

/// 一个 piece：要么指向源文件的区间（懒加载），要么是内存里的字面量。
#[derive(Clone, Debug)]
pub enum Piece {
    /// 源文件 `[start, start+len)`，**不复制字节**。
    Source {
        /// 源文件内起始偏移。
        start: u64,
        /// 长度。
        len: u64,
    },
    /// 内存字面量（粘贴、覆盖写、降级后的 fragment）。
    Literal(Arc<[u8]>),
}

impl Piece {
    /// 字节长度。
    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            Self::Source { len, .. } => *len,
            Self::Literal(b) => b.len() as u64,
        }
    }

    /// 是否为空（不变量要求恒 false）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Piece table。
#[derive(Debug, Default)]
pub struct PieceTable {
    pieces: Vec<Piece>,
    len: u64,
    source_len: u64,
}

impl PieceTable {
    /// 以整段源区间建立初始表（打开文件：O(1)，只有一个 placeholder）。
    #[must_use]
    pub fn from_source(len: u64) -> Self {
        let pieces = if len == 0 {
            Vec::new()
        } else {
            vec![Piece::Source { start: 0, len }]
        };
        Self {
            pieces,
            len,
            source_len: len,
        }
    }

    /// 当前文档长度。
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// 是否为空文档。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// piece 数（诊断用；B-tree 接上后仍应保持 O(改动量/4KiB)）。
    #[must_use]
    pub fn piece_count(&self) -> usize {
        self.pieces.len()
    }

    /// 源文件长度（编辑不改变它）。
    #[must_use]
    pub fn source_len(&self) -> u64 {
        self.source_len
    }

    /// 把 `[at, at+len)` 按 piece 边界切成若干段，返回 `(源区间, 目标偏移)`。
    ///
    /// `Source` 段需要调用方从 Provider 读；`Literal` 段直接给字节。
    #[must_use]
    pub fn spans(&self, at: u64, len: u64) -> Vec<Span<'_>> {
        let end = at.saturating_add(len).min(self.len);
        let mut out = Vec::new();
        let mut pos = 0u64;
        for p in &self.pieces {
            let next = pos + p.len();
            if next <= at || pos >= end {
                pos = next;
                continue;
            }
            let from = at.max(pos) - pos;
            let take = (end.min(next) - pos) - from;
            out.push(match p {
                Piece::Source { start, .. } => Span::Source {
                    start: start + from,
                    len: take,
                    at: at.max(pos),
                },
                Piece::Literal(b) => {
                    let s = usize::try_from(from).unwrap_or(0);
                    let e = s + usize::try_from(take).unwrap_or(0);
                    Span::Literal {
                        bytes: &b[s..e],
                        at: at.max(pos),
                    }
                }
            });
            pos = next;
        }
        out
    }

    /// 覆盖写：把 `[at, at+data.len())` 换成 `data`，**允许越过 EOF 扩展**（04 §4.2）。
    pub fn overwrite(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        if at > self.len {
            return Err(EditError::OutOfRange { at, len: self.len });
        }
        let end = at.saturating_add(data.len() as u64);
        self.split_at(at)?;
        if end < self.len {
            self.split_at(end)?;
        }
        let (i, j) = self.range_indices(at, end);
        let new_pieces = chunks(data);
        self.replace_range(i, j, new_pieces);
        Ok(())
    }

    /// 插入（不覆盖，长度增加）。`at` 可等于 `len`（追加）。
    pub fn insert(&mut self, at: u64, data: &[u8]) -> Result<(), EditError> {
        if at > self.len {
            return Err(EditError::OutOfRange { at, len: self.len });
        }
        self.split_at(at)?;
        let i = self.range_indices(at, at).0;
        self.replace_range(i, i, chunks(data));
        Ok(())
    }

    /// 删除 `[at, at+len)`（长度减少）。
    pub fn remove(&mut self, at: u64, len: u64) -> Result<(), EditError> {
        let end = at.saturating_add(len);
        if end > self.len {
            return Err(EditError::OutOfRange {
                at: end,
                len: self.len,
            });
        }
        self.split_at(at)?;
        if end < self.len {
            self.split_at(end)?;
        }
        let (i, j) = self.range_indices(at, end);
        self.replace_range(i, j, Vec::new());
        Ok(())
    }

    /// 只导出**内存字面量**（调试用）。Source 段不在里面——想要完整字节请走
    /// [`Self::spans`] 并用 Provider 补齐。这个方法刻意不叫 `to_bytes`：一个
    /// 会静默把 Source 当零字节的函数，迟早有人拿它去保存文件。
    #[must_use]
    pub fn literal_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for p in &self.pieces {
            if let Piece::Literal(b) = p {
                out.extend_from_slice(b);
            }
        }
        out
    }

    /// 从纯内存内容建表（测试与剪贴板来源）。
    #[must_use]
    pub fn from_bytes(data: &[u8]) -> Self {
        let pieces = chunks(data);
        let len = data.len() as u64;
        Self {
            pieces,
            len,
            source_len: 0,
        }
    }

    /// 在 `at` 处切一次，保证 `at` 是 piece 边界。
    fn split_at(&mut self, at: u64) -> Result<(), EditError> {
        if at == 0 || at == self.len {
            return Ok(());
        }
        let mut pos = 0u64;
        for idx in 0..self.pieces.len() {
            let plen = self.pieces[idx].len();
            if pos == at {
                // `at` 已经落在边界上（例如刚插入的区间起点），不需要再切。
                return Ok(());
            }
            if pos < at && at < pos + plen {
                let off = at - pos;
                let mut right = self.split_piece(idx, off)?;
                if let Some(r) = right.take() {
                    self.pieces.insert(idx + 1, r);
                }
                return Ok(());
            }
            pos += plen;
        }
        Err(EditError::OutOfRange { at, len: self.len })
    }

    /// 把第 `idx` 个 piece 在 `off` 处切开，返回右半。
    ///
    /// Source 的切分**不读源字节**，只改区间——切到 <4 KiB 的碎片由 T1.3 的
    /// hydrate 统一降级（那时才有 Provider）。这里若偷偷填零，就是把数据改坏。
    fn split_piece(&mut self, idx: usize, off: u64) -> Result<Option<Piece>, EditError> {
        let piece = self.pieces[idx].clone();
        if off == 0 || off == piece.len() {
            return Ok(None);
        }
        match piece {
            Piece::Source { start, len } => {
                self.pieces[idx] = Piece::Source { start, len: off };
                Ok(Some(Piece::Source {
                    start: start + off,
                    len: len - off,
                }))
            }
            Piece::Literal(b) => {
                let s = usize::try_from(off).unwrap_or(0);
                let left = b[..s].to_vec();
                let right = b[s..].to_vec();
                self.pieces[idx] = Piece::Literal(Arc::from(left));
                Ok(Some(Piece::Literal(Arc::from(right))))
            }
        }
    }

    fn range_indices(&self, from: u64, to: u64) -> (usize, usize) {
        let mut pos = 0u64;
        let mut i = self.pieces.len();
        let mut j = self.pieces.len();
        for (k, p) in self.pieces.iter().enumerate() {
            if pos == from && i == self.pieces.len() {
                i = k;
            }
            if pos == to && j == self.pieces.len() {
                j = k;
            }
            pos += p.len();
        }
        if from == self.len {
            i = self.pieces.len();
        }
        if to == self.len {
            j = self.pieces.len();
        }
        (i, j)
    }

    fn replace_range(&mut self, i: usize, j: usize, new: Vec<Piece>) {
        let removed: u64 = self.pieces[i..j].iter().map(Piece::len).sum();
        self.pieces.splice(i..j, new.iter().cloned());
        let added: u64 = new.iter().map(Piece::len).sum();
        self.len = self.len - removed + added;
        self.coalesce();
    }

    /// 合并相邻连续 Source、相邻 Literal（04 §3.2 规则 2）。
    fn coalesce(&mut self) {
        let mut out: Vec<Piece> = Vec::with_capacity(self.pieces.len());
        for p in self.pieces.drain(..) {
            if p.is_empty() {
                continue;
            }
            match (out.last_mut(), &p) {
                (Some(Piece::Source { start, len }), Piece::Source { start: s2, .. })
                    if *start + *len == *s2 =>
                {
                    *len += p.len();
                }
                (Some(Piece::Literal(a)), Piece::Literal(b))
                    if a.len() + b.len() <= MAX_LITERAL =>
                {
                    let mut v = a.to_vec();
                    v.extend_from_slice(b);
                    *a = Arc::from(v);
                }
                _ => out.push(p.clone()),
            }
        }
        self.pieces = out;
    }
}

/// 一段读取区间。
#[derive(Debug)]
pub enum Span<'a> {
    /// 来自源文件。
    Source {
        /// 源文件内起始偏移。
        start: u64,
        /// 长度。
        len: u64,
        /// 该段在文档中的起始偏移。
        at: u64,
    },
    /// 来自内存。
    Literal {
        /// 字节。
        bytes: &'a [u8],
        /// 该段在文档中的起始偏移。
        at: u64,
    },
}

/// 编辑失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// 目标区间越界。
    OutOfRange {
        /// 请求的偏移。
        at: u64,
        /// 当前长度。
        len: u64,
    },
}

fn chunks(data: &[u8]) -> Vec<Piece> {
    if data.is_empty() {
        return Vec::new();
    }
    data.chunks(MAX_LITERAL)
        .map(|c| Piece::Literal(Arc::from(c)))
        .collect()
}
