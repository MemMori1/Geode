//! `.geojournal` 写入/恢复的集成测试（T1.8）。
//!
//! 覆盖：正常往返、CRC 坏帧保留前缀、seq 跳号、base 指纹、FinalLen、
//! 空文件/头截断/只有头/帧截断等边界。

use geode_buffer::journal::{
    BaseFingerprint, BaseKind, Edit, JournalError, JournalHeader, JournalReader, JournalWriter,
    RecordBody,
};
use geode_core::Offset;
use geode_format::journal::{self, File as RawFile, Record as RawRecord};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// 测试用临时目录，`Drop` 时删除。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geode-journal-test-{}-{tag}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("创建临时目录");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn header() -> JournalHeader {
    JournalHeader::new([0x11; 16], BaseKind::File, 4096, 4096, [0xAB; 32])
}

fn write_journal(dir: &TempDir, name: &str) -> PathBuf {
    dir.file(name)
}

/// 一条 Overwrite（8 B 数据）的完整帧长度：13 + (16 + 8) + 4。
const OVERWRITE_FRAME: usize = 41;

fn expect_error(error: JournalError, code: &str) {
    assert_eq!(
        error.code(),
        Some(code),
        "期望 {code}，实际 {error}（detail: {:?}）",
        error.as_format().map(|e| &e.detail)
    );
}

#[test]
fn round_trip_n_records() {
    let dir = TempDir::new("round-trip");
    let path = write_journal(&dir, "doc.geojournal");

    let mut writer = JournalWriter::create(&path, header()).expect("create");
    assert_eq!(writer.seq(), 0);
    assert_eq!(writer.header(), &header());

    // 原始 append API：Overwrite 载荷 = at u64 │ len u64 │ data。
    let mut payload = Vec::new();
    payload.extend_from_slice(&0x10u64.to_be_bytes());
    payload.extend_from_slice(&4u64.to_be_bytes());
    payload.extend_from_slice(&[1, 2, 3, 4]);
    assert_eq!(writer.append(0x01, &payload).expect("append raw"), 1);

    assert_eq!(
        writer
            .append_insert(Offset::new(0x100), b"abc")
            .expect("insert"),
        2
    );
    assert_eq!(
        writer.append_remove(Offset::new(0x200), 2).expect("remove"),
        3
    );
    assert_eq!(writer.append_checkpoint("paste").expect("checkpoint"), 4);
    assert_eq!(
        writer
            .append_insert(Offset::new(0x300), b"de")
            .expect("insert"),
        5
    );
    // 4096 + 3 + 2 - 2 = 4099
    writer.finish(4099).expect("finish");
    assert!(writer.is_finished());

    let recovered = JournalReader::open(&path).expect("open");
    assert!(recovered.is_clean());
    assert!(!recovered.partial);
    assert_eq!(recovered.failure, None);
    assert_eq!(recovered.truncated_at, None);
    assert_eq!(recovered.header, header());
    assert_eq!(recovered.last_seq, 6);
    assert_eq!(recovered.final_len, Some(4099));
    assert_eq!(recovered.edits.len(), 6);

    assert_eq!(
        recovered.edits[0],
        Edit {
            seq: 1,
            body: RecordBody::Overwrite {
                at: Offset::new(0x10),
                data: vec![1, 2, 3, 4],
            },
        }
    );
    assert_eq!(
        recovered.edits[1].body,
        RecordBody::Insert {
            at: Offset::new(0x100),
            data: b"abc".to_vec(),
        }
    );
    assert_eq!(
        recovered.edits[2].body,
        RecordBody::Remove {
            at: Offset::new(0x200),
            len: 2,
        }
    );
    assert_eq!(
        recovered.edits[3].body,
        RecordBody::Checkpoint {
            label: "paste".to_owned(),
        }
    );
    assert!(matches!(
        recovered.edits[5].body,
        RecordBody::FinalLen { len: 4099 }
    ));
    assert_eq!(recovered.edits[5].kind(), 0x06);
    assert!(recovered.edits[5].is_control());
    assert!(!recovered.edits[0].is_control());
}

