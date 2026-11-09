//! `cargo xtask <task>`——项目自带的开发任务驱动。
//!
//! DESIGN: 18-engineering-plan.md §3.2 / §3.3
//!
//! 目前实现 `deps`：把 18 §3.2 的依赖矩阵变成**机器可判**的 CI 关卡。
//! 文档里的表靠人肉遵守，两年后必然腐化；这里让它每次 PR 都被检查一次。

#![deny(missing_docs)]

mod deps;
mod docserrors;

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("deps") => deps::run(),
        Some("docs-errors") => docserrors::run(std::env::args().any(|a| a == "--check")),
        Some("-h" | "--help" | "help") | None => {
            println!(
                "geode xtask\n\n\
                 用法: cargo xtask <task>\n\n\
                 任务:\n  \
                   deps          校验 crate 依赖方向与 GUI 污染（18 §3.2，码 E0101~E0104）\n  \
                   docs-errors   校验 docs/errors.toml 与文档的双向一致，并生成 error_codes.rs\n                        --check 只校验不写盘（CI 用）\n\n\
                 环境: 需在仓库根目录运行"
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("未知任务 `{other}`，可用: deps、docs-errors");
            ExitCode::FAILURE
        }
    }
}
