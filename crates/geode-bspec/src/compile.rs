//! BinSpec AST → 字节码编译器（M3 v1）。
//!
//! 编译器把 [`crate::ast::Program`] 中的入口结构体/联合体/函数翻译成
//! [`crate::bytecode::Program`]。v1 采用**编译期常量折叠 + 静态布局**策略：
//! 所有表达式都必须在编译期求值；不能求值的表达式会返回带错误码的诊断，
//! 而不是生成运行期表达式字节码。
//!
//! # v1 边界
//!
//! - `let` 只支持编译期常量；变量用于 `if`/`while`/`for` 条件和 `@` 偏移。
//! - `if`/`match` 条件必须是编译期常量，因此分支在编译期选定。
//! - `while`/`for` 在循环变量参与条件时静态展开（上限 1024 次）；条件与
//!   循环变量无关时生成运行期跳转循环，`break`/`continue` 通过回填跳转实现。
//! - 数组元素为结构体时逐个展开；基础类型数组用 `LoadBytes` 整体读取。
//! - enum/bitfield 只按底层 `repr` 读取整体整数，不拆分 bitfield 成员。
//! - `str` 的 `term` 在运行期才确定长度，因此其后的静态偏移未知；`str` 的
//!   `max` 缺省为当前定长结构的剩余字节，结构大小未知时退回 4096 字节。
//!
//! AST 当前没有 span 字段，本模块产生的所有诊断都带一个位于源码偏移 0 的
//! 空 [`ByteRange`] 作为占位；错误码与细节文本是稳定的，range 仅用于满足
//! [`geode_core::diag::Diag`] 的形状要求。

use std::collections::HashMap;

use crate::ast::{
    ArraySpec, AssignOp, BinaryOp, Block, ElseBranch, Expr, FnDecl, ForStmt, IfStmt, Item, Literal,
    MatchStmt, Member, Pattern, PragmaDecl, Prim, Stmt, StrArgs, StructDecl, TypeRef, UnaryOp,
    UnionDecl,
};
use crate::bytecode::{self, Const, Endian, Op, StrEnc};
use crate::value::Value;
use geode_core::diag::{Code, Diag};
use geode_core::error_codes;
use geode_core::{ByteRange, Offset};

/// 结构体/联合体嵌套或类型递归的最大深度。
const MAX_DEPTH: u32 = 64;

/// 静态展开循环的最大迭代次数。
const MAX_UNROLL: usize = 1024;

/// 结构体数组静态展开的最大元素数。
const MAX_ARRAY_UNROLL: u64 = 4096;

/// `str` 未指定 `size`/`max` 时的默认最大字节数。
const DEFAULT_STR_MAX: u32 = 4096;

type CResult<T> = Result<T, Vec<Diag>>;

/// 把一个可恢复错误变成带占位 `ByteRange` 的诊断数组。
fn diag(code: Code, detail: impl Into<String>) -> Vec<Diag> {
    vec![Diag::error(code, detail).with_range(ByteRange::empty_at(Offset::new(0)))]
}

/// BinSpec AST → 字节码编译器。
///
/// 编译器是无状态的；每次 [`Compiler::compile`] 都会创建一份新的内部状态。
/// 同一份 AST 可以重复编译不同入口。
#[derive(Debug, Clone, Copy, Default)]
pub struct Compiler;

impl Compiler {
    /// 创建一个编译器。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// 把 `program` 的 `entry` 编译成字节码。
    ///
    /// `entry` 可以是顶层 `struct`/`union` 名，也可以是 `fn` 名。找不到入口时
    /// 返回 `E4004`（与任务书一致；该码在错误码表中的正式含义是“定长结构
    /// 声明大小与成员实际大小不符”，这里复用它表示入口解析失败）。
    ///
    /// # Errors
    ///
    /// 返回编译期诊断数组；所有诊断都带占位 `ByteRange`（见模块文档）。
    pub fn compile(
        program: &crate::ast::Program,
        entry: &str,
    ) -> Result<bytecode::Program, Vec<Diag>> {
        Gen::new(program)?.compile_entry(entry)
    }
}

/// 顶层声明的索引表。
#[derive(Debug, Default)]
struct Decls {
    structs: HashMap<String, usize>,
    unions: HashMap<String, usize>,
    enums: HashMap<String, usize>,
    bitfields: HashMap<String, usize>,
    fns: HashMap<String, usize>,
}

impl Decls {
    fn build(program: &crate::ast::Program) -> Self {
        let mut decls = Self::default();
        for (index, item) in program.items.iter().enumerate() {
            match item {
                Item::Struct(decl) => {
                    decls.structs.entry(decl.name.clone()).or_insert(index);
                }
                Item::Union(decl) => {
                    decls.unions.entry(decl.name.clone()).or_insert(index);
                }
                Item::Enum(decl) => {
                    decls.enums.entry(decl.name.clone()).or_insert(index);
                }
                Item::Bitfield(decl) => {
                    decls.bitfields.entry(decl.name.clone()).or_insert(index);
                }
                Item::Fn(decl) => {
                    decls.fns.entry(decl.name.clone()).or_insert(index);
                }
                Item::Pragma(_) => {}
            }
        }
        decls
    }
}

/// 表达式求值环境：编译期常量变量 + 当前帧边界。
#[derive(Debug, Clone, Default)]
struct EvalEnv {
    vars: HashMap<String, Value>,
}

/// 编译一个结构/联合体帧时的静态游标状态。
#[derive(Debug, Clone, Copy)]
struct FrameCtx {
    start: Option<u64>,
    cursor: Option<u64>,
    end: Option<u64>,
    endian: Endian,
    depth: u32,
}

impl FrameCtx {
    fn root(endian: Endian) -> Self {
        Self {
            start: Some(0),
            cursor: Some(0),
            end: None,
            endian,
            depth: 0,
        }
    }
}

/// 编译器内部生成器。
struct Gen<'a> {
    program: &'a crate::ast::Program,
    decls: Decls,
    /// 每个顶层 item 生效的默认端序（pragma 作用域）。
    endian_at: Vec<Endian>,
    /// `#pragma image_base` 记录但不参与寻址转换。
    image_base: Option<u64>,
    code: Vec<Op>,
    consts: Vec<Const>,
    names: Vec<String>,
    types: Vec<String>,
    loops: Vec<LoopCtx>,
    /// 当前结构体作用域内已读取的标量字段（用于 `if` 条件重读字段值）。
    field_scopes: Vec<HashMap<String, FieldInfo>>,
}

/// 编译期循环上下文：运行期循环需要回填 `break`/`continue` 的跳转。
#[derive(Debug)]
enum LoopCtx {
    Runtime {
        breaks: Vec<usize>,
        continues: Vec<usize>,
        continue_target: Option<u32>,
    },
    Unrolled,
}

/// 语句/块的执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Normal,
    Break,
    Continue,
    Return,
}

/// 条件表达式中可重新读取的已读字段信息。
#[derive(Debug, Clone, Copy)]
struct FieldInfo {
    offset: u64,
    width: u8,
    kind: FieldKind,
    endian: Endian,
}

/// 条件字段的读取方式。
#[derive(Debug, Clone, Copy)]
enum FieldKind {
    Unsigned,
    Signed,
    Float,
    Bytes,
}

/// 表达式求值失败。
#[derive(Debug)]
struct EvalFail {
    code: Code,
    detail: String,
}

