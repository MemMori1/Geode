//! `.geojournal` 的写入与恢复（M1 / T1.8）。
//!
//! DESIGN: `23 §3.8`、`04 §4.6`、`17 §4.6`
//!
//! 本模块只做**字节层**的追加与校验：它不认识 `Buffer`/piece table，回放（把
//! [`Edit`] 应用到文档）由上层完成。布局与帧编码复用 [`geode_format::journal`]，
//! 这里不重复实现 CRC 或端序。
//!
//! ## 写入缓冲与 fsync 策略
//!
//! 写侧用 [`std::io::BufWriter`]（8 KiB）缓冲，默认按 [`SyncPolicy::default`]
//! **每 [`SYNC_EVERY_FRAMES`] 帧或每 [`SYNC_EVERY`] 秒 fsync 一次，二者取先**。
//! 这两个值就是 04 §4.6 / 17 §9.3 的"批量 fsync"口径；`kill -9` 的实测窗口见
//! `docs/spikes/S2-journal-crash-window.md`。
//!
//! ## 读侧四道关卡
//!
//! 1. 格式：magic/fmt_ver（不符 ⇒ `EB002`/`EB001`）；
//! 2. CRC：坏帧 ⇒ **保留前缀**、`partial = true`（`EB011`）；
//! 3. 序号：跳号/重复 ⇒ 同样按首个异常前截断（`EB012`）；
//! 4. 语义：`base_*` 指纹（[`JournalReader::open_with_base`]）与末条 `FinalLen`
//!    必须自洽，否则 `EB021`。
//!
//! 头截断/空文件返回 `EB013`，只有头（80 B）没有记录是合法的 Clean 空日志，
//! 这两种边界都不会 panic。

use geode_core::Offset;
use geode_format::FormatError;
use geode_format::journal::{self, HEADER_LEN};
pub use geode_format::journal::{BaseKind, RecordBody};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 默认按帧触发的 fsync 间隔：每 64 帧。
pub const SYNC_EVERY_FRAMES: u32 = 64;

/// 默认按时间触发的 fsync 间隔：1 秒。
pub const SYNC_EVERY: Duration = Duration::from_secs(1);

/// `.geojournal` 的 80 B 定长头（头无 CRC，见 23 §3.8）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalHeader {
    /// 文档 ULID 的 16 B 二进制形式。
    pub doc_uuid: [u8; 16],
    /// base 来源类型。
    pub base_kind: BaseKind,
    /// base 字节数（关卡 4）。
    pub base_size: u64,
    /// 头部采样长度。
    pub head_len: u32,
    /// base 头部 BLAKE3（32 B 原始摘要，关卡 4 的唯一依据）。
    pub base_head_blake3: [u8; 32],
}

impl JournalHeader {
    /// 用各字段构造头。
    #[must_use]
    pub fn new(
        doc_uuid: [u8; 16],
        base_kind: BaseKind,
        base_size: u64,
        head_len: u32,
        base_head_blake3: [u8; 32],
    ) -> Self {
        Self {
            doc_uuid,
            base_kind,
            base_size,
            head_len,
            base_head_blake3,
        }
    }

    /// 该头是否与磁盘 base 的指纹一致（关卡 4）。
    #[must_use]
    pub fn matches_base(&self, base: &BaseFingerprint) -> bool {
        self.base_size == base.size
            && self.head_len == base.head_len
            && self.base_head_blake3 == base.head_blake3
    }

    /// 把 80 B 头编码为字节。
    fn encode(&self) -> Vec<u8> {
        let file = journal::File {
            fmt_ver: 1,
            flags: 0,
            doc_uuid: self.doc_uuid,
            base_kind: self.base_kind,
            base_size: self.base_size,
            head_len: self.head_len,
            base_head_blake3: self.base_head_blake3,
            records: Vec::new(),
            partial: false,
        };
        journal::encode(&file)
    }

    /// 从解码结果取回头字段。
    fn from_file(file: &journal::File) -> Self {
        Self {
            doc_uuid: file.doc_uuid,
            base_kind: file.base_kind,
            base_size: file.base_size,
            head_len: file.head_len,
            base_head_blake3: file.base_head_blake3,
        }
    }
}

/// 磁盘 base 的指纹，用于关卡 4 的比对。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseFingerprint {
    /// base 字节数。
    pub size: u64,
    /// 头部采样长度（journal 创建时实际采样的字节数）。
    pub head_len: u32,
    /// 头部采样的 BLAKE3。
    pub head_blake3: [u8; 32],
}

impl BaseFingerprint {
    /// 用各字段构造指纹。
    #[must_use]
    pub fn new(size: u64, head_len: u32, head_blake3: [u8; 32]) -> Self {
        Self {
            size,
            head_len,
            head_blake3,
        }
    }
}

