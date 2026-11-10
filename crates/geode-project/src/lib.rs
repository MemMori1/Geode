//! Geode 项目层（L2）：把编辑模型与磁盘上的项目文件连接起来。
//!
//! DESIGN: 18-engineering-plan.md §3.2（L2 依赖方向）、04-buffer-and-edit-model.md
//! §4.5（PatchSet）、23-file-formats.md §3.6（.geopatch 记录语义）。
//!
//! 本 crate 目前只实现 T1.6 的补丁桥：编辑器内部的 [`geode_buffer::edit::Patch`]
//! 与 `.geopatch` 的 [`geode_format::patch::File`] 之间的双向转换。桥接层刻意
//! 不依赖 GUI，也不直接做文件 I/O；编解码仍由 `geode-format` 负责。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod error;
pub mod patch_bridge;

pub use error::ProjectError;
pub use patch_bridge::{Edit, EditBody};
