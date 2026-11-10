//! 把 [`Editor`] 的当前内容包成一个只读 [`Provider`]。
//!
//! DESIGN: 04 §4.6（保存事务）、17 §4（journal）
//!
//! 保存必须走 `Provider` 抽象（`save_provider_atomic` 只认 `&dyn Provider`），
//! 而编辑后的内容在 `Editor` 里、不在原文件里。这个适配器就是两者之间的桥：
//! `read_at` 读的是"编辑后的视图"，不是磁盘上的旧字节。
//!
//! 它**没有**写能力：保存是"读编辑视图 → 写临时文件 → rename"，
//! 一旦允许往视图里写，就会出现"保存时又改了一次"的二义性。

use crate::edit::Editor;
use geode_core::{ByteRange, Offset, ProviderId};
use geode_provider::{Caps, Provider, ProviderError, ReadReport, SizeInfo};

/// 以只读 `Provider` 的形式暴露 `Editor` 的当前内容。
pub struct EditorSource<'a> {
    editor: &'a Editor,
    id: ProviderId,
    name: String,
}

impl<'a> EditorSource<'a> {
    /// 用默认身份包装。
    pub fn new(editor: &'a Editor) -> Self {
        Self {
            editor,
            id: ProviderId::new(0),
            name: "editor".to_string(),
        }
    }

    /// 指定 id 与显示名（日志里要能区分"保存的是哪个视图"）。
    pub fn with_identity(editor: &'a Editor, id: ProviderId, name: impl Into<String>) -> Self {
        Self {
            editor,
            id,
            name: name.into(),
        }
    }
}

impl std::fmt::Debug for EditorSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorSource")
            .field("id", &self.id)
            .field("len", &self.editor.len())
            .finish()
    }
}

impl Provider for EditorSource<'_> {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn display_name(&self) -> String {
        self.name.clone()
    }

    fn caps(&self) -> Caps {
        Caps {
            random_read: true,
            writable: false,
            sized: true,
            volatile: false,
        }
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.editor.len())
    }

    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        let len = self.editor.len();
        if at.get() >= len {
            return Ok(ReadReport {
                filled: 0,
                holes: 0,
                eof_hit: true,
            });
        }
        let want = buf
            .len()
            .min(usize::try_from(len - at.get()).unwrap_or(usize::MAX));
        let data = self.editor.read(at.get(), want as u64);
        buf[..data.len()].copy_from_slice(&data);
        Ok(ReadReport {
            filled: data.len() as u64,
            holes: 0,
            eof_hit: data.len() < buf.len(),
        })
    }

    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        Some(vec![ByteRange::from_len(Offset::ZERO, self.editor.len())?])
    }
}