impl EvalFail {
    fn new(code: Code, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// 把 `EvalFail` 包装成诊断数组。
fn eval_diag(prefix: &str, fail: EvalFail) -> Vec<Diag> {
    diag(fail.code, format!("{prefix}：{}", fail.detail))
}

/// 把布局计算产生的诊断压平成表达式求值错误。
fn eval_fail_from_diags(errors: Vec<Diag>) -> EvalFail {
    match errors.first() {
        Some(first) => EvalFail::new(first.code, first.detail.clone()),
        None => EvalFail::new(error_codes::E4001, "布局计算失败"),
    }
}

impl<'a> Gen<'a> {
    fn new(program: &'a crate::ast::Program) -> CResult<Self> {
        let decls = Decls::build(program);
        let mut endian_at = Vec::with_capacity(program.items.len());
        let mut current = Endian::Little;
        for item in &program.items {
            endian_at.push(current);
            if let Item::Pragma(pragma) = item
                && pragma.name == "endian"
            {
                current = parse_endian_pragma(pragma)?;
            }
        }
        let mut compiler = Self {
            program,
            decls,
            endian_at,
            image_base: None,
            code: Vec::new(),
            consts: Vec::new(),
            names: Vec::new(),
            types: Vec::new(),
            loops: Vec::new(),
            field_scopes: Vec::new(),
        };
        compiler.read_image_base()?;
        Ok(compiler)
    }

    fn read_image_base(&mut self) -> CResult<()> {
        for item in &self.program.items {
            let Item::Pragma(pragma) = item else {
                continue;
            };
            if pragma.name != "image_base" {
                continue;
            }
            let Some(value) = &pragma.value else {
                return Err(diag(error_codes::E4001, "`#pragma image_base` 缺少常量值"));
            };
            let mut env = EvalEnv::default();
            let ctx = FrameCtx::root(Endian::Little);
            let evaluated = self
                .eval_u64(value, &mut env, &ctx)
                .map_err(|fail| eval_diag("image_base 必须是编译期常量", fail))?;
            self.image_base = Some(evaluated);
        }
        Ok(())
    }

    fn compile_entry(mut self, entry: &str) -> CResult<bytecode::Program> {
        // image_base 仅作为元信息记录；v1 不做 VA→文件偏移转换。
        let _image_base = self.image_base;
        let program = self.program;
        if let Some(&index) = self.decls.structs.get(entry) {
            let decl = match program.items.get(index) {
                Some(Item::Struct(decl)) => decl,
                _ => {
                    return Err(diag(
                        error_codes::E4001,
                        format!("入口 `{entry}` 的声明索引损坏"),
                    ));
                }
            };
            let endian = self.endian_at.get(index).copied().unwrap_or(Endian::Little);
            let mut ctx = FrameCtx::root(endian);
            let mut env = EvalEnv::default();
            self.compile_struct_decl(decl, entry, &mut ctx, &mut env, 0, endian)?;
        } else if let Some(&index) = self.decls.unions.get(entry) {
            let decl = match program.items.get(index) {
                Some(Item::Union(decl)) => decl,
                _ => {
                    return Err(diag(
                        error_codes::E4001,
                        format!("入口 `{entry}` 的声明索引损坏"),
                    ));
                }
            };
            let endian = self.endian_at.get(index).copied().unwrap_or(Endian::Little);
            let mut ctx = FrameCtx::root(endian);
            let mut env = EvalEnv::default();
            self.compile_union_decl(decl, entry, &mut ctx, &mut env, 0, endian)?;
        } else if let Some(&index) = self.decls.fns.get(entry) {
            let decl = match program.items.get(index) {
                Some(Item::Fn(decl)) => decl,
                _ => {
                    return Err(diag(
                        error_codes::E4001,
                        format!("入口 `{entry}` 的声明索引损坏"),
                    ));
                }
            };
            let endian = self.endian_at.get(index).copied().unwrap_or(Endian::Little);
            let mut ctx = FrameCtx::root(endian);
            let mut env = EvalEnv::default();
            self.compile_fn_decl(decl, &mut ctx, &mut env)?;
        } else {
            return Err(diag(
                error_codes::E4004,
                format!("找不到编译入口 `{entry}`（需要顶层 struct/union/fn）"),
            ));
        }
        self.emit(Op::Halt)?;
        Ok(bytecode::Program {
            code: self.code,
            consts: self.consts,
            names: self.names,
            types: self.types,
        })
    }

    fn emit(&mut self, op: Op) -> CResult<()> {
        let index = self.code.len();
        if u32::try_from(index).is_err() {
            return Err(diag(error_codes::E4001, "生成的字节码超过 u32 指令上限"));
        }
        self.code.push(op);
        Ok(())
    }

    fn code_len_u32(&self) -> CResult<u32> {
        u32::try_from(self.code.len())
            .map_err(|_| diag(error_codes::E4001, "生成的字节码超过 u32 指令上限"))
    }

    fn intern_name(&mut self, name: &str) -> CResult<u32> {
        if let Some(index) = self.names.iter().position(|candidate| candidate == name) {
            return u32::try_from(index)
                .map_err(|_| diag(error_codes::E4001, "名称表超过 u32 上限"));
        }
        let index = self.names.len();
        self.names.push(name.to_owned());
        u32::try_from(index).map_err(|_| diag(error_codes::E4001, "名称表超过 u32 上限"))
    }

    fn intern_type(&mut self, ty: &str) -> CResult<u32> {
        if let Some(index) = self.types.iter().position(|candidate| candidate == ty) {
            return u32::try_from(index)
                .map_err(|_| diag(error_codes::E4001, "类型表超过 u32 上限"));
        }
        let index = self.types.len();
        self.types.push(ty.to_owned());
        u32::try_from(index).map_err(|_| diag(error_codes::E4001, "类型表超过 u32 上限"))
    }

    fn emit_field(&mut self, name: &str, ty: &str) -> CResult<()> {
        let name_idx = self.intern_name(name)?;
        let type_idx = self.intern_type(ty)?;
        self.emit(Op::EmitField { name_idx, type_idx })
    }

    fn emit_mark_struct(&mut self, name: &str, size_hint: Option<u64>) -> CResult<()> {
        let name_idx = self.intern_name(name)?;
        let hint = match size_hint {
            Some(size) => u32::try_from(size).map_err(|_| {
                diag(
                    error_codes::E4001,
                    format!("结构 `{name}` 的大小超过 u32 上限"),
                )
            })?,
            None => 0,
        };
        self.emit(Op::MarkStruct {
            name_idx,
            size_hint: hint,
        })
    }

    fn push_const(&mut self, value: Const) -> CResult<u32> {
        let index = self.consts.len();
        self.consts.push(value);
        u32::try_from(index).map_err(|_| diag(error_codes::E4001, "常量池超过 u32 上限"))
    }

    fn push_value(&mut self, value: &Value) -> CResult<()> {
        let constant = match value {
            Value::UInt(v) => Const::UInt(*v),
            Value::Int(v) => Const::Int(*v),
            Value::Float(v) => Const::Float(*v),
            Value::Bool(v) => Const::UInt(u128::from(*v)),
            Value::Str(v) => Const::Str(v.clone()),
            Value::Bytes(v) => Const::Bytes(v.clone()),
            Value::None => Const::UInt(0),
        };
        let index = self.push_const(constant)?;
        self.emit(Op::PushConst(index))
    }

    fn push_bool(&mut self, value: bool) -> CResult<()> {
        let index = self.push_const(Const::UInt(u128::from(value)))?;
        self.emit(Op::PushConst(index))
    }
}

impl Gen<'_> {
    fn compile_struct_decl(
        &mut self,
        decl: &StructDecl,
        node_name: &str,
        outer: &mut FrameCtx,
        env: &mut EvalEnv,
        depth: u32,
        default_endian: Endian,
    ) -> CResult<()> {
        if depth >= MAX_DEPTH {
            return Err(diag(
                error_codes::E4007,
                format!("结构 `{}` 的嵌套/递归深度超过上限 {MAX_DEPTH}", decl.name),
            ));
        }
        let declared = match &decl.size {
            Some(expr) => Some(self.eval_u64(expr, env, outer).map_err(|fail| {
                eval_diag(
                    &format!("结构 `{}` 的定长大小必须是编译期常量", decl.name),
                    fail,
                )
            })?),
            None => None,
        };
        let layout_size = match declared {
            Some(size) => Some(size),
            None => self.size_of_struct_decl(decl, env, outer, depth)?,
        };
        let start = outer.cursor;
        let end = match (start, layout_size) {
            (Some(start), Some(size)) => Some(start.checked_add(size).ok_or_else(|| {
                diag(
                    error_codes::E4001,
                    format!("结构 `{}` 的结束偏移溢出 u64", decl.name),
                )
            })?),
            _ => None,
        };
        let mut ctx = FrameCtx {
            start,
            cursor: start,
            end,
            endian: default_endian,
            depth: depth.saturating_add(1),
        };
        self.emit_mark_struct(node_name, declared)?;
        self.field_scopes.push(HashMap::new());
        for member in &decl.members {
            self.compile_member(member, &mut ctx, env)?;
        }
        if let Some(size) = declared {
            self.pad_to_declared(&mut ctx, size)?;
        }
        self.field_scopes.pop();
        self.emit(Op::EndStruct)?;
        outer.cursor = ctx.cursor;
        Ok(())
    }

    fn compile_union_decl(
        &mut self,
        decl: &UnionDecl,
        node_name: &str,
        outer: &mut FrameCtx,
        env: &mut EvalEnv,
        depth: u32,
        default_endian: Endian,
    ) -> CResult<()> {
        if depth >= MAX_DEPTH {
            return Err(diag(
                error_codes::E4007,
                format!("联合体 `{}` 的嵌套/递归深度超过上限 {MAX_DEPTH}", decl.name),
            ));
        }
        let size = match &decl.size {
            Some(expr) => self.eval_u64(expr, env, outer).map_err(|fail| {
                eval_diag(
                    &format!("联合体 `{}` 的定长大小必须是编译期常量", decl.name),
                    fail,
                )
            })?,
            None => self
                .size_of_union_decl(decl, env, outer, depth)?
                .ok_or_else(|| {
                    diag(
                        error_codes::E4001,
                        format!(
                            "v1 要求联合体 `{}` 具有编译期已知大小；请写 `union {} : N`",
                            decl.name, decl.name
                        ),
                    )
                })?,
        };
        let Some(start) = outer.cursor else {
            return Err(diag(
                error_codes::E4001,
                format!(
                    "v1 要求联合体 `{}` 的起始偏移在编译期已知（前一个字段不能是变长字段）",
                    decl.name
                ),
            ));
        };
        let end = start.checked_add(size).ok_or_else(|| {
            diag(
                error_codes::E4001,
                format!("联合体 `{}` 的结束偏移溢出 u64", decl.name),
            )
        })?;
        let mut ctx = FrameCtx {
            start: Some(start),
            cursor: Some(start),
            end: Some(end),
            endian: default_endian,
            depth: depth.saturating_add(1),
        };
        self.emit_mark_struct(node_name, Some(size))?;
        self.field_scopes.push(HashMap::new());
        for member in &decl.members {
            let mut member = member.clone();
            if member.at.is_none() {
                member.at = Some(Expr::Literal(Literal::Int(u128::from(start))));
            }
            ctx.cursor = Some(start);
            self.compile_member(&member, &mut ctx, env)?;
        }
        self.field_scopes.pop();
        self.emit(Op::Seek(end))?;
        self.emit(Op::EndStruct)?;
        outer.cursor = Some(end);
        Ok(())
    }

    fn pad_to_declared(&mut self, ctx: &mut FrameCtx, size: u64) -> CResult<()> {
        let Some(start) = ctx.start else {
            return Ok(());
        };
        let target = start
            .checked_add(size)
            .ok_or_else(|| diag(error_codes::E4001, "结构结束偏移溢出 u64".to_owned()))?;
        let Some(cursor) = ctx.cursor else {
            self.emit(Op::Seek(target))?;
            ctx.cursor = Some(target);
            return Ok(());
        };
        let consumed = cursor.saturating_sub(start);
        if consumed > size {
            return Err(diag(
                error_codes::E4004,
                format!(
                    "定长结构声明 {size} 字节，但成员实际消费 {consumed} 字节（超出 {} 字节）",
                    consumed - size
                ),
            ));
        }
        if consumed < size {
            let padding = size - consumed;
            let delta = i64::try_from(padding)
                .map_err(|_| diag(error_codes::E4001, "结构尾部 padding 超过 i64 上限"))?;
            self.emit(Op::Skip(delta))?;
            ctx.cursor = Some(target);
        }
        Ok(())
    }

    fn compile_member(
        &mut self,
        member: &Member,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<()> {
        if is_padding(member) {
            let count = match &member.array {
                Some(ArraySpec::Fixed(expr)) => self
                    .eval_u64(expr, env, ctx)
                    .map_err(|fail| eval_diag("padding 长度必须是编译期常量", fail))?,
                _ => {
                    return Err(diag(
                        error_codes::E4001,
                        "padding 必须带常量长度，例如 `padding[4]`",
                    ));
                }
            };
            if count > 0 {
                let delta = i64::try_from(count)
                    .map_err(|_| diag(error_codes::E4001, "padding 长度超过 i64 上限"))?;
                self.emit(Op::Skip(delta))?;
                advance(ctx, count)?;
            }
            return Ok(());
        }

        if let Some(condition) = &member.cond {
            let value = match self.eval_bool(condition, env, ctx) {
                Ok(value) => Some(value),
                Err(_) if self.expr_uses_field(condition) => {
                    self.compile_condition(condition, ctx, env)?;
                    None
                }
                Err(fail) => {
                    return Err(eval_diag(
                        &format!("字段 `{}` 的 if 条件必须是编译期常量", member.name),
                        fail,
                    ));
                }
            };
            if let Some(value) = value {
                self.push_bool(value)?;
            }
            let patch = self.code.len();
            self.emit(Op::JmpIfNot(0))?;
            if value != Some(false) {
                self.compile_member_body(member, ctx, env)?;
            }
            let target = self.code_len_u32()?;
            patch_jump(&mut self.code, patch, target)?;
            return Ok(());
        }

        self.compile_member_body(member, ctx, env)
    }

    fn compile_member_body(
        &mut self,
        member: &Member,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<()> {
        let endian = member_endian(member, ctx.endian)?;
        if let Some(offset_expr) = &member.at {
            if !is_compile_time_expr(offset_expr) {
                return Err(diag(
                    error_codes::E4001,
                    format!(
                        "字段 `{}` 的 `@` 偏移必须是编译期常量（不能使用 `$`/`$size`/`$start`/`$end`）",
                        member.name
                    ),
                ));
            }
            let offset = self.eval_u64(offset_expr, env, ctx).map_err(|fail| {
                eval_diag(
                    &format!("字段 `{}` 的 `@` 偏移必须是编译期常量", member.name),
                    fail,
                )
            })?;
            self.emit(Op::Seek(offset))?;
            ctx.cursor = Some(offset);
        }
        let field_offset = ctx.cursor;
        let field_info = if member.array.is_none() {
            self.field_info_for(&member.ty, endian)
        } else {
            None
        };
        let saved = ctx.endian;
        ctx.endian = endian;
        let result = self.compile_type(
            &member.ty,
            &member.name,
            member.array.as_ref(),
            ctx,
            env,
            ctx.depth,
        );
        ctx.endian = saved;
        if let (Some(offset), Some(info), Some(scope)) =
            (field_offset, field_info, self.field_scopes.last_mut())
        {
            scope.insert(member.name.clone(), FieldInfo { offset, ..info });
        }
        result
    }

    fn compile_type(
        &mut self,
        ty: &TypeRef,
        name: &str,
        array: Option<&ArraySpec>,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
        depth: u32,
    ) -> CResult<()> {
        match array {
            Some(spec) => self.compile_array(ty, name, spec, ctx, env, depth),
            None => self.compile_scalar(ty, name, ctx, env, depth),
        }
    }

    fn compile_array(
        &mut self,
        ty: &TypeRef,
        name: &str,
        spec: &ArraySpec,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
        depth: u32,
    ) -> CResult<()> {
        match spec {
            ArraySpec::Fixed(expr) => {
                let count = self.eval_u64(expr, env, ctx).map_err(|fail| {
                    eval_diag(&format!("数组 `{name}` 的长度必须是编译期常量"), fail)
                })?;
                self.compile_fixed_array(ty, name, count, ctx, env, depth)
            }
            ArraySpec::Until(expr) => self.compile_until_array(ty, name, expr, ctx, env, depth),
            ArraySpec::Unsized => {
                let (Some(end), Some(cursor)) = (ctx.end, ctx.cursor) else {
                    return Err(diag(
                        error_codes::E4001,
                        format!(
                            "v1 不支持无界 `[]` 数组 `{name}`：需要结构体有编译期已知大小（如 `struct S : N`）"
                        ),
                    ));
                };
                let Some(elem_size) = self.size_of_type(ty, env, ctx, depth)? else {
                    return Err(diag(
                        error_codes::E4001,
                        format!("v1 不支持元素大小可变的 `[]` 数组 `{name}`（元素必须是定长类型）"),
                    ));
                };
                if elem_size == 0 {
                    return Err(diag(
                        error_codes::E4001,
                        format!("数组 `{name}` 的元素大小不能为 0"),
                    ));
                }
                let remaining = end.checked_sub(cursor).ok_or_else(|| {
                    diag(
                        error_codes::E4001,
                        format!("数组 `{name}` 的游标游标越过结构末尾"),
                    )
                })?;
                if remaining % elem_size != 0 {
                    return Err(diag(
                        error_codes::E4004,
                        format!(
                            "数组 `{name}` 的剩余区间 {remaining} 字节不能按元素宽度 {elem_size} 整除"
                        ),
                    ));
                }
                self.compile_fixed_array(ty, name, remaining / elem_size, ctx, env, depth)
            }
        }
    }

    fn compile_fixed_array(
        &mut self,
        ty: &TypeRef,
        name: &str,
        count: u64,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
        depth: u32,
    ) -> CResult<()> {
        let label = type_label(ty);
        if count == 0 {
            self.emit_field(name, &format!("{label}[0]"))?;
            return Ok(());
        }
        let Some(elem_size) = self.size_of_type(ty, env, ctx, depth)? else {
            return Err(diag(
                error_codes::E4001,
                format!(
                    "v1 不支持变长元素 `{label}` 的定长数组 `{name}[{count}]`（请改用 `[]` 或 `until`）"
                ),
            ));
        };
        if elem_size == 0 {
            return Err(diag(
                error_codes::E4001,
                format!("数组 `{name}` 的元素大小不能为 0"),
            ));
        }
        let total = count.checked_mul(elem_size).ok_or_else(|| {
            diag(
                error_codes::E4001,
                format!("数组 `{name}` 的总字节数溢出 u64"),
            )
        })?;
        if self.is_leaf_type(ty) {
            let len = u32::try_from(total).map_err(|_| {
                diag(
                    error_codes::E4001,
                    format!("数组 `{name}` 超过单次 LoadBytes 的 u32 上限"),
                )
            })?;
            self.emit(Op::LoadBytes { len })?;
            self.emit_field(name, &format!("{label}[{count}]"))?;
            advance(ctx, total)?;
        } else {
            if count > MAX_ARRAY_UNROLL {
                return Err(diag(
                    error_codes::E4001,
                    format!(
                        "数组 `{name}` 的元素需要逐个展开，v1 上限为 {MAX_ARRAY_UNROLL} 个元素"
                    ),
                ));
            }
            for index in 0..count {
                let child_name = format!("{name}[{index}]");
                self.compile_scalar(ty, &child_name, ctx, env, depth.saturating_add(1))?;
            }
        }
        Ok(())
    }

    fn compile_until_array(
        &mut self,
        ty: &TypeRef,
        name: &str,
        term: &Expr,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
        depth: u32,
    ) -> CResult<()> {
        let Some(load) = self.scalar_load(ty, ctx, depth)? else {
            return Err(diag(
                error_codes::E4001,
                format!(
                    "v1 只支持基础标量数组使用 `until(x)`（数组 `{name}` 的元素类型是 `{}`）",
                    type_label(ty)
                ),
            ));
        };
        let term_value = self.eval_expr(term, env, ctx).map_err(|fail| {
            eval_diag(
                &format!("数组 `{name}` 的 until 终止值必须是编译期常量"),
                fail,
            )
        })?;
        let start = self.code_len_u32()?;
        self.emit(load.op())?;
        self.emit(Op::Dup)?;
        self.push_value(&term_value)?;
        self.emit(Op::Eq)?;
        // 真：栈顶仍保留刚读到的值，丢弃它并退出；假：EmitField 消费该值。
        let emit_patch = self.code.len();
        self.emit(Op::JmpIfNot(0))?;
        self.emit(Op::Drop)?;
        let done_patch = self.code.len();
        self.emit(Op::Jmp(0))?;
        let emit_start = self.code_len_u32()?;
        patch_jump(&mut self.code, emit_patch, emit_start)?;
        self.emit_field(name, &type_label(ty))?;
        self.emit(Op::Jmp(start))?;
        let done = self.code_len_u32()?;
        patch_jump(&mut self.code, done_patch, done)?;
        ctx.cursor = None;
        Ok(())
    }

    fn compile_scalar(
        &mut self,
        ty: &TypeRef,
        name: &str,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
        depth: u32,
    ) -> CResult<()> {
        match ty {
            TypeRef::Prim(prim) => {
                let endian = ctx.endian;
                let width = prim.byte_size() as u8;
                match prim {
                    Prim::U8
                    | Prim::U16
                    | Prim::U24
                    | Prim::U32
                    | Prim::U48
                    | Prim::U64
                    | Prim::U128
                    | Prim::Bool
                    | Prim::Byte => {
                        self.emit(Op::LoadU { width, endian })?;
                    }
                    Prim::S8
                    | Prim::S16
                    | Prim::S24
                    | Prim::S32
                    | Prim::S48
                    | Prim::S64
                    | Prim::S128 => {
                        self.emit(Op::LoadI { width, endian })?;
                    }
                    Prim::F16 | Prim::F32 | Prim::F64 => {
                        self.emit(Op::LoadF { width, endian })?;
                    }
                    Prim::Char => {
                        self.emit(Op::LoadBytes { len: 1 })?;
                    }
                }
                self.emit_field(name, prim.as_str())?;
                advance(ctx, u64::from(width))?;
                Ok(())
            }
            TypeRef::Ptr { kind, .. } => {
                let endian = ctx.endian;
                let width = kind.byte_size() as u8;
                self.emit(Op::LoadU { width, endian })?;
                self.emit_field(name, kind.as_str())?;
                advance(ctx, u64::from(width))
            }
            TypeRef::Str(args) => self.compile_str(args, name, ctx, env),
            TypeRef::Named(type_name) => {
                if let Some(index) = self.decls.structs.get(type_name).copied() {
                    let program = self.program;
                    let decl = match program.items.get(index) {
                        Some(Item::Struct(decl)) => decl,
                        _ => {
                            return Err(diag(
                                error_codes::E4001,
                                format!("类型 `{type_name}` 的声明索引损坏"),
                            ));
                        }
                    };
                    let endian = self.endian_at.get(index).copied().unwrap_or(Endian::Little);
                    self.compile_struct_decl(decl, name, ctx, env, depth.saturating_add(1), endian)
                } else if let Some(index) = self.decls.unions.get(type_name).copied() {
                    let program = self.program;
                    let decl = match program.items.get(index) {
                        Some(Item::Union(decl)) => decl,
                        _ => {
                            return Err(diag(
                                error_codes::E4001,
                                format!("类型 `{type_name}` 的声明索引损坏"),
                            ));
                        }
                    };
                    let endian = self.endian_at.get(index).copied().unwrap_or(Endian::Little);
                    self.compile_union_decl(decl, name, ctx, env, depth.saturating_add(1), endian)
                } else if let Some(index) = self.decls.enums.get(type_name).copied() {
                    let program = self.program;
                    let repr = match program.items.get(index) {
                        Some(Item::Enum(decl)) => decl.repr.as_ref(),
                        _ => None,
                    };
                    self.compile_repr(repr, name, type_name, ctx)
                } else if let Some(index) = self.decls.bitfields.get(type_name).copied() {
                    let program = self.program;
                    let repr = match program.items.get(index) {
                        Some(Item::Bitfield(decl)) => decl.repr.as_ref(),
                        _ => None,
                    };
                    self.compile_repr(repr, name, type_name, ctx)
                } else {
                    Err(diag(
                        error_codes::E4003,
                        format!("未定义类型 `{type_name}`（字段 `{name}`）"),
                    ))
                }
            }
            TypeRef::InlineStruct(decl) => {
                let endian = ctx.endian;
                self.compile_struct_decl(decl, name, ctx, env, depth.saturating_add(1), endian)
            }
        }
    }

    fn compile_repr(
        &mut self,
        repr: Option<&TypeRef>,
        name: &str,
        type_name: &str,
        ctx: &mut FrameCtx,
    ) -> CResult<()> {
        let prim = match repr {
            None => Prim::U32,
            Some(TypeRef::Prim(prim)) => *prim,
            Some(other) => {
                return Err(diag(
                    error_codes::E4001,
                    format!(
                        "v1 只支持基础整数作为 enum/bitfield 的 repr（`{type_name}` 的 repr 是 `{}`）",
                        type_label(other)
                    ),
                ));
            }
        };
        let endian = ctx.endian;
        let width = prim.byte_size() as u8;
        match prim {
            Prim::U8
            | Prim::U16
            | Prim::U24
            | Prim::U32
            | Prim::U48
            | Prim::U64
            | Prim::U128
            | Prim::Bool
            | Prim::Byte => {
                self.emit(Op::LoadU { width, endian })?;
            }
            Prim::S8 | Prim::S16 | Prim::S24 | Prim::S32 | Prim::S48 | Prim::S64 | Prim::S128 => {
                self.emit(Op::LoadI { width, endian })?;
            }
            Prim::F16 | Prim::F32 | Prim::F64 => {
                return Err(diag(
                    error_codes::E4001,
                    format!("v1 不支持浮点 repr（`{type_name}`）"),
                ));
            }
            Prim::Char => {
                return Err(diag(
                    error_codes::E4001,
                    format!("v1 不支持 char repr（`{type_name}`）"),
                ));
            }
        }
        self.emit_field(name, type_name)?;
        advance(ctx, u64::from(width))
    }

    fn compile_str(
        &mut self,
        args: &StrArgs,
        name: &str,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<()> {
        let enc = parse_str_enc(args.enc.as_deref())?;
        let term = match &args.term {
            Some(expr) => {
                let value = self.eval_u64(expr, env, ctx).map_err(|fail| {
                    eval_diag(&format!("str `{name}` 的 term 必须是编译期常量"), fail)
                })?;
                if value > u64::from(u8::MAX) {
                    return Err(diag(
                        error_codes::E4001,
                        format!("str `{name}` 的 term 必须在 0..=255"),
                    ));
                }
                Some(value as u8)
            }
            None => None,
        };
        let size = match &args.size {
            Some(expr) => Some(self.eval_u64(expr, env, ctx).map_err(|fail| {
                eval_diag(&format!("str `{name}` 的 size 必须是编译期常量"), fail)
            })?),
            None => None,
        };
        let max = match &args.max {
            Some(expr) => Some(self.eval_u64(expr, env, ctx).map_err(|fail| {
                eval_diag(&format!("str `{name}` 的 max 必须是编译期常量"), fail)
            })?),
            None => None,
        };
        let effective = match (size, max) {
            (Some(size), Some(max)) => size.min(max),
            (Some(size), None) => size,
            (None, Some(max)) => max,
            (None, None) => {
                // 定长结构内默认读到当前帧末尾；否则使用保守上限。
                ctx.end
                    .zip(ctx.cursor)
                    .and_then(|(end, cursor)| end.checked_sub(cursor))
                    .unwrap_or(u64::from(DEFAULT_STR_MAX))
            }
        };
        let max = u32::try_from(effective).map_err(|_| {
            diag(
                error_codes::E4001,
                format!("str `{name}` 的 size/max 超过 u32 上限"),
            )
        })?;
        self.emit(Op::LoadStr { enc, term, max })?;
        self.emit_field(name, "str")?;
        if term.is_none() {
            advance(ctx, u64::from(max))?;
        } else {
            // 终止字节之前的长度只有运行期知道，后续静态偏移未知。
            ctx.cursor = None;
        }
        Ok(())
    }
}

impl Gen<'_> {
    fn compile_fn_decl(
        &mut self,
        decl: &FnDecl,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<()> {
        if !decl.params.is_empty() {
            return Err(diag(
                error_codes::E4001,
                format!(
                    "v1 不支持带参数的入口函数 `{}`（参数没有运行期局部变量可绑定）",
                    decl.name
                ),
            ));
        }
        let flow = self.compile_block(&decl.body, ctx, env)?;
        if flow == Flow::Return {
            return Ok(());
        }
        Ok(())
    }

    fn compile_block(
        &mut self,
        block: &Block,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        for stmt in &block.stmts {
            let flow = self.compile_stmt(stmt, ctx, env)?;
            if flow != Flow::Normal {
                return Ok(flow);
            }
        }
        Ok(Flow::Normal)
    }

    fn compile_stmt(
        &mut self,
        stmt: &Stmt,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        match stmt {
            Stmt::Let(let_stmt) => {
                let Some(init) = &let_stmt.init else {
                    return Err(diag(
                        error_codes::E4001,
                        format!("v1 的 `let {}` 必须有编译期常量初始值", let_stmt.name),
                    ));
                };
                let value = self.eval_expr(init, env, ctx).map_err(|fail| {
                    eval_diag(
                        &format!("v1 的 `let {}` 只支持编译期常量", let_stmt.name),
                        fail,
                    )
                })?;
                env.vars.insert(let_stmt.name.clone(), value);
                Ok(Flow::Normal)
            }
            Stmt::If(if_stmt) => self.compile_if_stmt(if_stmt, ctx, env),
            Stmt::Match(match_stmt) => self.compile_match_stmt(match_stmt, ctx, env),
            Stmt::For(for_stmt) => self.compile_for_stmt(for_stmt, ctx, env),
            Stmt::While(while_stmt) => {
                self.compile_while_stmt(&while_stmt.cond, &while_stmt.body, ctx, env)
            }
            Stmt::Return(value) => {
                if let Some(expr) = value {
                    let _ = self
                        .eval_expr(expr, env, ctx)
                        .map_err(|fail| eval_diag("v1 的 return 值必须是编译期常量", fail))?;
                }
                self.emit(Op::Halt)?;
                Ok(Flow::Return)
            }
            Stmt::Break => self.compile_loop_jump(true),
            Stmt::Continue => self.compile_loop_jump(false),
            Stmt::Expr(expr) => {
                let _ = self.eval_expr(expr, env, ctx).map_err(|fail| {
                    eval_diag("v1 的表达式语句必须是编译期可求值的常量表达式", fail)
                })?;
                Ok(Flow::Normal)
            }
            Stmt::Member(member) => {
                self.compile_member(member, ctx, env)?;
                Ok(Flow::Normal)
            }
        }
    }

    fn compile_if_stmt(
        &mut self,
        stmt: &IfStmt,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        let condition = self
            .eval_bool(&stmt.cond, env, ctx)
            .map_err(|fail| eval_diag("v1 的 if 条件必须是编译期常量", fail))?;
        let mut branch_env = env.clone();
        let flow = if condition {
            self.compile_block(&stmt.then_branch, ctx, &mut branch_env)?
        } else {
            match &stmt.else_branch {
                None => Flow::Normal,
                Some(ElseBranch::Block(block)) => {
                    self.compile_block(block, ctx, &mut branch_env)?
                }
                Some(ElseBranch::If(nested)) => {
                    self.compile_if_stmt(nested, ctx, &mut branch_env)?
                }
            }
        };
        merge_env(env, &branch_env);
        Ok(flow)
    }

    fn compile_match_stmt(
        &mut self,
        stmt: &MatchStmt,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        let scrutinee = self
            .eval_expr(&stmt.scrutinee, env, ctx)
            .map_err(|fail| eval_diag("v1 的 match 被匹配值必须是编译期常量", fail))?;
        for arm in &stmt.arms {
            let matched = match &arm.pattern {
                Pattern::Wildcard => true,
                Pattern::Expr(expr) => {
                    let value = self
                        .eval_expr(expr, env, ctx)
                        .map_err(|fail| eval_diag("v1 的 match 分支模式必须是编译期常量", fail))?;
                    values_equal(&scrutinee, &value)
                }
            };
            if matched {
                let mut branch_env = env.clone();
                let flow = self.compile_block(&arm.body, ctx, &mut branch_env)?;
                merge_env(env, &branch_env);
                return Ok(flow);
            }
        }
        Ok(Flow::Normal)
    }

    fn compile_while_stmt(
        &mut self,
        condition: &Expr,
        body: &Block,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        let value = self
            .eval_bool(condition, env, ctx)
            .map_err(|fail| eval_diag("v1 的 while 条件必须是编译期常量", fail))?;
        if !value {
            return Ok(Flow::Normal);
        }
        if expr_uses_vars(condition, env) {
            self.compile_unrolled_while(condition, body, ctx, env)
        } else {
            self.compile_runtime_while(body, ctx, env)
        }
    }

    fn compile_unrolled_while(
        &mut self,
        condition: &Expr,
        body: &Block,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        self.loops.push(LoopCtx::Unrolled);
        let mut iterations = 0usize;
        let result = loop {
            let value = match self.eval_bool(condition, env, ctx) {
                Ok(value) => value,
                Err(fail) => break Err(eval_diag("while 条件求值失败", fail)),
            };
            if !value {
                break Ok(Flow::Normal);
            }
            if iterations >= MAX_UNROLL {
                break Err(diag(
                    error_codes::E4001,
                    format!("while 静态展开超过 {MAX_UNROLL} 次（条件变量可能没有向终止方向变化）"),
                ));
            }
            iterations += 1;
            let mut body_env = env.clone();
            let flow = match self.compile_block(body, ctx, &mut body_env) {
                Ok(flow) => flow,
                Err(errors) => break Err(errors),
            };
            merge_env(env, &body_env);
            match flow {
                Flow::Break => break Ok(Flow::Normal),
                Flow::Continue | Flow::Normal => {}
                Flow::Return => break Ok(Flow::Return),
            }
        };
        self.loops.pop();
        result
    }

    fn compile_runtime_while(
        &mut self,
        body: &Block,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        let start = self.code_len_u32()?;
        self.loops.push(LoopCtx::Runtime {
            breaks: Vec::new(),
            continues: Vec::new(),
            continue_target: Some(start),
        });
        self.push_bool(true)?;
        let condition_patch = self.code.len();
        self.emit(Op::JmpIfNot(0))?;
        let mut body_env = env.clone();
        let flow = self.compile_block(body, ctx, &mut body_env)?;
        merge_env(env, &body_env);
        self.emit(Op::Jmp(start))?;
        let end = self.code_len_u32()?;
        patch_jump(&mut self.code, condition_patch, end)?;
        self.finish_runtime_loop(end)?;
        // 循环体至少可能读取字节；后续静态偏移保守地视为未知。
        ctx.cursor = None;
        Ok(if flow == Flow::Return {
            Flow::Return
        } else {
            Flow::Normal
        })
    }

    fn compile_for_stmt(
        &mut self,
        stmt: &ForStmt,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        let mut loop_env = env.clone();
        if let Some(init) = &stmt.init {
            let flow = self.compile_stmt(init, ctx, &mut loop_env)?;
            if flow != Flow::Normal {
                return Ok(flow);
            }
        }
        let uses_vars = stmt
            .cond
            .as_ref()
            .is_some_and(|condition| expr_uses_vars(condition, &loop_env))
            || stmt
                .step
                .as_ref()
                .is_some_and(|step| expr_uses_vars(step, &loop_env));
        if uses_vars {
            let flow = self.compile_unrolled_for(stmt, ctx, &mut loop_env)?;
            merge_env(env, &loop_env);
            return Ok(flow);
        }
        // 运行期 for 不会重新求值条件；因此条件必须能在这里折叠为 true。
        if let Some(condition) = &stmt.cond {
            let value = self
                .eval_bool(condition, &mut loop_env, ctx)
                .map_err(|fail| eval_diag("v1 的 for 条件必须是编译期常量", fail))?;
            if !value {
                return Ok(Flow::Normal);
            }
        }
        let flow = self.compile_runtime_for(stmt, ctx, &mut loop_env)?;
        merge_env(env, &loop_env);
        Ok(flow)
    }

    fn compile_unrolled_for(
        &mut self,
        stmt: &ForStmt,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        self.loops.push(LoopCtx::Unrolled);
        let mut iterations = 0usize;
        let result = loop {
            let condition = match &stmt.cond {
                Some(condition) => match self.eval_bool(condition, env, ctx) {
                    Ok(value) => value,
                    Err(fail) => break Err(eval_diag("for 条件求值失败", fail)),
                },
                None => true,
            };
            if !condition {
                break Ok(Flow::Normal);
            }
            if iterations >= MAX_UNROLL {
                break Err(diag(
                    error_codes::E4001,
                    format!("for 静态展开超过 {MAX_UNROLL} 次（条件变量可能没有向终止方向变化）"),
                ));
            }
            iterations += 1;
            let mut body_env = env.clone();
            let flow = match self.compile_block(&stmt.body, ctx, &mut body_env) {
                Ok(flow) => flow,
                Err(errors) => break Err(errors),
            };
            merge_env(env, &body_env);
            match flow {
                Flow::Break => break Ok(Flow::Normal),
                Flow::Return => break Ok(Flow::Return),
                Flow::Continue | Flow::Normal => {}
            }
            if let Some(step) = &stmt.step
                && let Err(fail) = self.eval_expr(step, env, ctx)
            {
                break Err(eval_diag("for 步进表达式求值失败", fail));
            }
        };
        self.loops.pop();
        result
    }

    fn compile_runtime_for(
        &mut self,
        stmt: &ForStmt,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<Flow> {
        let start = self.code_len_u32()?;
        self.loops.push(LoopCtx::Runtime {
            breaks: Vec::new(),
            continues: Vec::new(),
            continue_target: None,
        });
        self.push_bool(true)?;
        let condition_patch = self.code.len();
        self.emit(Op::JmpIfNot(0))?;
        let mut body_env = env.clone();
        let flow = self.compile_block(&stmt.body, ctx, &mut body_env)?;
        merge_env(env, &body_env);
        let continue_target = self.code_len_u32()?;
        if let Some(step) = &stmt.step {
            let _ = self
                .eval_expr(step, env, ctx)
                .map_err(|fail| eval_diag("for 步进表达式必须是编译期常量", fail))?;
        }
        self.emit(Op::Jmp(start))?;
        let end = self.code_len_u32()?;
        patch_jump(&mut self.code, condition_patch, end)?;
        if let Some(LoopCtx::Runtime {
            breaks, continues, ..
        }) = self.loops.pop()
        {
            for index in continues {
                patch_jump(&mut self.code, index, continue_target)?;
            }
            for index in breaks {
                patch_jump(&mut self.code, index, end)?;
            }
        }
        ctx.cursor = None;
        Ok(flow)
    }

    fn compile_loop_jump(&mut self, is_break: bool) -> CResult<Flow> {
        let Some(loop_ctx) = self.loops.last_mut() else {
            return Err(diag(
                error_codes::E4001,
                if is_break {
                    "`break` 不在循环内".to_owned()
                } else {
                    "`continue` 不在循环内".to_owned()
                },
            ));
        };
        match loop_ctx {
            LoopCtx::Unrolled => Ok(if is_break {
                Flow::Break
            } else {
                Flow::Continue
            }),
            LoopCtx::Runtime {
                breaks, continues, ..
            } => {
                let index = self.code.len();
                if u32::try_from(index).is_err() {
                    return Err(diag(error_codes::E4001, "生成的字节码超过 u32 指令上限"));
                }
                self.code.push(Op::Jmp(0));
                if is_break {
                    breaks.push(index);
                } else {
                    continues.push(index);
                }
                Ok(if is_break {
                    Flow::Break
                } else {
                    Flow::Continue
                })
            }
        }
    }

    fn finish_runtime_loop(&mut self, end: u32) -> CResult<()> {
        if let Some(LoopCtx::Runtime {
            breaks,
            continues,
            continue_target,
            ..
        }) = self.loops.pop()
        {
            let continue_target = continue_target.unwrap_or(end);
            for index in continues {
                patch_jump(&mut self.code, index, continue_target)?;
            }
            for index in breaks {
                patch_jump(&mut self.code, index, end)?;
            }
        }
        Ok(())
    }
}

/// 一个可循环读取的标量加载描述。
#[derive(Debug, Clone, Copy)]
struct ScalarLoad {
    width: u8,
    signed: bool,
    float: bool,
    endian: Endian,
}

impl ScalarLoad {
    fn op(self) -> Op {
        if self.float {
            Op::LoadF {
                width: self.width,
                endian: self.endian,
            }
        } else if self.signed {
            Op::LoadI {
                width: self.width,
                endian: self.endian,
            }
        } else {
            Op::LoadU {
                width: self.width,
                endian: self.endian,
            }
        }
    }
}

impl Gen<'_> {
    fn scalar_load(&self, ty: &TypeRef, ctx: &FrameCtx, depth: u32) -> CResult<Option<ScalarLoad>> {
        match ty {
            TypeRef::Prim(prim) => Ok(Some(ScalarLoad {
                width: prim.byte_size() as u8,
                signed: matches!(
                    prim,
                    Prim::S8
                        | Prim::S16
                        | Prim::S24
                        | Prim::S32
                        | Prim::S48
                        | Prim::S64
                        | Prim::S128
                ),
                float: matches!(prim, Prim::F16 | Prim::F32 | Prim::F64),
                endian: ctx.endian,
            })),
            TypeRef::Ptr { kind, .. } => Ok(Some(ScalarLoad {
                width: kind.byte_size() as u8,
                signed: false,
                float: false,
                endian: ctx.endian,
            })),
            TypeRef::Named(name) => {
                if let Some(index) = self.decls.enums.get(name).copied() {
                    let program = self.program;
                    let repr = match program.items.get(index) {
                        Some(Item::Enum(decl)) => decl.repr.as_ref(),
                        _ => None,
                    };
                    self.repr_load(repr, name, ctx, depth)
                } else if let Some(index) = self.decls.bitfields.get(name).copied() {
                    let program = self.program;
                    let repr = match program.items.get(index) {
                        Some(Item::Bitfield(decl)) => decl.repr.as_ref(),
                        _ => None,
                    };
                    self.repr_load(repr, name, ctx, depth)
                } else {
                    Ok(None)
                }
            }
            TypeRef::InlineStruct(_) | TypeRef::Str(_) => Ok(None),
        }
    }

    fn repr_load(
        &self,
        repr: Option<&TypeRef>,
        name: &str,
        ctx: &FrameCtx,
        _depth: u32,
    ) -> CResult<Option<ScalarLoad>> {
        match repr {
            None => Ok(Some(ScalarLoad {
                width: 4,
                signed: false,
                float: false,
                endian: ctx.endian,
            })),
            Some(TypeRef::Prim(prim)) => Ok(Some(ScalarLoad {
                width: prim.byte_size() as u8,
                signed: matches!(
                    prim,
                    Prim::S8
                        | Prim::S16
                        | Prim::S24
                        | Prim::S32
                        | Prim::S48
                        | Prim::S64
                        | Prim::S128
                ),
                float: matches!(prim, Prim::F16 | Prim::F32 | Prim::F64),
                endian: ctx.endian,
            })),
            Some(other) => Err(diag(
                error_codes::E4001,
                format!(
                    "v1 只支持基础整数作为 enum/bitfield 的 repr（`{name}` 的 repr 是 `{}`）",
                    type_label(other)
                ),
            )),
        }
    }

    fn field_info_for(&self, ty: &TypeRef, endian: Endian) -> Option<FieldInfo> {
        let (width, kind) = match ty {
            TypeRef::Prim(prim) => (prim.byte_size() as u8, field_kind(*prim)),
            TypeRef::Ptr { kind, .. } => (kind.byte_size() as u8, FieldKind::Unsigned),
            TypeRef::Named(name) => {
                let repr = if let Some(index) = self.decls.enums.get(name).copied() {
                    match self.program.items.get(index) {
                        Some(Item::Enum(decl)) => decl.repr.as_ref(),
                        _ => None,
                    }
                } else {
                    let index = self.decls.bitfields.get(name).copied()?;
                    match self.program.items.get(index) {
                        Some(Item::Bitfield(decl)) => decl.repr.as_ref(),
                        _ => None,
                    }
                };
                let prim = match repr {
                    None => Prim::U32,
                    Some(TypeRef::Prim(prim)) => *prim,
                    Some(_) => return None,
                };
                (prim.byte_size() as u8, field_kind(prim))
            }
            TypeRef::Str(_) | TypeRef::InlineStruct(_) => return None,
        };
        Some(FieldInfo {
            offset: 0,
            width,
            kind,
            endian,
        })
    }

    fn lookup_field(&self, name: &str) -> Option<FieldInfo> {
        self.field_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn is_leaf_type(&self, ty: &TypeRef) -> bool {
        match ty {
            TypeRef::Prim(_) | TypeRef::Ptr { .. } => true,
            TypeRef::Named(name) => {
                self.decls.enums.contains_key(name) || self.decls.bitfields.contains_key(name)
            }
            TypeRef::Str(_) | TypeRef::InlineStruct(_) => false,
        }
    }

    fn size_of_struct_decl(
        &self,
        decl: &StructDecl,
        env: &EvalEnv,
        ctx: &FrameCtx,
        depth: u32,
    ) -> CResult<Option<u64>> {
        if depth >= MAX_DEPTH {
            return Err(diag(
                error_codes::E4007,
                format!("结构 `{}` 的嵌套/递归深度超过上限 {MAX_DEPTH}", decl.name),
            ));
        }
        if let Some(size) = &decl.size {
            let value = self.eval_u64(size, &mut env.clone(), ctx).map_err(|fail| {
                eval_diag(
                    &format!("结构 `{}` 的定长大小必须是编译期常量", decl.name),
                    fail,
                )
            })?;
            return Ok(Some(value));
        }
        let mut cursor = 0u64;
        let mut local = *ctx;
        local.cursor = Some(0);
        local.start = Some(0);
        local.end = None;
        for member in &decl.members {
            if let Some(offset) = &member.at {
                match self.eval_u64(offset, &mut env.clone(), &local) {
                    Ok(value) => cursor = value,
                    Err(_) => return Ok(None),
                }
            }
            let Some(size) = self.member_payload_size(member, env, &local, depth)? else {
                return Ok(None);
            };
            cursor = cursor.checked_add(size).ok_or_else(|| {
                diag(
                    error_codes::E4001,
                    format!("结构 `{}` 的布局大小溢出 u64", decl.name),
                )
            })?;
            local.cursor = Some(cursor);
        }
        Ok(Some(cursor))
    }

    fn size_of_union_decl(
        &self,
        decl: &UnionDecl,
        env: &EvalEnv,
        ctx: &FrameCtx,
        depth: u32,
    ) -> CResult<Option<u64>> {
        if depth >= MAX_DEPTH {
            return Err(diag(
                error_codes::E4007,
                format!("联合体 `{}` 的嵌套/递归深度超过上限 {MAX_DEPTH}", decl.name),
            ));
        }
        if let Some(size) = &decl.size {
            let value = self.eval_u64(size, &mut env.clone(), ctx).map_err(|fail| {
                eval_diag(
                    &format!("联合体 `{}` 的定长大小必须是编译期常量", decl.name),
                    fail,
                )
            })?;
            return Ok(Some(value));
        }
        let mut max = 0u64;
        for member in &decl.members {
            let Some(size) = self.member_payload_size(member, env, ctx, depth)? else {
                return Ok(None);
            };
            max = max.max(size);
        }
        Ok(Some(max))
    }

    fn member_payload_size(
        &self,
        member: &Member,
        env: &EvalEnv,
        ctx: &FrameCtx,
        depth: u32,
    ) -> CResult<Option<u64>> {
        if is_padding(member) {
            let count = match &member.array {
                Some(ArraySpec::Fixed(expr)) => match self.eval_u64(expr, &mut env.clone(), ctx) {
                    Ok(value) => value,
                    Err(_) => return Ok(None),
                },
                _ => return Ok(None),
            };
            return Ok(Some(count));
        }
        if let Some(condition) = &member.cond {
            match self.eval_bool(condition, &mut env.clone(), ctx) {
                Ok(false) => return Ok(Some(0)),
                Ok(true) => {}
                Err(_) => return Ok(None),
            }
        }
        let Some(element_size) = self.size_of_type(&member.ty, env, ctx, depth)? else {
            return Ok(None);
        };
        let count = match &member.array {
            None => 1u64,
            Some(ArraySpec::Fixed(expr)) => match self.eval_u64(expr, &mut env.clone(), ctx) {
                Ok(value) => value,
                Err(_) => return Ok(None),
            },
            Some(ArraySpec::Until(_) | ArraySpec::Unsized) => return Ok(None),
        };
        element_size.checked_mul(count).map(Some).ok_or_else(|| {
            diag(
                error_codes::E4001,
                format!("字段 `{}` 的数组大小溢出 u64", member.name),
            )
        })
    }

    fn size_of_type(
        &self,
        ty: &TypeRef,
        env: &EvalEnv,
        ctx: &FrameCtx,
        depth: u32,
    ) -> CResult<Option<u64>> {
        if depth >= MAX_DEPTH {
            return Err(diag(
                error_codes::E4007,
                format!("类型布局的递归深度超过上限 {MAX_DEPTH}"),
            ));
        }
        match ty {
            TypeRef::Prim(prim) => Ok(Some(u64::from(prim.byte_size()))),
            TypeRef::Ptr { kind, .. } => Ok(Some(u64::from(kind.byte_size()))),
            TypeRef::Str(_) => Ok(None),
            TypeRef::InlineStruct(decl) => {
                self.size_of_struct_decl(decl, env, ctx, depth.saturating_add(1))
            }
            TypeRef::Named(name) => {
                let program = self.program;
                if let Some(index) = self.decls.structs.get(name).copied() {
                    let decl = match program.items.get(index) {
                        Some(Item::Struct(decl)) => decl,
                        _ => {
                            return Err(diag(
                                error_codes::E4001,
                                format!("类型 `{name}` 的声明索引损坏"),
                            ));
                        }
                    };
                    self.size_of_struct_decl(decl, env, ctx, depth.saturating_add(1))
                } else if let Some(index) = self.decls.unions.get(name).copied() {
                    let decl = match program.items.get(index) {
                        Some(Item::Union(decl)) => decl,
                        _ => {
                            return Err(diag(
                                error_codes::E4001,
                                format!("类型 `{name}` 的声明索引损坏"),
                            ));
                        }
                    };
                    self.size_of_union_decl(decl, env, ctx, depth.saturating_add(1))
                } else if let Some(index) = self.decls.enums.get(name).copied() {
                    let repr = match program.items.get(index) {
                        Some(Item::Enum(decl)) => decl.repr.as_ref(),
                        _ => None,
                    };
                    self.size_of_repr(repr, name)
                } else if let Some(index) = self.decls.bitfields.get(name).copied() {
                    let repr = match program.items.get(index) {
                        Some(Item::Bitfield(decl)) => decl.repr.as_ref(),
                        _ => None,
                    };
                    self.size_of_repr(repr, name)
                } else {
                    Err(diag(error_codes::E4003, format!("未定义类型 `{name}`")))
                }
            }
        }
    }

    fn size_of_repr(&self, repr: Option<&TypeRef>, name: &str) -> CResult<Option<u64>> {
        match repr {
            None => Ok(Some(4)),
            Some(TypeRef::Prim(prim)) => Ok(Some(u64::from(prim.byte_size()))),
            Some(other) => Err(diag(
                error_codes::E4001,
                format!(
                    "v1 只支持基础整数作为 enum/bitfield 的 repr（`{name}` 的 repr 是 `{}`）",
                    type_label(other)
                ),
            )),
        }
    }
}

fn field_kind(prim: Prim) -> FieldKind {
    match prim {
        Prim::F16 | Prim::F32 | Prim::F64 => FieldKind::Float,
        Prim::S8 | Prim::S16 | Prim::S24 | Prim::S32 | Prim::S48 | Prim::S64 | Prim::S128 => {
            FieldKind::Signed
        }
        Prim::Char => FieldKind::Bytes,
        _ => FieldKind::Unsigned,
    }
}

fn advance(ctx: &mut FrameCtx, amount: u64) -> CResult<()> {
    if let Some(cursor) = ctx.cursor {
        ctx.cursor = Some(
            cursor
                .checked_add(amount)
                .ok_or_else(|| diag(error_codes::E4001, "读游标溢出 u64"))?,
        );
    }
    Ok(())
}

fn patch_jump(code: &mut [Op], index: usize, target: u32) -> CResult<()> {
    let Some(op) = code.get_mut(index) else {
        return Err(diag(
            error_codes::E4001,
            format!("跳转回填下标 {index} 越界"),
        ));
    };
    match op {
        Op::Jmp(slot) | Op::JmpIf(slot) | Op::JmpIfNot(slot) => {
            *slot = target;
            Ok(())
        }
        _ => Err(diag(
            error_codes::E4001,
            format!("跳转回填下标 {index} 不是跳转指令"),
        )),
    }
}

fn is_padding(member: &Member) -> bool {
    member.attrs.iter().any(|attr| attr.name == "padding")
}

fn member_endian(member: &Member, default: Endian) -> CResult<Endian> {
    let Some(attr) = member.attrs.iter().find(|attr| attr.name == "endian") else {
        return Ok(default);
    };
    let Some(word) = attr.args.first() else {
        return Err(diag(
            error_codes::E4001,
            format!("字段 `{}` 的 endian 属性缺少参数", member.name),
        ));
    };
    parse_endian_word(word).ok_or_else(|| {
        diag(
            error_codes::E4001,
            format!(
                "字段 `{}` 的 endian 参数 `{word}` 无效（应为 little/big）",
                member.name
            ),
        )
    })
}

fn parse_endian_word(word: &str) -> Option<Endian> {
    match word.trim_matches('"').to_ascii_lowercase().as_str() {
        "little" | "le" | "0" => Some(Endian::Little),
        "big" | "be" | "1" => Some(Endian::Big),
        _ => None,
    }
}

fn parse_endian_pragma(pragma: &PragmaDecl) -> CResult<Endian> {
    let Some(value) = &pragma.value else {
        return Err(diag(
            error_codes::E4001,
            "`#pragma endian` 缺少 little/big 参数",
        ));
    };
    let word = match value {
        Expr::Path(word) | Expr::Literal(Literal::Str(word)) => word.clone(),
        Expr::Literal(Literal::Int(value)) => value.to_string(),
        _ => {
            return Err(diag(
                error_codes::E4001,
                "`#pragma endian` 的参数必须是 little/big",
            ));
        }
    };
    parse_endian_word(&word).ok_or_else(|| {
        diag(
            error_codes::E4001,
            format!("`#pragma endian` 的参数 `{word}` 无效（应为 little/big）"),
        )
    })
}

fn parse_str_enc(enc: Option<&str>) -> CResult<StrEnc> {
    match enc.unwrap_or("utf8").to_ascii_lowercase().as_str() {
        "utf8" | "utf-8" => Ok(StrEnc::Utf8),
        "ascii" => Ok(StrEnc::Ascii),
        "latin1" | "latin-1" => Ok(StrEnc::Latin1),
        "utf16le" | "utf-16le" => Ok(StrEnc::Utf16Le),
        "utf16be" | "utf-16be" => Ok(StrEnc::Utf16Be),
        other => Err(diag(
            error_codes::E4001,
            format!("v1 不支持字符串编码 `{other}`（支持 utf8/ascii/latin1/utf16le/utf16be）"),
        )),
    }
}

fn type_label(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Prim(prim) => prim.as_str().to_owned(),
        TypeRef::Named(name) => name.clone(),
        TypeRef::Ptr { kind, .. } => kind.as_str().to_owned(),
        TypeRef::Str(_) => "str".to_owned(),
        TypeRef::InlineStruct(_) => "struct".to_owned(),
    }
}

fn merge_env(target: &mut EvalEnv, source: &EvalEnv) {
    for (name, value) in &mut target.vars {
        if let Some(new_value) = source.vars.get(name) {
            *value = new_value.clone();
        }
    }
}

impl Gen<'_> {
    fn expr_uses_field(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Path(name) => self.lookup_field(name).is_some(),
            Expr::Literal(_)
            | Expr::Cursor
            | Expr::FileSize
            | Expr::StructStart
            | Expr::StructEnd
            | Expr::SizeOf(_) => false,
            Expr::Unary { expr, .. } => self.expr_uses_field(expr),
            Expr::Binary { lhs, rhs, .. } => self.expr_uses_field(lhs) || self.expr_uses_field(rhs),
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                self.expr_uses_field(cond)
                    || self.expr_uses_field(then_expr)
                    || self.expr_uses_field(else_expr)
            }
            Expr::Call { callee, args } => {
                self.expr_uses_field(callee) || args.iter().any(|arg| self.expr_uses_field(arg))
            }
            Expr::Index { base, index } => {
                self.expr_uses_field(base) || self.expr_uses_field(index)
            }
            Expr::Member { base, .. } => self.expr_uses_field(base),
            Expr::Len(inner) => self.expr_uses_field(inner),
            Expr::Assign { target, value, .. } => {
                self.expr_uses_field(target) || self.expr_uses_field(value)
            }
        }
    }

    /// 编译一个包含已读字段引用的条件表达式，结果压入栈顶。
    ///
    /// 条件求值期间会临时 `Seek` 到字段偏移重新读取；求值结束后恢复当前游标，
    /// 因而条件本身不会消费正在解析的字段。
    fn compile_condition(
        &mut self,
        expr: &Expr,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<()> {
        let saved = ctx.cursor.ok_or_else(|| {
            diag(
                error_codes::E4001,
                "v1 无法在游标未知的位置编译字段条件（前一个字段不是定长字段）",
            )
        })?;
        self.compile_runtime_expr(expr, ctx, env)?;
        self.emit(Op::Seek(saved))?;
        ctx.cursor = Some(saved);
        Ok(())
    }

    fn compile_runtime_expr(
        &mut self,
        expr: &Expr,
        ctx: &mut FrameCtx,
        env: &mut EvalEnv,
    ) -> CResult<()> {
        match expr {
            Expr::Literal(literal) => self.push_value(&literal_value(literal)),
            Expr::Path(name) => {
                if let Some(info) = self.lookup_field(name) {
                    self.emit(Op::Seek(info.offset))?;
                    ctx.cursor = Some(info.offset);
                    match info.kind {
                        FieldKind::Unsigned => self.emit(Op::LoadU {
                            width: info.width,
                            endian: info.endian,
                        })?,
                        FieldKind::Signed => self.emit(Op::LoadI {
                            width: info.width,
                            endian: info.endian,
                        })?,
                        FieldKind::Float => self.emit(Op::LoadF {
                            width: info.width,
                            endian: info.endian,
                        })?,
                        FieldKind::Bytes => self.emit(Op::LoadBytes {
                            len: u32::from(info.width),
                        })?,
                    }
                    advance(ctx, u64::from(info.width))
                } else {
                    let value = self
                        .eval_expr(expr, env, ctx)
                        .map_err(|fail| eval_diag("条件表达式求值失败", fail))?;
                    self.push_value(&value)
                }
            }
            Expr::Unary { op, expr } => {
                self.compile_runtime_expr(expr, ctx, env)?;
                match op {
                    UnaryOp::Pos => Ok(()),
                    UnaryOp::Neg => self.emit(Op::Neg),
                    UnaryOp::Not => self.emit(Op::Not),
                    UnaryOp::BitNot => Err(diag(
                        error_codes::E4001,
                        "v1 的运行期条件不支持按位取反 `~`（请使用编译期常量）",
                    )),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                self.compile_runtime_expr(lhs, ctx, env)?;
                self.compile_runtime_expr(rhs, ctx, env)?;
                let opcode = match op {
                    BinaryOp::Add => Op::Add,
                    BinaryOp::Sub => Op::Sub,
                    BinaryOp::Mul => Op::Mul,
                    BinaryOp::Div => Op::Div,
                    BinaryOp::Rem => Op::Mod,
                    BinaryOp::BitAnd => Op::And,
                    BinaryOp::BitOr => Op::Or,
                    BinaryOp::BitXor => Op::Xor,
                    BinaryOp::Shl => Op::Shl,
                    BinaryOp::Shr => Op::Shr,
                    BinaryOp::Eq => Op::Eq,
                    BinaryOp::Ne => Op::Ne,
                    BinaryOp::Lt => Op::Lt,
                    BinaryOp::Le => Op::Le,
                    BinaryOp::Gt => Op::Gt,
                    BinaryOp::Ge => Op::Ge,
                    BinaryOp::And => Op::And,
                    BinaryOp::Or => Op::Or,
                    BinaryOp::Pow => {
                        return Err(diag(
                            error_codes::E4001,
                            "v1 的运行期条件不支持幂运算 `**`（请使用编译期常量）",
                        ));
                    }
                };
                self.emit(opcode)
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                self.compile_runtime_expr(cond, ctx, env)?;
                let otherwise = self.code.len();
                self.emit(Op::JmpIfNot(0))?;
                self.compile_runtime_expr(then_expr, ctx, env)?;
                let done = self.code.len();
                self.emit(Op::Jmp(0))?;
                let else_start = self.code_len_u32()?;
                patch_jump(&mut self.code, otherwise, else_start)?;
                self.compile_runtime_expr(else_expr, ctx, env)?;
                let end = self.code_len_u32()?;
                patch_jump(&mut self.code, done, end)
            }
            Expr::Cursor
            | Expr::FileSize
            | Expr::StructStart
            | Expr::StructEnd
            | Expr::Call { .. }
            | Expr::SizeOf(_)
            | Expr::Len(_) => {
                let value = self
                    .eval_expr(expr, env, ctx)
                    .map_err(|fail| eval_diag("条件表达式求值失败", fail))?;
                self.push_value(&value)
            }
            Expr::Index { .. } | Expr::Member { .. } => Err(diag(
                error_codes::E4001,
                "v1 的运行期条件不支持索引/成员访问",
            )),
            Expr::Assign { .. } => Err(diag(error_codes::E4001, "v1 的条件表达式不能包含赋值")),
        }
    }

    fn eval_expr(&self, expr: &Expr, env: &mut EvalEnv, ctx: &FrameCtx) -> Result<Value, EvalFail> {
        self.eval_expr_inner(expr, env, ctx, 0)
    }

    fn eval_expr_inner(
        &self,
        expr: &Expr,
        env: &mut EvalEnv,
        ctx: &FrameCtx,
        depth: u32,
    ) -> Result<Value, EvalFail> {
        if depth >= MAX_DEPTH {
            return Err(EvalFail::new(
                error_codes::E4007,
                format!("表达式求值递归深度超过上限 {MAX_DEPTH}"),
            ));
        }
        match expr {
            Expr::Literal(literal) => Ok(literal_value(literal)),
            Expr::Path(path) => self.resolve_path(path, env, depth),
            Expr::Cursor => ctx
                .cursor
                .map(|value| Value::UInt(u128::from(value)))
                .ok_or_else(|| {
                    EvalFail::new(
                        error_codes::E4001,
                        "`$`（当前游标）在编译期未知；v1 要求前面的字段都是定长",
                    )
                }),
            Expr::FileSize => match (ctx.start, ctx.end) {
                (Some(start), Some(end)) if end >= start => {
                    Ok(Value::UInt(u128::from(end - start)))
                }
                _ => Err(EvalFail::new(
                    error_codes::E4001,
                    "v1 只在定长结构内支持 `$size`（当前结构大小编译期未知）",
                )),
            },
            Expr::StructStart => ctx
                .start
                .map(|value| Value::UInt(u128::from(value)))
                .ok_or_else(|| EvalFail::new(error_codes::E4001, "`$start` 在编译期未知")),
            Expr::StructEnd => ctx
                .end
                .map(|value| Value::UInt(u128::from(value)))
                .ok_or_else(|| {
                    EvalFail::new(
                        error_codes::E4001,
                        "`$end` 在编译期未知（结构不是定长结构）",
                    )
                }),
            Expr::Unary { op, expr } => {
                let value = self.eval_expr_inner(expr, env, ctx, depth + 1)?;
                unary_value(*op, value)
            }
            Expr::Binary { op, lhs, rhs } => {
                let left = self.eval_expr_inner(lhs, env, ctx, depth + 1)?;
                let right = self.eval_expr_inner(rhs, env, ctx, depth + 1)?;
                binary_value(*op, left, right)
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                let condition = self.eval_expr_inner(cond, env, ctx, depth + 1)?;
                if truthy(&condition).unwrap_or(false) {
                    self.eval_expr_inner(then_expr, env, ctx, depth + 1)
                } else {
                    self.eval_expr_inner(else_expr, env, ctx, depth + 1)
                }
            }
            Expr::Call { callee, args } => self.eval_call(callee, args, env, ctx, depth),
            Expr::Index { .. } | Expr::Member { .. } => Err(EvalFail::new(
                error_codes::E4001,
                "v1 不支持表达式中的索引/成员访问（运行期值不可用于编译期求值）",
            )),
            Expr::SizeOf(ty) => {
                let size = self
                    .size_of_type(ty, env, ctx, depth + 1)
                    .map_err(eval_fail_from_diags)?;
                size.map(|value| Value::UInt(u128::from(value)))
                    .ok_or_else(|| {
                        EvalFail::new(
                            error_codes::E4001,
                            "v1 不支持对变长类型求 `sizeof`；请给结构声明显式 `: N` 大小",
                        )
                    })
            }
            Expr::Len(inner) => {
                let value = self.eval_expr_inner(inner, env, ctx, depth + 1)?;
                match value {
                    Value::Str(text) => Ok(Value::UInt(text.len() as u128)),
                    Value::Bytes(bytes) => Ok(Value::UInt(bytes.len() as u128)),
                    other => Err(EvalFail::new(
                        error_codes::E4001,
                        format!("`len` 只支持字符串/字节，得到 {}", other.type_name()),
                    )),
                }
            }
            Expr::Assign { op, target, value } => {
                let Expr::Path(name) = target.as_ref() else {
                    return Err(EvalFail::new(
                        error_codes::E4001,
                        "v1 赋值目标只能是编译期 `let` 变量",
                    ));
                };
                let new_value = self.eval_expr_inner(value, env, ctx, depth + 1)?;
                let assigned = match op {
                    AssignOp::Assign => new_value,
                    AssignOp::AddAssign | AssignOp::SubAssign => {
                        let Some(current) = env.vars.get(name).cloned() else {
                            return Err(EvalFail::new(
                                error_codes::E4003,
                                format!("赋值目标 `{name}` 未定义"),
                            ));
                        };
                        let operation = if *op == AssignOp::AddAssign {
                            BinaryOp::Add
                        } else {
                            BinaryOp::Sub
                        };
                        binary_value(operation, current, new_value)?
                    }
                };
                env.vars.insert(name.clone(), assigned.clone());
                Ok(assigned)
            }
        }
    }

    fn resolve_path(&self, path: &str, env: &EvalEnv, depth: u32) -> Result<Value, EvalFail> {
        if let Some(value) = env.vars.get(path) {
            return Ok(value.clone());
        }
        if let Some((enum_name, variant)) = path.split_once("::") {
            return self.enum_variant_value(enum_name, variant, depth + 1);
        }
        let mut candidates = Vec::new();
        for item in &self.program.items {
            if let Item::Enum(decl) = item
                && decl.variants.iter().any(|variant| variant.name == path)
            {
                candidates.push(decl.name.clone());
            }
        }
        match candidates.as_slice() {
            [] => Err(EvalFail::new(
                error_codes::E4003,
                format!("未定义符号或枚举项 `{path}`"),
            )),
            [only] => self.enum_variant_value(only, path, depth + 1),
            _ => Err(EvalFail::new(
                error_codes::E4001,
                format!("枚举项 `{path}` 不唯一，请使用 `Enum::{path}`"),
            )),
        }
    }

    fn enum_variant_value(
        &self,
        enum_name: &str,
        variant_name: &str,
        depth: u32,
    ) -> Result<Value, EvalFail> {
        if depth >= MAX_DEPTH {
            return Err(EvalFail::new(
                error_codes::E4007,
                format!("枚举 `{enum_name}` 的值递归深度超过上限 {MAX_DEPTH}"),
            ));
        }
        let program = self.program;
        let Some(index) = self.decls.enums.get(enum_name).copied() else {
            return Err(EvalFail::new(
                error_codes::E4003,
                format!("未定义枚举 `{enum_name}`"),
            ));
        };
        let decl = match program.items.get(index) {
            Some(Item::Enum(decl)) => decl,
            _ => {
                return Err(EvalFail::new(
                    error_codes::E4001,
                    format!("枚举 `{enum_name}` 的声明索引损坏"),
                ));
            }
        };
        let mut next = 0u128;
        for variant in &decl.variants {
            let value = match &variant.value {
                Some(expr) => {
                    let mut env = EvalEnv::default();
                    let ctx = FrameCtx::root(Endian::Little);
                    self.eval_expr_inner(expr, &mut env, &ctx, depth + 1)?
                }
                None => Value::UInt(next),
            };
            if variant.name == variant_name {
                return Ok(value);
            }
            next = value.as_u128().map_or(0, |value| value.saturating_add(1));
        }
        Err(EvalFail::new(
            error_codes::E4003,
            format!("枚举 `{enum_name}` 没有项 `{variant_name}`"),
        ))
    }

    fn eval_call(
        &self,
        callee: &Expr,
        args: &[Expr],
        env: &mut EvalEnv,
        ctx: &FrameCtx,
        depth: u32,
    ) -> Result<Value, EvalFail> {
        let Expr::Path(name) = callee else {
            return Err(EvalFail::new(error_codes::E4001, "v1 只支持具名纯函数调用"));
        };
        if name.starts_with("std::mem::") {
            return Err(EvalFail::new(
                error_codes::E4001,
                format!("v1 不支持 `{name}`；`size_of` 请使用 `sizeof(T)`"),
            ));
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr_inner(arg, env, ctx, depth + 1)?);
        }
        match apply_pure_call(name, &values) {
            Some(result) => result,
            None => Err(EvalFail::new(
                error_codes::E4001,
                format!("v1 不支持函数 `{name}`"),
            )),
        }
    }

    fn eval_bool(&self, expr: &Expr, env: &mut EvalEnv, ctx: &FrameCtx) -> Result<bool, EvalFail> {
        let value = self.eval_expr(expr, env, ctx)?;
        truthy(&value).ok_or_else(|| {
            EvalFail::new(
                error_codes::E4001,
                format!("条件需要布尔/整数，得到 {}", value.type_name()),
            )
        })
    }

    fn eval_u64(&self, expr: &Expr, env: &mut EvalEnv, ctx: &FrameCtx) -> Result<u64, EvalFail> {
        let value = self.eval_expr(expr, env, ctx)?;
        value_to_u64(&value)
    }
}

fn is_compile_time_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_) | Expr::Path(_) | Expr::SizeOf(_) => true,
        Expr::Cursor | Expr::FileSize | Expr::StructStart | Expr::StructEnd => false,
        Expr::Unary { expr, .. } => is_compile_time_expr(expr),
        Expr::Binary { lhs, rhs, .. } => is_compile_time_expr(lhs) && is_compile_time_expr(rhs),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            is_compile_time_expr(cond)
                && is_compile_time_expr(then_expr)
                && is_compile_time_expr(else_expr)
        }
        Expr::Call { callee, args } => {
            is_compile_time_expr(callee) && args.iter().all(is_compile_time_expr)
        }
        Expr::Len(expr) => is_compile_time_expr(expr),
        Expr::Index { .. } | Expr::Member { .. } | Expr::Assign { .. } => false,
    }
}

