//! BinSpec 字节码：定长指令、常量池，以及可缓存的大端编码。
//!
//! DESIGN: 02-dsl-spec-draft.md §7.1（编译到字节码而非 AST 解释）、ADR-0006。
//!
//! 指令**定长 16 字节**（1 字节操作码 + 15 字节操作数），原因有三：
//! 1. `pc` 可以直接当下标用，跳转目标不需要二次解码；
//! 2. 解码时的边界检查只需一次乘法，不会被变长指令的畸形长度拖进分配炸弹；
//! 3. 单步调试（22 §4.5 `step_instr`）取"当前指令 ± 8 条"是一段连续内存。
//!
//! 编码格式是 Geode 私有的，**不承诺兼容任何外部字节码**；只承诺
//! `decode(encode(p)) == p`。

use crate::value::Value;
use std::fmt;

/// 单条指令编码后的固定字节数。
pub const INSTRUCTION_SIZE: usize = 16;

/// 字节码魔数。
const MAGIC: &[u8; 4] = b"GbSP";

/// 当前字节码格式版本。
const FORMAT_VERSION: u16 = 1;

/// 字节序。只影响标量读取，不递归到结构体（02 §3.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Endian {
    /// 小端。
    Little,
    /// 大端。
    Big,
}

impl Endian {
    /// 编码用的字节值。
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Little => 0,
            Self::Big => 1,
        }
    }

    /// 从编码字节还原；非法值返回 `None`。
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Little),
            1 => Some(Self::Big),
            _ => None,
        }
    }
}

/// `str` 的文本编码（02 §3.3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StrEnc {
    /// UTF-8。
    Utf8,
    /// 7 位 ASCII；≥0x80 的字节解码失败。
    Ascii,
    /// ISO-8859-1（逐字节映射到 U+0000..=U+00FF）。
    Latin1,
    /// UTF-16 小端。
    Utf16Le,
    /// UTF-16 大端。
    Utf16Be,
}

impl StrEnc {
    /// 编码用的字节值。
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Utf8 => 0,
            Self::Ascii => 1,
            Self::Latin1 => 2,
            Self::Utf16Le => 3,
            Self::Utf16Be => 4,
        }
    }

    /// 从编码字节还原；非法值返回 `None`。
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Utf8),
            1 => Some(Self::Ascii),
            2 => Some(Self::Latin1),
            3 => Some(Self::Utf16Le),
            4 => Some(Self::Utf16Be),
            _ => None,
        }
    }

    /// 每个码元占用的字节数（UTF-8/ASCII/Latin-1 为 1，UTF-16 为 2）。
    #[must_use]
    pub const fn unit_width(self) -> u8 {
        match self {
            Self::Utf8 | Self::Ascii | Self::Latin1 => 1,
            Self::Utf16Le | Self::Utf16Be => 2,
        }
    }
}

