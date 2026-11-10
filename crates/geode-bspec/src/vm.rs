//! BinSpec 字节码虚拟机。
//!
//! DESIGN: 02-dsl-spec-draft.md §7（执行模型）、§8.2/§8.3（错误码与恢复策略）。
//!
//! 为什么是字节码而不是 AST 解释（§7.1）：
//! 1. 指令条数就是天然的预算单位——`Budget::steps` 直接映射到 `pc` 前进次数；
//! 2. 每执行一条指令检查一次取消标志，取消延迟与指令粒度同阶（<1µs）；
//! 3. 操作数栈深由 `Budget::depth` 硬性限制，不会出现递归爆栈；
//! 4. 单步调试（22 §4.5 的 `step_instr`）只需保存 `pc`/游标/栈，代价极低。
//!
//! **软失败**是这一层的核心语义（02 §8.3）：字段越界、断言失败、文本解码失败
//! 都只产出诊断并继续，只有预算耗尽、非法字节码、栈下溢这类问题才 `Err`。

use crate::bytecode::{Const, Endian, Op, Program, StrEnc};
use crate::node::Node;
use crate::value::Value;
use geode_core::budget::Budget;
use geode_core::diag::Diag;
use geode_core::error_codes;
use geode_core::offset::Offset;
use geode_core::range::ByteRange;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// 一条指令执行后的去向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// 还有指令可执行。
    Continue,
    /// 已停机（`Halt` 或 `pc` 越过指令流末尾）。
    Halted,
}

/// 一次成功执行的产物。
#[derive(Clone, Debug)]
pub struct RunOutput {
    /// 结构树根节点（名字固定为 `root`）。
    pub root: Node,
    /// 执行期产生的诊断（软失败与警告）。
    pub diags: Vec<Diag>,
    /// 实际执行的指令条数。
    pub steps: u64,
    /// 从输入中读取的字节数（`Seek`/`Skip` 不计）。
    pub bytes_consumed: u64,
}

/// 执行期错误。每个变体都映射到 `errors.toml` 里的一个登记码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmError {
    /// 读取越过数据末尾（E3001）。
    OutOfBounds {
        /// 读取起点。
        at: u64,
        /// 期望字节数。
        want: u64,
        /// 实际剩余字节数。
        have: u64,
    },
    /// 整数除零/模零（E3004）。
    DivideByZero {
        /// 发生除法的读游标位置。
        at: u64,
    },
    /// 栈上值总量超过 `Budget::mem_vars`（E3003）。
    AllocLimit {
        /// 本次分配后需要的字节数。
        want: u64,
        /// 预算上限。
        limit: u64,
    },
    /// 产出节点总量超过 `Budget::output_bytes`（E2004）。
    OutputLimit {
        /// 已用字节数。
        used: u64,
        /// 预算上限。
        limit: u64,
    },
    /// 步数预算耗尽（E2001）。
    StepsExhausted {
        /// 步数上限。
        limit: u64,
    },
    /// 墙钟预算耗尽（E2002）。
    WallTimeExhausted {
        /// 毫秒上限。
        limit_ms: u64,
    },
    /// 嵌套深度超限（E2003）。
    DepthExceeded {
        /// 深度上限。
        limit: u32,
    },
    /// 用户取消（E2005）。
    Cancelled,
    /// 栈下溢：指令需要操作数但栈为空（E4001，字节码非法）。
    StackUnderflow,
    /// 操作数非法（宽度、移位量等）（E4001）。
    BadOperand {
        /// 细节。
        detail: &'static str,
    },
    /// 跳转目标越界（E4001）。
    BadJump {
        /// 目标指令下标。
        target: u32,
    },
    /// 常量/名字/类型索引越界（E4003，未定义符号）。
    BadIndex {
        /// 哪张表。
        table: &'static str,
        /// 索引。
        index: u32,
        /// 表长度。
        len: usize,
    },
    /// 类型不匹配（E4002）。
    TypeMismatch {
        /// 细节。
        detail: String,
    },
    /// 字节码结构非法（例如 `Halt` 时仍有未闭合的结构体）（E4001）。
    Malformed {
        /// 细节。
        detail: &'static str,
    },
}

impl VmError {
    /// 对应的登记错误码。
    #[must_use]
    pub fn code(&self) -> geode_core::diag::Code {
        match self {
            Self::OutOfBounds { .. } => error_codes::E3001,
            Self::DivideByZero { .. } => error_codes::E3004,
            Self::AllocLimit { .. } => error_codes::E3003,
            Self::OutputLimit { .. } => error_codes::E2004,
            Self::StepsExhausted { .. } => error_codes::E2001,
            Self::WallTimeExhausted { .. } => error_codes::E2002,
            Self::DepthExceeded { .. } => error_codes::E2003,
            Self::Cancelled => error_codes::E2005,
            Self::TypeMismatch { .. } => error_codes::E4002,
            Self::BadIndex { .. } => error_codes::E4003,
            Self::StackUnderflow | Self::BadOperand { .. } | Self::BadJump { .. } => {
                error_codes::E4001
            }
            Self::Malformed { .. } => error_codes::E4001,
        }
    }

    /// 转成一条不可恢复诊断（致命错误才走到这里）。
    #[must_use]
    pub fn to_diag(&self) -> Diag {
        Diag::error(self.code(), self.to_string()).fatal()
    }
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { at, want, have } => {
                write!(
                    f,
                    "读取越界：偏移 {at} 起需要 {want} 字节，只剩 {have} 字节"
                )
            }
            Self::DivideByZero { at } => write!(f, "偏移 {at} 处表达式除以零"),
            Self::AllocLimit { want, limit } => {
                write!(f, "栈上值分配超限：需要 {want} 字节，上限 {limit}")
            }
            Self::OutputLimit { used, limit } => {
                write!(f, "解析产物超限：已用 {used} 字节，上限 {limit}")
            }
            Self::StepsExhausted { limit } => write!(f, "执行步数预算耗尽（上限 {limit} 步）"),
            Self::WallTimeExhausted { limit_ms } => {
                write!(f, "墙钟时间预算耗尽（上限 {limit_ms} ms）")
            }
            Self::DepthExceeded { limit } => write!(f, "嵌套/栈深度超限（上限 {limit}）"),
            Self::Cancelled => f.write_str("解析已取消"),
            Self::StackUnderflow => f.write_str("操作数栈下溢"),
            Self::BadOperand { detail } => write!(f, "非法操作数：{detail}"),
            Self::BadJump { target } => write!(f, "跳转目标 {target} 越界"),
            Self::BadIndex { table, index, len } => {
                write!(f, "{table} 表索引 {index} 越界（长度 {len}）")
            }
            Self::TypeMismatch { detail } => write!(f, "类型不匹配：{detail}"),
            Self::Malformed { detail } => write!(f, "字节码结构非法：{detail}"),
        }
    }
}

impl std::error::Error for VmError {}

/// 栈上的一项：值 + 它在输入中的来源区间。
///
/// `span` 让 `EmitField` 不必猜字段覆盖了哪些字节——读指令天然知道。
#[derive(Clone, Debug)]
struct Slot {
    value: Value,
    span: Option<Span>,
}

/// 值在输入中的字节区间。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    offset: u64,
    size: u64,
}

impl Slot {
    /// 该值占用多少"变量内存"（标量按 8 字节计）。
    fn mem_bytes(&self) -> u64 {
        match &self.value {
            Value::Str(s) => s.len() as u64,
            Value::Bytes(b) => b.len() as u64,
            Value::None => 0,
            _ => 8,
        }
    }
}

/// 一个未闭合的结构体。
#[derive(Debug)]
struct StructFrame {
    name: String,
    offset: u64,
    size_hint: u64,
    children: Vec<Node>,
}

