//! `cargo xtask docs-errors`——诊断码登记表的**双向校验**与**常量表生成**。
//!
//! DESIGN: 00 §12（错误码分配登记表）、18 §10.2 P1
//!
//! 为什么需要它：126 个码散在 26 份文档里，人肉维护必然漂移——文档写了代码没有、
//! 代码产了文档没登记，两种都会在用户面前变成"未知错误码"。所以：
//!
//! 1. `docs/errors.toml` 是唯一事实来源；
//! 2. 本任务扫描 `docs/**/*.md` 出现的所有码，与本表**双向**比对，任一侧多出即失败；
//! 3. 由本表生成 `crates/geode-core/src/error_codes.rs`，CI 用 `--check` 禁止手改。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;

/// 登记表顶层结构。
#[derive(Deserialize)]
struct Registry {
    #[serde(default)]
    code: Vec<Entry>,
}

/// 一条登记项。
#[derive(Deserialize)]
struct Entry {
    id: String,
    segment: String,
    owner: String,
    severity: String,
    #[serde(default)]
    title: String,
}

/// 合法段位（00 §12）；新增段位必须先改文档再改这里。
const SEGMENTS: &[&str] = &[
    "E0", "E1", "E2", "E3", "E4", "E5", "E6", "E7", "E8", "E9", "EA", "EB", "EC",
];

/// 合法严重级别。
const SEVERITIES: &[&str] = &["info", "warning", "error", "fatal"];

/// 入口：`cargo xtask docs-errors [--check]`。
pub fn run(check_only: bool) -> ExitCode {
    let root = repo_root();
    let registry_path = root.join("docs/errors.toml");
    let out_path = root.join("crates/geode-core/src/error_codes.rs");

    let text = match std::fs::read_to_string(&registry_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读不到 {}：{e}", registry_path.display());
            return ExitCode::FAILURE;
        }
    };
    let registry: Registry = match toml::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} 解析失败：{e}", registry_path.display());
            return ExitCode::FAILURE;
        }
    };

    let mut problems: Vec<String> = Vec::new();
    let mut by_id: BTreeMap<&str, &Entry> = BTreeMap::new();
    for e in &registry.code {
        if by_id.insert(e.id.as_str(), e).is_some() {
            problems.push(format!("重复登记：{}", e.id));
        }
        if !SEGMENTS.contains(&e.segment.as_str()) {
            problems.push(format!("{}：未知段位 `{}`", e.id, e.segment));
        }
        if !e.id.starts_with(&e.segment) {
            problems.push(format!("{}：段位 `{}` 与码前缀不符", e.id, e.segment));
        }
        if !SEVERITIES.contains(&e.severity.as_str()) {
            problems.push(format!("{}：未知 severity `{}`", e.id, e.severity));
        }
        if e.owner.trim().is_empty() {
            problems.push(format!("{}：owner 为空", e.id));
        }
    }

    let documented = scan_docs(&root.join("docs"));
    let registered: BTreeSet<String> = by_id.keys().map(|k| (*k).to_string()).collect();
    for id in registered.difference(&documented) {
        problems.push(format!(
            "{id}：已登记但 docs/ 里没有任何文档提到（文档缺失）"
        ));
    }
    for id in documented.difference(&registered) {
        problems.push(format!(
            "{id}：docs/ 里出现但未登记（先补 docs/errors.toml）"
        ));
    }

    let untitled = registry
        .code
        .iter()
        .filter(|e| e.title.trim().is_empty())
        .count();
    println!(
        "登记 {} 个码 / {} 个段位 / 有释义 {} / 缺释义 {}",
        registry.code.len(),
        SEGMENTS.len(),
        registry.code.len() - untitled,
        untitled
    );

    if !problems.is_empty() {
        problems.sort();
        for p in &problems {
            eprintln!("error: {p}");
        }
        eprintln!("\n共 {} 处不一致。", problems.len());
        return ExitCode::FAILURE;
    }

    let generated = render(&registry.code);
    let current = std::fs::read_to_string(&out_path).unwrap_or_default();
    if current == generated {
        println!("{} 已是最新。", rel(&root, &out_path));
        return ExitCode::SUCCESS;
    }
    if check_only {
        eprintln!(
            "error: {} 与 docs/errors.toml 不同步；运行 `cargo xtask docs-errors` 重新生成。",
            rel(&root, &out_path)
        );
        return ExitCode::FAILURE;
    }
    if let Some(dir) = out_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(&out_path, generated) {
        Ok(()) => {
            println!("已生成 {}。", rel(&root, &out_path));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("写 {} 失败：{e}", out_path.display());
            ExitCode::FAILURE
        }
    }
}