/// 一条定长字节码指令。
///
/// 操作数语义见 `vm` 模块的指令语义表；这里的类型只保证"能无损编码"，
/// 不保证索引合法（索引检查发生在执行期，因为缓存的字节码可能来自旧版编译器）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// 读无符号整数：`width` 为 1/2/3/4/6/8/16 字节。
    LoadU {
        /// 字节宽度。
        width: u8,
        /// 字节序。
        endian: Endian,
    },
    /// 读有符号整数（符号扩展）：`width` 同 [`Op::LoadU`]。
    LoadI {
        /// 字节宽度。
        width: u8,
        /// 字节序。
        endian: Endian,
    },
    /// 读浮点数：`width` 为 2（f16）/4（f32）/8（f64）。
    ///
    /// 与 02 §3.2 一致，浮点同样受字节序影响，因此比任务书里的
    /// `LoadF{width}` 多一个 `endian` 操作数。
    LoadF {
        /// 字节宽度。
        width: u8,
        /// 字节序。
        endian: Endian,
    },
    /// 读定长原始字节。
    LoadBytes {
        /// 字节数。
        len: u32,
    },
    /// 读字符串：读到 `term` 终止字节（含终止字节）或读满 `max` 字节为止。
    LoadStr {
        /// 文本编码。
        enc: StrEnc,
        /// 终止字节；`None` 表示定长 `max` 字节。
        term: Option<u8>,
        /// 最大字节数（含终止字节）。
        max: u32,
    },
    /// 复制栈顶。
    Dup,
    /// 丢弃栈顶。
    Drop,
    /// 交换栈顶两项。
    Swap,
    /// 压入常量池中的常量。
    PushConst(u32),
    /// 无符号/有符号/浮点加法（按操作数类型提升）。
    Add,
    /// 减法。
    Sub,
    /// 乘法。
    Mul,
    /// 除法；整数除零报 E3004。
    Div,
    /// 取余；整数模零报 E3004。
    Mod,
    /// 按位与（布尔为逻辑与）。
    And,
    /// 按位或（布尔为逻辑或）。
    Or,
    /// 按位异或（布尔为逻辑异或）。
    Xor,
    /// 左移。
    Shl,
    /// 右移（有符号数为算术右移）。
    Shr,
    /// 按位取反（布尔为逻辑非）。
    Not,
    /// 取负。
    Neg,
    /// 等于。
    Eq,
    /// 不等于。
    Ne,
    /// 小于。
    Lt,
    /// 小于等于。
    Le,
    /// 大于。
    Gt,
    /// 大于等于。
    Ge,
    /// 无条件跳转到指令下标。
    Jmp(u32),
    /// 栈顶为真时跳转（弹出栈顶）。
    JmpIf(u32),
    /// 栈顶为假时跳转（弹出栈顶）。
    JmpIfNot(u32),
    /// 调用子程序：目标指令下标 + 形参个数。
    ///
    /// 被调方与调用方共享同一操作数栈；`argc` 目前只供调试器显示调用帧，
    /// 由编译器在生成代码时保证栈布局正确。
    Call(u32, u8),
    /// 从子程序返回；调用方与子程序共享同一操作数栈。
    Ret,
    /// 把读游标移到绝对偏移。
    Seek(u64),
    /// 把读游标相对移动。
    Skip(i64),
    /// 开始一个结构体节点；第二操作数是声明定长大小（0 表示未知）。
    MarkStruct {
        /// 结构名在 `Program::names` 中的下标。
        name_idx: u32,
        /// 声明大小提示，用于 `EndStruct` 的 `max(实际消费, 提示)`。
        size_hint: u32,
    },
    /// 结束最近一个结构体。
    EndStruct,
    /// 发射一个字段节点：弹出栈顶作为值（栈空表示"只有声明、没有值"）。
    EmitField {
        /// 字段名在 `Program::names` 中的下标。
        name_idx: u32,
        /// 类型名在 `Program::types` 中的下标。
        type_idx: u32,
    },
    /// 断言：`u32` 是失败文案在常量池中的下标；失败是**软错误**（E1006）。
    Assert(u32),
    /// 正常结束。
    Halt,
}

impl Op {
    /// 操作码字节。
    #[must_use]
    pub const fn opcode(self) -> u8 {
        match self {
            Self::Halt => 0x00,
            Self::LoadU { .. } => 0x01,
            Self::LoadI { .. } => 0x02,
            Self::LoadF { .. } => 0x03,
            Self::LoadBytes { .. } => 0x04,
            Self::LoadStr { .. } => 0x05,
            Self::Dup => 0x06,
            Self::Drop => 0x07,
            Self::Swap => 0x08,
            Self::PushConst(_) => 0x09,
            Self::Add => 0x0A,
            Self::Sub => 0x0B,
            Self::Mul => 0x0C,
            Self::Div => 0x0D,
            Self::Mod => 0x0E,
            Self::And => 0x0F,
            Self::Or => 0x10,
            Self::Xor => 0x11,
            Self::Shl => 0x12,
            Self::Shr => 0x13,
            Self::Not => 0x14,
            Self::Neg => 0x15,
            Self::Eq => 0x16,
            Self::Ne => 0x17,
            Self::Lt => 0x18,
            Self::Le => 0x19,
            Self::Gt => 0x1A,
            Self::Ge => 0x1B,
            Self::Jmp(_) => 0x1C,
            Self::JmpIf(_) => 0x1D,
            Self::JmpIfNot(_) => 0x1E,
            Self::Call(_, _) => 0x1F,
            Self::Ret => 0x20,
            Self::Seek(_) => 0x21,
            Self::Skip(_) => 0x22,
            Self::MarkStruct { .. } => 0x23,
            Self::EndStruct => 0x24,
            Self::EmitField { .. } => 0x25,
            Self::Assert(_) => 0x26,
        }
    }