/// BinSpec 字节码虚拟机。
pub struct Vm<'a> {
    /// 被执行的程序。
    pub program: &'a Program,
    /// 输入字节。
    pub data: &'a [u8],
    /// 本次执行的预算。
    pub budget: Budget,
    /// 当前指令下标。
    pc: usize,
    /// 读游标（由 `Seek`/`Skip`/读指令共同维护）。
    cursor: u64,
    /// 操作数栈。
    stack: Vec<Slot>,
    /// 未闭合的结构体栈。
    frames: Vec<StructFrame>,
    /// 返回地址栈（`Call`/`Ret`）。
    calls: Vec<u32>,
    /// 顶层节点（不在任何结构体内发射的字段）。
    top: Vec<Node>,
    /// 执行期诊断。
    diags: Vec<Diag>,
    /// 已执行步数。
    steps: u64,
    /// 已读字节数。
    bytes_read: u64,
    /// 栈上值占用的字节数。
    mem_used: u64,
    /// 已产出节点的估算字节数。
    out_used: u64,
    /// 起始时刻，用于墙钟预算。
    started: Instant,
    /// 是否已停机。
    halted: bool,
    /// 取消标志。
    cancel: Option<&'a AtomicBool>,
}

impl<'a> Vm<'a> {
    /// 用默认预算构造。
    #[must_use]
    pub fn new(program: &'a Program, data: &'a [u8]) -> Self {
        Self {
            program,
            data,
            budget: Budget::default(),
            pc: 0,
            cursor: 0,
            stack: Vec::new(),
            frames: Vec::new(),
            calls: Vec::new(),
            top: Vec::new(),
            diags: Vec::new(),
            steps: 0,
            bytes_read: 0,
            mem_used: 0,
            out_used: 0,
            started: Instant::now(),
            halted: false,
            cancel: None,
        }
    }

    /// 换一份预算。
    #[must_use]
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// 挂上取消标志（每执行一条指令检查一次）。
    #[must_use]
    pub fn with_cancel(mut self, flag: &'a AtomicBool) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// 当前指令下标（调试器用）。
    #[must_use]
    pub fn pc(&self) -> usize {
        self.pc
    }

    /// 当前读游标（调试器用）。
    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// 已执行的步数。
    #[must_use]
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// 执行到停机或出错。
    ///
    /// 致命错误（越界读、除零、预算耗尽……）返回 `Err`；软失败只进 `diags`。
    pub fn run(&mut self) -> Result<RunOutput, VmError> {
        loop {
            if self.step()? == StepOutcome::Halted {
                break;
            }
        }
        self.finish()
    }

    /// 执行一条指令。这是调试器单步（22 §4.5）的入口。
    pub fn step(&mut self) -> Result<StepOutcome, VmError> {
        if self.halted {
            return Ok(StepOutcome::Halted);
        }
        let Some(op) = self.program.code.get(self.pc).copied() else {
            // 指令流跑完视同 `Halt`：允许编译器省略末尾的 `Halt`。
            self.halted = true;
            return Ok(StepOutcome::Halted);
        };
        self.charge_step()?;
        self.pc = self.pc.saturating_add(1);
        self.execute(op)?;
        Ok(if self.halted {
            StepOutcome::Halted
        } else {
            StepOutcome::Continue
        })
    }

    /// 预算计费：步数、墙钟、取消。
    fn charge_step(&mut self) -> Result<(), VmError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.budget.steps {
            return Err(VmError::StepsExhausted {
                limit: self.budget.steps,
            });
        }
        if let Some(flag) = self.cancel
            && flag.load(Ordering::Relaxed)
        {
            return Err(VmError::Cancelled);
        }
        // 墙钟只在每 4096 步采样一次：`Instant::elapsed` 比一条指令贵得多。
        if self.steps == 1 || self.steps % 4096 == 0 {
            let elapsed_ms = self.started.elapsed().as_millis() as u64;
            if elapsed_ms > self.budget.wall_time_ms {
                return Err(VmError::WallTimeExhausted {
                    limit_ms: self.budget.wall_time_ms,
                });
            }
        }
        Ok(())
    }

    /// 长循环内的取消与墙钟检查。
    ///
    /// 一条 `LoadStr` 可能逐字节扫描几十 MB，若只在指令边界检查，
    /// 取消延迟会远超 §7.1 的 1µs 目标；这里每 64 KiB 补一次检查。
    fn check_interrupt(&self) -> Result<(), VmError> {
        if let Some(flag) = self.cancel
            && flag.load(Ordering::Relaxed)
        {
            return Err(VmError::Cancelled);
        }
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        if elapsed_ms > self.budget.wall_time_ms {
            return Err(VmError::WallTimeExhausted {
                limit_ms: self.budget.wall_time_ms,
            });
        }
        Ok(())
    }

