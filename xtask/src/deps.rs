//! 依赖方向检查器。
//!
//! DESIGN: 18 §3.2（允许依赖矩阵）、18 §3.3（`cargo xtask deps` 四条规则）
//!
//! 四条规则：
//! - `E0101` GUI 污染：`egui`/`eframe`/`egui-wgpu`/`wgpu`/`winit` 只允许出现在
//!   `geode-ui` 与 `app`。
//! - `E0102` 方向错误：内部依赖的层号必须**严格下降**。
//! - `E0103` 白名单：同层依赖只有在矩阵里显式列出才允许。
//! - `E0104` C 依赖未设 feature：`capstone`/`yara-x` 必须在 `[features]` 之后。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 层号与允许的内部依赖（与 18 §3.2 的表逐行对应）。
const MATRIX: &[(&str, i8, &[&str])] = &[
    // 与 docs/18-engineering-plan.md §3.2 的表逐行对应；改表必须同时改这里。
    ("geode-core", -1, &[]),
    // 同层显式例外：buffer 的职责就是"从 Provider 读 + 加编辑语义"，方向恒为 buffer → provider（反之禁止）。
    (
        "geode-buffer",
        0,
        &["geode-core", "geode-provider", "geode-format"],
    ),
    ("geode-provider", 0, &["geode-core"]),
    ("geode-annot", 0, &["geode-core"]),
    ("geode-format", 0, &["geode-core"]),
    ("geode-analysis", 1, &["geode-core", "geode-provider"]),
    ("geode-bspec", 1, &["geode-core"]),
    (
        "geode-viz",
        1,
        &["geode-core", "geode-bspec", "geode-buffer", "geode-annot"],
    ),
    ("geode-proc", 1, &["geode-core", "geode-analysis"]),
    ("geode-search", 1, &["geode-core", "geode-provider"]),
    ("geode-diff", 1, &["geode-core", "geode-provider"]),
    ("geode-disasm", 1, &["geode-core", "geode-provider"]),
    ("geode-yara", 1, &["geode-core", "geode-provider"]),
    ("geode-migrate", 2, &["geode-core", "geode-bspec"]),
    (
        "geode-project",
        2,
        &[
            "geode-core",
            "geode-provider",
            "geode-buffer",
            "geode-format",
            "geode-bspec",
            "geode-proc",
        ],
    ),
    (
        "geode-ai",
        2,
        &["geode-core", "geode-bspec", "geode-project"],
    ),
    ("geode-content", 2, &["geode-core", "geode-bspec"]),
    (
        "geode-plugin-host",
        2,
        &["geode-core", "geode-annot", "geode-provider", "geode-viz"],
    ),
    (
        "geode-ui",
        3,
        &[
            "geode-core",
            "geode-buffer",
            "geode-provider",
            "geode-annot",
            "geode-format",
            "geode-analysis",
            "geode-bspec",
            "geode-viz",
            "geode-proc",
            "geode-search",
            "geode-diff",
            "geode-disasm",
            "geode-yara",
            "geode-migrate",
            "geode-project",
            "geode-ai",
            "geode-content",
            "geode-plugin-host",
        ],
    ),
    // app 是唯一"拿具体实现去组装"的层，因此允许直接依赖 core/provider/buffer/format。
    (
        "app",
        4,
        &[
            "geode-core",
            "geode-buffer",
            "geode-provider",
            "geode-annot",
            "geode-format",
            "geode-project",
            "geode-ui",
            "geode-plugin-host",
        ],
    ),
    ("xtask", 4, &[]),
];

/// GUI 相关 crate：只允许 `geode-ui` 与 `app` 使用。
const GUI_DEPS: &[&str] = &[
    "egui",
    "eframe",
    "egui-wgpu",
    "egui-winit",
    "wgpu",
    "winit",
    "egui_extras",
];

/// 需要 C 编译器/原生库的 crate：必须放在可选 feature 之后。
const C_DEPS: &[&str] = &["capstone", "yara-x", "yara_sys", "libloading"];

fn layer_of(name: &str) -> Option<(i8, &'static [&'static str])> {
    MATRIX
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, l, d)| (*l, *d))
}

fn repo_root() -> PathBuf {
    // xtask 从仓库根调用；也允许从 xtask/ 里调用。
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd.join("Cargo.toml").exists() && cwd.join("crates").exists() {
        cwd
    } else if cwd.join("..").join("crates").exists() {
        cwd.join("..")
    } else {
        cwd
    }
}

struct Finding {
    code: &'static str,
    crate_name: String,
    detail: String,
}

