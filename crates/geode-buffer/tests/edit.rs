//! 编辑会话测试（T1.4 验收）。DESIGN: 04 §4.2、§4.5

use geode_buffer::edit::{EditKind, EditOp, Editor, PATCH_MERGE_GAP, SNAPSHOT_MIN_BYTES};
use geode_buffer::piece::PieceTable;
use geode_provider::MemoryProvider;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn editor(bytes: &[u8]) -> Editor {
    let src = Arc::new(MemoryProvider::new("test", bytes.to_vec()));
    let table = PieceTable::from_bytes(bytes);
    Editor::new(table, src)
}

fn timed_editor(bytes: &[u8]) -> (Editor, Arc<AtomicU64>) {
    let mut ed = editor(bytes);
    let t = Arc::new(AtomicU64::new(0));
    let base = Instant::now();
    let clock = {
        let t = Arc::clone(&t);
        move || base + Duration::from_millis(t.load(Ordering::SeqCst))
    };
    ed.set_clock(clock);
    (ed, t)
}

#[test]
fn overwrite_then_undo_restores() {
    let mut ed = editor(b"ABCDEF");
    ed.overwrite(1, b"xy").unwrap();
    assert_eq!(ed.materialize(), b"AxyDEF");
    assert_eq!(ed.undo_depth(), 1);
    assert!(ed.undo());
    assert_eq!(ed.materialize(), b"ABCDEF");
}

#[test]
fn insert_extends_and_undo_removes() {
    let mut ed = editor(b"AB");
    ed.insert(2, b"CD").unwrap();
    assert_eq!(ed.materialize(), b"ABCD");
    assert!(ed.undo());
    assert_eq!(ed.materialize(), b"AB");
}

#[test]
fn remove_then_undo_restores_bytes() {
    let mut ed = editor(b"0123456789");
    ed.remove(2, 3).unwrap();
    assert_eq!(ed.materialize(), b"0156789");
    assert!(ed.undo());
    assert_eq!(ed.materialize(), b"0123456789");
}

#[test]
fn fill_is_explicit_and_marks_kind() {
    let mut ed = editor(b"AAAA");
    ed.fill(1, 2, 0).unwrap();
    assert_eq!(ed.materialize(), b"A\0\0A");
    assert_eq!(ed.patches().last().unwrap().kind, EditKind::Fill);
}

#[test]
fn nibble_within_window_is_one_undo_step() {
    let (mut ed, t) = timed_editor(b"\x00");
    ed.nibble(0, true, 0x4).unwrap();
    t.fetch_add(100, Ordering::SeqCst); // 100 ms 后低半字节
    ed.nibble(0, false, 0x1).unwrap();
    assert_eq!(ed.materialize(), b"\x41");
    assert_eq!(ed.undo_depth(), 1, "400 ms 内应合并为一步");
    assert!(ed.undo());
    assert_eq!(ed.materialize(), b"\x00");
}

#[test]
fn nibble_after_window_is_two_steps() {
    let (mut ed, t) = timed_editor(b"\x00");
    ed.nibble(0, true, 0x4).unwrap();
    t.fetch_add(900, Ordering::SeqCst);
    ed.nibble(0, false, 0x1).unwrap();
    assert_eq!(ed.undo_depth(), 2);
}

#[test]
fn patches_merge_within_gap() {
    let mut ed = editor(&[0u8; 64]);
    ed.overwrite(0, &[1]).unwrap();
    ed.overwrite(8, &[2]).unwrap(); // gap 7 ≤ 16
    assert_eq!(ed.patches().len(), 1);
    assert_eq!(ed.patches()[0].old.len(), 9);
    ed.overwrite(9 + PATCH_MERGE_GAP + 1, &[3]).unwrap();
    assert_eq!(ed.patches().len(), 2, "超过归并间隙应新开 patch");
}

#[test]
fn random_edits_then_undo_all_restores_original() {
    let original: Vec<u8> = (0u32..512).map(|i| (i % 251) as u8).collect();
    let mut ed = editor(&original);
    let mut seed = 0x1234_5678_9ABC_DEF0u64;
    let mut next = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    for _ in 0..10_000 {
        let len = ed.len();
        match next(3) {
            0 => {
                let at = next(len.max(1));
                let n = 1 + next(8);
                ed.overwrite(at, &vec![0xAB; n as usize]).unwrap();
            }
            1 => {
                let at = next(len + 1);
                let n = 1 + next(8);
                ed.insert(at, &vec![0xCD; n as usize]).unwrap();
            }
            _ => {
                if len > 4 {
                    let at = next(len - 3);
                    let n = 1 + next(3.min(len - at));
                    ed.remove(at, n).unwrap();
                }
            }
        }
    }
    assert_ne!(ed.materialize(), original, "10000 次编辑后不应恰好等于原文");
    let undone = ed.undo_all();
    assert_eq!(undone, 10_000, "每一步都应可撤销");
    assert_eq!(ed.materialize(), original, "全部撤销后必须逐字节相等");
}