fn repo_root() -> PathBuf {
    // xtask 位于 <root>/xtask，编译期即已知。
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 必须在仓库根的下一层")
        .to_path_buf()
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

/// 扫描 `docs/` 下所有 Markdown（含 `adr/`、`img/` 的说明页），收集诊断码。
fn scan_docs(docs: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![docs.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    collect_codes(&text, &mut out);
                }
            }
        }
    }
    out
}

/// 从任意文本里按"词边界 + 形态"提取码：`E` + 可选 `A/B/C` + 3~4 位数字。
///
/// 不用正则库：形态足够窄，手写扫描比引依赖更可控，也避免把 `E2E`、`E10` 这类
/// 普通词误判成码。
fn collect_codes(text: &str, out: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'E' {
            i += 1;
            continue;
        }
        if i > 0 && is_word(bytes[i - 1]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if j < bytes.len() && matches!(bytes[j], b'A' | b'B' | b'C') {
            j += 1;
        }
        let digits_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        let digits = j - digits_start;
        let len = j - i;
        let boundary_ok = j >= bytes.len() || !is_word(bytes[j]);
        if boundary_ok && (3..=4).contains(&digits) && (4..=6).contains(&len) {
            let id = &text[i..j];
            // 段位必须落在登记过的 12 个段里（E0~E9 / EA / EB / EC）。
            if SEGMENTS.contains(&&id[..2]) {
                out.insert(id.to_string());
            }
        }
        i = j.max(i + 1);
    }
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 生成 `error_codes.rs`。模板改动等于改 API，需同步 ADR/文档。
fn render(entries: &[Entry]) -> String {
    let mut s = String::with_capacity(entries.len() * 90 + 1024);
    s.push_str(
        "//! 诊断码常量表——**由 `cargo xtask docs-errors` 从 `docs/errors.toml` 生成，不要手改**。\n\
         //!\n\
         //! DESIGN: 00 §12（错误码分配登记表）、18 §10.2 P1\n\
         //!\n\
         //! 用法：`Diag::error(error_codes::E1002, \"…\")`。码本身不上屏，句子由 UI 查文案表。\n\n\
         use crate::diag::Code;\n\n",
    );
    for e in entries {
        if e.title.trim().is_empty() {
            let _ = writeln!(s, "/// `{}`（释义待补，见 18 §10.4 的 27 号文档）", e.id);
        } else {
            let title = e.title.replace(['\n', '\r'], " ");
            let _ = writeln!(s, "/// `{}`：{}", e.id, title.trim());
        }
        let _ = writeln!(s, "pub const {}: Code = Code(\"{}\");\n", e.id, e.id);
    }
    s.push_str(
        "/// 全部已登记诊断码（按码升序，与 `docs/errors.toml` 一致）。\n\
         #[rustfmt::skip]\npub const ALL: &[Code] = &[\n",
    );
    for e in entries {
        let _ = writeln!(s, "    {},", e.id);
    }
    s.push_str("];\n\n");
    s.push_str(
        "#[cfg(test)]\nmod tests {\n    use super::*;\n\n    const SEGMENTS: &[&str] = &[\n        \"E0\", \"E1\", \"E2\", \"E3\", \"E4\", \"E5\", \"E6\", \"E7\", \"E8\", \"E9\", \"EA\", \"EB\", \"EC\",\n    ];\n\n    #[test]\n    fn all_is_sorted_and_unique() {\n        let ids: Vec<&str> = ALL.iter().map(|c| c.0).collect();\n        let mut sorted = ids.clone();\n        sorted.sort_unstable();\n        sorted.dedup();\n        assert_eq!(ids, sorted, \"ALL 必须按码升序且无重复\");\n    }\n\n    #[test]\n    fn segments_are_registered() {\n        for c in ALL {\n            assert!(SEGMENTS.contains(&c.segment()), \"{} 的段位未登记\", c);\n        }\n    }\n}\n",
    );
    s
}
