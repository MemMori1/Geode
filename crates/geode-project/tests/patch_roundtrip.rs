//! T1.6 补丁桥的往返、kind 与损坏输入测试。
//!
//! DESIGN: 04 §4.5（PatchSet）、23 §3.6（.geopatch 记录）、schemas/geopatch/v1.layout.md。
//!
//! 覆盖三层：
//! 1. 64 MiB 内存源随机 500 次编辑，全量逐字节对拍；
//! 2. 4 GiB 稀疏源随机 500 次编辑，只比对长度与改动窗口（绝不 materialize 全文件）；
//! 3. Insert/Remove/Fill kind、损坏的 FinalLen/seq/越界、encode→decode 记录一致性。

use geode_buffer::edit::{EditKind, Editor};
use geode_buffer::piece::PieceTable;
use geode_core::{ByteRange, Offset, ProviderId};
use geode_format::patch::{File, Record, RecordBody, decode, encode};
use geode_project::ProjectError;
use geode_project::patch_bridge::{EditBody, apply_patch_file, from_patch_file, to_patch_file};
use geode_provider::{Caps, Provider, ProviderError, ReadReport, SizeInfo};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const MIB: u64 = 1024 * 1024;

// ---------------------------------------------------------------------------
// 测试用 Provider
// ---------------------------------------------------------------------------

/// 共享字节源的只读 Provider，两个 Editor 可以指向同一份 64 MiB 内存。
#[derive(Debug)]
struct SharedBytesProvider {
    bytes: Arc<Vec<u8>>,
    reads: AtomicU64,
}

impl SharedBytesProvider {
    fn new(bytes: Arc<Vec<u8>>) -> Self {
        Self {
            bytes,
            reads: AtomicU64::new(0),
        }
    }
}

impl Provider for SharedBytesProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(0x5348_4152)
    }

    fn display_name(&self) -> String {
        "测试: 共享内存".into()
    }

    fn caps(&self) -> Caps {
        Caps::READ_ONLY_STABLE
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.bytes.len() as u64)
    }

    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        let start = usize::try_from(at.get()).unwrap_or(usize::MAX);
        if start >= self.bytes.len() {
            return Ok(ReadReport {
                filled: 0,
                holes: 0,
                eof_hit: true,
            });
        }
        let n = buf.len().min(self.bytes.len() - start);
        buf[..n].copy_from_slice(&self.bytes[start..start + n]);
        self.reads.fetch_add(n as u64, Ordering::Relaxed);
        Ok(ReadReport {
            filled: n as u64,
            holes: 0,
            eof_hit: n < buf.len(),
        })
    }

    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        Some(vec![ByteRange::from_len(
            Offset::ZERO,
            self.bytes.len() as u64,
        )?])
    }
}

/// 4 GiB 逻辑稀疏源：只保存少量种子区间，其余位置读出全 0。
///
/// `size` 是逻辑长度，不分配对应内存；`read_range` 只触碰请求窗口和种子。
#[derive(Debug)]
struct SparseProvider {
    size: u64,
    seeds: Vec<(u64, Vec<u8>)>,
    reads: AtomicU64,
}

impl SparseProvider {
    fn new(size: u64, seeds: Vec<(u64, Vec<u8>)>) -> Self {
        Self {
            size,
            seeds,
            reads: AtomicU64::new(0),
        }
    }

    fn bytes_read(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl Provider for SparseProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(0x5350_4152)
    }

    fn display_name(&self) -> String {
        "测试: 4 GiB 稀疏源".into()
    }

    fn caps(&self) -> Caps {
        Caps::READ_ONLY_STABLE
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.size)
    }

    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        let start = at.get();
        if start >= self.size {
            return Ok(ReadReport {
                filled: 0,
                holes: 0,
                eof_hit: true,
            });
        }
        let requested = buf.len() as u64;
        let available = requested.min(self.size - start);
        let end = start + available;
        buf.fill(0);

        let mut covered = 0u64;
        for (seed_start, data) in &self.seeds {
            let seed_end = seed_start.saturating_add(data.len() as u64);
            let lo = start.max(*seed_start);
            let hi = end.min(seed_end);
            if lo < hi {
                let dst = usize::try_from(lo - start).unwrap_or(0);
                let src = usize::try_from(lo - seed_start).unwrap_or(0);
                let n = usize::try_from(hi - lo).unwrap_or(0);
                buf[dst..dst + n].copy_from_slice(&data[src..src + n]);
                covered += hi - lo;
            }
        }

        self.reads.fetch_add(available, Ordering::Relaxed);
        Ok(ReadReport {
            filled: available,
            holes: available - covered,
            eof_hit: available < requested,
        })
    }
}