    /// 执行一条已取出的指令。
    fn execute(&mut self, op: Op) -> Result<(), VmError> {
        match op {
            Op::Halt => self.halted = true,
            Op::LoadU { width, endian } => {
                let at = self.cursor;
                let width = self.check_int_width(width)?;
                let raw = self.read_bytes(u64::from(width))?;
                let value = u128_from_bytes(raw, endian);
                self.push_slot(Slot {
                    value: Value::UInt(value),
                    span: Some(Span {
                        offset: at,
                        size: u64::from(width),
                    }),
                })?;
            }
            Op::LoadI { width, endian } => {
                let at = self.cursor;
                let width = self.check_int_width(width)?;
                let raw = self.read_bytes(u64::from(width))?;
                let unsigned = u128_from_bytes(raw, endian);
                let bits = u32::from(width) * 8;
                let value = if bits == 128 {
                    unsigned as i128
                } else {
                    let shift = 128 - bits;
                    ((unsigned << shift) as i128) >> shift
                };
                self.push_slot(Slot {
                    value: Value::Int(value),
                    span: Some(Span {
                        offset: at,
                        size: u64::from(width),
                    }),
                })?;
            }
            Op::LoadF { width, endian } => {
                let at = self.cursor;
                let value = match width {
                    2 => {
                        let raw = self.read_bytes(2)?;
                        let bits = match endian {
                            Endian::Big => u16::from_be_bytes([raw[0], raw[1]]),
                            Endian::Little => u16::from_le_bytes([raw[0], raw[1]]),
                        };
                        f16_to_f64(bits)
                    }
                    4 => {
                        let raw = self.read_bytes(4)?;
                        let bytes = [raw[0], raw[1], raw[2], raw[3]];
                        let bits = match endian {
                            Endian::Big => u32::from_be_bytes(bytes),
                            Endian::Little => u32::from_le_bytes(bytes),
                        };
                        f64::from(f32::from_bits(bits))
                    }
                    8 => {
                        let raw = self.read_bytes(8)?;
                        let mut bytes = [0u8; 8];
                        bytes.copy_from_slice(raw);
                        let bits = match endian {
                            Endian::Big => u64::from_be_bytes(bytes),
                            Endian::Little => u64::from_le_bytes(bytes),
                        };
                        f64::from_bits(bits)
                    }
                    _ => {
                        return Err(VmError::BadOperand {
                            detail: "浮点宽度只支持 2/4/8",
                        });
                    }
                };
                self.push_slot(Slot {
                    value: Value::Float(value),
                    span: Some(Span {
                        offset: at,
                        size: u64::from(width),
                    }),
                })?;
            }
            Op::LoadBytes { len } => {
                let at = self.cursor;
                let raw = self.read_bytes(u64::from(len))?;
                self.push_slot(Slot {
                    value: Value::Bytes(raw.to_vec()),
                    span: Some(Span {
                        offset: at,
                        size: u64::from(len),
                    }),
                })?;
            }
            Op::LoadStr { enc, term, max } => {
                self.load_str(enc, term, max)?;
            }
            Op::Dup => {
                let slot = self.stack.last().cloned().ok_or(VmError::StackUnderflow)?;
                self.push_slot(slot)?;
            }
            Op::Drop => {
                self.pop()?;
            }
            Op::Swap => {
                let len = self.stack.len();
                if len < 2 {
                    return Err(VmError::StackUnderflow);
                }
                self.stack.swap(len - 1, len - 2);
            }
            Op::PushConst(idx) => {
                let value = self
                    .program
                    .consts
                    .get(idx as usize)
                    .ok_or(VmError::BadIndex {
                        table: "consts",
                        index: idx,
                        len: self.program.consts.len(),
                    })?
                    .to_value();
                self.push_slot(Slot { value, span: None })?;
            }
            Op::Add => self.binary(BinOp::Add)?,
            Op::Sub => self.binary(BinOp::Sub)?,
            Op::Mul => self.binary(BinOp::Mul)?,
            Op::Div => self.binary(BinOp::Div)?,
            Op::Mod => self.binary(BinOp::Mod)?,
            Op::And => self.binary(BinOp::And)?,
            Op::Or => self.binary(BinOp::Or)?,
            Op::Xor => self.binary(BinOp::Xor)?,
            Op::Shl => self.binary(BinOp::Shl)?,
            Op::Shr => self.binary(BinOp::Shr)?,
            Op::Not => {
                let slot = self.pop()?;
                let value = match slot.value {
                    Value::Bool(b) => Value::Bool(!b),
                    Value::UInt(v) => Value::UInt(!v),
                    Value::Int(v) => Value::Int(!v),
                    other => {
                        return Err(VmError::TypeMismatch {
                            detail: format!("{} 不支持按位取反", other.type_name()),
                        });
                    }
                };
                self.push(value)?;
            }
            Op::Neg => {
                let slot = self.pop()?;
                let value = match slot.value {
                    Value::Int(v) => Value::Int(v.wrapping_neg()),
                    Value::Float(v) => Value::Float(-v),
                    other => {
                        return Err(VmError::TypeMismatch {
                            detail: format!("{} 不支持取负", other.type_name()),
                        });
                    }
                };
                self.push(value)?;
            }
            Op::Eq => self.compare(CmpOp::Eq)?,
            Op::Ne => self.compare(CmpOp::Ne)?,
            Op::Lt => self.compare(CmpOp::Lt)?,
            Op::Le => self.compare(CmpOp::Le)?,
            Op::Gt => self.compare(CmpOp::Gt)?,
            Op::Ge => self.compare(CmpOp::Ge)?,
            Op::Jmp(target) => self.jump(target)?,
            Op::JmpIf(target) => {
                let slot = self.pop()?;
                if truthy(&slot.value) {
                    self.jump(target)?;
                }
            }
            Op::JmpIfNot(target) => {
                let slot = self.pop()?;
                if !truthy(&slot.value) {
                    self.jump(target)?;
                }
            }
            Op::Call(target, _argc) => {
                if self.calls.len() >= self.budget.depth as usize {
                    return Err(VmError::DepthExceeded {
                        limit: self.budget.depth,
                    });
                }
                let ret = u32::try_from(self.pc).map_err(|_| VmError::BadJump { target })?;
                self.calls.push(ret);
                self.jump(target)?;
            }
            Op::Ret => {
                let ret = self.calls.pop().ok_or(VmError::Malformed {
                    detail: "Ret 没有对应的 Call",
                })?;
                self.jump(ret)?;
            }
            Op::Seek(at) => self.cursor = at,
            Op::Skip(delta) => self.cursor = self.cursor.saturating_add_signed(delta),
            Op::MarkStruct {
                name_idx,
                size_hint,
            } => {
                if self.frames.len() >= self.budget.depth as usize {
                    return Err(VmError::DepthExceeded {
                        limit: self.budget.depth,
                    });
                }
                let name = self.name_at(name_idx)?;
                self.frames.push(StructFrame {
                    name,
                    offset: self.cursor,
                    size_hint: u64::from(size_hint),
                    children: Vec::new(),
                });
            }
            Op::EndStruct => self.end_struct()?,
            Op::EmitField { name_idx, type_idx } => self.emit_field_instr(name_idx, type_idx)?,
            Op::Assert(msg_idx) => {
                let slot = self.pop()?;
                if !truthy(&slot.value) {
                    let msg = self.const_message(msg_idx)?;
                    self.diags
                        .push(Diag::error(error_codes::E1006, format!("断言失败：{msg}")));
                }
            }
        }
        Ok(())
    }