#[test]
fn crc_flip_keeps_prefix_and_marks_partial() {
    let dir = TempDir::new("crc-flip");
    let path = write_journal(&dir, "doc.geojournal");

    let mut writer = JournalWriter::create(&path, header()).expect("create");
    for i in 0..5u8 {
        writer
            .append_overwrite(Offset::new(u64::from(i) * 8), &[i; 8])
            .expect("append");
    }
    writer.finish(4096).expect("finish");

    let mut bytes = fs::read(&path).expect("read");
    // 翻转第 3 帧 CRC 的最后一个字节。
    let frame3 = 80 + 2 * OVERWRITE_FRAME;
    bytes[frame3 + OVERWRITE_FRAME - 1] ^= 0xFF;
    fs::write(&path, &bytes).expect("write corrupt");

    let recovered = JournalReader::open(&path).expect("partial 仍是 Ok");
    assert!(recovered.partial);
    assert!(!recovered.is_clean());
    assert_eq!(recovered.failure_code(), Some("EB011"));
    assert_eq!(recovered.edits.len(), 2, "只保留坏帧之前的前缀");
    assert_eq!(recovered.last_seq, 2);
    assert_eq!(recovered.truncated_at, Some(frame3 as u64));
    assert_eq!(recovered.final_len, None, "坏帧之后不再回放");
}

#[test]
fn seq_gap_is_eb012() {
    let dir = TempDir::new("seq-gap");
    let path = write_journal(&dir, "doc.geojournal");

    // seq 跳号且 CRC 必须自洽：用编解码器直接造一份合法 CRC 的坏 seq 文件。
    let mut file = RawFile::new([0x11; 16], BaseKind::File, 4096, 4096, [0xAB; 32]);
    for (index, seq) in [1u64, 2, 9].into_iter().enumerate() {
        file.records.push(RawRecord::new(
            seq,
            RecordBody::Overwrite {
                at: Offset::new(index as u64 * 8),
                data: vec![index as u8; 8],
            },
        ));
    }
    fs::write(&path, journal::encode(&file)).expect("write");

    let recovered = JournalReader::open(&path).expect("partial 仍是 Ok");
    assert!(recovered.partial);
    assert_eq!(recovered.failure_code(), Some("EB012"));
    assert_eq!(recovered.edits.len(), 2);
    assert_eq!(recovered.last_seq, 2);
    assert_eq!(
        recovered.truncated_at,
        Some(80 + 2 * OVERWRITE_FRAME as u64)
    );
}

#[test]
fn base_fingerprint_mismatch_is_eb021() {
    let dir = TempDir::new("base-mismatch");
    let path = write_journal(&dir, "doc.geojournal");

    let mut writer = JournalWriter::create(&path, header()).expect("create");
    writer
        .append_insert(Offset::new(4096), b"x")
        .expect("append");
    writer.finish(4097).expect("finish");

    let good = BaseFingerprint::new(4096, 4096, [0xAB; 32]);
    let recovered = JournalReader::open_with_base(&path, &good).expect("指纹一致");
    assert_eq!(recovered.final_len, Some(4097));

    let bad_hash = BaseFingerprint::new(4096, 4096, [0xAC; 32]);
    expect_error(
        JournalReader::open_with_base(&path, &bad_hash).expect_err("指纹不符"),
        "EB021",
    );

    let bad_size = BaseFingerprint::new(4097, 4096, [0xAB; 32]);
    expect_error(
        JournalReader::open_with_base(&path, &bad_size).expect_err("大小不符"),
        "EB021",
    );
}

#[test]
fn final_len_mismatch_is_eb021() {
    let dir = TempDir::new("final-len");
    let path = write_journal(&dir, "doc.geojournal");

    let mut writer = JournalWriter::create(&path, header()).expect("create");
    writer
        .append_insert(Offset::new(4096), b"abcd")
        .expect("append");
    writer.finish(9999).expect("finish");

    let error = JournalReader::open(&path).expect_err("FinalLen 不符");
    expect_error(error, "EB021");
}