// ---------------------------------------------------------------------------
// 随机编辑计划
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlannedKind {
    Overwrite,
    Insert,
    Remove,
    Fill,
}

#[derive(Clone, Copy, Debug)]
struct PlannedEdit {
    /// 编辑发生时的当前坐标（Editor 的 Patch::orig 也就是这个值）。
    at: u64,
    kind: PlannedKind,
    len: usize,
    byte: u8,
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
}

/// 生成 `count` 条位置严格递增的编辑。
///
/// 位置按大间隔递增，避开 `geode-buffer::Editor::push_patch` 的 16 B 归并窗口：
/// 该归并在插入/删除改变长度后使用旧坐标，合并出的 `Patch` 不再是合法的连续区间。
/// 编辑本身仍是随机位置、随机种类、随机长度。
fn plan_edits(size: u64, count: usize, seed: u64) -> Vec<PlannedEdit> {
    let mut rng = Rng::new(seed);
    let slot = size / (count as u64 + 2);
    let mut plan = Vec::with_capacity(count);

    for i in 0..count {
        let base = slot * (i as u64 + 1);
        let jitter = rng.below(slot.saturating_sub(4096).max(1));
        let at = base + jitter;
        let kind = match rng.below(4) {
            0 => PlannedKind::Overwrite,
            1 => PlannedKind::Insert,
            2 => PlannedKind::Remove,
            _ => PlannedKind::Fill,
        };
        let max_len = if kind == PlannedKind::Fill { 16 } else { 64 };
        let len = 1 + usize::try_from(rng.below(max_len)).unwrap_or(0);
        plan.push(PlannedEdit {
            at,
            kind,
            len,
            byte: rng.next_u64() as u8,
        });
    }
    plan
}

fn payload(edit: &PlannedEdit, index: usize) -> Vec<u8> {
    (0..edit.len)
        .map(|j| {
            edit.byte
                .wrapping_add((j as u8).wrapping_mul(17))
                .wrapping_add(index as u8)
        })
        .collect()
}

fn apply_plan(editor: &mut Editor, plan: &[PlannedEdit]) {
    for (index, edit) in plan.iter().enumerate() {
        let data = payload(edit, index);
        match edit.kind {
            PlannedKind::Overwrite => editor.overwrite(edit.at, &data).unwrap(),
            PlannedKind::Insert => editor.insert(edit.at, &data).unwrap(),
            PlannedKind::Remove => editor.remove(edit.at, edit.len as u64).unwrap(),
            PlannedKind::Fill => editor.fill(edit.at, edit.len as u64, edit.byte).unwrap(),
        }
    }
}

fn planned_final_len(size: u64, plan: &[PlannedEdit]) -> u64 {
    let mut len = i128::from(size);
    for edit in plan {
        len += match edit.kind {
            PlannedKind::Insert => edit.len as i128,
            PlannedKind::Remove => -(edit.len as i128),
            PlannedKind::Overwrite | PlannedKind::Fill => 0,
        };
    }
    u64::try_from(len).unwrap()
}

fn pattern_bytes(size: u64) -> Vec<u8> {
    let mut bytes = vec![0u8; usize::try_from(size).unwrap()];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = (i % 251) as u8 ^ ((i / 251) as u8).wrapping_mul(29);
    }
    bytes
}

fn new_editor(provider: Arc<dyn Provider>, size: u64) -> Editor {
    Editor::new(PieceTable::from_source(size), provider)
}

// ---------------------------------------------------------------------------
// 往返测试
// ---------------------------------------------------------------------------

