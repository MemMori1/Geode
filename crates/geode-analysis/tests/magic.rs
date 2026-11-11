//! `.gmagic` 解析与头部匹配的集成测试。
//!
//! 向量目录：`tests/vectors/gmagic/v1`（23 §3.7、23 §9.1）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use geode_analysis::{Endian, MagicPattern, MagicSet};
use geode_core::error_codes;

/// 返回 `.gmagic` 向量目录。
fn vector_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/gmagic/v1")
}

/// 读取并解析一个向量文件。
fn parse_vector(name: &str) -> Result<MagicSet, Vec<geode_core::Diag>> {
    let path = vector_dir().join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("读取 {} 失败：{err}", path.display()));
    MagicSet::parse(&text)
}

/// 断言诊断集合包含指定错误码。
fn assert_code(diags: &[geode_core::Diag], code: &str) {
    assert!(
        diags.iter().any(|diag| diag.code.0 == code),
        "期望错误码 {code}，实际为 {:?}",
        diags.iter().map(|diag| diag.code.0).collect::<Vec<_>>()
    );
}

#[test]
fn vectors_follow_manifest() {
    let manifest_path = vector_dir().join("manifest.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("读取 {} 失败：{err}", manifest_path.display()));
    let value: toml::Value = toml::from_str(&manifest).expect("manifest.toml 必须是合法 TOML");
    let vectors = value
        .get("vector")
        .and_then(toml::Value::as_array)
        .expect("manifest 必须有 [[vector]] 数组");
    assert!(!vectors.is_empty(), "manifest 不能为空");

    for vector in vectors {
        let path = vector
            .get("path")
            .and_then(toml::Value::as_str)
            .expect("每个向量必须有 path");
        let expect = vector
            .get("expect")
            .and_then(toml::Value::as_str)
            .expect("每个向量必须有 expect");
        let result = parse_vector(path);
        if expect == "ok" {
            assert!(
                result.is_ok(),
                "向量 {path} 应当解析成功，实际错误：{:?}",
                result
                    .err()
                    .map(|diags| diags.iter().map(|diag| diag.code.0).collect::<Vec<_>>())
            );
            assert!(
                !result.expect("已确认成功").is_empty(),
                "向量 {path} 解析成功但规则集为空"
            );
        } else {
            let diags = result
                .err()
                .unwrap_or_else(|| panic!("向量 {path} 应当失败于 {expect}，实际解析成功"));
            assert_code(&diags, expect);
        }
    }
}

#[test]
fn png_minimal_rule_matches_head() {
    let set = parse_vector("0001-minimal.toml").expect("0001 应解析成功");
    assert_eq!(set.len(), 1);
    assert_eq!(set.rules()[0].id, "png");
    assert_eq!(set.rules()[0].priority, 10);
    assert_eq!(set.rules()[0].template.as_deref(), Some("builtin/png"));

    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    let hits = set.match_head(png);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rule_id, "png");
    assert_eq!(hits[0].name, "PNG 图像");
    assert_eq!(hits[0].mime.as_deref(), Some("image/png"));
    assert!((hits[0].confidence - 0.95).abs() < f32::EPSILON);
    assert_eq!(hits[0].matched_len, 8);
    assert_eq!(set.best(png).map(|hit| hit.rule_id), Some("png".into()));

    let not_png = b"\x89PNG\r\n\x1a\x0b\x00\x00\x00\rIHDR";
    assert!(set.match_head(not_png).is_empty());
    assert!(set.best(not_png).is_none());
}

#[test]
fn match_sorting_is_priority_desc_then_id() {
    let text = r#"
schema_version = 1

[magic.low]
name = "低"
mime = "application/octet-stream"
priority = 1

[[magic.low.patterns]]
type = "bytes"
bytes = "41"

[magic.high-b]
name = "高 b"
mime = "application/octet-stream"
priority = 9

[[magic.high-b.patterns]]
type = "bytes"
bytes = "41"

[magic.high-a]
name = "高 a"
mime = "application/octet-stream"
priority = 9

[[magic.high-a.patterns]]
type = "bytes"
bytes = "41"
"#;
    let set = MagicSet::parse(text).expect("解析失败");
    let hits = set.match_head(b"A");
    let ids: Vec<&str> = hits.iter().map(|hit| hit.rule_id.as_str()).collect();
    assert_eq!(ids, ["high-a", "high-b", "low"]);
    assert_eq!(set.best(b"A").map(|hit| hit.rule_id), Some("high-a".into()));
}