fn literal_value(literal: &Literal) -> Value {
    match literal {
        Literal::Int(value) => Value::UInt(*value),
        Literal::Float(value) => Value::Float(*value),
        Literal::Str(value) => Value::Str(value.clone()),
        Literal::Char(value) => Value::UInt(u128::from(*value as u32)),
        Literal::Bool(value) => Value::Bool(*value),
        Literal::Null => Value::None,
    }
}

fn truthy(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::UInt(value) => Some(*value != 0),
        Value::Int(value) => Some(*value != 0),
        Value::Float(value) => Some(*value != 0.0),
        Value::Str(value) => Some(!value.is_empty()),
        Value::Bytes(value) => Some(!value.is_empty()),
        Value::None => Some(false),
    }
}

fn value_to_u64(value: &Value) -> Result<u64, EvalFail> {
    if let Some(value) = value.as_u128() {
        return u64::try_from(value).map_err(|_| {
            EvalFail::new(
                error_codes::E4001,
                format!("期望 u64 范围内的整数，得到 {value}"),
            )
        });
    }
    if let Some(value) = value.as_f64()
        && value.is_finite()
        && value >= 0.0
        && value.fract() == 0.0
        && value <= u64::MAX as f64
    {
        return Ok(value as u64);
    }
    Err(EvalFail::new(
        error_codes::E4001,
        format!("期望非负整数，得到 {}", value.type_name()),
    ))
}

