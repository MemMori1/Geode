//! Geode 搜索子系统（M2 T2.6）。
//!
//! 提供四种共享流式扫描框架的查询引擎：
//!
//! - [`HexPattern`]：带 `??` / `?F` 半字节通配的 hex 模式；
//! - [`TextQuery`]：UTF-8 / UTF-16LE / UTF-16BE / ASCII / Latin-1 文本；
//! - [`RegexQuery`]：`regex::bytes` 线性时间正则；
//! - [`NumericQuery`]：u8..u64、i8..i64、f32/f64 的闭区间范围。
//!
//! 统一入口是 [`search`]，按块读取 [`geode_provider::Provider`]，支持跨块
//! 重叠、取消与结果上限截断。设计依据见 `docs/08-search-subsystem.md`。
//!
//! # 示例
//!
//! ```
//! use geode_provider::MemoryProvider;
//! use geode_search::{search, HexPattern, SearchOptions, SearchQuery};
//!
//! let provider = MemoryProvider::new("demo", b"xxABxx".to_vec());
//! let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
//! let outcome = search(&provider, &query, &SearchOptions::default(), &|| false)
//!     .expect("搜索成功");
//! assert_eq!(outcome.hits.len(), 1);
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod error;
mod numeric;
mod pattern;
mod regex_query;
mod search;
mod text;

pub use error::{SearchError, Warning};
pub use numeric::{Endian, NumericKind, NumericQuery, NumericValue};
pub use pattern::{HexPattern, MaskByte};
pub use regex_query::RegexQuery;
pub use search::{SearchOptions, SearchOutcome, SearchQuery, search};
pub use text::{TextEncoding, TextQuery};