#[test]
fn confidence_tiers() {
    let cases = [
        (
            r#"
schema_version = 1
[magic.exact]
name = "exact"
mime = "application/octet-stream"
[[magic.exact.patterns]]
type = "bytes"
bytes = "AABB"
"#,
            vec![0xAA, 0xBB],
            0.95,
        ),
        (
            r#"
schema_version = 1
[magic.masked]
name = "masked"
mime = "application/octet-stream"
[[magic.masked.patterns]]
type = "bytes"
bytes = "AABB"
mask = "FF00"
"#,
            vec![0xAA, 0xCC],
            0.85,
        ),
        (
            r#"
schema_version = 1
[magic.ascii]
name = "ascii"
mime = "application/octet-stream"
[[magic.ascii.patterns]]
type = "ascii"
text = "GIF8"
"#,
            b"GIF8".to_vec(),
            0.80,
        ),
        (
            r#"
schema_version = 1
[magic.u16]
name = "u16"
mime = "application/octet-stream"
[[magic.u16.patterns]]
type = "u16"
value = 0x1234
endian = "big"
"#,
            vec![0x12, 0x34],
            0.90,
        ),
        (
            r#"
schema_version = 1
[magic.u32]
name = "u32"
mime = "application/octet-stream"
[[magic.u32.patterns]]
type = "u32"
value = 0x12345678
endian = "little"
"#,
            vec![0x78, 0x56, 0x34, 0x12],
            0.90,
        ),
    ];

    for (text, data, expected) in cases {
        let set = MagicSet::parse(text).expect("解析失败");
        let hit = set.best(&data).expect("应当命中");
        assert!(
            (hit.confidence - expected).abs() < f32::EPSILON,
            "期望置信度 {expected}，实际 {}",
            hit.confidence
        );
    }
}

#[test]
fn multiple_patterns_take_highest_confidence_and_longest_end() {
    let text = r#"
schema_version = 1
[magic.multi]
name = "multi"
mime = "application/octet-stream"
[[magic.multi.patterns]]
type = "ascii"
text = "MZ"
[[magic.multi.patterns]]
type = "u16"
offset = 2
value = 0x90
endian = "big"
"#;
    let set = MagicSet::parse(text).expect("解析失败");
    let hit = set.best(&[b'M', b'Z', 0x00, 0x90]).expect("应当命中");
    assert!((hit.confidence - 0.90).abs() < f32::EPSILON);
    assert_eq!(hit.matched_len, 4);
}

#[test]
fn u16_u32_endianness() {
    let text = r#"
schema_version = 1
[magic.be16]
name = "be16"
mime = "application/octet-stream"
[[magic.be16.patterns]]
type = "u16"
value = 0x0102
[[magic.be16.patterns]]
type = "u16"
value = 0x0304
endian = "be"

[magic.le16]
name = "le16"
mime = "application/octet-stream"
[[magic.le16.patterns]]
type = "u16"
value = 0x0102
endian = "le"

[magic.le32]
name = "le32"
mime = "application/octet-stream"
[[magic.le32.patterns]]
type = "u32"
value = 0x01020304
endian = "little"
"#;
    let set = MagicSet::parse(text).expect("解析失败");
    assert_eq!(
        set.best(&[0x01, 0x02]).map(|hit| hit.rule_id),
        Some("be16".into())
    );
    assert_eq!(
        set.best(&[0x02, 0x01]).map(|hit| hit.rule_id),
        Some("le16".into())
    );
    assert_eq!(
        set.best(&[0x04, 0x03, 0x02, 0x01]).map(|hit| hit.rule_id),
        Some("le32".into())
    );
}