    /// 把自身写进 16 字节缓冲区（未用到的字节保持 0）。
    fn write_into(self, buf: &mut [u8; INSTRUCTION_SIZE]) {
        *buf = [0u8; INSTRUCTION_SIZE];
        buf[0] = self.opcode();
        match self {
            Self::Halt
            | Self::Dup
            | Self::Drop
            | Self::Swap
            | Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::Mod
            | Self::And
            | Self::Or
            | Self::Xor
            | Self::Shl
            | Self::Shr
            | Self::Not
            | Self::Neg
            | Self::Eq
            | Self::Ne
            | Self::Lt
            | Self::Le
            | Self::Gt
            | Self::Ge
            | Self::Ret
            | Self::EndStruct => {}
            Self::LoadU { width, endian } | Self::LoadI { width, endian } => {
                buf[1] = width;
                buf[2] = endian.code();
            }
            Self::LoadF { width, endian } => {
                buf[1] = width;
                buf[2] = endian.code();
            }
            Self::LoadBytes { len } => buf[1..5].copy_from_slice(&len.to_be_bytes()),
            Self::LoadStr { enc, term, max } => {
                buf[1] = enc.code();
                match term {
                    Some(t) => {
                        buf[2] = 1;
                        buf[3] = t;
                    }
                    None => buf[2] = 0,
                }
                buf[4..8].copy_from_slice(&max.to_be_bytes());
            }
            Self::PushConst(idx) | Self::Jmp(idx) | Self::JmpIf(idx) | Self::JmpIfNot(idx) => {
                buf[1..5].copy_from_slice(&idx.to_be_bytes());
            }
            Self::Call(target, argc) => {
                buf[1..5].copy_from_slice(&target.to_be_bytes());
                buf[5] = argc;
            }
            Self::Seek(at) => buf[1..9].copy_from_slice(&at.to_be_bytes()),
            Self::Skip(delta) => buf[1..9].copy_from_slice(&delta.to_be_bytes()),
            Self::MarkStruct {
                name_idx,
                size_hint,
            } => {
                buf[1..5].copy_from_slice(&name_idx.to_be_bytes());
                buf[5..9].copy_from_slice(&size_hint.to_be_bytes());
            }
            Self::EmitField { name_idx, type_idx } => {
                buf[1..5].copy_from_slice(&name_idx.to_be_bytes());
                buf[5..9].copy_from_slice(&type_idx.to_be_bytes());
            }
            Self::Assert(msg_idx) => buf[1..5].copy_from_slice(&msg_idx.to_be_bytes()),
        }
    }