fn value_to_u32(value: &Value) -> Result<u32, EvalFail> {
    let value = value_to_u64(value)?;
    u32::try_from(value)
        .map_err(|_| EvalFail::new(error_codes::E4001, format!("{value} 超过 u32 上限")))
}

fn unary_value(op: UnaryOp, value: Value) -> Result<Value, EvalFail> {
    match op {
        UnaryOp::Pos => Ok(value),
        UnaryOp::Neg => match value {
            Value::Float(value) => Ok(Value::Float(-value)),
            Value::Int(value) => value
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数取负溢出")),
            Value::UInt(value) => {
                let value = i128::try_from(value).map_err(|_| {
                    EvalFail::new(error_codes::E4001, "无符号整数取负超过 i128 范围")
                })?;
                Ok(Value::Int(-value))
            }
            Value::Bool(value) => Ok(Value::Int(-i128::from(value))),
            other => Err(EvalFail::new(
                error_codes::E4001,
                format!("无法对 {} 取负", other.type_name()),
            )),
        },
        UnaryOp::Not => match value {
            Value::Bool(value) => Ok(Value::Bool(!value)),
            Value::UInt(value) => Ok(Value::UInt(!value)),
            Value::Int(value) => Ok(Value::Int(!value)),
            other => Err(EvalFail::new(
                error_codes::E4001,
                format!("无法对 {} 做逻辑非", other.type_name()),
            )),
        },
        UnaryOp::BitNot => match value {
            Value::UInt(value) => Ok(Value::UInt(!value)),
            Value::Int(value) => Ok(Value::Int(!value)),
            Value::Bool(value) => Ok(Value::Int(!i128::from(value))),
            other => Err(EvalFail::new(
                error_codes::E4001,
                format!("无法对 {} 做按位取反", other.type_name()),
            )),
        },
    }
}