#[test]
fn random_500_edits_roundtrip_64mib_byte_exact() {
    let size = 64 * MIB;
    let source = Arc::new(pattern_bytes(size));
    let provider: Arc<dyn Provider> = Arc::new(SharedBytesProvider::new(Arc::clone(&source)));
    let mut editor = new_editor(Arc::clone(&provider), size);
    let plan = plan_edits(size, 500, 0xC0FF_EE00_1234_5678);
    assert!(plan.iter().any(|edit| edit.kind == PlannedKind::Overwrite));
    assert!(plan.iter().any(|edit| edit.kind == PlannedKind::Insert));
    assert!(plan.iter().any(|edit| edit.kind == PlannedKind::Remove));
    assert!(plan.iter().any(|edit| edit.kind == PlannedKind::Fill));
    apply_plan(&mut editor, &plan);

    assert_eq!(
        editor.patches().len(),
        plan.len(),
        "位置间隔足够大，不应发生归并"
    );
    let file = to_patch_file(editor.patches(), size);
    assert_eq!(file.base_size, size);
    assert_eq!(file.flags, 0);
    assert!(matches!(
        file.records.last().map(|r| &r.body),
        Some(RecordBody::FinalLen { final_size }) if *final_size == planned_final_len(size, &plan)
    ));
    assert_eq!(editor.len(), planned_final_len(size, &plan));

    let encoded = encode(&file);
    let decoded = decode(&encoded).unwrap();
    assert_eq!(file, decoded, "encode→decode 后 File 必须完全一致");

    let mut restored = new_editor(Arc::clone(&provider), size);
    apply_patch_file(&mut restored, &decoded).unwrap();
    assert_eq!(restored.len(), editor.len());

    let final_len = planned_final_len(size, &plan);
    let left = editor.materialize();
    let right = restored.materialize();
    assert_eq!(left.len(), usize::try_from(final_len).unwrap());
    assert_eq!(right.len(), left.len());
    assert!(left == right, "64 MiB 全量逐字节对拍失败");
    eprintln!("64 MiB 全量对拍：{} B（500 次编辑）", left.len());
}

#[test]
fn random_500_edits_roundtrip_4gib_sparse_windows() {
    let size = 4 * 1024 * MIB; // 4 GiB
    let plan = plan_edits(size, 500, 0x4A1B_0000_0000_0001_u64);
    let final_len = planned_final_len(size, &plan);

    // 在每个编辑点附近放 1 KiB 非零种子；其余 4 GiB 都是空洞，不分配内存。
    let mut seeds = Vec::with_capacity(plan.len());
    for (index, edit) in plan.iter().enumerate() {
        let start = edit.at.saturating_sub(512);
        let len = usize::try_from(1024u64.min(size - start)).unwrap();
        let data = (0..len)
            .map(|j| {
                (index as u8)
                    .wrapping_mul(31)
                    .wrapping_add((j as u8).wrapping_mul(7))
            })
            .collect();
        seeds.push((start, data));
    }

    let sparse = Arc::new(SparseProvider::new(size, seeds));
    let provider: Arc<dyn Provider> = sparse.clone();
    let mut editor = new_editor(Arc::clone(&provider), size);
    apply_plan(&mut editor, &plan);
    assert_eq!(editor.patches().len(), plan.len());
    assert_eq!(editor.len(), final_len);

    let file = to_patch_file(editor.patches(), size);
    assert_eq!(file.records.len(), plan.len() + 1);
    let decoded = decode(&encode(&file)).unwrap();
    assert_eq!(file, decoded);

    let mut restored = new_editor(Arc::clone(&provider), size);
    apply_patch_file(&mut restored, &decoded).unwrap();
    assert_eq!(restored.len(), final_len);

    // 只比对每个改动点附近 128 B；绝不 materialize 4 GiB。
    let window = 128u64;
    let mut compared = 0u64;
    for edit in &plan {
        let start = edit.at.saturating_sub(32);
        let len = window.min(final_len - start);
        let left = editor.read(start, len);
        let right = restored.read(start, len);
        assert!(
            left == right,
            "4 GiB 改动窗口不一致：at={}，窗口 [{start}, {}+{len})",
            edit.at,
            start
        );
        compared += len;
    }
    assert_eq!(compared, window * plan.len() as u64);
    let read = sparse.bytes_read();
    assert!(
        read < 16 * MIB,
        "4 GiB 测试实际读了 {read} B，超过 16 MiB 预算"
    );
    eprintln!("4 GiB 改动窗口对拍：{compared} B，Provider 实读 {read} B");
}

