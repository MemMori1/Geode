//! BinSpec 抽象语法树。
//!
//! DESIGN: 02-dsl-spec-draft.md §3（类型）、§4（语句）、§5（表达式）、§6（属性）。
//!
//! AST 只承载语法，不做名字解析、不做类型检查——那些属于后续 `bytecode`/`verify`
//! 阶段。所有节点都可 `Clone`/`PartialEq`，方便增量解析对比与新旧的语法树比较。

/// 一个 BinSpec 源文件。
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// 顶层项，按源码顺序排列。
    pub items: Vec<Item>,
}

/// 顶层项。
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// `struct Name { ... }`
    Struct(StructDecl),
    /// `union Name { ... }`
    Union(UnionDecl),
    /// `enum Name : u16 { ... }`
    Enum(EnumDecl),
    /// `bitfield Name : u8 { ... }`
    Bitfield(BitfieldDecl),
    /// `fn name(...) -> T { ... }`
    Fn(FnDecl),
    /// `#pragma endian = little;`
    Pragma(PragmaDecl),
}

/// 结构体声明。
///
/// `size` 对应 `struct Header : 30 { ... }` 的显式定长；普通变长结构为 `None`。
#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    /// 结构体名。
    pub name: String,
    /// 附着在声明上的属性（`#[...]`）。
    pub attrs: Vec<Attr>,
    /// 成员列表。
    pub members: Vec<Member>,
    /// 显式定长（`struct X : 30`）；变长结构为 `None`。
    pub size: Option<Expr>,
}

/// 联合体声明。所有成员同偏移；`size` 可显式指定。
#[derive(Debug, Clone, PartialEq)]
pub struct UnionDecl {
    /// 联合体名。
    pub name: String,
    /// 声明属性。
    pub attrs: Vec<Attr>,
    /// 成员列表。
    pub members: Vec<Member>,
    /// 显式定长（`union X : 8`）；未指定时由后端取成员最大值。
    pub size: Option<Expr>,
}

/// 枚举声明。
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    /// 枚举名。
    pub name: String,
    /// 底层整数类型（`enum Format : u16`）。
    pub repr: Option<TypeRef>,
    /// 声明属性。
    pub attrs: Vec<Attr>,
    /// 枚举项。
    pub variants: Vec<EnumVariant>,
}

/// 一个枚举项。
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    /// 枚举项名。
    pub name: String,
    /// 显式值（`Png = 0x8950`）。
    pub value: Option<Expr>,
    /// 属性。
    pub attrs: Vec<Attr>,
}

/// 位域声明。
#[derive(Debug, Clone, PartialEq)]
pub struct BitfieldDecl {
    /// 位域名。
    pub name: String,
    /// 存储单元类型（`bitfield Flags : u8`）。
    pub repr: Option<TypeRef>,
    /// 声明属性；`#[bits: msb]` 也在这里。
    pub attrs: Vec<Attr>,
    /// 位域成员。
    pub members: Vec<BitfieldMember>,
}

/// 一个位域成员。
#[derive(Debug, Clone, PartialEq)]
pub struct BitfieldMember {
    /// 成员名；`_ : 6` 这类占位位为 `None`。
    pub name: Option<String>,
    /// 位宽表达式。
    pub width: Expr,
    /// 属性。
    pub attrs: Vec<Attr>,
}

/// 函数声明。
#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    /// 函数名。
    pub name: String,
    /// 声明属性。
    pub attrs: Vec<Attr>,
    /// 参数列表。
    pub params: Vec<Param>,
    /// 返回类型（`-> u32`）；无返回值为 `None`。
    pub ret: Option<TypeRef>,
    /// 函数体。
    pub body: Block,
}

/// 函数参数。
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// 参数名。
    pub name: String,
    /// 参数类型。
    pub ty: TypeRef,
}

/// `#pragma` 声明。
#[derive(Debug, Clone, PartialEq)]
pub struct PragmaDecl {
    /// pragma 名（如 `endian`、`image_base`）。
    pub name: String,
    /// 可选值（如 `little`、`0x400000`）。
    pub value: Option<Expr>,
    /// 属性（保留位，当前语法不接受 pragma 前置属性）。
    pub attrs: Vec<Attr>,
}

