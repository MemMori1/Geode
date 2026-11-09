//! `cargo xtask vectors-check`：守 `tests/vectors/` 的**结构**约定。
//!
//! DESIGN: 23 §3.2（schema 工件与测试向量）、18 §10.2 P3
//!
//! 它检查的是"样本账本"而不是样本本身：每个格式都必须有 manifest，且至少 3 个
//! **故意损坏**的样本写明期望错误码，错误码还必须是 `docs/errors.toml` 里登记过的。
//! 真正的字节级解析属于 M1 的 `geode-format`，届时再加 `--run` 真跑一遍。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 一个格式的向量清单。
#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    vector: Vec<Vector>,
}

/// 单条向量。
#[derive(Deserialize)]
struct Vector {
    path: String,
    expect: String,
    #[serde(default)]
    note: String,
}

/// 跑一遍结构检查。
pub fn run(root: &Path) -> Result<String, String> {
    let mut formats: BTreeSet<String> = BTreeSet::new();
    let schemas = root.join("schemas");
    for e in fs::read_dir(&schemas).map_err(|e| format!("{}: {e}", schemas.display()))? {
        let p = e.map_err(|e| e.to_string())?.path();
        if p.is_dir() {
            if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
                formats.insert(n.to_owned());
            }
        }
    }
    if formats.is_empty() {
        return Err("schemas/ 下没有格式目录".into());
    }

    let known = registered_codes(&root.join("docs/errors.toml"))?;
    let mut problems = Vec::new();
    let mut total = 0usize;

    for fmt in &formats {
        let dir = root.join("tests/vectors").join(fmt).join("v1");
        let manifest_path = dir.join("manifest.toml");
        let text = match fs::read_to_string(&manifest_path) {
            Ok(t) => t,
            Err(_) => {
                problems.push(format!("{fmt}: 缺 {}", manifest_path.display()));
                continue;
            }
        };
        let manifest: Manifest = match toml::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                problems.push(format!("{fmt}: manifest 解析失败: {e}"));
                continue;
            }
        };

        let mut ok = 0usize;
        let mut bad = 0usize;
        for v in &manifest.vector {
            total += 1;
            if !dir.join(&v.path).is_file() {
                problems.push(format!("{fmt}: 向量文件不存在 {}", v.path));
            }
            if v.note.trim().is_empty() {
                problems.push(format!("{fmt}: {} 没写 note（说明它坏在哪）", v.path));
            }
            if v.expect == "ok" {
                ok += 1;
            } else {
                bad += 1;
                if !known.contains(&v.expect) {
                    problems.push(format!(
                        "{fmt}: {} 的期望码 {} 未在 docs/errors.toml 登记",
                        v.path, v.expect
                    ));
                }
            }
        }
        if ok == 0 {
            problems.push(format!("{fmt}: 没有一条 expect=\"ok\" 的正样本"));
        }
        if bad < 3 {
            problems.push(format!(
                "{fmt}: 损坏样本只有 {bad} 条，23 §3.2/M1 验收要求 ≥3"
            ));
        }
    }

    if problems.is_empty() {
        Ok(format!(
            "向量检查通过：{} 个格式、{total} 条样本",
            formats.len()
        ))
    } else {
        Err(format!(
            "向量检查失败（{} 处）：\n  {}",
            problems.len(),
            problems.join("\n  ")
        ))
    }
}

/// 从 `docs/errors.toml` 取全部已登记的码。
fn registered_codes(path: &Path) -> Result<BTreeSet<String>, String> {
    #[derive(Deserialize)]
    struct Registry {
        #[serde(default)]
        code: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reg: Registry = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(reg.code.into_iter().map(|e| e.id).collect())
}

/// 便利函数：拼出某格式的向量目录（供未来 `--run` 用）。
#[allow(dead_code)]
pub fn vector_dir(root: &Path, format: &str) -> PathBuf {
    root.join("tests/vectors").join(format).join("v1")
}