#[test]
fn insert_remove_fill_patch_kinds_and_records() {
    let size = 4096u64;
    let source = Arc::new(pattern_bytes(size));
    let provider: Arc<dyn Provider> = Arc::new(SharedBytesProvider::new(source));
    let mut editor = new_editor(provider, size);

    editor.overwrite(10, b"AAAA").unwrap();
    editor.insert(20, b"BBB").unwrap();
    editor.remove(30, 2).unwrap();
    editor.fill(40, 4, 0xEE).unwrap();

    let kinds: Vec<EditKind> = editor.patches().iter().map(|patch| patch.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EditKind::Overwrite,
            EditKind::Insert,
            EditKind::Remove,
            EditKind::Fill,
        ]
    );

    let file = to_patch_file(editor.patches(), size);
    assert_eq!(file.records.len(), 5);
    assert!(matches!(
        file.records[0].body,
        RecordBody::Overwrite { at, .. } if at.get() == 10
    ));
    assert!(matches!(
        file.records[1].body,
        RecordBody::Insert { at, .. } if at.get() == 20
    ));
    assert!(matches!(
        file.records[2].body,
        RecordBody::Remove { at, len: 2 } if at.get() == 30
    ));
    assert!(matches!(
        file.records[3].body,
        RecordBody::Overwrite { at, ref data } if at.get() == 40 && data == &[0xEE; 4]
    ));
    assert!(matches!(
        file.records[4].body,
        RecordBody::FinalLen { final_size: 4097 }
    ));
    assert_eq!(file.records[4].seq, 5);

    let decoded = decode(&encode(&file)).unwrap();
    assert_eq!(file, decoded);
    let mut restored = new_editor(
        Arc::new(SharedBytesProvider::new(Arc::new(pattern_bytes(size)))),
        size,
    );
    apply_patch_file(&mut restored, &decoded).unwrap();
    assert!(restored.materialize() == editor.materialize());
}

#[test]
fn overwrite_past_eof_splits_into_overwrite_and_insert() {
    let size = 16u64;
    let mut editor = new_editor(
        Arc::new(SharedBytesProvider::new(Arc::new(pattern_bytes(size)))),
        size,
    );
    // at=14，old 只有 2 B，new 是 8 B：必须拆成 Overwrite(2) + Insert(6)。
    editor.overwrite(14, b"01234567").unwrap();
    let file = to_patch_file(editor.patches(), size);
    assert!(matches!(
        file.records[0].body,
        RecordBody::Overwrite { at, ref data } if at.get() == 14 && data.len() == 2
    ));
    assert!(matches!(
        file.records[1].body,
        RecordBody::Insert { at, ref data } if at.get() == 16 && data == b"234567"
    ));
    assert!(matches!(
        file.records[2].body,
        RecordBody::FinalLen { final_size: 22 }
    ));

    let decoded = decode(&encode(&file)).unwrap();
    let mut restored = new_editor(
        Arc::new(SharedBytesProvider::new(Arc::new(pattern_bytes(size)))),
        size,
    );
    apply_patch_file(&mut restored, &decoded).unwrap();
    assert!(restored.materialize() == editor.materialize());
}

// ---------------------------------------------------------------------------
// 损坏输入
// ---------------------------------------------------------------------------

fn small_file() -> File {
    let size = 256u64;
    let mut editor = new_editor(
        Arc::new(SharedBytesProvider::new(Arc::new(pattern_bytes(size)))),
        size,
    );
    editor.overwrite(10, b"abcdef").unwrap();
    editor.insert(40, b"XYZ").unwrap();
    editor.remove(80, 4).unwrap();
    to_patch_file(editor.patches(), size)
}

#[test]
fn wrong_final_len_is_eb014() {
    let mut bad = small_file();
    let last = bad.records.last_mut().expect("必须有 FinalLen");
    match &mut last.body {
        RecordBody::FinalLen { final_size } => *final_size += 1,
        other => panic!("最后一条不是 FinalLen：{other:?}"),
    }

    // 通过 encode→decode 证明这是"格式合法但语义错误"的 FinalLen。
    let decoded = decode(&encode(&bad)).unwrap();
    let error = from_patch_file(&decoded).unwrap_err();
    assert_eq!(error.code(), "EB014");
    assert!(matches!(error, ProjectError::FinalLenMismatch { .. }));

    let size = decoded.base_size;
    let mut editor = new_editor(
        Arc::new(SharedBytesProvider::new(Arc::new(pattern_bytes(size)))),
        size,
    );
    assert_eq!(
        apply_patch_file(&mut editor, &decoded).unwrap_err().code(),
        "EB014"
    );
}

