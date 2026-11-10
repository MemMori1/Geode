//! Geode 的入口：解析命令行、打开数据源、交给 UI。
//!
//! DESIGN: 18 §3.1（`app/src/{main,cli,app,wiring}`）、25 §4（启动画面）
//!
//! M0 阶段：还没有 GUI，先把"打开 4 GiB 文件不整读"这条硬指标跑通。

#![deny(missing_docs)]

use geode_core::Offset;
use geode_provider::page_cache::CachedProvider;
use geode_provider::{FileProvider, Provider};
mod window;

use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("geode {VERSION}");
            ExitCode::SUCCESS
        }
        Some("--inspect") => match args.get(1) {
            Some(path) => match inspect(path) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            },
            None => {
                eprintln!("`--inspect` 需要一个文件路径");
                ExitCode::FAILURE
            }
        },
        Some(flag) if flag.starts_with('-') => {
            eprintln!("未知参数 `{flag}`（`geode --help` 查看用法）");
            ExitCode::FAILURE
        }
        Some(path) => match window::run(std::path::Path::new(path)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
    }
}

fn print_help() {
    println!(
        "geode {VERSION} — 二进制结构分析工具（M2 视图）\n\n\
         用法: geode <文件>            打开图形窗口\n     \
               geode --inspect <文件>  无窗口探测（打印大小/caps/头部 256B）\n\
         选项:\n  \
           -h, --help     显示本帮助\n  \
           -V, --version  显示版本\n\n\
         快捷键: 方向键移动游标，PageUp/PageDown 翻页，滚轮滚动，右侧滚动条可拖。"
    );
}

/// 打开一个数据源并打印基本信息 + 头部十六进制转储。
fn inspect(path: &str) -> Result<(), String> {
    let start = std::time::Instant::now();
    let provider = CachedProvider::new(FileProvider::open(path).map_err(|e| e.to_string())?);
    let open_ms = start.elapsed().as_secs_f64() * 1000.0;

    let size = provider.size().lower_bound();
    println!("{}", provider.display_name());
    println!(
        "大小    : {size} B ({:.2} GiB)",
        size as f64 / (1u64 << 30) as f64
    );
    println!("能力    : {}", provider.caps());
    println!("打开耗时: {open_ms:.3} ms（M0 验收①：4 GiB < 200 ms）");

    let want = 256u64.min(size);
    let (bytes, report) = provider
        .read_exact_vec(Offset::ZERO, want)
        .map_err(|e| e.to_string())?;
    println!(
        "首读    : {} B（空洞 {} B，{}）",
        report.filled,
        report.holes,
        if report.eof_hit {
            "到达末尾"
        } else {
            "未到末尾"
        }
    );
    print_hexdump(&bytes);
    Ok(())
}

fn print_hexdump(bytes: &[u8]) {
    for (row, chunk) in bytes.chunks(16).enumerate() {
        print!("{:08X}  ", row * 16);
        for (i, b) in chunk.iter().enumerate() {
            if i == 8 {
                print!(" ");
            }
            print!("{b:02X} ");
        }
        for i in chunk.len()..16 {
            if i == 8 {
                print!(" ");
            }
            print!("   ");
        }
        print!(" |");
        for b in chunk {
            print!(
                "{}",
                if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    '.'
                }
            );
        }
        println!("|");
    }
}