/// 一条从 journal 恢复出来的记录。
///
/// `edits` 按盘上顺序保留**所有**记录（含 `Checkpoint`/`SaveMark`/`FinalLen`
/// 控制记录），保证恢复方能看到完整日志；用 [`Edit::is_control`] 过滤。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// 记录序号（从 1 起严格 +1）。
    pub seq: u64,
    /// 记录体。
    pub body: RecordBody,
}

impl Edit {
    /// 该记录的 `kind` 字节。
    #[must_use]
    pub fn kind(&self) -> u8 {
        self.body.kind()
    }

    /// 是否是控制记录（`SaveMark`/`FinalLen`），而不是可回放的编辑。
    #[must_use]
    pub fn is_control(&self) -> bool {
        matches!(
            self.body,
            RecordBody::SaveMark { .. } | RecordBody::FinalLen { .. }
        )
    }
}

/// 恢复结果。
#[derive(Clone, Debug)]
pub struct Recovered {
    /// 80 B 头；调用方用它与磁盘 base 比对（关卡 4）。
    pub header: JournalHeader,
    /// 已成功校验的记录前缀，按盘上顺序。
    pub edits: Vec<Edit>,
    /// 是否只恢复了前缀（CRC/序号/截断异常）。
    pub partial: bool,
    /// 首个异常帧的字节偏移；`None` 表示整份文件完整。
    pub truncated_at: Option<u64>,
    /// 最后一条成功恢复记录的 `seq`；没有记录时为 0。
    pub last_seq: u64,
    /// 首个异常（`partial = true` 时必有）；`None` 表示 Clean。
    pub failure: Option<FormatError>,
    /// 末条 `FinalLen` 声明的回放后长度；没有 `FinalLen` 时为 `None`。
    pub final_len: Option<u64>,
}

impl Recovered {
    /// 是否完整恢复（没有 CRC/序号/截断异常）。
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.partial
    }

    /// 首个异常的错误码（例如 `EB011`/`EB012`/`EB013`）。
    #[must_use]
    pub fn failure_code(&self) -> Option<&'static str> {
        self.failure.as_ref().map(|error| error.code)
    }
}

/// journal 写入/恢复错误。
#[derive(Debug)]
pub enum JournalError {
    /// 文件系统错误。
    Io(std::io::Error),
    /// 格式层错误（`EB001`/`EB002`/`EB011`/`EB012`/`EB013`/`EB014`/`EB021`）。
    Format(FormatError),
    /// writer 已经 `finish`，不能再追加。
    Closed,
}

impl JournalError {
    /// 已登记的错误码；`Io`/`Closed` 没有 `EB` 码。
    #[must_use]
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Format(error) => Some(error.code),
            Self::Io(_) | Self::Closed => None,
        }
    }

    /// 取格式层错误（若有）。
    #[must_use]
    pub fn as_format(&self) -> Option<&FormatError> {
        match self {
            Self::Format(error) => Some(error),
            Self::Io(_) | Self::Closed => None,
        }
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal I/O 错误：{error}"),
            Self::Format(error) => write!(f, "{error}"),
            Self::Closed => write!(f, "journal writer 已 finish，不能继续追加"),
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Format(error) => Some(error),
            Self::Closed => None,
        }
    }
}

impl From<std::io::Error> for JournalError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FormatError> for JournalError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

/// fsync 触发策略。
///
/// `every_frames == 0` 关闭按帧触发；`every == Duration::ZERO` 关闭按时间触发；
/// 两者都关闭时只有显式 [`JournalWriter::sync`] / [`JournalWriter::finish`] 才落盘。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncPolicy {
    /// 每多少帧触发一次 fsync。
    pub every_frames: u32,
    /// 最长多少时间触发一次 fsync。
    pub every: Duration,
}

impl SyncPolicy {
    /// 每帧都 fsync（偏执模式，慢）。
    #[must_use]
    pub fn every_frame() -> Self {
        Self {
            every_frames: 1,
            every: Duration::ZERO,
        }
    }
}

impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            every_frames: SYNC_EVERY_FRAMES,
            every: SYNC_EVERY,
        }
    }
}

/// `.geojournal` 追加写入器。
pub struct JournalWriter {
    out: BufWriter<File>,
    path: PathBuf,
    header: JournalHeader,
    seq: u64,
    frames_since_sync: u32,
    last_sync: Instant,
    policy: SyncPolicy,
    finished: bool,
}

impl JournalWriter {
    /// 创建（或截断重写）journal，写入 80 B 头并 fsync。
    ///
    /// 父目录必须已存在；"保存成功后截断重写"就是再次调用本函数。
    pub fn create(path: impl AsRef<Path>, header: JournalHeader) -> Result<Self, JournalError> {
        Self::create_with_policy(path, header, SyncPolicy::default())
    }