pub fn run() -> ExitCode {
    let root = repo_root();
    let mut paths: Vec<(String, PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let manifest = entry.path().join("Cargo.toml");
        if manifest.exists() {
            let name = entry.file_name().to_string_lossy().to_string();
            paths.push((name, manifest));
        }
    }
    for name in ["app", "xtask"] {
        let manifest = root.join(name).join("Cargo.toml");
        if manifest.exists() {
            paths.push((name.to_string(), manifest));
        }
    }
    paths.sort();

    let mut findings = Vec::new();
    let mut checked = 0usize;

    for (dir_name, manifest) in &paths {
        let text = match std::fs::read_to_string(manifest) {
            Ok(t) => t,
            Err(e) => {
                findings.push(Finding {
                    code: "E0102",
                    crate_name: dir_name.clone(),
                    detail: format!("无法读取 {manifest:?}: {e}"),
                });
                continue;
            }
        };
        let doc: toml::Value = match toml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                findings.push(Finding {
                    code: "E0102",
                    crate_name: dir_name.clone(),
                    detail: format!("Cargo.toml 解析失败: {e}"),
                });
                continue;
            }
        };

        let crate_name = doc
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or(dir_name)
            .to_string();
        checked += 1;

        let Some((own_layer, allowed)) = layer_of(&crate_name) else {
            findings.push(Finding {
                code: "E0103",
                crate_name: crate_name.clone(),
                detail: "未在 18 §3.2 的依赖矩阵中登记——新增 crate 必须先更新矩阵与本文档"
                    .to_string(),
            });
            continue;
        };
        let may_use_gui = matches!(crate_name.as_str(), "geode-ui" | "app");
        let features = doc.get("features").and_then(|f| f.as_table());
        let optional_deps = optional_dep_names(&doc);

        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(table) = doc.get(section).and_then(|t| t.as_table()) else {
                continue;
            };
            for dep in table.keys() {
                // E0101：GUI 污染
                if GUI_DEPS.contains(&dep.as_str()) && !may_use_gui {
                    findings.push(Finding {
                        code: "E0101",
                        crate_name: crate_name.clone(),
                        detail: format!(
                            "{section} 引入了 GUI 依赖 `{dep}`（只允许 geode-ui 与 app）"
                        ),
                    });
                }

                // E0104：C 依赖必须在 feature 之后
                if C_DEPS.contains(&dep.as_str()) && !optional_deps.contains(dep) {
                    findings.push(Finding {
                        code: "E0104",
                        crate_name: crate_name.clone(),
                        detail: format!("C 依赖 `{dep}` 必须是 optional 并挂在 feature 后"),
                    });
                }

                // E0102 / E0103：内部依赖方向与白名单
                if dep.starts_with("geode-") {
                    match layer_of(dep) {
                        None => findings.push(Finding {
                            code: "E0103",
                            crate_name: crate_name.clone(),
                            detail: format!("依赖 `{dep}` 未登记在矩阵中"),
                        }),
                        Some((dep_layer, _)) => {
                            let same_layer = dep_layer == own_layer;
                            if dep_layer >= own_layer {
                                if !(same_layer && allowed.contains(&dep.as_str())) {
                                    findings.push(Finding {
                                        code: "E0102",
                                        crate_name: crate_name.clone(),
                                        detail: format!(
                                            "层 {own_layer} 依赖了层 {dep_layer} 的 `{dep}`（层号必须严格下降，同层须在矩阵白名单）"
                                        ),
                                    });
                                }
                            } else if !allowed.contains(&dep.as_str()) {
                                findings.push(Finding {
                                    code: "E0103",
                                    crate_name: crate_name.clone(),
                                    detail: format!("`{dep}` 不在允许列表 {:?} 中", allowed),
                                });
                            }
                        }
                    }
                }
            }
        }

        // E0104 补充：feature 里出现 C 依赖却没有对应 optional dep
        if let Some(f) = features {
            for (feat, vals) in f {
                let vals: Vec<String> = vals
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                for v in &vals {
                    let name = v.strip_prefix("dep:").unwrap_or(v);
                    if C_DEPS.contains(&name) && !optional_deps.iter().any(|d| d == name) {
                        findings.push(Finding {
                            code: "E0104",
                            crate_name: crate_name.clone(),
                            detail: format!(
                                "feature `{feat}` 引用了非 optional 的 C 依赖 `{name}`"
                            ),
                        });
                    }
                }
            }
        }
    }

    if findings.is_empty() {
        println!("deps: {checked} 个 crate 检查通过（E0101~E0104 全绿）");
        println!("  矩阵来源: docs/18-engineering-plan.md §3.2；新增 crate 请先更新矩阵再写代码");
        return ExitCode::SUCCESS;
    }

    for f in &findings {
        eprintln!("{} [{}] {}", f.code, f.crate_name, f.detail);
    }
    eprintln!("\ndeps: {} 处违规", findings.len());
    ExitCode::FAILURE
}

fn optional_dep_names(doc: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(table) = doc.get("dependencies").and_then(|t| t.as_table()) {
        for (name, spec) in table {
            let is_opt = spec
                .get("optional")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_opt {
                out.push(name.clone());
            }
        }
    }
    out
}

#[allow(dead_code)]
fn exists(p: &Path) -> bool {
    p.exists()
}