#[test]
fn shuffled_seq_is_eb012() {
    let mut bad = small_file();
    bad.records[0].seq = 2;
    bad.records[1].seq = 1;

    let error = from_patch_file(&bad).unwrap_err();
    assert_eq!(error.code(), "EB012");
    assert!(matches!(error, ProjectError::SeqMismatch { .. }));

    // 编解码层也会独立拒绝 seq 不连续（FormatError 同码 EB012）。
    let format_error = decode(&encode(&bad)).unwrap_err();
    assert_eq!(format_error.code, "EB012");
}

#[test]
fn out_of_bounds_record_is_eb013() {
    let mut bad = small_file();
    bad.records[0].body = RecordBody::Overwrite {
        at: Offset::new(u64::MAX - 4),
        data: vec![1, 2, 3, 4],
    };
    let error = from_patch_file(&bad).unwrap_err();
    assert_eq!(error.code(), "EB013");
    assert!(matches!(error, ProjectError::OutOfBounds { .. }));
}

#[test]
fn encode_decode_preserves_all_record_bodies() {
    let mut file = File::new(1024);
    file.records = vec![
        Record::new(
            1,
            RecordBody::Comment {
                text: "注释".into(),
            },
        ),
        Record::new(
            2,
            RecordBody::Checkpoint {
                label: "组".into()
            },
        ),
        Record::new(
            3,
            RecordBody::Unknown {
                kind: 0x77,
                payload: vec![9, 8, 7],
            },
        ),
        Record::new(
            4,
            RecordBody::Overwrite {
                at: Offset::new(10),
                data: b"hello".to_vec(),
            },
        ),
        Record::new(
            5,
            RecordBody::Insert {
                at: Offset::new(15),
                data: b"world".to_vec(),
            },
        ),
        Record::new(
            6,
            RecordBody::Remove {
                at: Offset::new(20),
                len: 2,
            },
        ),
        Record::new(7, RecordBody::FinalLen { final_size: 1027 }),
    ];

    let decoded = decode(&encode(&file)).unwrap();
    assert_eq!(decoded, file);
    assert_eq!(decoded.records, file.records);
}

#[test]
fn from_patch_file_skips_unknown_and_metadata_records() {
    let mut file = small_file();
    file.records.insert(
        0,
        Record::new(
            1,
            RecordBody::Unknown {
                kind: 0x55,
                payload: vec![1],
            },
        ),
    );
    // 重新编号，保持 seq 连续；FinalLen 仍在末尾。
    for (index, record) in file.records.iter_mut().enumerate() {
        record.seq = index as u64 + 1;
    }
    let edits = from_patch_file(&file).unwrap();
    assert_eq!(edits.len(), 3);
    assert!(
        edits
            .iter()
            .all(|edit| !matches!(edit.body, EditBody::Remove(0)))
    );
}

#[test]
fn close_edits_roundtrip_without_merge_bug() {
    // 回归：≤16 字节的相邻插入/删除曾被 patch 归并成非法区间，导致导出后无法重放。
    use geode_buffer::edit::Editor;
    use geode_buffer::piece::PieceTable;
    use geode_provider::MemoryProvider;
    use std::sync::Arc;

    let src: Arc<MemoryProvider> =
        Arc::new(MemoryProvider::new("close", b"0123456789abcdef".to_vec()));
    let mut ed = Editor::new(PieceTable::from_source(16), src);
    ed.overwrite(0, b"AB").unwrap();
    ed.overwrite(2, b"CD").unwrap(); // 紧邻上一次
    ed.insert(4, b"XY").unwrap(); // 插入夹在中间
    ed.remove(6, 2).unwrap();
    ed.overwrite(6, b"ZZ").unwrap();
    let want = ed.materialize();

    let file = to_patch_file(ed.patches(), 16);
    let edits = from_patch_file(&file).expect("导入成功");
    assert_eq!(
        edits.len(),
        file.records.len() - 1,
        "每条记录对应一次编辑（末条是 FinalLen）"
    );

    let src2: Arc<MemoryProvider> =
        Arc::new(MemoryProvider::new("close2", b"0123456789abcdef".to_vec()));
    let mut ed2 = Editor::new(PieceTable::from_source(16), src2);
    apply_patch_file(&mut ed2, &file).expect("应用成功");
    assert_eq!(ed2.materialize(), want, "重放后必须逐字节相等");
}
