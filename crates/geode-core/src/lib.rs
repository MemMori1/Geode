//! Geode 的**叶子 crate**：所有 crate 共享的契约类型，除此之外不依赖任何东西。
//!
//! DESIGN: 00-architecture-overview.md §4（核心类型）、04 §3、05 §4.2
//!
//! 这一层存在的唯一理由是"让上层不必互相认识"。因此：
//!
//! - **没有 I/O、没有 GUI、没有全局状态**，只有纯粹的值语义；
//! - 偏移一律用 [`Offset`]（u64）而不是 `usize`——4GB 以上的文件在 32 位环境会溢出，
//!   而"把行内索引转 usize"是唯一允许的转换点（见 05 §4.2）；
//! - 区间一律是**半开** `[start, end)`，空区间合法，非法区间在构造时就拒绝。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod budget;
pub mod diag;
pub mod error_codes;
pub mod ids;
pub mod inspector;
pub mod offset;
pub mod range;
pub mod value;
pub mod version;

pub use budget::Budget;
pub use diag::{Diag, Severity};
pub use ids::{DocId, EditId, JobId, ProviderId, ViewId};
pub use offset::Offset;
pub use range::ByteRange;
pub use value::Value;
pub use version::Version;
