//! `cargo xtask schema-check`：守 `23 §3.2` 的 **JSON Schema 2020-12 子集**约定。
//!
//! 为什么要自己写：JSON Schema 是全集的方言，今天谁都能往里塞一个 `$ref`，
//! 明天移动端/插件/第三方校验器就跑不动了。这个检查把"允许的词汇表"变成机器可判。
//!
//! 规则（23 §3.2）：
//! 1. 只许出现白名单关键词；
//! 2. 禁止跨文件 `$ref` 与 `format`；
//! 3. **每个 object 必须显式写 `additionalProperties: true`**（未知字段要保留，D3）；
//! 4. 每个属性必须有 `description`，且含 `§` 与节号（否则读者不知道去哪查）。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 允许的关键词（与 23 §3.2 一字不差）。
const ALLOWED: &[&str] = &[
    "type",
    "properties",
    "required",
    "enum",
    "pattern",
    "minimum",
    "maximum",
    "items",
    "additionalProperties",
    "const",
    "oneOf",
    "description",
    "minItems",
    "maxItems",
];

/// 跑一遍检查，返回人读的报告。
pub fn run(root: &Path) -> Result<String, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect(&root.join("schemas"), &mut files)?;
    if files.is_empty() {
        return Err("schemas/ 下没有找到任何 v*.json".into());
    }
    files.sort();

    let mut problems: Vec<String> = Vec::new();
    let mut count = 0usize;
    for f in &files {
        let text = fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("{}: JSON 解析失败: {e}", f.display()))?;
        check_node(&doc, f, "$", &mut problems);
        count += 1;
    }

    if problems.is_empty() {
        Ok(format!(
            "schema 检查通过：{count} 个文件，{} 条规则",
            ALLOWED.len()
        ))
    } else {
        Err(format!(
            "schema 检查失败（{} 处）：\n  {}",
            problems.len(),
            problems.join("\n  ")
        ))
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in rd {
        let p = entry.map_err(|e| e.to_string())?.path();
        if p.is_dir() {
            collect(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "json") {
            out.push(p);
        }
    }
    Ok(())
}

fn check_node(node: &serde_json::Value, file: &Path, path: &str, problems: &mut Vec<String>) {
    let serde_json::Value::Object(map) = node else {
        if let serde_json::Value::Array(items) = node {
            for (i, it) in items.iter().enumerate() {
                check_node(it, file, &format!("{path}[{i}]"), problems);
            }
        }
        return;
    };

    for k in map.keys() {
        if !ALLOWED.contains(&k.as_str()) {
            problems.push(format!(
                "{} {path}: 关键词 `{k}` 不在子集内",
                file.display()
            ));
        }
    }
    if map.contains_key("$ref") || map.contains_key("format") {
        problems.push(format!("{} {path}: 禁止 $ref / format", file.display()));
    }

    if map.get("type").and_then(|t| t.as_str()) == Some("object") {
        match map.get("additionalProperties") {
            Some(serde_json::Value::Bool(true)) => {}
            Some(_) => problems.push(format!(
                "{} {path}: additionalProperties 必须是 true（未知字段保留）",
                file.display()
            )),
            None => problems.push(format!(
                "{} {path}: object 缺 additionalProperties",
                file.display()
            )),
        }
        if let Some(serde_json::Value::Object(props)) = map.get("properties") {
            if let Some(serde_json::Value::Array(req)) = map.get("required") {
                let names: BTreeSet<&str> = props.keys().map(|s| s.as_str()).collect();
                for r in req {
                    if let Some(r) = r.as_str() {
                        if !names.contains(r) {
                            problems.push(format!(
                                "{} {path}: required 里的 `{r}` 没有对应 properties",
                                file.display()
                            ));
                        }
                    }
                }
            }
            for (name, sub) in props {
                let p = format!("{path}.properties.{name}");
                let has_desc = sub
                    .get("description")
                    .and_then(|d| d.as_str())
                    .is_some_and(|d| d.contains('§') && d.chars().any(|c| c.is_ascii_digit()));
                if !has_desc {
                    problems.push(format!(
                        "{} {p}: 缺 description 或没写节号（§）",
                        file.display()
                    ));
                }
                check_node(sub, file, &p, problems);
            }
        }
        return;
    }

    if let Some(sub) = map.get("items") {
        check_node(sub, file, &format!("{path}.items"), problems);
    }
    if let Some(serde_json::Value::Array(v)) = map.get("oneOf") {
        for (i, it) in v.iter().enumerate() {
            check_node(it, file, &format!("{path}.oneOf[{i}]"), problems);
        }
    }
}