#[test]
fn legacy_test_table_requires_all_conditions() {
    let text = r#"
schema_version = 1
[magic.mz]
name = "MZ"
mime = "application/x-dosexec"
read = 64

[[magic.mz.test]]
off = 0
len = 2
value = "4D5A"

[[magic.mz.test]]
off = 2
len = 1
value = "90"

[[magic.mz.test]]
off = 3
len = 1
value = "FF"
required = false
"#;
    let set = MagicSet::parse(text).expect("解析失败");
    let hit = set.best(b"MZ\x90").expect("必需条件满足即可命中");
    assert!((hit.confidence - 0.95).abs() < f32::EPSILON);
    assert_eq!(hit.matched_len, 3);
    assert!(set.best(b"MZ\x00").is_none(), "第二个必需条件不满足");
}

#[test]
fn mask_length_mismatch_is_eb005() {
    let text = r#"
schema_version = 1
[magic.bad]
name = "bad"
mime = "application/octet-stream"
[[magic.bad.patterns]]
type = "bytes"
bytes = "AABB"
mask = "FF"
"#;
    let diags = MagicSet::parse(text).expect_err("mask 长度不等应失败");
    assert_code(&diags, "EB005");
}

#[test]
fn empty_id_is_rejected() {
    let text = r#"
schema_version = 1
[magic.""]
name = "empty"
mime = "application/octet-stream"
[[magic."".patterns]]
type = "bytes"
bytes = "00"
"#;
    let diags = MagicSet::parse(text).expect_err("空 id 应失败");
    assert_code(&diags, "EB005");
}

#[test]
fn too_many_patterns_is_eb020() {
    let mut text = String::from(
        "schema_version = 1\n[magic.many]\nname = \"many\"\nmime = \"application/octet-stream\"\n",
    );
    for _ in 0..=64 {
        text.push_str("[[magic.many.patterns]]\ntype = \"bytes\"\nbytes = \"00\"\n");
    }
    let diags = MagicSet::parse(&text).expect_err("65 条 pattern 应失败");
    assert_code(&diags, "EB020");
}

#[test]
fn too_many_rules_is_eb020() {
    let mut text = String::from("schema_version = 1\n");
    for index in 0..=10_000 {
        text.push_str(&format!(
            "[magic.r{index:05}]\nname = \"r{index}\"\nmime = \"application/octet-stream\"\n\
             [[magic.r{index:05}.patterns]]\ntype = \"bytes\"\nbytes = \"00\"\n"
        ));
    }
    let diags = MagicSet::parse(&text).expect_err("10,001 条规则应失败");
    assert_code(&diags, "EB020");
}

#[test]
fn read_window_and_offset_are_checked() {
    let valid = r#"
schema_version = 1
[magic.window]
name = "window"
mime = "application/octet-stream"
read = 8
[[magic.window.patterns]]
type = "bytes"
offset = 7
bytes = "AA"
"#;
    let set = MagicSet::parse(valid).expect("offset 7 + 1 <= read 8 应通过");
    assert!(set.best(&[0; 7]).is_none());
    assert!(set.best(&[0; 8]).is_none());
    let mut data = [0_u8; 8];
    data[7] = 0xAA;
    assert_eq!(set.best(&data).map(|hit| hit.matched_len), Some(8));

    let invalid = r#"
schema_version = 1
[magic.window]
name = "window"
mime = "application/octet-stream"
read = 8
[[magic.window.patterns]]
type = "bytes"
offset = 8
bytes = "AA"
"#;
    let diags = MagicSet::parse(invalid).expect_err("offset 8 + 1 > read 8 应失败");
    assert_code(&diags, "EB005");

    let too_large = r#"
schema_version = 1
[magic.window]
name = "window"
mime = "application/octet-stream"
read = 4097
[[magic.window.patterns]]
type = "bytes"
bytes = "AA"
"#;
    let diags = MagicSet::parse(too_large).expect_err("read 4097 应失败");
    assert_code(&diags, "EB005");
}