    /// 读 `want` 字节并推进游标。
    fn read_bytes(&mut self, want: u64) -> Result<&'a [u8], VmError> {
        let at = self.cursor;
        let have = (self.data.len() as u64).saturating_sub(at);
        if want > have {
            return Err(VmError::OutOfBounds { at, want, have });
        }
        let start = usize::try_from(at).map_err(|_| VmError::BadOperand {
            detail: "读游标超出地址空间",
        })?;
        let end = usize::try_from(at + want).map_err(|_| VmError::BadOperand {
            detail: "读游标超出地址空间",
        })?;
        self.cursor = at + want;
        self.bytes_read = self.bytes_read.saturating_add(want);
        Ok(&self.data[start..end])
    }

    /// 取一段已读过的数据切片（用于字符串解码）。
    fn slice_read(&self, at: u64, len: u64) -> Result<&'a [u8], VmError> {
        let start = usize::try_from(at).map_err(|_| VmError::BadOperand {
            detail: "读游标超出地址空间",
        })?;
        let end = usize::try_from(at.saturating_add(len)).map_err(|_| VmError::BadOperand {
            detail: "读游标超出地址空间",
        })?;
        self.data.get(start..end).ok_or(VmError::OutOfBounds {
            at,
            want: len,
            have: (self.data.len() as u64).saturating_sub(at),
        })
    }

    /// `LoadStr`：读到终止字节（含）或读满 `max` 字节。
    fn load_str(&mut self, enc: StrEnc, term: Option<u8>, max: u32) -> Result<(), VmError> {
        let at = self.cursor;
        let mut max = u64::from(max);
        let mut consumed = 0u64;
        let mut hit_term = false;
        // UTF-16 的终止符是一个 16 位码元，且必须按码元对齐读取。
        if enc.unit_width() == 2 {
            max &= !1;
        }
        match term {
            Some(t) if enc.unit_width() == 2 => {
                let term_unit = u16::from(t);
                while consumed + 2 <= max {
                    if consumed % 65_536 == 0 {
                        self.check_interrupt()?;
                    }
                    let raw = self.read_bytes(2)?;
                    let unit = if enc == StrEnc::Utf16Le {
                        u16::from_le_bytes([raw[0], raw[1]])
                    } else {
                        u16::from_be_bytes([raw[0], raw[1]])
                    };
                    consumed += 2;
                    if unit == term_unit {
                        hit_term = true;
                        break;
                    }
                }
            }
            Some(t) => {
                while consumed < max {
                    if consumed % 65_536 == 0 {
                        self.check_interrupt()?;
                    }
                    if self.read_bytes(1)?[0] == t {
                        consumed += 1;
                        hit_term = true;
                        break;
                    }
                    consumed += 1;
                }
            }
            None => {
                self.read_bytes(max)?;
                consumed = max;
            }
        }
        let raw = self.slice_read(at, consumed)?;
        // 把终止符本身剥掉（单字节编码 1 字节，UTF-16 一个码元 2 字节）。
        let payload = if hit_term && !raw.is_empty() {
            let trim = usize::from(enc.unit_width());
            &raw[..raw.len() - trim]
        } else {
            raw
        };
        match decode_text(enc, payload) {
            Ok(text) => self.push_slot(Slot {
                value: Value::Str(text),
                span: Some(Span {
                    offset: at,
                    size: consumed,
                }),
            })?,
            Err(()) => {
                let mut diag = Diag::warning(
                    error_codes::E1002,
                    format!("偏移 {at} 处 {consumed} 字节不是合法 {enc:?} 文本"),
                );
                if let Some(range) = ByteRange::from_len(Offset::new(at), consumed) {
                    diag = diag.with_range(range);
                }
                self.diags.push(diag);
                self.push_slot(Slot {
                    value: Value::Bytes(payload.to_vec()),
                    span: Some(Span {
                        offset: at,
                        size: consumed,
                    }),
                })?;
            }
        }
        Ok(())
    }

    /// 校验整数宽度。
    fn check_int_width(&self, width: u8) -> Result<u8, VmError> {
        match width {
            1 | 2 | 3 | 4 | 6 | 8 | 16 => Ok(width),
            _ => Err(VmError::BadOperand {
                detail: "整数宽度只支持 1/2/3/4/6/8/16",
            }),
        }
    }

    /// 取名字表条目。
    fn name_at(&self, idx: u32) -> Result<String, VmError> {
        self.program
            .names
            .get(idx as usize)
            .cloned()
            .ok_or(VmError::BadIndex {
                table: "names",
                index: idx,
                len: self.program.names.len(),
            })
    }

    /// 取类型表条目。
    fn type_at(&self, idx: u32) -> Result<String, VmError> {
        self.program
            .types
            .get(idx as usize)
            .cloned()
            .ok_or(VmError::BadIndex {
                table: "types",
                index: idx,
                len: self.program.types.len(),
            })
    }

    /// 取常量作为断言文案。
    fn const_message(&self, idx: u32) -> Result<String, VmError> {
        let c = self
            .program
            .consts
            .get(idx as usize)
            .ok_or(VmError::BadIndex {
                table: "consts",
                index: idx,
                len: self.program.consts.len(),
            })?;
        Ok(match c {
            Const::Str(s) => s.clone(),
            other => other.to_string(),
        })
    }

    /// 压栈并计费（栈深 + 变量内存）。
    fn push(&mut self, value: Value) -> Result<(), VmError> {
        self.push_slot(Slot { value, span: None })
    }

    /// 压栈（带 span）。
    fn push_slot(&mut self, slot: Slot) -> Result<(), VmError> {
        if self.stack.len() >= self.budget.depth as usize {
            return Err(VmError::DepthExceeded {
                limit: self.budget.depth,
            });
        }
        let want = self.mem_used.saturating_add(slot.mem_bytes());
        if want > self.budget.mem_vars {
            return Err(VmError::AllocLimit {
                want,
                limit: self.budget.mem_vars,
            });
        }
        self.mem_used = want;
        self.stack.push(slot);
        Ok(())
    }

    /// 弹栈并退费。
    fn pop(&mut self) -> Result<Slot, VmError> {
        let slot = self.stack.pop().ok_or(VmError::StackUnderflow)?;
        self.mem_used = self.mem_used.saturating_sub(slot.mem_bytes());
        Ok(slot)
    }

    /// 二元运算：弹出两个操作数，压入结果。
    fn binary(&mut self, op: BinOp) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let value = eval_bin(op, &a.value, &b.value, self.cursor)?;
        self.push(value)
    }

    /// 比较运算：弹出两个操作数，压入布尔。
    fn compare(&mut self, op: CmpOp) -> Result<(), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        let value = eval_cmp(op, &a.value, &b.value)?;
        self.push(Value::Bool(value))
    }

    /// 跳转并检查目标。
    fn jump(&mut self, target: u32) -> Result<(), VmError> {
        let target_usize = target as usize;
        if target_usize > self.program.code.len() {
            return Err(VmError::BadJump { target });
        }
        self.pc = target_usize;
        Ok(())
    }

    /// `EmitField`：把栈顶（若有）变成字段节点。
    fn emit_field_instr(&mut self, name_idx: u32, type_idx: u32) -> Result<(), VmError> {
        let name = self.name_at(name_idx)?;
        let ty = self.type_at(type_idx)?;
        let slot = if self.stack.is_empty() {
            None
        } else {
            Some(self.pop()?)
        };
        let (offset, size, value) = match slot {
            Some(slot) => match slot.span {
                Some(span) => (span.offset, span.size, Some(slot.value)),
                None => (self.cursor, infer_size(&ty), Some(slot.value)),
            },
            None => {
                // 只有声明没有值：按声明定长前移游标（02 §8.3 的恢复策略）。
                let size = infer_size(&ty);
                let offset = self.cursor;
                self.cursor = offset.saturating_add(size).min(self.data.len() as u64);
                (offset, size, None)
            }
        };
        self.emit_node(name, ty, offset, size, value)
    }

    /// 发射一个字段节点；越界时降级为 `unknown_0xNN`（软失败）。
    fn emit_node(
        &mut self,
        name: String,
        ty: String,
        offset: u64,
        size: u64,
        value: Option<Value>,
    ) -> Result<(), VmError> {
        let len = self.data.len() as u64;
        let end = offset.saturating_add(size);
        if offset <= len && end <= len {
            self.charge_output(&name, &ty, value_payload(value.as_ref()))?;
            let node = Node::leaf(name, ty, offset, size, value);
            self.attach(node);
            return Ok(());
        }
        let clamped = offset.min(len);
        let node_size = size.min(len - clamped);
        let unknown_name = format!("unknown_0x{clamped:02X}");
        self.charge_output(&unknown_name, "unknown", 0)?;
        self.attach(Node::leaf(
            unknown_name,
            "unknown",
            clamped,
            node_size,
            None,
        ));
        let mut diag = Diag::warning(
            error_codes::E3001,
            format!(
                "字段 {name}（{ty}）声明 {size} 字节，偏移 {offset} 起只剩 {} 字节，已降级为未知节点",
                len - clamped
            ),
        );
        if let Some(range) = ByteRange::from_len(Offset::new(clamped), node_size) {
            diag = diag.with_range(range);
        }
        self.diags.push(diag);
        Ok(())
    }

    /// 把节点挂到当前结构体（或顶层）。
    fn attach(&mut self, node: Node) {
        match self.frames.last_mut() {
            Some(frame) => frame.children.push(node),
            None => self.top.push(node),
        }
    }

    /// 结束最近的结构体。
    fn end_struct(&mut self) -> Result<(), VmError> {
        let frame = self.frames.pop().ok_or(VmError::Malformed {
            detail: "EndStruct 没有对应的 MarkStruct",
        })?;
        let size = self
            .cursor
            .saturating_sub(frame.offset)
            .max(frame.size_hint);
        self.charge_output(&frame.name, &frame.name, 0)?;
        let mut node = Node::branch(frame.name.clone(), frame.name, frame.offset, size);
        for child in frame.children {
            node.push_child(child);
        }
        self.attach(node);
        Ok(())
    }

    /// 产出计费：每节点约 64 B 固定开销 + 名字/类型/值负载。
    fn charge_output(&mut self, name: &str, ty: &str, payload: u64) -> Result<(), VmError> {
        let cost = 64u64
            .saturating_add(name.len() as u64)
            .saturating_add(ty.len() as u64)
            .saturating_add(payload);
        let used = self.out_used.saturating_add(cost);
        if used > self.budget.output_bytes {
            return Err(VmError::OutputLimit {
                used,
                limit: self.budget.output_bytes,
            });
        }
        self.out_used = used;
        Ok(())
    }

    /// 收尾：拼装根节点、修路径、检查结构体是否闭合。
    fn finish(&mut self) -> Result<RunOutput, VmError> {
        if !self.frames.is_empty() {
            return Err(VmError::Malformed {
                detail: "停机时仍有未闭合的 MarkStruct",
            });
        }
        let mut root = Node::branch("root", "root", 0, 0);
        let mut end = 0u64;
        for child in std::mem::take(&mut self.top) {
            end = end.max(child.offset.saturating_add(child.size));
            root.push_child(child);
        }
        root.size = end;
        fix_paths(&mut root, "");
        Ok(RunOutput {
            root,
            diags: std::mem::take(&mut self.diags),
            steps: self.steps,
            bytes_consumed: self.bytes_read,
        })
    }
}

