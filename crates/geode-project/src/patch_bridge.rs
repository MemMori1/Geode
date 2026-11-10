//! `.geopatch` 与编辑器补丁集之间的桥。
//!
//! DESIGN: 04-buffer-and-edit-model.md §4.5、23-file-formats.md §3.6、
//! `schemas/geopatch/v1.layout.md`。
//!
//! 编辑器的 [`Patch`] 是**编辑时刻的连续操作记录**：`orig.start()` 是编辑发生时
//! 的当前坐标，`old` 是该坐标处被替换的字节，`new` 是替换后的字节。`.geopatch`
//! 的记录同样按顺序应用，因此导出时保持 `Editor::patches()` 的顺序与坐标即可；
//! 把同一个 `File` 重放到同长度的新编辑器，就能逐字节重现原编辑器的结果。
//!
//! 注意：`04 §4.5` 把 `Patch::orig` 描述为"原始坐标"，但 `geode-buffer` 当前实现
//! 直接记录编辑时的当前坐标（`push_patch(at, ..)` 不做 base 坐标换算）。桥接层
//! 以实际实现为准；若将来 buffer 改成真正的原始坐标，此处需要重新引入长度差换算。
//!
//! 线上格式只有等长 `Overwrite` / `Insert` / `Remove` 三种改变内容的记录；
//! `Fill` 规范化为 `Overwrite`，而长度变化的 `Patch`（例如越过 EOF 的覆盖）会
//! 拆成 `Overwrite` + `Insert`（或 `Overwrite` + `Remove`）。

use crate::ProjectError;
use geode_buffer::edit::Patch;
use geode_core::Offset;
use geode_format::patch::{File, Record, RecordBody};
use std::fmt;

/// 从 `.geopatch` 读出的一条待应用编辑。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// 顺序应用坐标：前面记录都已应用后的目标偏移。
    pub at: u64,
    /// 编辑体。
    pub body: EditBody,
}

/// 编辑体；与线上格式的可改变内容记录一一对应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditBody {
    /// 等长覆盖。
    Overwrite(Vec<u8>),
    /// 在 `at` 前插入。
    Insert(Vec<u8>),
    /// 删除 `[at, at + len)`。
    Remove(u64),
}

/// 把编辑器补丁集导出为 `.geopatch` 的内存表示。
///
/// `base_size` 是补丁所基于的原始缓冲区长度（通常是 `Editor::len()` 在编辑前的值）。
/// 返回值已经包含末尾的 [`RecordBody::FinalLen`]；头部由 [`File::new`] 构造，
/// `flags` 保持为 0。
///
/// 记录按 `patches` 的现有顺序输出，`at` 直接取 `Patch::orig.start()`。补丁是
/// 顺序操作日志，不能按 `orig` 重排：插入/删除会改变后续编辑的当前坐标。
#[must_use]
pub fn to_patch_file(patches: &[Patch], base_size: u64) -> File {
    let mut file = File::new(base_size);
    // 最终长度 = base_size + Σ(每条补丁的 new.len - old.len)。
    // 用 i128 避免中间计算在 u64 上回绕。
    let mut delta: i128 = 0;

    for patch in patches {
        let at = patch.orig.start().get();
        let old_len = patch.old.len();
        let new_len = patch.new.len();
        let common = old_len.min(new_len);

        // 通用差分：按 old/new 的公共前缀与尾部差异展开，纯 Insert/Remove
        // （common == 0）会自然落成 Insert/Remove 记录，Fill 与等长覆盖落成
        // Overwrite。不要只按 kind 分支，否则会丢掉 old/new 长度不等时的差异。
        if common > 0 {
            push_body(
                &mut file,
                RecordBody::Overwrite {
                    at: Offset::new(at),
                    data: patch.new[..common].to_vec(),
                },
            );
        }
        if new_len > old_len {
            push_body(
                &mut file,
                RecordBody::Insert {
                    at: Offset::new(at.saturating_add(common as u64)),
                    data: patch.new[common..].to_vec(),
                },
            );
        } else if old_len > new_len {
            push_body(
                &mut file,
                RecordBody::Remove {
                    at: Offset::new(at.saturating_add(common as u64)),
                    len: (old_len - new_len) as u64,
                },
            );
        }
        delta += i128::from(new_len as u64) - i128::from(old_len as u64);
    }

    let final_size = u64::try_from(i128::from(base_size) + delta).unwrap_or(0);
    let seq = file.records.len() as u64 + 1;
    file.records
        .push(Record::new(seq, RecordBody::FinalLen { final_size }));
    file
}

