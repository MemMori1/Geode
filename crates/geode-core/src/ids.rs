//! 各类稳定 id。
//!
//! DESIGN: 00 §4.4、06 §3.1（`ProviderId`）、14 §3.1（插件拿到的也是这些 id）

use core::fmt;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident, $ty:ty) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($ty);

        impl $name {
            /// 从裸值构造。
            #[inline]
            pub const fn new(raw: $ty) -> Self {
                Self(raw)
            }

            /// 取出裸值。
            #[inline]
            pub const fn get(self) -> $ty {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

id_type!(
    /// 打开的数据源实例。进程内存/GDB 等 volatile 源同样占用一个 DocId。
    DocId, u64
);
id_type!(
    /// 视图（同一 DocId 可被多个视图共享，见 05 §3.1）。
    ViewId, u64
);
id_type!(
    /// 后台任务（可取消，见 00 §3.3 的 `Job`）。
    JobId, u64
);
id_type!(
    /// 一次编辑操作（进入 journal 与撤销栈，见 04 §4.5、17 §3.3）。
    EditId, u64
);
id_type!(
    /// Provider 实现的身份，用于日志与"同一文件"判定。
    ProviderId, u32
);

/// 单调递增的 id 分配器（每个会话一个，不跨进程持久化）。
#[derive(Debug, Default)]
pub struct IdGen {
    next: u64,
}

impl IdGen {
    /// 新建，从 1 开始。
    pub fn new() -> Self {
        Self { next: 1 }
    }

    /// 取下一个 id（不实现 `Iterator`：这个生成器不产出序列，只是发号）。
    pub fn next_id(&mut self) -> u64 {
        let v = self.next;
        self.next += 1;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idgen_starts_at_one_and_increments() {
        let mut g = IdGen::new();
        assert_eq!(g.next_id(), 1);
        assert_eq!(g.next_id(), 2);
    }

    #[test]
    fn ids_are_distinct_types() {
        let d = DocId::new(1);
        assert_eq!(d.to_string(), "DocId(1)");
    }
}
