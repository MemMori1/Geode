//! 协作式取消信号。
//!
//! DESIGN: 00 §6（后台任务必须在 100ms 内响应取消）、10 §3 的 `ScanCtx.cancel`。
//!
//! `geode-core` 目前还没有提供跨 crate 的 `CancelToken`，而本 crate 不允许修改其它
//! crate；这里给出一个最小实现，后续 core 落地同名类型时可直接替换。它只做一件事：
//! 让扫描循环在每个 1 MiB 块边界用一次 relaxed load 检查是否该停下。

use core::sync::atomic::{AtomicBool, Ordering};

/// 取消令牌：可由任意线程置位，扫描线程只读。
#[derive(Debug, Default)]
pub struct CancelToken {
    flag: AtomicBool,
}

impl CancelToken {
    /// 新建一个未取消的令牌。
    pub const fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
        }
    }

    /// 请求取消。重复调用无副作用。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// 是否已请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_can_be_cancelled_once_or_many_times() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }
}