/// 结构/联合的成员声明。
///
/// `padding[4];` 没有类型名与字段名，前端把它规范化为
/// `Member { ty: Prim(Byte), name: "_", array: Some(Fixed(4)), attrs: [padding] }`，
/// 这样后端只需要看 `padding` 属性就知道它不产生值。见 `PARSER-DEVIATION` 注释。
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// 成员类型。
    pub ty: TypeRef,
    /// 成员名；padding 成员固定为 `"_"`。
    pub name: String,
    /// 数组规格；标量为 `None`。
    pub array: Option<ArraySpec>,
    /// `@` 指定的读取偏移。
    pub at: Option<Expr>,
    /// `if` 守卫条件。
    pub cond: Option<Expr>,
    /// 成员属性。
    pub attrs: Vec<Attr>,
}

/// 数组成员的规格。
#[derive(Debug, Clone, PartialEq)]
pub enum ArraySpec {
    /// 定长/表达式定长：`u8 magic[4]`、`u8 data[count]`、`u8 data[] size=n`。
    Fixed(Expr),
    /// 读到终止条件：`char s[] until(0)`、`u8 data[until(0)]`。
    Until(Expr),
    /// 读到文件尾：`u8 data[]`。
    Unsized,
}

/// 类型引用。
#[derive(Debug, Clone, PartialEq)]
pub enum TypeRef {
    /// 基础类型。
    Prim(Prim),
    /// 用户定义类型（结构体/联合体/枚举/别名）。
    Named(String),
    /// 指针类型：`off32`、`rva64`、`rel32`……可选尖括号里的目标类型 `off32<T>`。
    Ptr {
        /// 指针空间与位宽。
        kind: PtrKind,
        /// 目标类型；裸指针（只有空间/位宽、没有目标类型）为 `None`。
        inner: Option<Box<TypeRef>>,
    },
    /// 参数化字符串类型：`str(enc="utf8", term=0)`。
    Str(StrArgs),
    /// 匿名内联结构体：`struct { ... } eocd;`（02 §10 示例需要）。
    InlineStruct(Box<StructDecl>),
}

/// 02 §3.1 的全部基础类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prim {
    /// 8 位无符号。
    U8,
    /// 16 位无符号。
    U16,
    /// 24 位无符号。
    U24,
    /// 32 位无符号。
    U32,
    /// 48 位无符号。
    U48,
    /// 64 位无符号。
    U64,
    /// 128 位无符号。
    U128,
    /// 8 位有符号。
    S8,
    /// 16 位有符号。
    S16,
    /// 24 位有符号。
    S24,
    /// 32 位有符号。
    S32,
    /// 48 位有符号。
    S48,
    /// 64 位有符号。
    S64,
    /// 128 位有符号。
    S128,
    /// 16 位浮点（读入后提升为 f64）。
    F16,
    /// 32 位浮点。
    F32,
    /// 64 位浮点。
    F64,
    /// 布尔：1 字节，0=false，非 0=true。
    Bool,
    /// ASCII 字符：1 字节。
    Char,
    /// 原始字节：1 字节，无符号。
    Byte,
}

impl Prim {
    /// 按源码拼写解析基础类型名。
    #[must_use]
    pub fn parse_name(name: &str) -> Option<Self> {
        Some(match name {
            "u8" => Self::U8,
            "u16" => Self::U16,
            "u24" => Self::U24,
            "u32" => Self::U32,
            "u48" => Self::U48,
            "u64" => Self::U64,
            "u128" => Self::U128,
            "s8" => Self::S8,
            "s16" => Self::S16,
            "s24" => Self::S24,
            "s32" => Self::S32,
            "s48" => Self::S48,
            "s64" => Self::S64,
            "s128" => Self::S128,
            "f16" => Self::F16,
            "f32" => Self::F32,
            "f64" => Self::F64,
            "bool" => Self::Bool,
            "char" => Self::Char,
            "byte" => Self::Byte,
            _ => return None,
        })
    }

    /// 源码拼写。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U24 => "u24",
            Self::U32 => "u32",
            Self::U48 => "u48",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::S8 => "s8",
            Self::S16 => "s16",
            Self::S24 => "s24",
            Self::S32 => "s32",
            Self::S48 => "s48",
            Self::S64 => "s64",
            Self::S128 => "s128",
            Self::F16 => "f16",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Char => "char",
            Self::Byte => "byte",
        }
    }

    /// 固定字节宽度（02 §3.1 保证所有基础类型定长）。
    #[must_use]
    pub const fn byte_size(self) -> u32 {
        match self {
            Self::U8 | Self::S8 | Self::Bool | Self::Char | Self::Byte => 1,
            Self::U16 | Self::S16 | Self::F16 => 2,
            Self::U24 | Self::S24 => 3,
            Self::U32 | Self::S32 | Self::F32 => 4,
            Self::U48 | Self::S48 => 6,
            Self::U64 | Self::S64 | Self::F64 => 8,
            Self::U128 | Self::S128 => 16,
        }
    }
}