    /// 从 16 字节缓冲区解出一条指令。
    fn read_from(buf: &[u8; INSTRUCTION_SIZE], at: u32) -> Result<Self, BytecodeError> {
        let opcode = buf[0];
        let u32_at = |offset: usize| {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&buf[offset..offset + 4]);
            u32::from_be_bytes(bytes)
        };
        let u64_at = |offset: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&buf[offset..offset + 8]);
            u64::from_be_bytes(bytes)
        };
        let i64_at = |offset: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&buf[offset..offset + 8]);
            i64::from_be_bytes(bytes)
        };
        let endian =
            || Endian::from_code(buf[2]).ok_or(BytecodeError::Operand { at, what: "endian" });
        let op = match opcode {
            0x00 => Self::Halt,
            0x01 => Self::LoadU {
                width: buf[1],
                endian: endian()?,
            },
            0x02 => Self::LoadI {
                width: buf[1],
                endian: endian()?,
            },
            0x03 => Self::LoadF {
                width: buf[1],
                endian: endian()?,
            },
            0x04 => Self::LoadBytes { len: u32_at(1) },
            0x05 => {
                let enc = StrEnc::from_code(buf[1]).ok_or(BytecodeError::Operand {
                    at,
                    what: "str_enc",
                })?;
                let term = match buf[2] {
                    0 => None,
                    1 => Some(buf[3]),
                    _ => {
                        return Err(BytecodeError::Operand {
                            at,
                            what: "has_term",
                        });
                    }
                };
                Self::LoadStr {
                    enc,
                    term,
                    max: u32_at(4),
                }
            }
            0x06 => Self::Dup,
            0x07 => Self::Drop,
            0x08 => Self::Swap,
            0x09 => Self::PushConst(u32_at(1)),
            0x0A => Self::Add,
            0x0B => Self::Sub,
            0x0C => Self::Mul,
            0x0D => Self::Div,
            0x0E => Self::Mod,
            0x0F => Self::And,
            0x10 => Self::Or,
            0x11 => Self::Xor,
            0x12 => Self::Shl,
            0x13 => Self::Shr,
            0x14 => Self::Not,
            0x15 => Self::Neg,
            0x16 => Self::Eq,
            0x17 => Self::Ne,
            0x18 => Self::Lt,
            0x19 => Self::Le,
            0x1A => Self::Gt,
            0x1B => Self::Ge,
            0x1C => Self::Jmp(u32_at(1)),
            0x1D => Self::JmpIf(u32_at(1)),
            0x1E => Self::JmpIfNot(u32_at(1)),
            0x1F => Self::Call(u32_at(1), buf[5]),
            0x20 => Self::Ret,
            0x21 => Self::Seek(u64_at(1)),
            0x22 => Self::Skip(i64_at(1)),
            0x23 => Self::MarkStruct {
                name_idx: u32_at(1),
                size_hint: u32_at(5),
            },
            0x24 => Self::EndStruct,
            0x25 => Self::EmitField {
                name_idx: u32_at(1),
                type_idx: u32_at(5),
            },
            0x26 => Self::Assert(u32_at(1)),
            _ => return Err(BytecodeError::Opcode { at, byte: opcode }),
        };
        Ok(op)
    }
}

/// 常量池条目。
#[derive(Clone, Debug)]
pub enum Const {
    /// 无符号整数。
    UInt(u128),
    /// 有符号整数。
    Int(i128),
    /// 浮点数。
    Float(f64),
    /// 字符串。
    Str(String),
    /// 原始字节。
    Bytes(Vec<u8>),
}

impl Const {
    /// 转成运行时值。
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::UInt(v) => Value::UInt(*v),
            Self::Int(v) => Value::Int(*v),
            Self::Float(v) => Value::Float(*v),
            Self::Str(v) => Value::Str(v.clone()),
            Self::Bytes(v) => Value::Bytes(v.clone()),
        }
    }
}

/// 浮点按**位**比较：编码往返必须逐位相等，`NaN` 也不该破坏这条性质。
impl PartialEq for Const {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::UInt(a), Self::UInt(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UInt(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Str(v) => f.write_str(v),
            Self::Bytes(v) => write!(f, "<{} bytes>", v.len()),
        }
    }
}

/// 一段可缓存的字节码程序。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    /// 指令流。
    pub code: Vec<Op>,
    /// 常量池。
    pub consts: Vec<Const>,
    /// 字段名/结构名表。
    pub names: Vec<String>,
    /// 类型名表。
    pub types: Vec<String>,
}