fn binary_value(op: BinaryOp, left: Value, right: Value) -> Result<Value, EvalFail> {
    match op {
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
            numeric_binary(op, left, right)
        }
        BinaryOp::Eq => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::Ne => Ok(Value::Bool(!values_equal(&left, &right))),
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let ordering = compare_values(&left, &right).ok_or_else(|| {
                EvalFail::new(
                    error_codes::E4001,
                    format!("无法比较 {} 与 {}", left.type_name(), right.type_name()),
                )
            })?;
            let result = match op {
                BinaryOp::Lt => ordering == std::cmp::Ordering::Less,
                BinaryOp::Le => ordering != std::cmp::Ordering::Greater,
                BinaryOp::Gt => ordering == std::cmp::Ordering::Greater,
                BinaryOp::Ge => ordering != std::cmp::Ordering::Less,
                _ => false,
            };
            Ok(Value::Bool(result))
        }
        BinaryOp::And | BinaryOp::Or => {
            if let (Value::Bool(left), Value::Bool(right)) = (&left, &right) {
                return Ok(Value::Bool(if op == BinaryOp::And {
                    *left && *right
                } else {
                    *left || *right
                }));
            }
            integer_bitwise(op, left, right)
        }
        BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => integer_bitwise(op, left, right),
        BinaryOp::Shl | BinaryOp::Shr => {
            let shift = value_to_u32(&right)?;
            match left {
                Value::Int(value) => {
                    let result = if op == BinaryOp::Shl {
                        value.checked_shl(shift)
                    } else {
                        value.checked_shr(shift)
                    };
                    result
                        .map(Value::Int)
                        .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数移位溢出"))
                }
                other => {
                    let value = other.as_u128().ok_or_else(|| {
                        EvalFail::new(
                            error_codes::E4001,
                            format!("无法对 {} 做移位", other.type_name()),
                        )
                    })?;
                    let result = if op == BinaryOp::Shl {
                        value.checked_shl(shift)
                    } else {
                        value.checked_shr(shift)
                    };
                    result
                        .map(Value::UInt)
                        .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数移位溢出"))
                }
            }
        }
        BinaryOp::Pow => {
            if matches!(left, Value::Float(_)) || matches!(right, Value::Float(_)) {
                let base = left
                    .as_f64()
                    .ok_or_else(|| EvalFail::new(error_codes::E4001, "幂运算需要数值"))?;
                let exponent = right
                    .as_f64()
                    .ok_or_else(|| EvalFail::new(error_codes::E4001, "幂运算需要数值"))?;
                return Ok(Value::Float(base.powf(exponent)));
            }
            let exponent = value_to_u32(&right)?;
            match left {
                Value::Int(value) => value
                    .checked_pow(exponent)
                    .map(Value::Int)
                    .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数幂溢出")),
                other => {
                    let value = other
                        .as_u128()
                        .ok_or_else(|| EvalFail::new(error_codes::E4001, "幂运算需要数值"))?;
                    value
                        .checked_pow(exponent)
                        .map(Value::UInt)
                        .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数幂溢出"))
                }
            }
        }
    }
}