#[test]
fn empty_file_and_truncated_header_are_eb013() {
    let dir = TempDir::new("truncated-header");

    let empty = write_journal(&dir, "empty.geojournal");
    fs::write(&empty, b"").expect("write empty");
    expect_error(JournalReader::open(&empty).expect_err("空文件"), "EB013");

    let short = write_journal(&dir, "short.geojournal");
    fs::write(&short, b"GEO").expect("write short");
    expect_error(
        JournalReader::open(&short).expect_err("不足 magic"),
        "EB013",
    );

    let half = write_journal(&dir, "half.geojournal");
    let mut bytes = header_bytes(&header());
    bytes.truncate(40);
    fs::write(&half, &bytes).expect("write half header");
    expect_error(JournalReader::open(&half).expect_err("头截断"), "EB013");
}

#[test]
fn bad_magic_and_version_are_format_errors() {
    let dir = TempDir::new("bad-header");
    let base = header_bytes(&header());

    let magic = write_journal(&dir, "magic.geojournal");
    let mut bytes = base.clone();
    bytes[0] ^= 0xFF;
    fs::write(&magic, &bytes).expect("write magic");
    expect_error(
        JournalReader::open(&magic).expect_err("magic 不符"),
        "EB002",
    );

    let version = write_journal(&dir, "version.geojournal");
    let mut bytes = base;
    bytes[8..10].copy_from_slice(&2u16.to_be_bytes());
    fs::write(&version, &bytes).expect("write version");
    expect_error(
        JournalReader::open(&version).expect_err("版本不符"),
        "EB001",
    );
}

#[test]
fn header_only_is_a_clean_empty_journal() {
    let dir = TempDir::new("header-only");
    let path = write_journal(&dir, "doc.geojournal");

    let mut writer = JournalWriter::create(&path, header()).expect("create");
    writer.sync().expect("sync");
    drop(writer);

    let recovered = JournalReader::open(&path).expect("open");
    assert!(recovered.is_clean());
    assert_eq!(recovered.edits.len(), 0);
    assert_eq!(recovered.last_seq, 0);
    assert_eq!(recovered.final_len, None);
    assert_eq!(recovered.truncated_at, None);
    assert_eq!(recovered.header, header());
}

#[test]
fn truncated_last_frame_is_partial_not_panic() {
    let dir = TempDir::new("truncated-frame");
    let path = write_journal(&dir, "doc.geojournal");

    let mut writer = JournalWriter::create(&path, header()).expect("create");
    for i in 0..3u8 {
        writer
            .append_overwrite(Offset::new(u64::from(i) * 8), &[i; 8])
            .expect("append");
    }
    writer.finish(4096).expect("finish");
    drop(writer);

    let bytes = fs::read(&path).expect("read");
    let final_len_at = 80 + 3 * OVERWRITE_FRAME;
    fs::write(&path, &bytes[..bytes.len() - 2]).expect("truncate");

    let recovered = JournalReader::open(&path).expect("partial");
    assert!(recovered.partial);
    assert_eq!(recovered.failure_code(), Some("EB013"));
    assert_eq!(recovered.edits.len(), 3);
    assert_eq!(recovered.last_seq, 3);
    assert_eq!(recovered.truncated_at, Some(final_len_at as u64));
}

#[test]
fn append_after_finish_is_closed() {
    let dir = TempDir::new("closed");
    let path = write_journal(&dir, "doc.geojournal");

    let mut writer = JournalWriter::create(&path, header()).expect("create");
    writer.finish(4096).expect("finish");
    let error = writer.append(0x01, &[]).expect_err("closed");
    assert!(matches!(error, JournalError::Closed));
    assert_eq!(error.code(), None);
}

/// 测试内复刻头的 80 B 编码（`JournalHeader::encode` 是私有实现细节）。
fn header_bytes(header: &JournalHeader) -> Vec<u8> {
    let file = RawFile::new(
        header.doc_uuid,
        header.base_kind,
        header.base_size,
        header.head_len,
        header.base_head_blake3,
    );
    journal::encode(&file)
}