#[test]
fn overwrite_past_eof_appends() {
    let mut ed = editor(b"AB");
    ed.overwrite(2, b"CD").unwrap();
    assert_eq!(ed.materialize(), b"ABCD");
    assert!(ed.undo());
    assert_eq!(ed.materialize(), b"AB");
}

#[test]
fn out_of_range_does_not_panic() {
    let mut ed = editor(b"AB");
    assert!(ed.overwrite(99, b"x").is_err());
    assert!(ed.insert(99, b"x").is_err());
    assert!(ed.remove(1, 99).is_err());
    assert_eq!(ed.materialize(), b"AB");
}

#[test]
fn undo_budget_drops_oldest_steps() {
    let mut ed = editor(&[0u8; 4096]);
    ed.set_undo_limit(64);
    for i in 0..20u8 {
        ed.overwrite(u64::from(i) * 8, &[i; 8]).unwrap();
    }
    assert!(ed.dropped_steps() > 0, "超出预算必须丢最早的步");
    assert!(ed.undo_bytes() <= ed.undo_limit());
    assert!(ed.undo_depth() >= 1, "至少保留最近一步，否则撤销键变摆设");
}

#[test]
fn default_undo_limit_matches_spec() {
    use geode_buffer::edit::default_undo_limit;
    assert_eq!(default_undo_limit(0), 64 << 20);
    assert_eq!(default_undo_limit(100 << 20), 64 << 20);
    assert_eq!(default_undo_limit(30 << 30), 512 << 20);
}

#[test]
fn close_inserts_do_not_merge_into_invalid_patch() {
    // 插入会改变后续坐标；归并两个相距 ≤16 的插入会拼出一个"覆盖"语义的
    // 假 patch（04 §4.5 要求 patch 是可顺序重放的记录，不是排序后的 diff）。
    let mut ed = editor(b"abcdef");
    ed.insert(1, b"X").unwrap();
    ed.insert(2, b"Y").unwrap();

    let patches = ed.patches();
    assert_eq!(patches.len(), 2, "相邻插入必须各自成一条记录");
    assert!(patches.iter().all(|p| p.kind == EditKind::Insert));
    assert_eq!(patches[0].orig.len(), 0);
    assert_eq!(patches[1].orig.len(), 0);
    assert_eq!(ed.materialize(), b"aXYbcdef".to_vec());
}

#[test]
fn overlapping_overwrites_do_not_merge() {
    let mut ed = editor(b"0123456789");
    ed.overwrite(4, b"AB").unwrap();
    ed.overwrite(4, b"CD").unwrap(); // 同一位置覆盖，不得与前一条拼成重复字节

    let patches = ed.patches();
    assert_eq!(patches.len(), 2);
    assert_eq!(ed.materialize(), b"0123CD6789".to_vec());
}

#[test]
fn edit_op_bytes_roundtrip_all_variants() {
    let ops = [
        EditOp::Overwrite {
            at: 0x0102_0304_0506_0708,
            data: b"hello".to_vec(),
        },
        EditOp::Insert {
            at: 42,
            data: Vec::new(),
        },
        EditOp::Remove {
            at: 7,
            len: 0xFFFF_FFFF,
        },
        EditOp::Fill {
            at: 3,
            data: vec![0xAA; 9],
        },
    ];
    for op in &ops {
        let bytes = op.to_bytes();
        let back = EditOp::from_bytes(&bytes).expect("自己编的字节必然可解码");
        assert_eq!(&back, op, "from_bytes(to_bytes(x)) 必须等于 x");
        assert_eq!(
            op.try_to_bytes().expect("普通操作必然可编码"),
            bytes,
            "to_bytes 与 try_to_bytes 必须一致"
        );
    }
}

#[test]
fn edit_op_layout_matches_journal_kinds() {
    // docs/23 §3.8：kind u8 │ at u64 │ len u32 │ data[len]，全部大端。
    let op = EditOp::Overwrite {
        at: 0x0102_0304_0506_0708,
        data: b"abc".to_vec(),
    };
    let bytes = op.to_bytes();
    assert_eq!(
        bytes,
        vec![
            0x01, // Overwrite
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // at
            0x00, 0x00, 0x00, 0x03, // len
            b'a', b'b', b'c',
        ]
    );
    assert_eq!(EditOp::Remove { at: 1, len: 2 }.to_bytes()[0], 0x03);
    assert_eq!(
        EditOp::Insert {
            at: 0,
            data: vec![1]
        }
        .to_bytes()[0],
        0x02
    );
    assert_eq!(
        EditOp::Fill {
            at: 0,
            data: vec![1]
        }
        .to_bytes()[0],
        0x07,
        "journal 无 Fill kind，使用向后兼容的扩展 tag"
    );
}

