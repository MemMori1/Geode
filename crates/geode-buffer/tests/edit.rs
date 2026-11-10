//! 编辑会话测试（T1.4 验收）。DESIGN: 04 §4.2、§4.5

use geode_buffer::edit::{EditKind, Editor, PATCH_MERGE_GAP};
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
    for _ in 0..2000 {
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
    assert_ne!(ed.materialize(), original, "2000 次编辑后不应恰好等于原文");
    let undone = ed.undo_all();
    assert_eq!(undone, 2000, "每一步都应可撤销");
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
