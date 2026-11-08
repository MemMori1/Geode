//! 内存数据源——测试与"粘贴为数据源"用。
//!
//! DESIGN: 06 §3.1（最小实现，用于把 Provider 契约跑通）

use crate::{Caps, Provider, ProviderError, ReadReport, SizeInfo};
use geode_core::{ByteRange, Offset, ProviderId};

/// 一段常驻内存的字节。
#[derive(Debug)]
pub struct MemoryProvider {
    id: ProviderId,
    name: String,
    bytes: Vec<u8>,
    volatile: bool,
}

impl MemoryProvider {
    /// 从字节构造。
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let name = name.into();
        let id = ProviderId::new(0x4D45_4D00 | (name.len() as u32 & 0xFF));
        Self {
            id,
            name,
            bytes,
            volatile: false,
        }
    }

    /// 标记为易变（进程内存语义：不缓存）。
    #[must_use]
    pub fn volatile(mut self) -> Self {
        self.volatile = true;
        self
    }

    /// 底层切片。
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl Provider for MemoryProvider {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn display_name(&self) -> String {
        format!("内存: {}", self.name)
    }

    fn caps(&self) -> Caps {
        if self.volatile {
            Caps::VOLATILE_READ_ONLY
        } else {
            Caps::READ_ONLY_STABLE
        }
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.bytes.len() as u64)
    }

    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        let start = usize::try_from(at.get()).unwrap_or(usize::MAX);
        if start >= self.bytes.len() {
            return Ok(ReadReport {
                filled: 0,
                holes: 0,
                eof_hit: true,
            });
        }
        let n = buf.len().min(self.bytes.len() - start);
        buf[..n].copy_from_slice(&self.bytes[start..start + n]);
        Ok(ReadReport {
            filled: n as u64,
            holes: 0,
            eof_hit: n < buf.len(),
        })
    }

    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        Some(vec![ByteRange::from_len(
            Offset::ZERO,
            self.bytes.len() as u64,
        )?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_short_reads() {
        let p = MemoryProvider::new("t", (0..32u8).collect());
        let mut buf = [0u8; 8];
        assert_eq!(p.read_range(Offset::new(28), &mut buf).unwrap().filled, 4);
        assert_eq!(&buf[..4], &[28, 29, 30, 31]);
    }

    #[test]
    fn volatile_caps() {
        assert!(MemoryProvider::new("t", vec![]).volatile().caps().volatile);
    }
}