#[test]
fn edit_op_from_bytes_rejects_corruption_without_panic() {
    use geode_buffer::edit::EditOpError;
    use std::panic::catch_unwind;

    let header = |tag: u8, at: u64, len: u32| {
        let mut v = vec![tag];
        v.extend_from_slice(&at.to_be_bytes());
        v.extend_from_slice(&len.to_be_bytes());
        v
    };

    // 空输入 / 头部截断。
    assert!(matches!(
        EditOp::from_bytes(&[]),
        Err(EditOpError::Truncated { need: 13, got: 0 })
    ));
    assert!(matches!(
        EditOp::from_bytes(&header(0x01, 0, 0)[..5]),
        Err(EditOpError::Truncated { .. })
    ));
    // 声明了数据长度但数据不足。
    assert!(matches!(
        EditOp::from_bytes(&header(0x01, 0, 10)),
        Err(EditOpError::Truncated { .. })
    ));
    // 未知 tag（0x04 在 journal 里是 Checkpoint，不是编辑操作）。
    assert!(matches!(
        EditOp::from_bytes(&header(0x04, 0, 0)),
        Err(EditOpError::UnknownTag(0x04))
    ));
    // 尾随字节：Overwrite 与 Remove 都不接受。
    let mut trailing = header(0x01, 0, 1);
    trailing.extend_from_slice(b"xy");
    assert!(matches!(
        EditOp::from_bytes(&trailing),
        Err(EditOpError::TrailingBytes { extra: 1 })
    ));
    let mut remove_trailing = header(0x03, 0, 0);
    remove_trailing.push(0xFF);
    assert!(matches!(
        EditOp::from_bytes(&remove_trailing),
        Err(EditOpError::TrailingBytes { extra: 1 })
    ));
    // 任何字节串都不许 panic。
    for len in 0..40usize {
        let fuzz: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
        let r = catch_unwind(|| EditOp::from_bytes(&fuzz));
        assert!(r.is_ok(), "from_bytes 在长度 {len} 的输入上 panic 了");
    }
}

#[test]
fn replay_applies_ops_in_order_and_is_undoable() {
    let ops = vec![
        EditOp::Overwrite {
            at: 1,
            data: b"XY".to_vec(),
        },
        EditOp::Insert {
            at: 4,
            data: b"!!".to_vec(),
        },
        EditOp::Remove { at: 0, len: 2 },
        EditOp::Fill {
            at: 1,
            data: vec![b'z'; 3],
        },
    ];
    let mut ed = editor(b"abcdef");
    ed.replay(&ops).unwrap();
    assert_eq!(ed.materialize(), b"Yzzzef".to_vec());
    assert_eq!(ed.undo_all(), ops.len());
    assert_eq!(ed.materialize(), b"abcdef".to_vec());

    // 中途越界：前一步保留，后一步返回 Err，不 panic。
    let mut ed = editor(b"AB");
    let err = ed.replay(&[
        EditOp::Overwrite {
            at: 0,
            data: b"X".to_vec(),
        },
        EditOp::Remove { at: 99, len: 1 },
    ]);
    assert!(err.is_err());
    assert_eq!(ed.materialize(), b"XB");
}

#[test]
fn large_remove_uses_cow_snapshot_and_undo_restores_exactly() {
    assert_eq!(SNAPSHOT_MIN_BYTES, 1 << 20, "阈值必须是 1 MiB");
    let original: Vec<u8> = (0u32..(2 << 20)).map(|i| (i % 251) as u8).collect();
    let mut ed = editor(&original);

    ed.remove(0, 2 << 20).unwrap();
    assert!(ed.is_empty());
    assert_eq!(ed.undo_snapshot_count(), 1, "2 MiB 删除必须走快照路径");
    assert!(
        ed.undo_bytes() >= 2 << 20,
        "快照必须计入 undo_bytes 预算（实际 {}）",
        ed.undo_bytes()
    );

    assert!(ed.undo());
    assert_eq!(ed.undo_snapshot_count(), 0);
    assert_eq!(ed.materialize(), original, "撤销快照后必须逐字节相等");
}

#[test]
fn small_edits_do_not_take_snapshots() {
    let mut ed = editor(b"abcdef");
    ed.overwrite(0, b"X").unwrap();
    ed.insert(1, b"Y").unwrap();
    ed.remove(0, 1).unwrap();
    assert_eq!(ed.undo_snapshot_count(), 0, "小编辑仍是逐字节逆操作");
    assert_eq!(ed.undo_all(), 3);
    assert_eq!(ed.materialize(), b"abcdef".to_vec());
}

#[test]
fn large_overwrite_uses_snapshot_too() {
    let original = vec![0u8; (1 << 20) + 4];
    let mut ed = editor(&original);
    ed.overwrite(0, &vec![0x5A; 1 << 20]).unwrap();
    assert_eq!(ed.undo_snapshot_count(), 1, "1 MiB 覆盖也必须走快照");
    assert!(ed.undo());
    assert_eq!(ed.materialize(), original);
}

#[test]
fn large_remove_on_source_table_uses_snapshot() {
    let original: Vec<u8> = (0u32..(2 << 20)).map(|i| (i % 251) as u8).collect();
    let src = Arc::new(MemoryProvider::new("test", original.clone()));
    let table = PieceTable::from_source(original.len() as u64);
    let mut ed = Editor::new(table, src);
    ed.remove(0, 2 << 20).unwrap();
    assert_eq!(ed.undo_snapshot_count(), 1, "源表大删除也必须走快照");
    assert!(ed.undo());
    assert_eq!(ed.materialize(), original, "源表快照撤销后逐字节相等");
}
