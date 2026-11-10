//! 语法前端诊断：`ParseError` 与 `Diagnostics` 收集器。
//!
//! DESIGN: 02-dsl-spec-draft.md §8（诊断结构）、22-bspec-toolchain.md §3.2
//!（`SrcSpan` 与 `ByteRange` 严格分型）。
//!
//! **错误码说明**：`docs/errors.toml` 里 `E4001` 的标题是“语法错误：此处不该出现
//! 该 token”，并非词法专用码；E4 段目前没有“词法错误”码，且本任务不允许改
//! `docs/errors.toml`。因此词法错误暂时复用语义最接近的 `E4001`（`E_LEX`），待错误码
//! 表增补词法码后再切换。词法/语法错误的源码区间一律写入 `Diag.range`——`geode_core`
//! 的 `Diag` 还没有 `SrcSpan` 字段，而本任务不允许改 `geode-core`；`span_of` 可无损
//! 还原出 `SrcSpan`。

use crate::span::SrcSpan;
use geode_core::diag::{Code, Diag, Severity};
use geode_core::{ByteRange, Offset};

/// 词法错误码（暂复用 E4001，原因见模块文档）。
pub const E_LEX: Code = Code("E4001");

/// 语法错误码（与 `errors.toml` 的 E4001 对应）。
pub const E_SYNTAX: Code = Code("E4001");

/// 把错误码 + 源码区间 + 细节组装成 `geode_core::Diag`。
///
/// 源码区间写进 `range`：`geode-core::diag::Diag` 目前只有这一个字节区间字段。
/// `start <= end` 由 `SrcSpan` 保证；万一调用方用公开字段造出倒置区间，
/// `ByteRange::new` 返回 `None`，诊断仍然保留，只是没有高亮区间。
#[must_use]
pub fn diag_at(code: Code, span: SrcSpan, detail: impl Into<String>) -> Diag {
    let start = Offset::new(u64::from(span.start));
    let end = Offset::new(u64::from(span.end));
    let mut diag = Diag::error(code, detail);
    if let Some(range) = ByteRange::new(start, end) {
        diag = diag.with_range(range);
    }
    diag
}

/// 从诊断的 `range` 还原源码区间。
///
/// 返回 `None` 表示这不是本模块产生的带源码位置的诊断，或偏移超出 `u32`。
#[must_use]
pub fn span_of(diag: &Diag) -> Option<SrcSpan> {
    let range = diag.range?;
    let start = u32::try_from(range.start().get()).ok()?;
    let end = u32::try_from(range.end().get()).ok()?;
    Some(SrcSpan::new(start, end))
}

/// 解析错误：轻量包装一条 `geode_core::diag::Diag`。
#[derive(Clone, Debug)]
pub struct ParseError {
    /// 底层诊断（错误码、区间、细节）。
    pub diag: Diag,
}

impl ParseError {
    /// 用错误码、源码区间和细节构造。
    #[must_use]
    pub fn new(code: Code, span: SrcSpan, detail: impl Into<String>) -> Self {
        Self {
            diag: diag_at(code, span, detail),
        }
    }

    /// 构造一条语法错误（E4001）。
    #[must_use]
    pub fn syntax(span: SrcSpan, detail: impl Into<String>) -> Self {
        Self::new(E_SYNTAX, span, detail)
    }

    /// 取出底层诊断。
    #[must_use]
    pub fn into_diag(self) -> Diag {
        self.diag
    }

    /// 该错误涉及的源码区间。
    #[must_use]
    pub fn span(&self) -> Option<SrcSpan> {
        span_of(&self.diag)
    }
}

impl From<ParseError> for Diag {
    fn from(value: ParseError) -> Self {
        value.diag
    }
}

/// 诊断收集器。
///
/// 解析器在错误恢复时持续 `push`，最后一次性交还调用方；这样“一次至少报 3 个错”
/// 不需要把错误塞进 `Result` 的每个中间层。
#[derive(Clone, Debug, Default)]
pub struct Diagnostics(
    /// 已收集的诊断，按产生顺序排列。
    pub Vec<Diag>,
);

impl Diagnostics {
    /// 空收集器。
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// 追加一条诊断。
    pub fn push(&mut self, diag: Diag) {
        self.0.push(diag);
    }

    /// 便捷追加：错误码 + 源码区间 + 细节。
    pub fn push_error(&mut self, code: Code, span: SrcSpan, detail: impl Into<String>) {
        self.push(diag_at(code, span, detail));
    }

    /// 是否已经收集到 error/fatal 级诊断。
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.0
            .iter()
            .any(|d| matches!(d.severity, Severity::Error | Severity::Fatal))
    }

    /// 已收集数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 只读遍历。
    pub fn iter(&self) -> impl Iterator<Item = &Diag> {
        self.0.iter()
    }

    /// 取出内部向量。
    #[must_use]
    pub fn into_vec(self) -> Vec<Diag> {
        self.0
    }
}

impl From<Vec<Diag>> for Diagnostics {
    fn from(value: Vec<Diag>) -> Self {
        Self(value)
    }
}

impl FromIterator<Diag> for Diagnostics {
    fn from_iter<T: IntoIterator<Item = Diag>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_collect_and_report() {
        let mut diagnostics = Diagnostics::new();
        assert!(!diagnostics.has_errors());
        diagnostics.push_error(E_SYNTAX, SrcSpan::new(2, 5), "缺少分号");
        diagnostics.push(diag_at(E_LEX, SrcSpan::new(7, 8), "非法字符"));
        assert!(diagnostics.has_errors());
        assert_eq!(diagnostics.len(), 2);
        assert!(!diagnostics.is_empty());
        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 2);
        assert_eq!(span_of(&diags[0]), Some(SrcSpan::new(2, 5)));
    }

    #[test]
    fn parse_error_carries_span() {
        let err = ParseError::syntax(SrcSpan::new(10, 14), "期望 `;`");
        assert_eq!(err.span(), Some(SrcSpan::new(10, 14)));
        assert_eq!(err.diag.code, E_SYNTAX);
        assert_eq!(err.into_diag().severity, Severity::Error);
    }
}
