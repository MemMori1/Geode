//! R1 降级与局部 defrag（T1.3）。DESIGN: 04 §3.3 R1/R2/R3、§4.4
//!
//! 为什么必须在这一层：降级要把源字节读进来，而 `PieceTable` 本身没有 Provider。
//! 语义层硬填零 = 静默改坏数据，所以这件事只能由"能读"的层来做。

use crate::piece::{EditError, Piece, PieceTable, Span};
use crate::tree::PieceTree;
use geode_core::Offset;
use geode_provider::{Provider, ProviderError};

/// placeholder（`Piece::Source`）的最小尺寸。
///
/// 4 KiB = 页缓存与 IO 的粒度；更小的源片段没有语义价值，只会把 piece 数抬高
/// （04 §3.3 的 32 倍放大论证）。
pub const MIN_PLACEHOLDER: u64 = 4096;

/// 降级失败。
#[derive(Debug)]
pub enum HydrateError {
    /// 读源失败。
    Provider(ProviderError),
    /// 编辑越界（`defragment` 的区间）。
    Edit(EditError),
    /// 短读：需要 `want` 字节，只拿到 `got`。
    ShortRead {
        /// 期望字节数。
        want: u64,
        /// 实际字节数。
        got: u64,
    },
}

impl From<ProviderError> for HydrateError {
    fn from(e: ProviderError) -> Self {
        Self::Provider(e)
    }
}

impl std::fmt::Display for HydrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(e) => write!(f, "读源失败: {e}"),
            Self::Edit(e) => write!(f, "区间越界: {e}"),
            Self::ShortRead { want, got } => write!(f, "短读: 期望 {want} 字节，得到 {got}"),
        }
    }
}

impl std::error::Error for HydrateError {}

impl PieceTable {
    /// **R1**：把所有 `len < 4096` 的 placeholder 读成 literal。返回降级了几片。
    ///
    /// 幂等；`R2`（literal 永不升级为 placeholder）是结构性的——`Piece::Literal`
    /// 根本没有源区间可指。
    pub fn demote_small_placeholders(&mut self, src: &dyn Provider) -> Result<usize, HydrateError> {
        let mut demoted = 0usize;
        for i in 0..self.pieces.len() {
            let Piece::Source { start, len } = self.pieces[i] else {
                continue;
            };
            if len >= MIN_PLACEHOLDER {
                continue;
            }
            let buf = read_exact(src, start, len)?;
            self.pieces[i] = Piece::Literal(buf.into());
            demoted += 1;
        }
        if demoted > 0 {
            self.coalesce();
        }
        Ok(demoted)
    }

    /// 把 `[at, at+len)` 覆盖到的所有 piece 局部 literal 化（04 §4.4 的 `defragment`）。
    ///
    /// 用途：反复删/插产生的"缝"会让同一段源被切成多片，piece 数随时间增长。
    /// 局部 literal 化是**有界**的代价（只影响这一段），不是全文件重写。
    pub fn defragment(
        &mut self,
        at: u64,
        len: u64,
        src: &dyn Provider,
    ) -> Result<(), HydrateError> {
        if at.saturating_add(len) > self.len() {
            return Err(HydrateError::Edit(EditError::OutOfRange {
                at,
                len: self.len(),
            }));
        }
        if len == 0 {
            return Ok(());
        }
        // 先切齐边界，保证要处理的区间正好由若干整片组成。
        self.split_at(at).map_err(HydrateError::Edit)?;
        self.split_at(at + len).map_err(HydrateError::Edit)?;
        let mut pos = 0u64;
        let mut i = 0usize;
        while i < self.pieces.len() {
            let plen = self.pieces[i].len();
            let start = pos;
            pos += plen;
            if start < at || start + plen > at + len {
                i += 1;
                continue;
            }
            if let Piece::Source { start: so, len } = self.pieces[i] {
                let buf = read_exact(src, so, len)?;
                self.pieces[i] = Piece::Literal(buf.into());
            }
            i += 1;
        }
        self.coalesce();
        Ok(())
    }
}

/// **按需填页**：只把 `[at, at+len)` 覆盖到的 placeholder 降级为 literal。
///
/// 与 [`PieceTable::demote_small_placeholders`] 的区别是**不做全量扫描**：
/// 滚动时只填当前视口（通常 1~2 个 64 KiB 页），4 GiB 文档的其余部分保持
/// `Piece::Source`，内存占用与打开耗时都不随文件大小增长（04 §4.1.1）。
///
/// 区间按文档长度裁剪；已越界的部分静默忽略（与读取路径的"永不 panic"一致）。
/// 返回本次降级的 placeholder 片数。短读仍然报 [`HydrateError::ShortRead`]，
/// **不填零**。
pub trait EnsureFilled {
    /// 见 trait 文档。
    fn ensure_filled(
        &mut self,
        at: u64,
        len: u64,
        src: &dyn Provider,
    ) -> Result<usize, HydrateError>;
}

impl EnsureFilled for PieceTable {
    fn ensure_filled(
        &mut self,
        at: u64,
        len: u64,
        src: &dyn Provider,
    ) -> Result<usize, HydrateError> {
        let end = at.saturating_add(len).min(self.len());
        if at >= end {
            return Ok(0);
        }
        let segments = collect_source_segments(self.spans(at, end - at));
        apply_segments(segments, src, |doc_at, bytes| self.overwrite(doc_at, bytes))
    }
}

impl EnsureFilled for PieceTree {
    fn ensure_filled(
        &mut self,
        at: u64,
        len: u64,
        src: &dyn Provider,
    ) -> Result<usize, HydrateError> {
        let end = at.saturating_add(len).min(self.len());
        if at >= end {
            return Ok(0);
        }
        let segments = collect_source_segments(self.spans(at, end - at));
        // 树没有暴露可变 piece 遍历；等价长度的 `overwrite` 是公开 API，
        // 且只作用于请求区间内的 Source 段，字节不变、区间外不动。
        apply_segments(segments, src, |doc_at, bytes| self.overwrite(doc_at, bytes))
    }
}

/// 便捷函数形式：`hydrate::ensure_filled(&mut tree, at, len, &provider)`。
pub fn ensure_filled<B: EnsureFilled + ?Sized>(
    buffer: &mut B,
    at: u64,
    len: u64,
    src: &dyn Provider,
) -> Result<usize, HydrateError> {
    buffer.ensure_filled(at, len, src)
}

fn collect_source_segments(spans: Vec<Span<'_>>) -> Vec<(u64, u64, u64)> {
    spans
        .into_iter()
        .filter_map(|span| match span {
            Span::Source { start, len, at } => Some((at, start, len)),
            Span::Literal { .. } => None,
        })
        .collect()
}

fn apply_segments(
    segments: Vec<(u64, u64, u64)>,
    src: &dyn Provider,
    mut overwrite: impl FnMut(u64, &[u8]) -> Result<(), EditError>,
) -> Result<usize, HydrateError> {
    let mut demoted = 0usize;
    for (doc_at, source_start, len) in segments {
        let bytes = read_exact(src, source_start, len)?;
        overwrite(doc_at, &bytes).map_err(HydrateError::Edit)?;
        demoted += 1;
    }
    Ok(demoted)
}

pub(crate) fn read_exact(src: &dyn Provider, at: u64, len: u64) -> Result<Vec<u8>, HydrateError> {
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
    let report = src.read_range(Offset::new(at), &mut buf)?;
    if report.filled != len {
        return Err(HydrateError::ShortRead {
            want: len,
            got: report.filled,
        });
    }
    Ok(buf)
}
