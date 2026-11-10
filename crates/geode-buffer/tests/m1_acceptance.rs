//! M1 验收（18 §10 的五条）中可在本机自动化的部分。
//!
//! DESIGN: 18 §10 M1①~⑤、PLAN T1.9
//!
//! 明确不在这里做的：
//! - ④ 的 1 GiB 保存吞吐：本机磁盘实测 110.9 MiB/s，低于 500 MB/s 预算（环境限制，见 PLAN）；
//! - ③ 的"重启弹提示"：属于 UI 层（25 §4），这里只验"恢复出 N 条修改"的数据面。

use geode_buffer::edit::Editor;
use geode_buffer::journal::{BaseKind, JournalHeader, JournalReader, JournalWriter};
use geode_buffer::piece::PieceTable;
use geode_core::Offset;
use geode_provider::{FileProvider, MemoryProvider};
use std::sync::Arc;
use std::time::Instant;

const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;

fn temp_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "geode-m1-{tag}-{}-{}.geojournal",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

/// M1①：4 GiB 文件 0x8000_0000 处插入 16 字节，1 秒内可见（读得到）。
#[test]
fn insert_at_2gib_is_visible_within_one_second() {
    let path = temp_path("insert4g");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(FOUR_GIB).unwrap();
    drop(f);

    let provider: Arc<FileProvider> = Arc::new(FileProvider::open(&path).unwrap());
    let mut ed = Editor::new(PieceTable::from_source(FOUR_GIB), provider);
    let data = [0xAA_u8; 16];

    let t0 = Instant::now();
    ed.insert(0x8000_0000, &data).unwrap();
    let mut back = vec![0u8; 16];
    let n = ed.read(0x8000_0000, 16);
    back.copy_from_slice(&n);
    let elapsed = t0.elapsed();

    assert_eq!(back, data, "插入的 16 字节必须立刻读得到");
    assert_eq!(ed.len(), FOUR_GIB + 16);
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "插入+回读耗时 {elapsed:?} 超过 1 秒"
    );
    drop(ed);
    let _ = std::fs::remove_file(&path);
}

/// M1② 的数据面：10,000 次随机编辑后全部撤销，逐字节相等。
#[test]
fn ten_thousand_edits_undo_all_byte_equal() {
    let original: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    let src: Arc<MemoryProvider> = Arc::new(MemoryProvider::new("m1", original.clone()));
    let mut ed = Editor::new(PieceTable::from_bytes(&original), src);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..10_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let at = x % (ed.len().max(1));
        match x % 4 {
            0 => {
                let n = (x % 8 + 1) as usize;
                ed.overwrite(at, &vec![(x & 0xff) as u8; n]).unwrap();
            }
            1 => ed.insert(at, b"xy").unwrap(),
            2 if ed.len() > 8 => {
                let n = (1 + x % 4).min(ed.len() - at);
                if n > 0 {
                    ed.remove(at, n).unwrap();
                }
            }
            _ => ed.fill(at, 1, 0).unwrap(),
        }
    }
    let undone = ed.undo_all();
    assert!(undone > 0);
    assert_eq!(ed.materialize(), original, "撤销全部后必须逐字节相等");
}

/// M1③ 的数据面：journal 尾部被截断后，仍能恢复出完整前缀并标 Partial。
#[test]
fn journal_recovers_prefix_after_truncation() {
    let path = temp_path("recover");
    let header = JournalHeader::new([7u8; 16], BaseKind::File, 4096, 0, [0u8; 32]);
    {
        let mut w = JournalWriter::create(&path, header).unwrap();
        w.append_insert(Offset::ZERO, b"AAAA").unwrap();
        w.append_overwrite(Offset::new(4), b"BBBB").unwrap();
        w.append_remove(Offset::new(100), 4).unwrap();
        w.sync().unwrap();
    }
    // 砍掉最后一个字节，模拟掉电时最后一条没写完
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();

    let rec = JournalReader::open(&path).unwrap();
    assert!(rec.partial, "尾部截断必须标 Partial");
    let edits: Vec<_> = rec.edits.iter().filter(|e| !e.is_control()).collect();
    assert!(edits.len() >= 2, "至少恢复出前两条，实际 {}", edits.len());
    assert!(rec.truncated_at.is_some());
    let _ = std::fs::remove_file(&path);
}