impl Program {
    /// 编码成自定义的大端字节码。
    ///
    /// 布局：`magic(4) | version(2) | reserved(2) | code_len(4) | consts_len(4)
    /// | names_len(4) | types_len(4) | code | consts | names | types`。
    /// 各段长度上限都是 `u32`；超限返回 [`BytecodeError::TooLarge`] 而不是截断。
    pub fn encode(&self) -> Result<Vec<u8>, BytecodeError> {
        let code_len = u32::try_from(self.code.len()).map_err(|_| BytecodeError::TooLarge {
            section: "code",
            len: self.code.len(),
        })?;
        let consts_len = u32::try_from(self.consts.len()).map_err(|_| BytecodeError::TooLarge {
            section: "consts",
            len: self.consts.len(),
        })?;
        let names_len = u32::try_from(self.names.len()).map_err(|_| BytecodeError::TooLarge {
            section: "names",
            len: self.names.len(),
        })?;
        let types_len = u32::try_from(self.types.len()).map_err(|_| BytecodeError::TooLarge {
            section: "types",
            len: self.types.len(),
        })?;

        // 粗估容量，避免编码大程序时反复扩容（估小了也只是多几次 realloc）。
        let mut out = Vec::with_capacity(24 + self.code.len() * INSTRUCTION_SIZE);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&code_len.to_be_bytes());
        out.extend_from_slice(&consts_len.to_be_bytes());
        out.extend_from_slice(&names_len.to_be_bytes());
        out.extend_from_slice(&types_len.to_be_bytes());

        let mut buf = [0u8; INSTRUCTION_SIZE];
        for op in &self.code {
            op.write_into(&mut buf);
            out.extend_from_slice(&buf);
        }
        for c in &self.consts {
            encode_const(c, &mut out)?;
        }
        for n in &self.names {
            encode_str(n, &mut out)?;
        }
        for t in &self.types {
            encode_str(t, &mut out)?;
        }
        Ok(out)
    }

    /// 从 [`Program::encode`] 的产物解码。
    pub fn decode(bytes: &[u8]) -> Result<Self, BytecodeError> {
        let mut cur = Cursor::new(bytes);
        if cur.take(4)? != MAGIC {
            return Err(BytecodeError::Magic);
        }
        let version = cur.u16()?;
        if version != FORMAT_VERSION {
            return Err(BytecodeError::Version(version));
        }
        let _reserved = cur.u16()?;
        let code_len = cur.u32()?;
        let consts_len = cur.u32()?;
        let names_len = cur.u32()?;
        let types_len = cur.u32()?;

        // 先做容量合理性检查：恶意头部的 40 亿条指令不能变成 64 GiB 分配。
        let remaining = cur.remaining();
        let code_bytes = usize::try_from(code_len)
            .ok()
            .and_then(|n| n.checked_mul(INSTRUCTION_SIZE))
            .ok_or(BytecodeError::TooLarge {
                section: "code",
                len: code_len as usize,
            })?;
        if code_bytes > remaining {
            return Err(BytecodeError::Truncated {
                at: cur.position(),
                want: code_bytes,
                have: remaining,
            });
        }
        let mut code = Vec::with_capacity(code_len as usize);
        for i in 0..code_len {
            let mut buf = [0u8; INSTRUCTION_SIZE];
            buf.copy_from_slice(cur.take(INSTRUCTION_SIZE)?);
            code.push(Op::read_from(&buf, i)?);
        }

        let mut consts = Vec::with_capacity(bounded_capacity(consts_len, &cur)?);
        for _ in 0..consts_len {
            consts.push(decode_const(&mut cur)?);
        }
        let mut names = Vec::with_capacity(bounded_capacity(names_len, &cur)?);
        for _ in 0..names_len {
            names.push(decode_str(&mut cur)?);
        }
        let mut types = Vec::with_capacity(bounded_capacity(types_len, &cur)?);
        for _ in 0..types_len {
            types.push(decode_str(&mut cur)?);
        }
        Ok(Self {
            code,
            consts,
            names,
            types,
        })
    }
}

/// 编码/解码错误。字节码是缓存产物，坏掉时宁可报错也不猜。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BytecodeError {
    /// 输入在读取某段时提前结束。
    Truncated {
        /// 读取起点。
        at: usize,
        /// 期望字节数。
        want: usize,
        /// 实际剩余字节数。
        have: usize,
    },
    /// 魔数不匹配（不是 Geode 字节码）。
    Magic,
    /// 版本号不认识。
    Version(u16),
    /// 未知操作码。
    Opcode {
        /// 指令下标。
        at: u32,
        /// 原始操作码字节。
        byte: u8,
    },
    /// 操作数取值非法（例如 endian 编码不是 0/1）。
    Operand {
        /// 指令下标（或字节偏移，视解码阶段而定）。
        at: u32,
        /// 哪个操作数。
        what: &'static str,
    },
    /// 字符串不是合法 UTF-8。
    Utf8 {
        /// 字符串起点。
        at: usize,
    },
    /// 某段长度超出 `u32`，无法编码。
    TooLarge {
        /// 段名。
        section: &'static str,
        /// 实际长度。
        len: usize,
    },
}

