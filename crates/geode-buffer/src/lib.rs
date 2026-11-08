//! 文档缓冲区。
//!
//! DESIGN: 04-buffer-and-edit-model.md §3.1、§4.1
//!
//! **M0 只实现只读骨架**：`Buffer` 持有 `Provider`，对外暴露 `len()` / `version()` /
//! `read()`。这一层的存在是为了让渲染层（05）在 M1 换实现时无需改动——
//! piece table 只是把 `read()` 的内部分解方式从"直接转发"换成"跨 piece 拼接"。
//!
//! `version()` 是**整个异步体系的正确性支点**（00 §6）：所有后台任务拿到结果后
//! 必须比对 `version()`，不一致就丢弃。因此它必须单调递增、且只在编辑提交时变。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use geode_core::{ByteRange, Offset};
use geode_provider::{Caps, Provider, ProviderError, SizeInfo};
use std::fmt;

pub use geode_core::Version;

/// 打开中的文档。
pub struct Buffer {
    provider: Box<dyn Provider>,
    version: Version,
    len: u64,
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("source", &self.provider.display_name())
            .field("len", &self.len)
            .field("version", &self.version)
            .finish()
    }
}

impl Buffer {
    /// 从数据源创建，版本从 0 开始。
    pub fn new(provider: Box<dyn Provider>) -> Self {
        let len = provider.size().lower_bound();
        Self {
            provider,
            version: Version::new(0),
            len,
        }
    }

    /// 当前可见长度。编辑未提交前不变（M0 无编辑）。
    pub fn len(&self) -> u64 {
        self.len
    }

    /// 是否为空文档（0 字节也必须能打开，见 20 §4.3 边界样本）。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 数据源能力。
    pub fn caps(&self) -> Caps {
        self.provider.caps()
    }

    /// 数据源显示名。
    pub fn display_name(&self) -> String {
        self.provider.display_name()
    }

    /// 精确度未知的长度信息。
    pub fn size_info(&self) -> SizeInfo {
        self.provider.size()
    }

    /// 当前版本号。
    pub fn version(&self) -> Version {
        self.version
    }

    /// 读一段，**越界自动裁剪**，短读合法。
    ///
    /// 返回实际填充量。调用方（渲染层）不得假设 `buf` 被填满。
    pub fn read(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        // 先按文档长度裁剪，避免把"文档末尾之外"的字节读进来。
        let avail = self.len.saturating_sub(at.get());
        let want = usize::try_from(avail.min(buf.len() as u64)).unwrap_or(buf.len());
        let mut report = self.provider.read_range(at, &mut buf[..want])?;
        if want < buf.len() {
            report.eof_hit = true;
        }
        Ok(report)
    }

    /// 读一个区间，返回新分配的 Vec（渲染层用于块加载）。
    pub fn read_range(&self, range: ByteRange) -> Result<Vec<u8>, ProviderError> {
        let mut buf = vec![0u8; usize::try_from(range.len()).unwrap_or(0)];
        let report = self.read(range.start(), &mut buf)?;
        buf.truncate(usize::try_from(report.filled).unwrap_or(buf.len()));
        Ok(buf)
    }

    /// 把数据源的新长度同步进来（外部追加/截断后调用，会推进版本号）。
    pub fn resync_len(&mut self, new_len: u64) {
        if new_len != self.len {
            self.len = new_len;
            self.version = self.version.next();
        }
    }

    /// 提交一次编辑后推进版本号。
    ///
    /// 这是**唯一**允许推进版本的入口（M1 由 piece table 的内部操作调用）。
    pub fn commit_edit(&mut self, new_len: u64) -> Version {
        self.len = new_len;
        self.version = self.version.next();
        self.version
    }
}

/// 读结果的重导出，避免调用方同时依赖两个 crate 的类型名。
pub type ReadReport = geode_provider::ReadReport;

#[cfg(test)]
mod tests {
    use super::*;
    use geode_provider::MemoryProvider;

    fn buf_of(n: u64) -> Buffer {
        let bytes: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        Buffer::new(Box::new(MemoryProvider::new("t", bytes)))
    }

    #[test]
    fn reads_clipped_at_eof() {
        let b = buf_of(32);
        let mut out = [0u8; 16];
        let r = b.read(Offset::new(24), &mut out).unwrap();
        assert_eq!(r.filled, 8);
        assert!(r.eof_hit);
    }

    #[test]
    fn read_far_past_end_is_empty_not_panic() {
        let b = buf_of(4);
        let mut out = [0u8; 8];
        let r = b.read(Offset::new(1 << 40), &mut out).unwrap();
        assert_eq!(r.filled, 0);
        assert!(r.eof_hit);
    }

    #[test]
    fn empty_document_opens_and_reads_zero() {
        let b = buf_of(0);
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        let mut out = [0u8; 4];
        assert_eq!(b.read(Offset::ZERO, &mut out).unwrap().filled, 0);
    }

    #[test]
    fn version_is_monotonic_and_only_on_commit() {
        let mut b = buf_of(8);
        let v0 = b.version();
        assert_eq!(v0.get(), 0);
        let _ = b.read(Offset::ZERO, &mut [0u8; 4]).unwrap();
        assert_eq!(b.version(), v0, "读操作不推进版本");
        let v1 = b.commit_edit(9);
        assert!(v1 > v0);
        b.resync_len(9);
        assert_eq!(b.version(), v1, "长度未变则不推进版本");
    }
}