/// 指针空间与位宽（02 §3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PtrKind {
    /// 文件绝对偏移，32 位。
    Off32,
    /// 文件绝对偏移，64 位。
    Off64,
    /// 相对镜像基址，32 位。
    Rva32,
    /// 相对镜像基址，64 位。
    Rva64,
    /// 相对自身位置，32 位。
    Rel32,
    /// 相对自身位置，64 位。
    Rel64,
    /// 绝对虚拟地址，32 位。
    Va32,
    /// 绝对虚拟地址，64 位。
    Va64,
}

impl PtrKind {
    /// 按源码拼写解析指针类型名。
    #[must_use]
    pub fn parse_name(name: &str) -> Option<Self> {
        Some(match name {
            "off32" => Self::Off32,
            "off64" => Self::Off64,
            "rva32" => Self::Rva32,
            "rva64" => Self::Rva64,
            "rel32" => Self::Rel32,
            "rel64" => Self::Rel64,
            "va32" => Self::Va32,
            "va64" => Self::Va64,
            _ => return None,
        })
    }

    /// 源码拼写。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off32 => "off32",
            Self::Off64 => "off64",
            Self::Rva32 => "rva32",
            Self::Rva64 => "rva64",
            Self::Rel32 => "rel32",
            Self::Rel64 => "rel64",
            Self::Va32 => "va32",
            Self::Va64 => "va64",
        }
    }

    /// 指针值的字节宽度。
    #[must_use]
    pub const fn byte_size(self) -> u32 {
        match self {
            Self::Off32 | Self::Rva32 | Self::Rel32 | Self::Va32 => 4,
            Self::Off64 | Self::Rva64 | Self::Rel64 | Self::Va64 => 8,
        }
    }
}

/// `str(...)` 的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct StrArgs {
    /// 编码（`enc="utf8"`）。
    pub enc: Option<String>,
    /// 终止符（`term=0`）。
    pub term: Option<Expr>,
    /// 定长字节数（`size=len`）。
    pub size: Option<Expr>,
    /// 最大读取字节数（`max=256`）。
    pub max: Option<Expr>,
}

/// 表达式。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// 字面量。
    Literal(Literal),
    /// 路径（`hdr.width` 的 `hdr`、`std::checksum::crc32`、`Format::Png`）。
    Path(String),
    /// `$`：当前读取偏移。
    Cursor,
    /// `$size`：文件总大小。
    FileSize,
    /// `$start`：当前 struct/union 起始偏移。
    StructStart,
    /// `$end`：当前定长 struct/union 结束偏移。
    StructEnd,
    /// 一元运算。
    Unary {
        /// 运算符。
        op: UnaryOp,
        /// 操作数。
        expr: Box<Expr>,
    },
    /// 二元运算。
    Binary {
        /// 运算符。
        op: BinaryOp,
        /// 左操作数。
        lhs: Box<Expr>,
        /// 右操作数。
        rhs: Box<Expr>,
    },
    /// 三元条件 `cond ? a : b`。
    Ternary {
        /// 条件。
        cond: Box<Expr>,
        /// 条件为真时的值。
        then_expr: Box<Expr>,
        /// 条件为假时的值。
        else_expr: Box<Expr>,
    },
    /// 函数调用（含 `std::` 内建）。
    Call {
        /// 被调用者。
        callee: Box<Expr>,
        /// 实参。
        args: Vec<Expr>,
    },
    /// 索引 `arr[i]`。
    Index {
        /// 被索引表达式。
        base: Box<Expr>,
        /// 下标。
        index: Box<Expr>,
    },
    /// 成员访问 `hdr.width`。
    Member {
        /// 基表达式。
        base: Box<Expr>,
        /// 成员名。
        name: String,
    },
    /// `sizeof(T)`。
    SizeOf(Box<TypeRef>),
    /// `len(x)`。
    Len(Box<Expr>),
    /// 赋值表达式（`for` 的步进 `i = i + 1` 需要）。
    Assign {
        /// 赋值运算符。
        op: AssignOp,
        /// 被赋值目标。
        target: Box<Expr>,
        /// 右值。
        value: Box<Expr>,
    },
}