fn numeric_binary(op: BinaryOp, left: Value, right: Value) -> Result<Value, EvalFail> {
    if matches!(left, Value::Float(_)) || matches!(right, Value::Float(_)) {
        let left = left
            .as_f64()
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "算术运算需要数值"))?;
        let right = right
            .as_f64()
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "算术运算需要数值"))?;
        return match op {
            BinaryOp::Add => Ok(Value::Float(left + right)),
            BinaryOp::Sub => Ok(Value::Float(left - right)),
            BinaryOp::Mul => Ok(Value::Float(left * right)),
            BinaryOp::Div => Ok(Value::Float(left / right)),
            BinaryOp::Rem => Ok(Value::Float(left % right)),
            _ => Err(EvalFail::new(error_codes::E4001, "不是算术运算")),
        };
    }
    if matches!(left, Value::Int(_)) || matches!(right, Value::Int(_)) {
        let left = left
            .as_i128()
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "有符号整数超出 i128 范围"))?;
        let right = right
            .as_i128()
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "有符号整数超出 i128 范围"))?;
        return match op {
            BinaryOp::Add => left
                .checked_add(right)
                .map(Value::Int)
                .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数加法溢出")),
            BinaryOp::Sub => left
                .checked_sub(right)
                .map(Value::Int)
                .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数减法溢出")),
            BinaryOp::Mul => left
                .checked_mul(right)
                .map(Value::Int)
                .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数乘法溢出")),
            BinaryOp::Div => {
                if right == 0 {
                    Err(EvalFail::new(error_codes::E4001, "整数除零"))
                } else {
                    left.checked_div(right)
                        .map(Value::Int)
                        .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数除法溢出"))
                }
            }
            BinaryOp::Rem => {
                if right == 0 {
                    Err(EvalFail::new(error_codes::E4001, "整数模零"))
                } else {
                    left.checked_rem(right)
                        .map(Value::Int)
                        .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数取余溢出"))
                }
            }
            _ => Err(EvalFail::new(error_codes::E4001, "不是算术运算")),
        };
    }
    let left = left
        .as_u128()
        .ok_or_else(|| EvalFail::new(error_codes::E4001, "算术运算需要整数"))?;
    let right = right
        .as_u128()
        .ok_or_else(|| EvalFail::new(error_codes::E4001, "算术运算需要整数"))?;
    match op {
        BinaryOp::Add => left
            .checked_add(right)
            .map(Value::UInt)
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数加法溢出")),
        BinaryOp::Sub => left
            .checked_sub(right)
            .map(Value::UInt)
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数减法下溢")),
        BinaryOp::Mul => left
            .checked_mul(right)
            .map(Value::UInt)
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数乘法溢出")),
        BinaryOp::Div => {
            if right == 0 {
                Err(EvalFail::new(error_codes::E4001, "整数除零"))
            } else {
                left.checked_div(right)
                    .map(Value::UInt)
                    .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数除法溢出"))
            }
        }
        BinaryOp::Rem => {
            if right == 0 {
                Err(EvalFail::new(error_codes::E4001, "整数模零"))
            } else {
                left.checked_rem(right)
                    .map(Value::UInt)
                    .ok_or_else(|| EvalFail::new(error_codes::E4001, "整数取余溢出"))
            }
        }
        _ => Err(EvalFail::new(error_codes::E4001, "不是算术运算")),
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Str(left), Value::Str(right)) => left == right,
        (Value::Bytes(left), Value::Bytes(right)) => left == right,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::None, Value::None) => true,
        _ => {
            if let (Some(left), Some(right)) = (left.as_u128(), right.as_u128()) {
                return left == right;
            }
            if let (Some(left), Some(right)) = (left.as_i128(), right.as_i128()) {
                return left == right;
            }
            if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
                return left == right;
            }
            false
        }
    }
}

