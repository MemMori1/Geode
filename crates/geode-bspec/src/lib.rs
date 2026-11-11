//! BinSpec：Geode 的结构描述语言。
//!
//! DESIGN: 02-dsl-spec-draft.md（语言规格）、03-ai-dsl-generation.md（Verifier）、
//! 22-bspec-toolchain.md（fmt/lint/LSP 复用同一套 lexer/parser）。
//!
//! 分层：`lexer` → `parser`（产出 `ast`）→ `bytecode`（编译）→ `vm`（执行）→ `node`
//! （结构树），`verify` 在 `node` 上跑不变量。**解析与执行都不碰 GUI**。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod ast;
pub mod bytecode;
pub mod compile;
pub mod diag;
pub mod lexer;
pub mod node;
pub mod parser;
pub mod span;
pub mod value;
pub mod verify;
pub mod vm;

pub use compile::Compiler;