/// 字面量。
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// 整数（VM 内部按 u128 存，写出时按字段类型截断）。
    Int(u128),
    /// 浮点。
    Float(f64),
    /// 字符串。
    Str(String),
    /// 字符。
    Char(char),
    /// 布尔。
    Bool(bool),
    /// 空值。
    Null,
}

/// 一元运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `-`
    Neg,
    /// `+`
    Pos,
    /// `!`
    Not,
    /// `~`
    BitNot,
}

/// 二元运算符（优先级见 02 §5.2 的 C 规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    And,
    /// `||`
    Or,
    /// `**` 幂运算（02 §5.2 补充）。
    Pow,
}

/// 赋值运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignOp {
    /// `=`
    Assign,
    /// `+=`
    AddAssign,
    /// `-=`
    SubAssign,
}

/// 语句。
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let name = expr;`
    Let(LetStmt),
    /// `if cond { ... } else { ... }`
    If(IfStmt),
    /// `match expr { ... }`
    Match(MatchStmt),
    /// C 风格 `for (init; cond; step) { ... }`
    For(ForStmt),
    /// `while cond { ... }`
    While(WhileStmt),
    /// `return expr;` / `return;`
    Return(Option<Expr>),
    /// `break;`
    Break,
    /// `continue;`
    Continue,
    /// 表达式语句。
    Expr(Expr),
    /// 块内的成员声明（`Entry e;`、`u8 data[4];`）。
    ///
    /// 这是 02 §4.4 控制流示例所需的扩展：控制流块里消费字节的声明不是
    /// `let`，也不产生值。先保留为独立变体，后续编译阶段再与顶层 `Member` 合并。
    Member(Member),
}

/// `let` 语句。
#[derive(Debug, Clone, PartialEq)]
pub struct LetStmt {
    /// 变量名。
    pub name: String,
    /// 可选类型标注（`let x: u32 = ...`）。
    pub ty: Option<TypeRef>,
    /// 可选初值。
    pub init: Option<Expr>,
}

/// `if` 语句。
#[derive(Debug, Clone, PartialEq)]
pub struct IfStmt {
    /// 条件。
    pub cond: Expr,
    /// 真分支。
    pub then_branch: Block,
    /// 假分支（块或 `else if`）。
    pub else_branch: Option<ElseBranch>,
}

/// `else` 分支。
#[derive(Debug, Clone, PartialEq)]
pub enum ElseBranch {
    /// `else { ... }`
    Block(Block),
    /// `else if ...`
    If(Box<IfStmt>),
}

/// `match` 语句。
#[derive(Debug, Clone, PartialEq)]
pub struct MatchStmt {
    /// 被匹配表达式。
    pub scrutinee: Expr,
    /// 分支列表。
    pub arms: Vec<MatchArm>,
}

/// 一个 `match` 分支。
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    /// 模式。
    pub pattern: Pattern,
    /// 分支体。
    pub body: Block,
}

/// `match` 模式。
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// 通配 `_`。
    Wildcard,
    /// 常量/路径模式。
    Expr(Expr),
}

/// C 风格 `for` 语句。
#[derive(Debug, Clone, PartialEq)]
pub struct ForStmt {
    /// 初始化语句（`u32 i = 0` 或 `let i = 0`）。
    pub init: Option<Box<Stmt>>,
    /// 循环条件。
    pub cond: Option<Expr>,
    /// 每轮步进。
    pub step: Option<Expr>,
    /// 循环体。
    pub body: Block,
}

/// `while` 语句。
#[derive(Debug, Clone, PartialEq)]
pub struct WhileStmt {
    /// 循环条件。
    pub cond: Expr,
    /// 循环体。
    pub body: Block,
}

/// 语句块。
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// 块内语句。
    pub stmts: Vec<Stmt>,
}

/// 属性 `#[name(args)]`。
///
/// `args` 是“token-ish”字符串：每个逗号分隔参数由原始 token 文本直接拼接而成，
/// 字符串字面量去引号，保留 `hdr.length == 8` 这类表达式的可读形态。
#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    /// 属性名（可带命名空间，如 `ai:conf`）。
    pub name: String,
    /// 逗号分隔的参数文本。
    pub args: Vec<String>,
}