    /// 同 [`JournalWriter::create`]，但指定 fsync 策略。
    pub fn create_with_policy(
        path: impl AsRef<Path>,
        header: JournalHeader,
        policy: SyncPolicy,
    ) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        let mut out = BufWriter::new(file);
        let bytes = header.encode();
        debug_assert_eq!(bytes.len(), HEADER_LEN, "journal 头必须是 80 B");
        out.write_all(&bytes)?;
        out.flush()?;
        out.get_ref().sync_all()?;
        Ok(Self {
            out,
            path,
            header,
            seq: 0,
            frames_since_sync: 0,
            last_sync: Instant::now(),
            policy,
            finished: false,
        })
    }

    /// 追加一帧原始 `kind`/`payload`，返回分配的 `seq`。
    ///
    /// `payload` 必须是该 `kind` 的**完整**线上载荷（例如 Overwrite 的
    /// `at u64 │ len u64 │ data`）。常规编辑请用 [`JournalWriter::append_body`]
    /// 或 `append_*` 便捷方法，避免手写载荷。
    pub fn append(&mut self, kind: u8, payload: &[u8]) -> Result<u64, JournalError> {
        self.append_body(RecordBody::Unknown {
            kind,
            payload: payload.to_vec(),
        })
    }

    /// 追加一条类型化记录，返回分配的 `seq`。
    ///
    /// `FinalLen` 只应由 [`JournalWriter::finish`] 写入；提前写入会让后续记录
    /// 落在 `FinalLen` 之后，读侧会按 `EB014` 判为异常。
    pub fn append_body(&mut self, body: RecordBody) -> Result<u64, JournalError> {
        if self.finished {
            return Err(JournalError::Closed);
        }
        let seq = self.seq.checked_add(1).ok_or_else(|| {
            JournalError::Format(FormatError::new("EB021", "journal seq 溢出 u64"))
        })?;
        let mut file = journal::File {
            fmt_ver: 1,
            flags: 0,
            doc_uuid: self.header.doc_uuid,
            base_kind: self.header.base_kind,
            base_size: self.header.base_size,
            head_len: self.header.head_len,
            base_head_blake3: self.header.base_head_blake3,
            records: Vec::new(),
            partial: false,
        };
        file.records.push(journal::Record::new(seq, body));
        let bytes = journal::encode(&file);
        // `encode` 写的是「80 B 头 + 一帧」，只取帧部分复用同一份帧编码/CRC。
        self.out.write_all(&bytes[HEADER_LEN..])?;
        self.seq = seq;
        self.frames_since_sync = self.frames_since_sync.saturating_add(1);

        let due_frames =
            self.policy.every_frames != 0 && self.frames_since_sync >= self.policy.every_frames;
        let due_time =
            !self.policy.every.is_zero() && self.last_sync.elapsed() >= self.policy.every;
        if due_frames || due_time {
            self.sync_now()?;
        }
        Ok(seq)
    }

    /// 追加等长替换。
    pub fn append_overwrite(&mut self, at: Offset, data: &[u8]) -> Result<u64, JournalError> {
        self.append_body(RecordBody::Overwrite {
            at,
            data: data.to_vec(),
        })
    }

    /// 追加插入。
    pub fn append_insert(&mut self, at: Offset, data: &[u8]) -> Result<u64, JournalError> {
        self.append_body(RecordBody::Insert {
            at,
            data: data.to_vec(),
        })
    }

    /// 追加删除。
    pub fn append_remove(&mut self, at: Offset, len: u64) -> Result<u64, JournalError> {
        self.append_body(RecordBody::Remove { at, len })
    }

    /// 追加撤销分组边界。
    pub fn append_checkpoint(&mut self, label: &str) -> Result<u64, JournalError> {
        self.append_body(RecordBody::Checkpoint {
            label: label.to_owned(),
        })
    }

    /// 立即 flush + fsync，并重置触发计时。
    pub fn sync(&mut self) -> Result<(), JournalError> {
        self.sync_now()
    }

    /// 追加末条 `FinalLen` 并 fsync；之后 writer 关闭。
    pub fn finish(&mut self, final_len: u64) -> Result<(), JournalError> {
        self.append_body(RecordBody::FinalLen { len: final_len })?;
        self.sync_now()?;
        self.finished = true;
        Ok(())
    }

    /// 已写入的最后一帧 `seq`。
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// 创建时使用的头。
    #[must_use]
    pub fn header(&self) -> &JournalHeader {
        &self.header
    }

    /// journal 路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 是否已经 `finish`。
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn sync_now(&mut self) -> Result<(), JournalError> {
        self.out.flush()?;
        self.out.get_ref().sync_all()?;
        self.frames_since_sync = 0;
        self.last_sync = Instant::now();
        Ok(())
    }
}