impl fmt::Display for BytecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { at, want, have } => {
                write!(
                    f,
                    "字节码在 {at} 处截断：需要 {want} 字节，只剩 {have} 字节"
                )
            }
            Self::Magic => f.write_str("字节码魔数不是 GbSP"),
            Self::Version(v) => write!(f, "不支持的字节码版本 {v}"),
            Self::Opcode { at, byte } => write!(f, "指令 {at} 的操作码 0x{byte:02X} 未知"),
            Self::Operand { at, what } => write!(f, "指令 {at} 的操作数 {what} 非法"),
            Self::Utf8 { at } => write!(f, "偏移 {at} 处的字符串不是合法 UTF-8"),
            Self::TooLarge { section, len } => {
                write!(f, "{section} 段长度 {len} 超出 u32 上限，无法编码")
            }
        }
    }
}

impl std::error::Error for BytecodeError {}

fn encode_const(c: &Const, out: &mut Vec<u8>) -> Result<(), BytecodeError> {
    match c {
        Const::UInt(v) => {
            out.push(1);
            out.extend_from_slice(&v.to_be_bytes());
        }
        Const::Int(v) => {
            out.push(2);
            // i128 按补码位模式写出：`as u128` 是位重解释，不改变位。
            out.extend_from_slice(&(*v as u128).to_be_bytes());
        }
        Const::Float(v) => {
            out.push(3);
            out.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        Const::Str(s) => {
            out.push(4);
            encode_str(s, out)?;
        }
        Const::Bytes(b) => {
            out.push(5);
            let len = u32::try_from(b.len()).map_err(|_| BytecodeError::TooLarge {
                section: "const bytes",
                len: b.len(),
            })?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(b);
        }
    }
    Ok(())
}

fn decode_const(cur: &mut Cursor<'_>) -> Result<Const, BytecodeError> {
    match cur.u8()? {
        1 => Ok(Const::UInt(cur.u128()?)),
        2 => Ok(Const::Int(cur.u128()? as i128)),
        3 => Ok(Const::Float(f64::from_bits(cur.u64()?))),
        4 => Ok(Const::Str(decode_str(cur)?)),
        5 => {
            let len = cur.u32()? as usize;
            Ok(Const::Bytes(cur.take(len)?.to_vec()))
        }
        _ => Err(BytecodeError::Operand {
            at: cur.position() as u32,
            what: "const_tag",
        }),
    }
}

fn encode_str(s: &str, out: &mut Vec<u8>) -> Result<(), BytecodeError> {
    let len = u32::try_from(s.len()).map_err(|_| BytecodeError::TooLarge {
        section: "string",
        len: s.len(),
    })?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn decode_str(cur: &mut Cursor<'_>) -> Result<String, BytecodeError> {
    let len = cur.u32()? as usize;
    let at = cur.position();
    let bytes = cur.take(len)?;
    let s = std::str::from_utf8(bytes).map_err(|_| BytecodeError::Utf8 { at })?;
    Ok(s.to_owned())
}

/// 每个条目至少占 1 字节；声明数超过剩余字节数时直接判截断，避免爆分配。
fn bounded_capacity(count: u32, cur: &Cursor<'_>) -> Result<usize, BytecodeError> {
    let count = count as usize;
    let remaining = cur.remaining();
    if count > remaining {
        return Err(BytecodeError::Truncated {
            at: cur.position(),
            want: count,
            have: remaining,
        });
    }
    Ok(count)
}

/// 只读字节游标。所有 `take` 都检查边界，绝不让切片越过输入。
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    const fn position(&self) -> usize {
        self.pos
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, want: usize) -> Result<&'a [u8], BytecodeError> {
        let have = self.remaining();
        if want > have {
            return Err(BytecodeError::Truncated {
                at: self.pos,
                want,
                have,
            });
        }
        let start = self.pos;
        self.pos += want;
        Ok(&self.bytes[start..start + want])
    }

    fn u8(&mut self) -> Result<u8, BytecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, BytecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, BytecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, BytecodeError> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(a))
    }

    fn u128(&mut self) -> Result<u128, BytecodeError> {
        let mut a = [0u8; 16];
        a.copy_from_slice(self.take(16)?);
        Ok(u128::from_be_bytes(a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Program {
        Program {
            code: vec![
                Op::Seek(0),
                Op::LoadU {
                    width: 4,
                    endian: Endian::Big,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Call(5, 1),
                Op::Halt,
                Op::Ret,
            ],
            consts: vec![
                Const::UInt(u128::MAX),
                Const::Int(i128::MIN),
                Const::Float(f64::NAN),
                Const::Str("名字".into()),
                Const::Bytes(vec![0, 1, 2, 255]),
            ],
            names: vec!["magic".into(), String::new()],
            types: vec!["u32".into()],
        }
    }

    #[test]
    fn encode_decode_roundtrip_all_consts() {
        let p = sample();
        let bytes = p.encode().expect("编码不应失败");
        assert_eq!(&bytes[..4], b"GbSP");
        let back = Program::decode(&bytes).expect("解码不应失败");
        assert_eq!(back, p);
    }

    #[test]
    fn every_op_roundtrips() {
        let ops = vec![
            Op::LoadU {
                width: 3,
                endian: Endian::Little,
            },
            Op::LoadI {
                width: 6,
                endian: Endian::Big,
            },
            Op::LoadF {
                width: 8,
                endian: Endian::Big,
            },
            Op::LoadBytes { len: 4096 },
            Op::LoadStr {
                enc: StrEnc::Utf16Be,
                term: Some(0),
                max: 128,
            },
            Op::Dup,
            Op::Drop,
            Op::Swap,
            Op::PushConst(7),
            Op::Add,
            Op::Sub,
            Op::Mul,
            Op::Div,
            Op::Mod,
            Op::And,
            Op::Or,
            Op::Xor,
            Op::Shl,
            Op::Shr,
            Op::Not,
            Op::Neg,
            Op::Eq,
            Op::Ne,
            Op::Lt,
            Op::Le,
            Op::Gt,
            Op::Ge,
            Op::Jmp(99),
            Op::JmpIf(98),
            Op::JmpIfNot(97),
            Op::Call(96, 3),
            Op::Ret,
            Op::Seek(u64::MAX),
            Op::Skip(i64::MIN),
            Op::MarkStruct {
                name_idx: 1,
                size_hint: 12,
            },
            Op::EndStruct,
            Op::EmitField {
                name_idx: 2,
                type_idx: 3,
            },
            Op::Assert(4),
            Op::Halt,
        ];
        let p = Program {
            code: ops.clone(),
            ..Program::default()
        };
        let back = Program::decode(&p.encode().expect("编码不应失败")).expect("解码不应失败");
        assert_eq!(back.code, ops);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut bytes = sample().encode().expect("编码不应失败");
        bytes[0] = b'X';
        assert_eq!(Program::decode(&bytes), Err(BytecodeError::Magic));
    }

    #[test]
    fn decode_rejects_truncated_and_unknown_opcode() {
        assert!(matches!(
            Program::decode(b"GbSP"),
            Err(BytecodeError::Truncated { .. })
        ));

        let mut p = Program {
            code: vec![Op::Halt],
            ..Program::default()
        };
        let mut bytes = p.encode().expect("编码不应失败");
        // 头部 24 字节，第一条指令的操作码在 24。
        bytes[24] = 0x7F;
        assert_eq!(
            Program::decode(&bytes),
            Err(BytecodeError::Opcode { at: 0, byte: 0x7F })
        );

        // 编码是定长的：一条指令 16 字节，头部 24 字节。
        p.code = vec![Op::Dup];
        assert_eq!(p.encode().expect("编码不应失败").len(), 24 + 16);
    }
}
