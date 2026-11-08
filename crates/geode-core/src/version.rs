//! 缓冲区版本号。
//!
//! DESIGN: 00 §6（线程模型与异步结果的正确性）、04 §3.1
//!
//! 后台任务（搜索、哈希、BinSpec 解析、diff）拿到结果后**必须**比对
//! `buffer.version()`：不一致就丢弃。这是唯一能防止"旧结果覆盖新视图"的机制，
//! 因此版本号只增不减，且只在实际提交修改时前进。

use core::fmt;

/// 单调递增的版本号。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(u64);

impl Version {
    /// 初始版本。
    pub const INITIAL: Self = Self(0);

    /// 从裸值构造（用于反序列化与测试）。
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// 裸值。
    pub const fn get(self) -> u64 {
        self.0
    }

    /// 下一个版本。
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increments_monotonically() {
        let a = Version::INITIAL;
        let b = a.next();
        assert!(b > a);
        assert_eq!(b.get(), 1);
        assert_eq!(b.to_string(), "v1");
    }
}