/// 把 `.geopatch` 的内存表示转换为按顺序应用的编辑列表。
///
/// 该函数做完整的前置校验：
/// - `seq` 必须从 1 开始严格 +1（否则 [`ProjectError::SeqMismatch`]，EB012）；
/// - 每条内容记录的区间不得越过当前有效长度（否则 [`ProjectError::OutOfBounds`]，EB013）；
/// - `FinalLen` 必须位于末尾，且等于累计长度（否则 [`ProjectError::FinalLenMismatch`]，EB021）。
///
/// `Unknown` 记录被跳过；`Checkpoint` / `Comment` / `FileMeta` 不产生编辑。
/// `FinalLen` 本身不产生编辑。
pub fn from_patch_file(file: &File) -> Result<Vec<Edit>, ProjectError> {
    let total = file.records.len();
    let mut edits = Vec::new();
    let mut current: i128 = i128::from(file.base_size);
    let mut final_len: Option<u64> = None;

    for (index, record) in file.records.iter().enumerate() {
        let expected_seq = index as u64 + 1;
        if record.seq != expected_seq {
            return Err(ProjectError::SeqMismatch {
                expected: expected_seq,
                actual: record.seq,
            });
        }

        match &record.body {
            RecordBody::Overwrite { at, data } => {
                let at = at.get();
                check_read(current, at, data.len() as u64)?;
                edits.push(Edit {
                    at,
                    body: EditBody::Overwrite(data.clone()),
                });
            }
            RecordBody::Insert { at, data } => {
                let at = at.get();
                // 插入允许在 EOF 处追加，因此边界是 `<= current`。
                if at > as_u64(current) {
                    return Err(ProjectError::OutOfBounds {
                        at,
                        len: data.len() as u64,
                        limit: as_u64(current),
                    });
                }
                edits.push(Edit {
                    at,
                    body: EditBody::Insert(data.clone()),
                });
                current += i128::from(data.len() as u64);
            }
            RecordBody::Remove { at, len } => {
                let at = at.get();
                check_read(current, at, *len)?;
                edits.push(Edit {
                    at,
                    body: EditBody::Remove(*len),
                });
                current -= i128::from(*len);
            }
            RecordBody::FinalLen { final_size } => {
                if index + 1 != total {
                    return Err(ProjectError::FinalLenNotLast { index, total });
                }
                final_len = Some(*final_size);
            }
            // 无内容副作用的记录：跳过，但序号仍然参与连续性校验。
            RecordBody::Checkpoint { .. }
            | RecordBody::Comment { .. }
            | RecordBody::FileMeta { .. }
            | RecordBody::Unknown { .. } => {}
        }
    }

    if let Some(actual) = final_len {
        let expected = as_u64(current);
        if actual != expected {
            return Err(ProjectError::FinalLenMismatch { expected, actual });
        }
    }
    Ok(edits)
}

/// 把 `.geopatch` 依序应用到编辑器。
///
/// 应用前会检查编辑器长度等于 `file.base_size`；`Unknown` 记录跳过，
/// `Checkpoint` / `Comment` / `FileMeta` 忽略。任何格式或越界错误都在修改编辑器前
/// 由 [`from_patch_file`] 报出，因此校验失败不会留下半应用的编辑。
pub fn apply_patch_file(
    editor: &mut geode_buffer::edit::Editor,
    file: &File,
) -> Result<(), ProjectError> {
    let edits = from_patch_file(file)?;
    let actual = editor.len();
    if actual != file.base_size {
        return Err(ProjectError::BaseSizeMismatch {
            expected: file.base_size,
            actual,
        });
    }

    for edit in edits {
        let Edit { at, body } = edit;
        let result = match body {
            EditBody::Overwrite(data) => editor.overwrite(at, &data),
            EditBody::Insert(data) => editor.insert(at, &data),
            EditBody::Remove(len) => editor.remove(at, len),
        };
        result.map_err(|error| ProjectError::EditFailed {
            at,
            detail: error.to_string(),
        })?;
    }
    Ok(())
}

fn push_body(file: &mut File, body: RecordBody) {
    let seq = file.records.len() as u64 + 1;
    file.records.push(Record::new(seq, body));
}

fn as_u64(value: i128) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn check_read(current: i128, at: u64, len: u64) -> Result<(), ProjectError> {
    let limit = as_u64(current);
    let end = u128::from(at) + u128::from(len);
    if end > u128::from(limit) {
        return Err(ProjectError::OutOfBounds { at, len, limit });
    }
    Ok(())
}

impl fmt::Display for EditBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overwrite(data) => write!(f, "覆盖 {} B", data.len()),
            Self::Insert(data) => write!(f, "插入 {} B", data.len()),
            Self::Remove(len) => write!(f, "删除 {len} B"),
        }
    }
}
