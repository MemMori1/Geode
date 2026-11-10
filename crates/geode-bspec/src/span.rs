//! 源码位置：`SrcSpan` 与 `LineIndex`。
//!
//! DESIGN: 22-bspec-toolchain.md §3.2——源码偏移与数据区间**严格分型**：
//! `SrcSpan` 只表示编辑器里 UTF-8 源码的字节区间，永远不能和 `geode_core::ByteRange`
//! （文件数据区间）互相赋值。两者都叫“字节”，但一个属于模板源码、一个属于被解析的文件。

use core::fmt;

/// 源码里的半开字节区间 `[start, end)`。
///
/// 与数据区间 `geode_core::ByteRange` 严格区分：本类型的偏移量是**模板源码**里的
/// UTF-8 字节位置，不是被解析文件的偏移。字段用 `u32` 是因为单个模板文件不可能
/// 超过 4 GiB；这能让 AST/诊断更紧凑。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SrcSpan {
    /// 起始字节偏移（含）。
    pub start: u32,
    /// 结束字节偏移（不含）。
    pub end: u32,
}

impl SrcSpan {
    /// 构造区间；若 `start > end` 会自动交换，保证半开区间合法。
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        if start <= end {
            Self { start, end }
        } else {
            Self {
                start: end,
                end: start,
            }
        }
    }

    /// 位于 `at` 的空区间。
    #[must_use]
    pub const fn empty(at: u32) -> Self {
        Self { start: at, end: at }
    }

    /// 区间长度（字节）。
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// 是否为空。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// 是否包含某个字节偏移（半开区间）。
    #[must_use]
    pub const fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }

    /// 合并两个区间，取最小包络。
    ///
    /// 这是错误恢复时最常用的操作：当前 token 与上一次失败位置可能不相邻，
    /// 合并后 UI 能一次性高亮整段。
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        let start = if self.start < other.start {
            self.start
        } else {
            other.start
        };
        let end = if self.end > other.end {
            self.end
        } else {
            other.end
        };
        Self { start, end }
    }
}

/// 可合并源码区间的抽象。
///
/// 单独提供 trait 是为了让 AST 节点、token、诊断都能用统一的 `Span::merge`；
/// `SrcSpan` 同时提供同名固有方法，调用方不需要额外 `use`。
pub trait Span {
    /// 合并两个区间，返回最小包络。
    fn merge(self, other: Self) -> Self
    where
        Self: Sized;
}

impl Span for SrcSpan {
    fn merge(self, other: Self) -> Self {
        SrcSpan::merge(self, other)
    }
}

impl fmt::Display for SrcSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// 源码行索引：把 UTF-8 字节偏移映射为 1-based 行列。
///
/// 行首表只记录 `\n` 之后的第一个字节；列号按**字符**计数（不是字节、也不是
/// UTF-16 code unit），因此 `"é"` 只算 1 列。UI 若需要 UTF-16 列，可在本类型之上
/// 再做一次转换。
#[derive(Clone, Debug)]
pub struct LineIndex {
    /// 每一行第一个字节的偏移；第一项恒为 0。
    line_starts: Vec<u32>,
    /// 源文本副本，用于按字符计算列号。
    text: String,
    /// 源码总字节数（超过 `u32::MAX` 时饱和）。
    len: u32,
}

impl LineIndex {
    /// 为一整段源码建立行索引。
    #[must_use]
    pub fn new(src: &str) -> Self {
        let mut line_starts = Vec::with_capacity(src.lines().count().saturating_add(1));
        line_starts.push(0);
        for (idx, byte) in src.bytes().enumerate() {
            if byte == b'\n' {
                let next = idx.saturating_add(1);
                let next = u32::try_from(next).unwrap_or(u32::MAX);
                line_starts.push(next);
            }
        }
        let len = u32::try_from(src.len()).unwrap_or(u32::MAX);
        Self {
            line_starts,
            text: src.to_owned(),
            len,
        }
    }

    /// 把字节偏移映射为 `(行, 列)`，两者均为 1-based。
    ///
    /// 偏移超出源码末尾时按末尾处理；落在多字节 UTF-8 字符中间时向下对齐到
    /// 该字符的起始字节，保证不会返回半个字符的列号。
    #[must_use]
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let offset = offset.min(self.len) as usize;
        let line_idx = match self.line_starts.binary_search(&(offset as u32)) {
            Ok(idx) => idx,
            Err(0) => 0,
            Err(idx) => idx.saturating_sub(1),
        };
        let line_start = match self.line_starts.get(line_idx) {
            Some(v) => *v as usize,
            None => 0,
        };
        let mut end = offset;
        while end > line_start && !self.text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let column = match self.text.get(line_start..end) {
            Some(prefix) => match u32::try_from(prefix.chars().count()) {
                Ok(v) => v.saturating_add(1),
                Err(_) => u32::MAX,
            },
            None => 1,
        };
        let line = match u32::try_from(line_idx) {
            Ok(v) => v.saturating_add(1),
            Err(_) => u32::MAX,
        };
        (line, column)
    }

    /// 行首偏移表（第一项恒为 0）。
    #[must_use]
    pub fn line_starts(&self) -> &[u32] {
        &self.line_starts
    }

    /// 行数（末尾换行会额外产生一个空行，与编辑器行为一致）。
    #[must_use]
    pub fn line_count(&self) -> u32 {
        u32::try_from(self.line_starts.len()).unwrap_or(u32::MAX)
    }

    /// 源码总字节数（超过 `u32::MAX` 时饱和）。
    #[must_use]
    pub fn source_len(&self) -> u32 {
        self.len
    }

    /// 第 `line` 行（1-based）的行首偏移；越界返回 `None`。
    #[must_use]
    pub fn line_start(&self, line: u32) -> Option<u32> {
        let idx = usize::try_from(line.checked_sub(1)?).ok()?;
        self.line_starts.get(idx).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_is_one_based() {
        let index = LineIndex::new("abc\ndef");
        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(2), (1, 3));
        assert_eq!(index.line_col(4), (2, 1));
        assert_eq!(index.line_col(6), (2, 3));
    }

    #[test]
    fn line_col_counts_multibyte_as_one_column() {
        // 第 1 行：é（2 字节）+ 你（3 字节）+ a，`\n` 在字节 6，第 2 行从字节 7 开始。
        let index = LineIndex::new("é你a\n好x");
        assert_eq!(index.line_col(0), (1, 1));
        assert_eq!(index.line_col(2), (1, 2));
        assert_eq!(index.line_col(5), (1, 3));
        assert_eq!(index.line_col(6), (1, 4));
        assert_eq!(index.line_col(7), (2, 1));
        assert_eq!(index.line_col(10), (2, 2));
        assert_eq!(index.line_col(11), (2, 3));
    }

    #[test]
    fn line_col_clamps_and_handles_trailing_newline() {
        let index = LineIndex::new("a\n");
        assert_eq!(index.line_col(99), (2, 1));
        assert_eq!(index.line_count(), 2);
        assert_eq!(index.line_start(2), Some(2));
        assert_eq!(index.line_start(3), None);
    }

    #[test]
    fn merge_takes_envelope() {
        let a = SrcSpan::new(3, 8);
        let b = SrcSpan::new(1, 4);
        assert_eq!(a.merge(b), SrcSpan::new(1, 8));
        assert_eq!(Span::merge(a, b), SrcSpan::new(1, 8));
        assert_eq!(SrcSpan::new(9, 2), SrcSpan::new(2, 9));
        assert_eq!(SrcSpan::new(2, 9).len(), 7);
    }
}
