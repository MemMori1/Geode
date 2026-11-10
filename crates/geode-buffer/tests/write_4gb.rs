//! T1.1 验收：4 GiB 文件随机位置写入 1 MiB，回读逐字节相等。
//!
//! DESIGN: 18 §10（M1 验收①）、04 §4.6
//!
//! 这里刻意分两层：
//! - `pwrite_at_4gib`：只验 Provider 的写路径（不重写整个文件），秒级，CI 常跑；
//! - `save_4gib_roundtrip`：完整"编辑 → 保存 → 重开 → 校验"，要重写 4 GiB，默认 `#[ignore]`。

use geode_buffer::edit::Editor;
use geode_buffer::editor_source::EditorSource;
use geode_buffer::piece::PieceTable;
use geode_buffer::save::save_provider_atomic;
use geode_core::Offset;
use geode_provider::{FileProvider, Provider};
use std::sync::Arc;

const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;
const AT: u64 = 0x8000_0000; // 2 GiB 处
const LEN: u64 = 1024 * 1024;

fn pattern(n: u64) -> Vec<u8> {
    (0..n).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

fn temp_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "geode-t11-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

#[test]
fn pwrite_at_4gib() {
    let path = temp_path("pwrite");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(FOUR_GIB).unwrap();
    drop(file);

    let provider = FileProvider::open_rw(&path).unwrap();
    assert!(provider.is_writable(), "open_rw 必须给可写句柄");
    let data = pattern(LEN);
    let written = provider.write_at(Offset::new(AT), &data).unwrap();
    assert_eq!(written as u64, LEN);
    provider.sync_all().unwrap();

    let mut back = vec![0u8; LEN as usize];
    let report = provider.read_range(Offset::new(AT), &mut back).unwrap();
    assert_eq!(report.filled, LEN);
    assert_eq!(back, data, "4 GiB 偏移处回读必须逐字节相等");
    assert_eq!(provider.size().lower_bound(), FOUR_GIB, "长度不变");
    drop(provider);

    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "完整 4 GiB 保存需重写整文件（约 40s + 8 GiB 空闲磁盘）；发布前手动跑"]
fn save_4gib_roundtrip() {
    let path = temp_path("save");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(FOUR_GIB).unwrap();
    drop(file);

    let provider: Arc<FileProvider> = Arc::new(FileProvider::open(&path).unwrap());
    let table = PieceTable::from_source(FOUR_GIB);
    let mut editor = Editor::new(table, provider.clone());
    let data = pattern(LEN);
    editor.overwrite(AT, &data).unwrap();

    let src = EditorSource::new(&editor);
    let stats = save_provider_atomic(&src, &path).unwrap();
    assert_eq!(stats.bytes, FOUR_GIB, "保存后长度必须等于编辑视图长度");
    assert!(stats.replaced);

    let check = FileProvider::open(&path).unwrap();
    let mut back = vec![0u8; LEN as usize];
    let report = check.read_range(Offset::new(AT), &mut back).unwrap();
    assert_eq!(report.filled, LEN);
    assert_eq!(back, data);
    assert_eq!(check.size().lower_bound(), FOUR_GIB);
    drop(check);

    let _ = std::fs::remove_file(&path);
}
