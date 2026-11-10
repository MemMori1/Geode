//! 向量驱动测试：逐条解码 `tests/vectors/{geopatch,geojournal}/v1/`，断言错误码，
//! 并对 `expect = "ok"` 的样本断言 `encode(decode(bytes)) == bytes`。
//!
//! DESIGN: 23 §3.6 / §3.8

use std::fs;
use std::path::Path;

/// manifest 中的一条向量。
struct Vector {
    name: String,
    expect: String,
    note: String,
    bytes: Vec<u8>,
}

/// 读取并解析某个格式的 `manifest.toml`。
fn load_vectors(format: &str) -> Vec<Vector> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/vectors")
        .join(format)
        .join("v1");
    let manifest_path = dir.join("manifest.toml");
    let text = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("读不到 {}: {error}", manifest_path.display()));
    let manifest: toml::Value = toml::from_str(&text)
        .unwrap_or_else(|error| panic!("解析 {} 失败: {error}", manifest_path.display()));
    let entries = manifest
        .get("vector")
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("{} 缺少 [[vector]]", manifest_path.display()));

    entries
        .iter()
        .map(|entry| {
            let name = entry
                .get("path")
                .and_then(toml::Value::as_str)
                .expect("vector.path 必须是字符串")
                .to_owned();
            let expect = entry
                .get("expect")
                .and_then(toml::Value::as_str)
                .expect("vector.expect 必须是字符串")
                .to_owned();
            let note = entry
                .get("note")
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_owned();
            let bytes = fs::read(dir.join(&name))
                .unwrap_or_else(|error| panic!("读不到 {}: {error}", dir.join(&name).display()));
            Vector {
                name,
                expect,
                note,
                bytes,
            }
        })
        .collect()
}

/// 跑一组向量，返回失败描述列表。
fn run<F: std::fmt::Debug>(
    format: &str,
    decode: fn(&[u8]) -> Result<F, geode_format::FormatError>,
    encode: fn(&F) -> Vec<u8>,
) -> Vec<String> {
    let vectors = load_vectors(format);
    let mut failures = Vec::new();
    println!("[{format}] {} 条向量", vectors.len());
    for vector in &vectors {
        match decode(&vector.bytes) {
            Ok(file) => {
                if vector.expect == "ok" {
                    let reencoded = encode(&file);
                    if reencoded == vector.bytes {
                        println!(
                            "[{format}] {} expect=ok => ok；encode 字节完全一致（{} B）",
                            vector.name,
                            vector.bytes.len()
                        );
                    } else {
                        let detail = format!(
                            "[{format}] {} expect=ok => 解码成功但 encode 不一致（原 {} B，重编码 {} B）",
                            vector.name,
                            vector.bytes.len(),
                            reencoded.len()
                        );
                        println!("{detail}");
                        failures.push(detail);
                    }
                } else {
                    let detail = format!(
                        "[{format}] {} expect={} => 解码意外成功（note: {}）",
                        vector.name, vector.expect, vector.note
                    );
                    println!("{detail}");
                    failures.push(detail);
                }
            }
            Err(error) => {
                if error.code == vector.expect {
                    println!(
                        "[{format}] {} expect={} => {}；detail: {}",
                        vector.name, vector.expect, error.code, error.detail
                    );
                } else {
                    let detail = format!(
                        "[{format}] {} expect={} => 实际 {}（detail: {}；note: {}）",
                        vector.name, vector.expect, error.code, error.detail, vector.note
                    );
                    println!("{detail}");
                    failures.push(detail);
                }
            }
        }
    }
    failures
}

#[test]
fn geopatch_vectors() {
    let failures = run(
        "geopatch",
        geode_format::patch::decode as fn(&[u8]) -> Result<geode_format::patch::File, _>,
        geode_format::patch::encode as fn(&geode_format::patch::File) -> Vec<u8>,
    );
    assert!(
        failures.is_empty(),
        "geopatch 向量失败 {} 条：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn geojournal_vectors() {
    let failures = run(
        "geojournal",
        geode_format::journal::decode as fn(&[u8]) -> Result<geode_format::journal::File, _>,
        geode_format::journal::encode as fn(&geode_format::journal::File) -> Vec<u8>,
    );
    assert!(
        failures.is_empty(),
        "geojournal 向量失败 {} 条：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 显式记录当前两个目录的实际向量数，防止 manifest 被漏读。
#[test]
fn vector_counts_match_manifest() {
    for format in ["geopatch", "geojournal"] {
        let count = load_vectors(format).len();
        println!("[{format}] manifest 向量数 = {count}");
        assert!(count > 0, "{format} 没有向量");
    }
}