fn compare_values(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Str(left), Value::Str(right)) => Some(left.cmp(right)),
        (Value::Bytes(left), Value::Bytes(right)) => Some(left.cmp(right)),
        _ => {
            if let (Some(left), Some(right)) = (left.as_u128(), right.as_u128()) {
                return Some(left.cmp(&right));
            }
            if let (Some(left), Some(right)) = (left.as_i128(), right.as_i128()) {
                return Some(left.cmp(&right));
            }
            left.as_f64()?.partial_cmp(&right.as_f64()?)
        }
    }
}

fn integer_bitwise(op: BinaryOp, left: Value, right: Value) -> Result<Value, EvalFail> {
    if let (Value::Bool(left), Value::Bool(right)) = (&left, &right) {
        return Ok(Value::Bool(match op {
            BinaryOp::And | BinaryOp::BitAnd => *left && *right,
            BinaryOp::Or | BinaryOp::BitOr => *left || *right,
            BinaryOp::BitXor => *left ^ *right,
            _ => {
                return Err(EvalFail::new(error_codes::E4001, "不支持的布尔位运算"));
            }
        }));
    }
    if matches!(left, Value::Int(_)) || matches!(right, Value::Int(_)) {
        let left = left
            .as_i128()
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "有符号整数超出 i128 范围"))?;
        let right = right
            .as_i128()
            .ok_or_else(|| EvalFail::new(error_codes::E4001, "有符号整数超出 i128 范围"))?;
        return Ok(Value::Int(match op {
            BinaryOp::And | BinaryOp::BitAnd => left & right,
            BinaryOp::Or | BinaryOp::BitOr => left | right,
            BinaryOp::BitXor => left ^ right,
            _ => return Err(EvalFail::new(error_codes::E4001, "不支持的位运算")),
        }));
    }
    let left = left
        .as_u128()
        .ok_or_else(|| EvalFail::new(error_codes::E4001, "位运算需要整数"))?;
    let right = right
        .as_u128()
        .ok_or_else(|| EvalFail::new(error_codes::E4001, "位运算需要整数"))?;
    Ok(Value::UInt(match op {
        BinaryOp::And | BinaryOp::BitAnd => left & right,
        BinaryOp::Or | BinaryOp::BitOr => left | right,
        BinaryOp::BitXor => left ^ right,
        _ => return Err(EvalFail::new(error_codes::E4001, "不支持的位运算")),
    }))
}