/// 二元算术/位运算的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
}

/// 比较运算的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// 数值三元组：运算统一提升到"有符号/无符号/浮点"之一。
#[derive(Clone, Copy)]
enum Num {
    U(u128),
    I(i128),
    F(f64),
}

impl Num {
    fn as_f64(self) -> f64 {
        match self {
            Self::U(v) => v as f64,
            Self::I(v) => v as f64,
            Self::F(v) => v,
        }
    }

    fn as_i128(self) -> i128 {
        match self {
            Self::U(v) => i128::try_from(v).unwrap_or(i128::MAX),
            Self::I(v) => v,
            Self::F(v) => v as i128,
        }
    }

    fn as_u128(self) -> u128 {
        match self {
            Self::U(v) => v,
            Self::I(v) => u128::try_from(v).unwrap_or(0),
            Self::F(v) => v as u128,
        }
    }
}

/// 把值看成数值；布尔按 0/1 参与算术。
fn num(value: &Value) -> Option<Num> {
    match value {
        Value::UInt(v) => Some(Num::U(*v)),
        Value::Int(v) => Some(Num::I(*v)),
        Value::Float(v) => Some(Num::F(*v)),
        Value::Bool(b) => Some(Num::U(u128::from(*b))),
        _ => None,
    }
}

/// 真值判定。
fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::UInt(v) => *v != 0,
        Value::Int(v) => *v != 0,
        Value::Float(v) => *v != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::None => false,
    }
}

/// 计算二元运算。
fn eval_bin(op: BinOp, a: &Value, b: &Value, at: u64) -> Result<Value, VmError> {
    // 布尔之间的与/或/异或走逻辑语义（其余组合按整数位运算）。
    if let (Value::Bool(x), Value::Bool(y)) = (a, b) {
        match op {
            BinOp::And => return Ok(Value::Bool(*x && *y)),
            BinOp::Or => return Ok(Value::Bool(*x || *y)),
            BinOp::Xor => return Ok(Value::Bool(*x ^ *y)),
            _ => {}
        }
    }
    let (x, y) = match (num(a), num(b)) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            return Err(VmError::TypeMismatch {
                detail: format!("{} 与 {} 不能做 {:?}", a.type_name(), b.type_name(), op),
            });
        }
    };
    let float = matches!(x, Num::F(_)) || matches!(y, Num::F(_));
    let signed = matches!(x, Num::I(_)) || matches!(y, Num::I(_));
    if float {
        bin_float(op, x.as_f64(), y.as_f64(), at)
    } else if signed {
        let shift = shift_amount(y)?;
        bin_i128(op, x.as_i128(), y.as_i128(), shift, at)
    } else {
        let shift = shift_amount(y)?;
        bin_u128(op, x.as_u128(), y.as_u128(), shift, at)
    }
}

/// 取移位量。
fn shift_amount(y: Num) -> Result<u32, VmError> {
    match y {
        Num::U(v) => u32::try_from(v).map_err(|_| VmError::BadOperand {
            detail: "移位量超出 u32",
        }),
        Num::I(v) => u32::try_from(v).map_err(|_| VmError::BadOperand {
            detail: "移位量不能为负或超出 u32",
        }),
        Num::F(_) => Err(VmError::TypeMismatch {
            detail: "浮点数不能作移位量".into(),
        }),
    }
}

fn bin_float(op: BinOp, a: f64, b: f64, at: u64) -> Result<Value, VmError> {
    let v = match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => {
            if b == 0.0 {
                return Err(VmError::DivideByZero { at });
            }
            a / b
        }
        BinOp::Mod => {
            if b == 0.0 {
                return Err(VmError::DivideByZero { at });
            }
            a % b
        }
        BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Shl | BinOp::Shr => {
            return Err(VmError::TypeMismatch {
                detail: format!("浮点数不支持 {op:?}"),
            });
        }
    };
    Ok(Value::Float(v))
}

fn bin_i128(op: BinOp, a: i128, b: i128, shift: u32, at: u64) -> Result<Value, VmError> {
    let v = match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div => {
            if b == 0 {
                return Err(VmError::DivideByZero { at });
            }
            a.checked_div(b).unwrap_or(i128::MIN)
        }
        BinOp::Mod => {
            if b == 0 {
                return Err(VmError::DivideByZero { at });
            }
            a.checked_rem(b).unwrap_or(0)
        }
        BinOp::And => a & b,
        BinOp::Or => a | b,
        BinOp::Xor => a ^ b,
        BinOp::Shl => a.checked_shl(shift).unwrap_or(0),
        BinOp::Shr => a.checked_shr(shift).unwrap_or(0),
    };
    Ok(Value::Int(v))
}

fn bin_u128(op: BinOp, a: u128, b: u128, shift: u32, at: u64) -> Result<Value, VmError> {
    let v = match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div => {
            if b == 0 {
                return Err(VmError::DivideByZero { at });
            }
            a / b
        }
        BinOp::Mod => {
            if b == 0 {
                return Err(VmError::DivideByZero { at });
            }
            a % b
        }
        BinOp::And => a & b,
        BinOp::Or => a | b,
        BinOp::Xor => a ^ b,
        BinOp::Shl => a.checked_shl(shift).unwrap_or(0),
        BinOp::Shr => a.checked_shr(shift).unwrap_or(0),
    };
    Ok(Value::UInt(v))
}

/// 计算比较运算。
fn eval_cmp(op: CmpOp, a: &Value, b: &Value) -> Result<bool, VmError> {
    if matches!(op, CmpOp::Eq | CmpOp::Ne)
        && (matches!(a, Value::Bool(_)) || matches!(b, Value::Bool(_)))
    {
        let equal = a == b;
        return Ok(if op == CmpOp::Eq { equal } else { !equal });
    }
    let ord = match (num(a), num(b)) {
        (Some(x), Some(y)) => match (x, y) {
            (Num::U(x), Num::U(y)) => Some(x.cmp(&y)),
            (Num::I(x), Num::I(y)) => Some(x.cmp(&y)),
            (Num::F(x), Num::F(y)) => x.partial_cmp(&y),
            (x, y) => x.as_f64().partial_cmp(&y.as_f64()),
        },
        _ => match (a, b) {
            (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
            (Value::Bytes(x), Value::Bytes(y)) => Some(x.cmp(y)),
            _ => {
                return Err(VmError::TypeMismatch {
                    detail: format!("{} 与 {} 不能比较", a.type_name(), b.type_name()),
                });
            }
        },
    };
    Ok(match ord {
        Some(o) => match op {
            CmpOp::Eq => o.is_eq(),
            CmpOp::Ne => o.is_ne(),
            CmpOp::Lt => o.is_lt(),
            CmpOp::Le => o.is_le(),
            CmpOp::Gt => o.is_gt(),
            CmpOp::Ge => o.is_ge(),
        },
        // NaN：所有大小比较都为假，与 IEEE-754 一致。
        None => false,
    })
}

/// 按字节序把任意宽度（1..=16）的字节拼成 `u128`。
fn u128_from_bytes(bytes: &[u8], endian: Endian) -> u128 {
    let mut value = 0u128;
    match endian {
        Endian::Big => {
            for b in bytes {
                value = (value << 8) | u128::from(*b);
            }
        }
        Endian::Little => {
            for b in bytes.iter().rev() {
                value = (value << 8) | u128::from(*b);
            }
        }
    }
    value
}

/// IEEE-754 半精度 → 双精度。
fn f16_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = (bits >> 10) & 0x1F;
    let fraction = bits & 0x03FF;
    match exponent {
        0 => sign * f64::from(fraction) * 2f64.powi(-24),
        0x1F => {
            if fraction == 0 {
                sign * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => sign * (1.0 + f64::from(fraction) / 1024.0) * 2f64.powi(i32::from(exponent) - 15),
    }
}

/// 按编码解码文本；失败返回 `Err(())`，由调用方降级为字节视图（E1002）。
fn decode_text(enc: StrEnc, bytes: &[u8]) -> Result<String, ()> {
    match enc {
        StrEnc::Utf8 => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ()),
        StrEnc::Ascii => {
            if bytes.iter().all(u8::is_ascii) {
                Ok(bytes.iter().map(|b| char::from(*b)).collect())
            } else {
                Err(())
            }
        }
        StrEnc::Latin1 => Ok(bytes.iter().map(|b| char::from(*b)).collect()),
        StrEnc::Utf16Le | StrEnc::Utf16Be => {
            if bytes.len() % 2 != 0 {
                return Err(());
            }
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| match enc {
                    StrEnc::Utf16Le => u16::from_le_bytes([c[0], c[1]]),
                    _ => u16::from_be_bytes([c[0], c[1]]),
                })
                .collect();
            char::decode_utf16(units)
                .collect::<Result<String, _>>()
                .map_err(|_| ())
        }
    }
}

