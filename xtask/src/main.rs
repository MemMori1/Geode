//! `cargo xtask <task>`——项目自带的开发任务驱动。
//!
//! DESIGN: 18-engineering-plan.md §3.2 / §3.3
//!
//! 目前实现 `deps`：把 18 §3.2 的依赖矩阵变成**机器可判**的 CI 关卡。
//! 文档里的表靠人肉遵守，两年后必然腐化；这里让它每次 PR 都被检查一次。

#![deny(missing_docs)]

mod deps;
mod docserrors;
mod schemas;
mod vectors;

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("deps") => deps::run(),
        Some("docs-errors") => docserrors::run(std::env::args().any(|a| a == "--check")),
        Some("vectors-check") => match vectors::run(&schemas_root()) {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        },
        Some("schema-check") => match schemas::run(&schemas_root()) {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        },
        Some("-h" | "--help" | "help") | None => {
            println!(
                "geode xtask\n\n\
                 用法: cargo xtask <task>\n\n\
                 任务:\n  \
                   deps          校验 crate 依赖方向与 GUI 污染（18 §3.2，码 E0101~E0104）\n  \
                   docs-errors   校验 docs/errors.toml 与文档的双向一致，并生成 error_codes.rs\n                        --check 只校验不写盘（CI 用）\n  \
                   schema-check  校验 schemas/*/v*.json 是否守住 23 §3.2 的 JSON Schema 子集\n  \
                   vectors-check 校验 tests/vectors 每个格式有 manifest 且 ≥3 条损坏样本\n\n\
                 环境: 需在仓库根目录运行"
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("未知任务 `{other}`，可用: deps、docs-errors、schema-check、vectors-check");
            ExitCode::FAILURE
        }
    }
}

/// `schema-check` 的根目录：从当前目录向上找 workspace。
fn schemas_root() -> std::path::PathBuf {
    let mut dir = std::env::current_dir().expect("当前目录");
    loop {
        if let Ok(t) = std::fs::read_to_string(dir.join("Cargo.toml")) {
            if t.contains("[workspace]") {
                return dir;
            }
        }
        if !dir.pop() {
            panic!("找不到 workspace 根");
        }
    }
}