fn apply_pure_call(name: &str, args: &[Value]) -> Option<Result<Value, EvalFail>> {
    match name {
        "min" | "max" => {
            if args.len() != 2 {
                return Some(Err(EvalFail::new(
                    error_codes::E4001,
                    format!("`{name}` 需要 2 个参数"),
                )));
            }
            let Some(ordering) = compare_values(&args[0], &args[1]) else {
                return Some(Err(EvalFail::new(
                    error_codes::E4001,
                    format!("`{name}` 的参数不可比较"),
                )));
            };
            let take_left = if name == "min" {
                ordering != std::cmp::Ordering::Greater
            } else {
                ordering != std::cmp::Ordering::Less
            };
            Some(Ok(if take_left {
                args[0].clone()
            } else {
                args[1].clone()
            }))
        }
        "abs" => {
            if args.len() != 1 {
                return Some(Err(EvalFail::new(
                    error_codes::E4001,
                    "`abs` 需要 1 个参数",
                )));
            }
            Some(match &args[0] {
                Value::Float(value) => Ok(Value::Float(value.abs())),
                Value::Int(value) => value
                    .checked_abs()
                    .map(Value::Int)
                    .ok_or_else(|| EvalFail::new(error_codes::E4001, "`abs` 溢出")),
                Value::UInt(value) => Ok(Value::UInt(*value)),
                other => Err(EvalFail::new(
                    error_codes::E4001,
                    format!("`abs` 不支持 {}", other.type_name()),
                )),
            })
        }
        "clamp" => {
            if args.len() != 3 {
                return Some(Err(EvalFail::new(
                    error_codes::E4001,
                    "`clamp` 需要 3 个参数",
                )));
            }
            let (Some(low), Some(high)) = (
                compare_values(&args[1], &args[2]),
                compare_values(&args[2], &args[1]),
            ) else {
                return Some(Err(EvalFail::new(
                    error_codes::E4001,
                    "`clamp` 的参数不可比较",
                )));
            };
            if low == std::cmp::Ordering::Greater && high == std::cmp::Ordering::Less {
                return Some(Err(EvalFail::new(
                    error_codes::E4001,
                    "`clamp` 的下界大于上界",
                )));
            }
            let result = if compare_values(&args[0], &args[1]) == Some(std::cmp::Ordering::Less) {
                args[1].clone()
            } else if compare_values(&args[0], &args[2]) == Some(std::cmp::Ordering::Greater) {
                args[2].clone()
            } else {
                args[0].clone()
            };
            Some(Ok(result))
        }
        "len" => {
            if args.len() != 1 {
                return Some(Err(EvalFail::new(
                    error_codes::E4001,
                    "`len` 需要 1 个参数",
                )));
            }
            Some(match &args[0] {
                Value::Str(value) => Ok(Value::UInt(value.len() as u128)),
                Value::Bytes(value) => Ok(Value::UInt(value.len() as u128)),
                other => Err(EvalFail::new(
                    error_codes::E4001,
                    format!("`len` 不支持 {}", other.type_name()),
                )),
            })
        }
        _ => None,
    }
}

fn expr_uses_vars(expr: &Expr, env: &EvalEnv) -> bool {
    match expr {
        Expr::Literal(_)
        | Expr::Cursor
        | Expr::FileSize
        | Expr::StructStart
        | Expr::StructEnd
        | Expr::SizeOf(_) => false,
        Expr::Path(name) => env.vars.contains_key(name),
        Expr::Unary { expr, .. } => expr_uses_vars(expr, env),
        Expr::Binary { lhs, rhs, .. } => expr_uses_vars(lhs, env) || expr_uses_vars(rhs, env),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            expr_uses_vars(cond, env)
                || expr_uses_vars(then_expr, env)
                || expr_uses_vars(else_expr, env)
        }
        Expr::Call { callee, args } => {
            expr_uses_vars(callee, env) || args.iter().any(|arg| expr_uses_vars(arg, env))
        }
        Expr::Index { base, index } => expr_uses_vars(base, env) || expr_uses_vars(index, env),
        Expr::Member { base, .. } => expr_uses_vars(base, env),
        Expr::Len(inner) => expr_uses_vars(inner, env),
        // 赋值总是编译成对 let 变量的改写，需要走静态展开路径。
        Expr::Assign { .. } => true,
    }
}