/// 按类型名推断声明大小；未知类型返回 0。
fn infer_size(type_name: &str) -> u64 {
    match type_name {
        "u8" | "s8" | "i8" | "bool" | "char" | "byte" => 1,
        "u16" | "s16" | "i16" | "f16" => 2,
        "u24" | "s24" | "i24" => 3,
        "u32" | "s32" | "i32" | "f32" => 4,
        "u48" | "s48" | "i48" => 6,
        "u64" | "s64" | "i64" | "f64" => 8,
        "u128" | "s128" | "i128" => 16,
        _ => 0,
    }
}

/// 值在节点里的估算负载。
fn value_payload(value: Option<&Value>) -> u64 {
    match value {
        None | Some(Value::None) => 0,
        Some(Value::Str(s)) => s.len() as u64,
        Some(Value::Bytes(b)) => b.len() as u64,
        Some(_) => 8,
    }
}

/// 递归重写整棵树的 `path`（`push_child` 只补一层，嵌套结构需要全量修正）。
fn fix_paths(node: &mut Node, parent: &str) {
    node.path = if parent.is_empty() {
        node.name.clone()
    } else {
        format!("{parent}.{}", node.name)
    };
    let prefix = node.path.clone();
    for child in &mut node.children {
        fix_paths(child, &prefix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Const, Op};
    use geode_core::diag::Severity;
    use proptest::prelude::*;

    fn prog(code: Vec<Op>) -> Program {
        Program {
            code,
            ..Program::default()
        }
    }

    fn prog_full(code: Vec<Op>, consts: Vec<Const>, names: Vec<&str>, types: Vec<&str>) -> Program {
        Program {
            code,
            consts,
            names: names.into_iter().map(str::to_owned).collect(),
            types: types.into_iter().map(str::to_owned).collect(),
        }
    }

    fn run(program: &Program, data: &[u8]) -> Result<RunOutput, VmError> {
        Vm::new(program, data).run()
    }

    fn find<'a>(root: &'a Node, path: &str) -> Option<&'a Node> {
        let mut found = None;
        root.walk(&mut |n| {
            if n.path == path {
                found = Some(n);
            }
        });
        found
    }

    #[test]
    fn empty_program_halts() {
        let out = run(&Program::default(), b"").expect("空程序应正常停机");
        assert_eq!(out.root.name, "root");
        assert_eq!(out.root.count(), 1);
        assert_eq!(out.steps, 0);
        assert!(out.diags.is_empty());
    }

    #[test]
    fn halt_only_program() {
        let out = run(&prog(vec![Op::Halt]), b"abc").expect("Halt 应正常停机");
        assert_eq!(out.steps, 1);
        assert_eq!(out.bytes_consumed, 0);
        assert_eq!(out.root.children.len(), 0);
    }

    #[test]
    fn load_u32_big_endian() {
        let p = prog_full(
            vec![
                Op::Seek(0),
                Op::LoadU {
                    width: 4,
                    endian: Endian::Big,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["magic"],
            vec!["u32"],
        );
        let out = run(&p, &[0x89, 0x50, 0x4E, 0x47]).expect("应成功");
        let node = find(&out.root, "root.magic").expect("应有 magic 节点");
        assert_eq!(node.value, Some(Value::UInt(0x8950_4E47)));
        assert_eq!((node.offset, node.size), (0, 4));
        assert_eq!(out.bytes_consumed, 4);
    }

    #[test]
    fn load_u32_little_endian() {
        let p = prog_full(
            vec![
                Op::LoadU {
                    width: 4,
                    endian: Endian::Little,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["v"],
            vec!["u32"],
        );
        let out = run(&p, &[0x47, 0x4E, 0x50, 0x89]).expect("应成功");
        let node = find(&out.root, "root.v").expect("应有 v 节点");
        assert_eq!(node.value, Some(Value::UInt(0x8950_4E47)));
    }

    #[test]
    fn load_i16_sign_extends() {
        let p = prog_full(
            vec![
                Op::LoadI {
                    width: 2,
                    endian: Endian::Little,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["delta"],
            vec!["s16"],
        );
        let out = run(&p, &[0xFE, 0xFF]).expect("应成功");
        let node = find(&out.root, "root.delta").expect("应有 delta 节点");
        assert_eq!(node.value, Some(Value::Int(-2)));
    }

    #[test]
    fn load_f16_and_f32() {
        let p = prog_full(
            vec![
                Op::LoadF {
                    width: 2,
                    endian: Endian::Big,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::LoadF {
                    width: 4,
                    endian: Endian::Big,
                },
                Op::EmitField {
                    name_idx: 1,
                    type_idx: 1,
                },
                Op::Halt,
            ],
            vec![],
            vec!["half", "single"],
            vec!["f16", "f32"],
        );
        let data = [0x3C, 0x00, 0x3F, 0xC0, 0x00, 0x00];
        let out = run(&p, &data).expect("应成功");
        assert_eq!(
            find(&out.root, "root.half").and_then(|n| n.value.clone()),
            Some(Value::Float(1.0))
        );
        assert_eq!(
            find(&out.root, "root.single").and_then(|n| n.value.clone()),
            Some(Value::Float(1.5))
        );
    }

    #[test]
    fn load_str_stops_at_terminator() {
        let p = prog_full(
            vec![
                Op::LoadStr {
                    enc: StrEnc::Utf8,
                    term: Some(0),
                    max: 32,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["name"],
            vec!["str"],
        );
        let out = run(&p, b"hi\0rest").expect("应成功");
        let node = find(&out.root, "root.name").expect("应有 name 节点");
        assert_eq!(node.value, Some(Value::Str("hi".into())));
        assert_eq!(node.size, 3);
        assert_eq!(out.bytes_consumed, 3);
    }

    #[test]
    fn load_utf16_string_stops_at_wide_terminator() {
        let p = prog_full(
            vec![
                Op::LoadStr {
                    enc: StrEnc::Utf16Le,
                    term: Some(0),
                    max: 32,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["wide"],
            vec!["str"],
        );
        // "hi" + 0x0000：高字节为 0 不能被误判成终止符。
        let data = [b'h', 0x00, b'i', 0x00, 0x00, 0x00];
        let out = run(&p, &data).expect("应成功");
        let node = find(&out.root, "root.wide").expect("应有 wide 节点");
        assert_eq!(node.value, Some(Value::Str("hi".into())));
        assert_eq!(node.size, 6);
    }

    #[test]
    fn load_str_invalid_utf8_degrades_to_bytes() {
        let p = prog_full(
            vec![
                Op::LoadStr {
                    enc: StrEnc::Utf8,
                    term: Some(0),
                    max: 8,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["name"],
            vec!["str"],
        );
        let out = run(&p, &[0xFF, 0x00]).expect("软失败不应终止");
        let node = find(&out.root, "root.name").expect("应有 name 节点");
        assert_eq!(node.value, Some(Value::Bytes(vec![0xFF])));
        assert_eq!(out.diags.len(), 1);
        assert_eq!(out.diags[0].code, error_codes::E1002);
        assert_eq!(out.diags[0].severity, Severity::Warning);
    }

    #[test]
    fn load_bytes_out_of_bounds_is_fatal() {
        let p = prog(vec![Op::LoadBytes { len: 4 }, Op::Halt]);
        let err = run(&p, &[1, 2]).expect_err("越界读应失败");
        assert_eq!(
            err,
            VmError::OutOfBounds {
                at: 0,
                want: 4,
                have: 2
            }
        );
        assert_eq!(err.code(), error_codes::E3001);
    }

    #[test]
    fn jump_loop_counts_down() {
        // 0: push 3; 1: push 1; 2: sub; 3: dup; 4: push 0; 5: gt;
        // 6: jmp_if_not 8; 7: jmp 1; 8: emit; 9: halt
        let p = prog_full(
            vec![
                Op::PushConst(0),
                Op::PushConst(1),
                Op::Sub,
                Op::Dup,
                Op::PushConst(2),
                Op::Gt,
                Op::JmpIfNot(8),
                Op::Jmp(1),
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![Const::UInt(3), Const::UInt(1), Const::UInt(0)],
            vec!["counter"],
            vec!["u8"],
        );
        let out = run(&p, &[0]).expect("循环应正常结束");
        let node = find(&out.root, "root.counter").expect("应有 counter 节点");
        assert_eq!(node.value, Some(Value::UInt(0)));
        // 循环体要反复执行，步数应明显多于指令条数。
        assert!(out.steps > 10);
    }

    #[test]
    fn call_and_ret_share_stack() {
        // 0: call 3; 1: emit; 2: halt; 3: push 42; 4: ret
        let p = prog_full(
            vec![
                Op::Call(3, 0),
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
                Op::PushConst(0),
                Op::Ret,
            ],
            vec![Const::UInt(42)],
            vec!["answer"],
            vec!["u8"],
        );
        let out = run(&p, &[0]).expect("Call/Ret 应成功");
        let node = find(&out.root, "root.answer").expect("应有 answer 节点");
        assert_eq!(node.value, Some(Value::UInt(42)));
    }

    #[test]
    fn assert_failure_is_soft() {
        let p = prog_full(
            vec![Op::PushConst(0), Op::Assert(1), Op::Halt],
            vec![Const::UInt(0), Const::Str("magic 必须非零".into())],
            vec![],
            vec![],
        );
        let out = run(&p, b"").expect("断言失败是软错误");
        assert_eq!(out.diags.len(), 1);
        assert_eq!(out.diags[0].code, error_codes::E1006);
        assert_eq!(out.diags[0].severity, Severity::Error);
        assert!(out.diags[0].recoverable);
        assert!(out.diags[0].detail.contains("magic 必须非零"));
    }

    #[test]
    fn assert_success_is_silent() {
        let p = prog_full(
            vec![Op::PushConst(0), Op::Assert(1), Op::Halt],
            vec![Const::UInt(1), Const::Str("ok".into())],
            vec![],
            vec![],
        );
        let out = run(&p, b"").expect("断言通过");
        assert!(out.diags.is_empty());
    }

    #[test]
    fn emit_out_of_bounds_yields_unknown_node() {
        let p = prog_full(
            vec![
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["tail"],
            vec!["u32"],
        );
        let out = run(&p, &[0xAA, 0xBB]).expect("越界字段应软失败");
        assert_eq!(out.root.children.len(), 1);
        let node = &out.root.children[0];
        assert!(node.name.starts_with("unknown_0x"), "实际 {}", node.name);
        assert_eq!(node.type_name, "unknown");
        assert_eq!((node.offset, node.size), (0, 2));
        assert_eq!(out.diags.len(), 1);
        assert_eq!(out.diags[0].code, error_codes::E3001);
        assert_eq!(out.diags[0].severity, Severity::Warning);
    }

    #[test]
    fn steps_budget_exhaustion_is_e2001() {
        let p = prog(vec![Op::Jmp(0)]);
        let budget = Budget {
            steps: 10,
            ..Budget::default()
        };
        let err = Vm::new(&p, b"")
            .with_budget(budget)
            .run()
            .expect_err("死循环应超预算");
        assert_eq!(err, VmError::StepsExhausted { limit: 10 });
        assert_eq!(err.code(), error_codes::E2001);
    }

    #[test]
    fn divide_by_zero_is_e3004() {
        let p = prog_full(
            vec![Op::PushConst(0), Op::PushConst(1), Op::Div, Op::Halt],
            vec![Const::UInt(10), Const::UInt(0)],
            vec![],
            vec![],
        );
        let err = run(&p, b"").expect_err("除零应失败");
        assert_eq!(err.code(), error_codes::E3004);
        assert!(matches!(err, VmError::DivideByZero { at: 0 }));
    }

    #[test]
    fn stack_depth_limit_is_e2003() {
        let p = prog_full(
            vec![
                Op::PushConst(0),
                Op::PushConst(0),
                Op::PushConst(0),
                Op::Halt,
            ],
            vec![Const::UInt(1)],
            vec![],
            vec![],
        );
        let budget = Budget {
            depth: 2,
            ..Budget::default()
        };
        let err = Vm::new(&p, b"")
            .with_budget(budget)
            .run()
            .expect_err("栈应溢出");
        assert_eq!(err, VmError::DepthExceeded { limit: 2 });
        assert_eq!(err.code(), error_codes::E2003);
    }

    #[test]
    fn output_budget_limit_is_e2004() {
        let p = prog_full(
            vec![
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["a"],
            vec!["u8"],
        );
        let budget = Budget {
            output_bytes: 10,
            ..Budget::default()
        };
        let err = Vm::new(&p, &[0])
            .with_budget(budget)
            .run()
            .expect_err("产出应超限");
        assert!(matches!(err, VmError::OutputLimit { .. }));
        assert_eq!(err.code(), error_codes::E2004);
    }

    #[test]
    fn mem_budget_limit_is_e3003() {
        let p = prog(vec![Op::LoadBytes { len: 4 }, Op::Halt]);
        let budget = Budget {
            mem_vars: 2,
            ..Budget::default()
        };
        let err = Vm::new(&p, &[1, 2, 3, 4])
            .with_budget(budget)
            .run()
            .expect_err("分配应超限");
        assert!(matches!(err, VmError::AllocLimit { .. }));
        assert_eq!(err.code(), error_codes::E3003);
    }

    #[test]
    fn dup_and_swap_and_drop() {
        // push 5; push 3; swap; sub -> 3-5 = -2; dup; drop（丢掉副本）; emit
        let p = prog_full(
            vec![
                Op::PushConst(0),
                Op::PushConst(1),
                Op::Swap,
                Op::Sub,
                Op::Dup,
                Op::Drop,
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![Const::Int(5), Const::Int(3)],
            vec!["n"],
            vec!["s8"],
        );
        let out = run(&p, &[0]).expect("应成功");
        let node = find(&out.root, "root.n").expect("应有 n 节点");
        assert_eq!(node.value, Some(Value::Int(-2)));
    }

    #[test]
    fn seek_and_skip_move_cursor() {
        let p = prog_full(
            vec![
                Op::Seek(2),
                Op::LoadU {
                    width: 1,
                    endian: Endian::Big,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Skip(-3),
                Op::LoadU {
                    width: 1,
                    endian: Endian::Big,
                },
                Op::EmitField {
                    name_idx: 1,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["third", "first"],
            vec!["u8"],
        );
        let out = run(&p, &[10, 20, 30, 40]).expect("应成功");
        let third = find(&out.root, "root.third").expect("应有 third");
        let first = find(&out.root, "root.first").expect("应有 first");
        assert_eq!(third.value, Some(Value::UInt(30)));
        assert_eq!((third.offset, third.size), (2, 1));
        assert_eq!(first.value, Some(Value::UInt(10)));
        assert_eq!((first.offset, first.size), (0, 1));
    }

    #[test]
    fn mark_struct_builds_tree_and_paths() {
        let p = prog_full(
            vec![
                Op::MarkStruct {
                    name_idx: 0,
                    size_hint: 0,
                },
                Op::LoadU {
                    width: 1,
                    endian: Endian::Big,
                },
                Op::EmitField {
                    name_idx: 1,
                    type_idx: 0,
                },
                Op::EndStruct,
                Op::Halt,
            ],
            vec![],
            vec!["header", "magic"],
            vec!["u8"],
        );
        let out = run(&p, &[7]).expect("结构体应闭合");
        assert_eq!(out.root.children.len(), 1);
        let header = &out.root.children[0];
        assert_eq!(header.name, "header");
        assert_eq!(header.path, "root.header");
        assert_eq!((header.offset, header.size), (0, 1));
        assert_eq!(header.children.len(), 1);
        assert_eq!(header.children[0].path, "root.header.magic");
        assert_eq!(header.children[0].value, Some(Value::UInt(7)));
    }

    #[test]
    fn unterminated_struct_is_malformed() {
        let p = prog_full(
            vec![
                Op::MarkStruct {
                    name_idx: 0,
                    size_hint: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["header"],
            vec![],
        );
        let err = run(&p, b"").expect_err("未闭合结构体应报错");
        assert!(matches!(err, VmError::Malformed { .. }));
    }

    #[test]
    fn type_mismatch_is_e4002() {
        let p = prog_full(
            vec![Op::PushConst(0), Op::PushConst(1), Op::Add, Op::Halt],
            vec![Const::Str("a".into()), Const::UInt(1)],
            vec![],
            vec![],
        );
        let err = run(&p, b"").expect_err("字符串加整数应类型错误");
        assert_eq!(err.code(), error_codes::E4002);
    }

    #[test]
    fn cancel_flag_stops_execution() {
        let p = prog(vec![Op::Jmp(0)]);
        let flag = AtomicBool::new(true);
        let err = Vm::new(&p, b"")
            .with_cancel(&flag)
            .run()
            .expect_err("应被取消");
        assert_eq!(err, VmError::Cancelled);
        assert_eq!(err.code(), error_codes::E2005);
    }

    #[test]
    fn encoded_program_runs_identically() {
        let p = prog_full(
            vec![
                Op::LoadU {
                    width: 2,
                    endian: Endian::Little,
                },
                Op::EmitField {
                    name_idx: 0,
                    type_idx: 0,
                },
                Op::Halt,
            ],
            vec![],
            vec!["v"],
            vec!["u16"],
        );
        let bytes = p.encode().expect("编码失败");
        let decoded = Program::decode(&bytes).expect("解码失败");
        let a = run(&p, &[0x34, 0x12]).expect("原程序应成功");
        let b = run(&decoded, &[0x34, 0x12]).expect("解码程序应成功");
        assert_eq!(a.root, b.root);
        assert_eq!(a.steps, b.steps);
    }

    // ---- proptest ----

    fn arb_const() -> impl Strategy<Value = Const> {
        prop_oneof![
            any::<u128>().prop_map(Const::UInt),
            any::<i128>().prop_map(Const::Int),
            any::<f64>().prop_map(Const::Float),
            any::<String>().prop_map(Const::Str),
            prop::collection::vec(any::<u8>(), 0..8).prop_map(Const::Bytes),
        ]
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        (
            0u8..38,
            any::<u32>(),
            any::<u32>(),
            any::<u64>(),
            any::<i64>(),
            any::<u8>(),
        )
            .prop_map(|(tag, a, b, c, d, e)| {
                let endian = if a % 2 == 0 {
                    Endian::Little
                } else {
                    Endian::Big
                };
                let enc = match b % 5 {
                    0 => StrEnc::Utf8,
                    1 => StrEnc::Ascii,
                    2 => StrEnc::Latin1,
                    3 => StrEnc::Utf16Le,
                    _ => StrEnc::Utf16Be,
                };
                let iw = [1u8, 2, 3, 4, 6, 8, 16][(a as usize) % 7];
                let fw = [2u8, 4, 8][(b as usize) % 3];
                match tag {
                    0 => Op::LoadU { width: iw, endian },
                    1 => Op::LoadI { width: iw, endian },
                    2 => Op::LoadF { width: fw, endian },
                    3 => Op::LoadBytes { len: a },
                    4 => Op::LoadStr {
                        enc,
                        term: if e % 2 == 0 { None } else { Some(e) },
                        max: b,
                    },
                    5 => Op::Dup,
                    6 => Op::Drop,
                    7 => Op::Swap,
                    8 => Op::PushConst(a),
                    9 => Op::Add,
                    10 => Op::Sub,
                    11 => Op::Mul,
                    12 => Op::Div,
                    13 => Op::Mod,
                    14 => Op::And,
                    15 => Op::Or,
                    16 => Op::Xor,
                    17 => Op::Shl,
                    18 => Op::Shr,
                    19 => Op::Not,
                    20 => Op::Neg,
                    21 => Op::Eq,
                    22 => Op::Ne,
                    23 => Op::Lt,
                    24 => Op::Le,
                    25 => Op::Gt,
                    26 => Op::Ge,
                    27 => Op::Jmp(a),
                    28 => Op::JmpIf(a),
                    29 => Op::JmpIfNot(a),
                    30 => Op::Call(a, e),
                    31 => Op::Ret,
                    32 => Op::Seek(c),
                    33 => Op::Skip(d),
                    34 => Op::MarkStruct {
                        name_idx: a,
                        size_hint: b,
                    },
                    35 => Op::EndStruct,
                    36 => Op::EmitField {
                        name_idx: a,
                        type_idx: b,
                    },
                    _ => Op::Assert(a),
                }
            })
    }

    fn arb_program() -> impl Strategy<Value = Program> {
        (
            prop::collection::vec(arb_op(), 0..6),
            prop::collection::vec(arb_const(), 0..4),
            prop::collection::vec(prop::collection::vec(any::<char>(), 0..6), 0..3),
            prop::collection::vec(prop::collection::vec(any::<char>(), 0..6), 0..3),
        )
            .prop_map(|(code, consts, names, types)| Program {
                code,
                consts,
                names: names.into_iter().map(String::from_iter).collect(),
                types: types.into_iter().map(String::from_iter).collect(),
            })
    }

    proptest! {
        #[test]
        fn encode_decode_identity(p in arb_program()) {
            let bytes = p.encode().expect("编码不应失败");
            let back = Program::decode(&bytes).expect("解码不应失败");
            prop_assert_eq!(back, p);
        }

        #[test]
        fn arbitrary_data_never_panics(data in prop::collection::vec(any::<u8>(), 0..64)) {
            let p = prog_full(
                vec![
                    Op::Seek(0),
                    Op::LoadU { width: 4, endian: Endian::Big },
                    Op::EmitField { name_idx: 0, type_idx: 0 },
                    Op::LoadStr { enc: StrEnc::Utf8, term: Some(0), max: 16 },
                    Op::EmitField { name_idx: 1, type_idx: 1 },
                    Op::MarkStruct { name_idx: 2, size_hint: 4 },
                    Op::LoadF { width: 8, endian: Endian::Little },
                    Op::EmitField { name_idx: 1, type_idx: 2 },
                    Op::EndStruct,
                    Op::Assert(0),
                    Op::Halt,
                ],
                vec![Const::Str("x".into())],
                vec!["a", "b", "s"],
                vec!["u32", "str", "f64"],
            );
            let budget = Budget { steps: 10_000, ..Budget::default() };
            let result = Vm::new(&p, &data).with_budget(budget).run();
            // 只要求"不 panic"：越界/类型错误都允许返回 Err。
            let _ = result;
        }
    }
}