#[test]
fn match_head_never_reads_past_4096() {
    let text = r#"
schema_version = 1
[magic.tail]
name = "tail"
mime = "application/octet-stream"
read = 4096
[[magic.tail.patterns]]
type = "bytes"
offset = 4095
bytes = "AA"
"#;
    let set = MagicSet::parse(text).expect("解析失败");

    let mut exact = vec![0_u8; 4096];
    exact[4095] = 0xAA;
    assert_eq!(set.best(&exact).map(|hit| hit.matched_len), Some(4096));

    let short = vec![0_u8; 4095];
    assert!(set.best(&short).is_none());

    // 更长的数据不会扩大读取窗口：4096 之后即便出现 AA 也不参与匹配。
    let mut long = vec![0_u8; 8192];
    long[5000] = 0xAA;
    assert!(set.best(&long).is_none());
}

#[test]
fn min_size_is_enforced() {
    let text = r#"
schema_version = 1
[magic.png]
name = "png"
mime = "image/png"
min_size = 8
[[magic.png.patterns]]
type = "bytes"
bytes = "89504E47"
"#;
    let set = MagicSet::parse(text).expect("解析失败");
    assert!(set.best(&[0x89, b'P', b'N', b'G']).is_none());
    assert!(set.best(&[0x89, b'P', b'N', b'G', 0, 0, 0, 0]).is_some());
}

#[test]
fn load_reads_file_and_rejects_oversize() {
    let dir = std::env::temp_dir();
    let unique = format!(
        "geode-gmagic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("时间")
            .as_nanos()
    );
    let good = dir.join(format!("{unique}.toml"));
    fs::write(
        &good,
        fs::read(vector_dir().join("0001-minimal.toml")).expect("读取向量"),
    )
    .expect("写临时文件");
    let set = MagicSet::load(&good).expect("load 应成功");
    assert_eq!(set.len(), 1);
    let _ = fs::remove_file(&good);

    let oversize = dir.join(format!("{unique}-big.toml"));
    fs::write(&oversize, vec![b'#'; 4 * 1024 * 1024 + 1]).expect("写大文件");
    let diags = MagicSet::load(&oversize).expect_err("超过 4 MiB 应失败");
    assert_code(&diags, "EB020");
    let _ = fs::remove_file(&oversize);
}

#[test]
fn thousand_rules_best_is_fast_enough() {
    let mut text = String::from("schema_version = 1\n");
    for index in 0..1000 {
        text.push_str(&format!(
            "[magic.r{index:04}]\nname = \"r{index}\"\nmime = \"application/octet-stream\"\n\
             priority = {}\n[[magic.r{index:04}.patterns]]\ntype = \"bytes\"\nbytes = \"00\"\n",
            index % 7
        ));
    }
    let set = MagicSet::parse(&text).expect("1000 条规则应解析成功");
    assert_eq!(set.len(), 1000);

    let data = vec![0_u8; 64];
    // 预热，避免把首次调用的冷分支算进断言。
    assert!(set.best(&data).is_some());

    let start = Instant::now();
    for _ in 0..100 {
        assert!(set.best(&data).is_some());
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "1000 条规则 100 次 best() 耗时 {elapsed:?}，超过粗略上限 500 ms"
    );
}

#[test]
fn malformed_toml_is_eb003() {
    let diags = MagicSet::parse("schema_version = ").expect_err("语法错应失败");
    assert_code(&diags, "EB003");
}

#[test]
fn missing_schema_version_is_eb004() {
    let diags = MagicSet::parse("[magic.a]\nname = \"a\"\nmime = \"application/octet-stream\"\n")
        .expect_err("缺 schema_version 应失败");
    assert_code(&diags, "EB004");
}

#[test]
fn endian_enum_is_public() {
    let pattern = MagicPattern::U16 {
        offset: 0,
        value: 1,
        endian: Endian::Little,
    };
    assert_eq!(pattern.len(), 2);
    assert_eq!(pattern.offset(), 0);
    assert_eq!(pattern.confidence(), 0.90);
    assert!(!pattern.is_empty());
    let _ = error_codes::EB031;
}