/// `.geojournal` 读取器。
pub struct JournalReader;

impl JournalReader {
    /// 打开并恢复 journal。
    ///
    /// 头级错误（magic/版本/头截断）返回 `Err`；帧级异常（CRC/序号/截断）
    /// 返回 `Ok(Recovered { partial: true, .. })`，前缀仍可用于恢复。
    /// 关卡 4 的 `FinalLen` 自洽性在这里校验；`base_*` 与磁盘文件的比对需要
    /// 调用方提供指纹，见 [`JournalReader::open_with_base`]。
    pub fn open(path: impl AsRef<Path>) -> Result<Recovered, JournalError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// 打开 journal，并校验头里的 `base_*` 与给定磁盘指纹一致（关卡 4）。
    pub fn open_with_base(
        path: impl AsRef<Path>,
        base: &BaseFingerprint,
    ) -> Result<Recovered, JournalError> {
        let recovered = Self::open(path)?;
        if !recovered.header.matches_base(base) {
            return Err(base_mismatch(&recovered.header, base));
        }
        Ok(recovered)
    }

    /// 从内存字节恢复（测试与导出用）。
    pub fn from_bytes(bytes: &[u8]) -> Result<Recovered, JournalError> {
        let decoded = journal::decode_prefix(bytes)?;
        let file = decoded.file;
        let header = JournalHeader::from_file(&file);
        let edits: Vec<Edit> = file
            .records
            .into_iter()
            .map(|record| Edit {
                seq: record.seq,
                body: record.body,
            })
            .collect();
        let partial = decoded.error.is_some();
        let truncated_at = decoded.error.as_ref().map(|error| {
            error
                .offset
                .map_or(bytes.len() as u64, |offset| offset as u64)
        });
        let last_seq = edits.last().map_or(0, |edit| edit.seq);
        let final_len = validate_final_len(&header, &edits)?;

        Ok(Recovered {
            header,
            edits,
            partial,
            truncated_at,
            last_seq,
            failure: decoded.error,
            final_len,
        })
    }
}

/// 按 replay 语义计算长度，并校验末条 `FinalLen`（关卡 4）。
///
/// 返回 `FinalLen` 声明的长度（没有 `FinalLen` 时为 `None`）。长度不守恒
/// （删除越过 0、加法溢出）与声明不符都返回 `EB021`。
fn validate_final_len(header: &JournalHeader, edits: &[Edit]) -> Result<Option<u64>, JournalError> {
    let mut len = header.base_size;
    let mut declared = None;

    for edit in edits {
        match &edit.body {
            RecordBody::Overwrite { .. } => {}
            RecordBody::Insert { data, .. } => {
                len = len.checked_add(data.len() as u64).ok_or_else(|| {
                    JournalError::Format(FormatError::new(
                        "EB021",
                        format!("回放长度溢出 u64（第 {} 条 Insert）", edit.seq),
                    ))
                })?;
            }
            RecordBody::Remove { len: remove, .. } => {
                len = len.checked_sub(*remove).ok_or_else(|| {
                    JournalError::Format(FormatError::new(
                        "EB021",
                        format!(
                            "回放长度下溢：第 {} 条 Remove 删除 {remove} B，当前仅 {len} B",
                            edit.seq
                        ),
                    ))
                })?;
            }
            RecordBody::Checkpoint { .. } | RecordBody::Unknown { .. } => {}
            RecordBody::SaveMark { new_base_size, .. } => {
                // 保存成功后 journal 会被截断重写；若 SaveMark 仍在流内，说明
                // 它之后记录的是新 base 上的编辑。
                len = *new_base_size;
            }
            RecordBody::FinalLen { len: final_len } => declared = Some(*final_len),
        }
    }

    if let Some(final_len) = declared {
        if final_len != len {
            return Err(JournalError::Format(
                FormatError::new(
                    "EB021",
                    format!("FinalLen 与回放长度不符：声明 {final_len} B，回放得到 {len} B"),
                )
                .with_recovered(edits.len()),
            ));
        }
    }
    Ok(declared)
}

/// 构造关卡 4 的 base 指纹不符错误。
fn base_mismatch(header: &JournalHeader, base: &BaseFingerprint) -> JournalError {
    JournalError::Format(FormatError::new(
        "EB021",
        format!(
            "journal base 指纹不符：头 base_size={} head_len={} blake3={}；\
             磁盘 size={} head_len={} blake3={}。原文件可能已被外部修改，需要强制恢复",
            header.base_size,
            header.head_len,
            hex(&header.base_head_blake3),
            base.size,
            base.head_len,
            hex(&base.head_blake3),
        ),
    ))
}

/// 小写 hex，仅用于错误细节。
fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
