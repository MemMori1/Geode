//! BinSpec 递归下降解析器。
//!
//! DESIGN: 02-dsl-spec-draft.md §3（类型）、§4（语句）、§5（表达式）、§6（属性）。
//!
//! 解析器面向编辑器：错误不抛异常、不在第一处停止，而是记录带 [`SrcSpan`] 的诊断后
//! 跳到下一个 `;`/`}` 继续。一次解析至少给调用方 3 条错误（源码里确实有 3 处错误时），
//! 这样 LSP 不会让用户改一个错、再按一次保存才看见下一个。
//!
//! 公开入口有两个：
//! - [`parse`]：解析已经 lex 好的 token 流；
//! - [`parse_source`]：词法 + 语法一条龙。
//!
//! [`SrcSpan`]: crate::span::SrcSpan

use crate::ast::{
    ArraySpec, AssignOp, Attr, BinaryOp, BitfieldDecl, BitfieldMember, Block, ElseBranch, EnumDecl,
    EnumVariant, Expr, FnDecl, ForStmt, IfStmt, Item, LetStmt, Literal, MatchArm, MatchStmt,
    Member, Param, Pattern, PragmaDecl, Prim, Program, PtrKind, Stmt, StrArgs, StructDecl, TypeRef,
    UnaryOp, UnionDecl, WhileStmt,
};
use crate::diag::{Diagnostics, ParseError};
use crate::lexer::{Lexer, Token, TokenKind};
use crate::span::SrcSpan;
use geode_core::diag::Diag;

/// 表达式/块的递归嵌套上限；防止恶意或损坏的输入把栈打爆。
const MAX_DEPTH: u32 = 256;

/// 解析结果别名。
type PResult<T> = Result<T, ParseError>;

/// 解析 token 流。
///
/// # Errors
/// 返回按源码顺序排列的全部语法诊断；每条诊断都带 [`SrcSpan`]。
pub fn parse(tokens: &[Token]) -> Result<Program, Vec<Diag>> {
    Parser::new(tokens).parse()
}

/// 词法 + 语法解析一段源码。
///
/// # Errors
/// 词法错误优先返回；词法通过后返回全部语法诊断。
pub fn parse_source(src: &str) -> Result<Program, Vec<Diag>> {
    match Lexer::new(src).tokenize() {
        Ok(tokens) => parse(&tokens),
        Err(diagnostics) => Err(diagnostics),
    }
}

/// 有状态的递归下降解析器。
///
/// 通常直接用 [`parse`]/[`parse_source`]；需要复用缓冲区或做增量解析时可手动构造。
pub struct Parser {
    /// 输入 token（不含结尾 Eof；Eof 单独放在 `eof`）。
    tokens: Vec<Token>,
    /// 当前读取位置。
    pos: usize,
    /// 文件结束哨兵。
    eof: Token,
    /// 已收集诊断。
    diagnostics: Diagnostics,
    /// 当前递归深度。
    depth: u32,
}

impl Parser {
    /// 从 token 切片创建解析器。
    #[must_use]
    pub fn new(tokens: &[Token]) -> Self {
        let mut tokens = tokens.to_vec();
        let eof = match tokens.last() {
            Some(token) if token.kind == TokenKind::Eof => match tokens.pop() {
                Some(token) => token,
                None => Token::new(TokenKind::Eof, SrcSpan::empty(0)),
            },
            Some(token) => Token::new(TokenKind::Eof, SrcSpan::empty(token.span.end)),
            None => Token::new(TokenKind::Eof, SrcSpan::empty(0)),
        };
        Self {
            tokens,
            pos: 0,
            eof,
            diagnostics: Diagnostics::new(),
            depth: 0,
        }
    }

    /// 解析整个程序。
    ///
    /// # Errors
    /// 任一顶层项或语句无法解析时，返回收集到的全部诊断。
    pub fn parse(mut self) -> Result<Program, Vec<Diag>> {
        let mut items = Vec::new();
        while !self.at_eof() {
            let before = self.pos;
            match self.parse_item() {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {}
                Err(error) => {
                    self.record(error);
                    if self.pos == before {
                        self.bump();
                    }
                    if !self.looks_like_item_start() && !self.at_rbrace() {
                        self.recover_statement();
                    }
                    if self.at_rbrace() {
                        self.bump();
                    }
                }
            }
        }
        if self.diagnostics.has_errors() {
            Err(self.diagnostics.into_vec())
        } else {
            Ok(Program { items })
        }
    }

    /// 记录一条诊断。
    fn record(&mut self, error: ParseError) {
        self.diagnostics.push(error.into_diag());
    }

    /// 当前 token；越过末尾时返回 Eof 哨兵。
    fn current(&self) -> &Token {
        match self.tokens.get(self.pos) {
            Some(token) => token,
            None => &self.eof,
        }
    }

    /// 向前看第 `n` 个 token（`n=0` 即当前 token）。
    fn peek_token(&self, n: usize) -> &Token {
        match self.tokens.get(self.pos.saturating_add(n)) {
            Some(token) => token,
            None => &self.eof,
        }
    }

    /// 当前 token 是否为指定类型的单位变体。
    fn at(&self, kind: &TokenKind) -> bool {
        core::mem::discriminant(&self.current().kind) == core::mem::discriminant(kind)
    }

    /// 向前看第 `n` 个 token 是否为指定类型。
    fn peek_is(&self, n: usize, kind: &TokenKind) -> bool {
        core::mem::discriminant(&self.peek_token(n).kind) == core::mem::discriminant(kind)
    }

    /// 当前是否文件结束。
    fn at_eof(&self) -> bool {
        self.at(&TokenKind::Eof)
    }

    /// 当前是否 `}`。
    fn at_rbrace(&self) -> bool {
        self.at(&TokenKind::RBrace)
    }

    /// 向前看 `n` 处的标识符文本。
    fn ident_at(&self, n: usize) -> Option<&str> {
        match &self.peek_token(n).kind {
            TokenKind::Ident(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// 消费当前 token 并返回它；Eof 不前进。
    fn bump(&mut self) -> Token {
        let token = self.current().clone();
        if !self.at_eof() {
            self.pos = self.pos.saturating_add(1);
        }
        token
    }

    /// 当前是指定类型时消费并返回 `true`。
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// 期望一个单位 token 类型。
    fn expect(&mut self, kind: &TokenKind, what: &str) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            Err(self.err_here(format!("期望{what}，实际是 {}", self.current().describe())))
        }
    }

    /// 期望一个标识符。
    fn expect_ident(&mut self, what: &str) -> PResult<String> {
        match self.current().kind.clone() {
            TokenKind::Ident(name) => {
                self.bump();
                Ok(name)
            }
            _ => Err(self.err_here(format!("期望{what}，实际是 {}", self.current().describe()))),
        }
    }

    /// 当前 token 位置的语法错误。
    fn err_here(&self, detail: impl Into<String>) -> ParseError {
        ParseError::syntax(self.current().span, detail)
    }

    /// 进入一层递归；超过上限时返回错误而不是栈溢出。
    fn enter(&mut self) -> PResult<()> {
        if self.depth >= MAX_DEPTH {
            return Err(self.err_here("语法嵌套过深（超过 256 层）"));
        }
        self.depth = self.depth.saturating_add(1);
        Ok(())
    }

    /// 离开一层递归。
    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// 跳到下一个 `;` 或 `}`；`;` 会一并消费，`}` 留给外层。
    fn recover_statement(&mut self) {
        let mut brace_depth = 0_u32;
        while !self.at_eof() {
            if self.at(&TokenKind::LBrace) {
                brace_depth = brace_depth.saturating_add(1);
                self.bump();
            } else if self.at_rbrace() {
                if brace_depth == 0 {
                    return;
                }
                brace_depth = brace_depth.saturating_sub(1);
                self.bump();
            } else if self.at(&TokenKind::Semi) {
                self.bump();
                return;
            } else {
                self.bump();
            }
        }
    }

    /// 当前 token 是否像顶层项的开始。
    fn looks_like_item_start(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::KwStruct
                | TokenKind::KwUnion
                | TokenKind::KwEnum
                | TokenKind::KwBitfield
                | TokenKind::KwFn
                | TokenKind::Hash
                | TokenKind::AttrStart
        )
    }

    /// 当前 token 是否像成员声明的开始。
    fn looks_like_member(&self) -> bool {
        match &self.current().kind {
            TokenKind::KwPadding | TokenKind::KwBe | TokenKind::KwLe => true,
            TokenKind::KwStruct => self.peek_is(1, &TokenKind::LBrace),
            TokenKind::Ident(name) => {
                if Prim::parse_name(name).is_some()
                    || PtrKind::parse_name(name).is_some()
                    || name == "str"
                {
                    return true;
                }
                if name == "rest" && self.peek_is(1, &TokenKind::LBracket) {
                    return true;
                }
                self.ident_at(1).is_some()
            }
            _ => false,
        }
    }

    /// 当前 token 是否像块内语句的开始。
    fn looks_like_stmt_start(&self) -> bool {
        self.at(&TokenKind::KwLet)
            || self.at(&TokenKind::KwIf)
            || self.at(&TokenKind::KwMatch)
            || self.at(&TokenKind::KwFor)
            || self.at(&TokenKind::KwWhile)
            || self.at(&TokenKind::KwReturn)
            || self.at(&TokenKind::KwBreak)
            || self.at(&TokenKind::KwContinue)
            || self.looks_like_member()
    }

    // ---------------------------------------------------------------------
    // 顶层项
    // ---------------------------------------------------------------------

    /// 解析一个顶层项；多余的分号返回 `None`。
    fn parse_item(&mut self) -> PResult<Option<Item>> {
        if self.eat(&TokenKind::Semi) {
            return Ok(None);
        }
        let attrs = self.parse_attrs();
        match self.current().kind.clone() {
            TokenKind::KwStruct => Ok(Some(Item::Struct(self.parse_struct_decl(attrs)?))),
            TokenKind::KwUnion => Ok(Some(Item::Union(self.parse_union_decl(attrs)?))),
            TokenKind::KwEnum => Ok(Some(Item::Enum(self.parse_enum_decl(attrs)?))),
            TokenKind::KwBitfield => Ok(Some(Item::Bitfield(self.parse_bitfield_decl(attrs)?))),
            TokenKind::KwFn => Ok(Some(Item::Fn(self.parse_fn_decl(attrs)?))),
            TokenKind::Hash => self.parse_pragma().map(|pragma| Some(Item::Pragma(pragma))),
            _ => Err(self.err_here(format!(
                "期望顶层声明（struct/union/enum/bitfield/fn/#pragma），实际是 {}",
                self.current().describe()
            ))),
        }
    }

    // ---------------------------------------------------------------------
    // 属性
    // ---------------------------------------------------------------------

    /// 解析零个或多个 `#[...]` 属性。
    ///
    /// 属性内部的错误在这里就地恢复，不阻塞外层声明；最终由 `parse` 统一返回。
    fn parse_attrs(&mut self) -> Vec<Attr> {
        let mut attrs = Vec::new();
        while self.at(&TokenKind::AttrStart) {
            self.bump();
            match self.parse_attr_body() {
                Ok(attr) => attrs.push(attr),
                Err(error) => {
                    self.record(error);
                    self.skip_attr_tail();
                }
            }
        }
        attrs
    }

    /// 解析 `#[` 之后、`]` 之前的属性体。
    fn parse_attr_body(&mut self) -> PResult<Attr> {
        let mut name = self.expect_ident("属性名")?;
        // `ai:conf(...)` 这类带命名空间的属性；`bits: lsb` 的冒号不是名字的一部分。
        loop {
            if self.at(&TokenKind::ColonColon) && self.ident_at(1).is_some() {
                self.bump();
                if let Ok(segment) = self.expect_ident("属性名段") {
                    name.push_str("::");
                    name.push_str(&segment);
                }
            } else if self.at(&TokenKind::Colon)
                && self.ident_at(1).is_some()
                && self.peek_is(2, &TokenKind::LParen)
            {
                self.bump();
                if let Ok(segment) = self.expect_ident("属性名段") {
                    name.push(':');
                    name.push_str(&segment);
                }
            } else {
                break;
            }
        }

        let mut args = Vec::new();
        if self.eat(&TokenKind::Colon) {
            let arg = self.collect_attr_arg();
            if !arg.is_empty() {
                args.push(arg);
            }
        } else if self.eat(&TokenKind::LParen) {
            if !self.at(&TokenKind::RParen) {
                loop {
                    let arg = self.collect_attr_arg();
                    if !arg.is_empty() {
                        args.push(arg);
                    }
                    if self.eat(&TokenKind::Comma) {
                        if self.at(&TokenKind::RParen) {
                            break;
                        }
                        continue;
                    }
                    break;
                }
            }
            self.expect(&TokenKind::RParen, "`)`")?;
        } else if self.eat(&TokenKind::Eq) {
            let arg = self.collect_attr_arg();
            if !arg.is_empty() {
                args.push(arg);
            }
        }
        self.expect(&TokenKind::RBracket, "`]`")?;
        Ok(Attr { name, args })
    }

    /// 收集一个属性参数，直到同层的 `,`/`)`/`]`；不消费终止符。
    fn collect_attr_arg(&mut self) -> String {
        let mut text = String::new();
        let mut depth = 0_i32;
        while !self.at_eof() {
            match &self.current().kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    depth = depth.saturating_add(1);
                    text.push_str(&self.current().text());
                    self.bump();
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth = depth.saturating_sub(1);
                    text.push_str(&self.current().text());
                    self.bump();
                }
                TokenKind::Comma if depth == 0 => break,
                _ => {
                    text.push_str(&self.current().text());
                    self.bump();
                }
            }
        }
        text
    }

    /// 属性体解析失败后跳到 `]`；若先遇到声明边界则停下，避免吞掉后文。
    fn skip_attr_tail(&mut self) {
        while !self.at_eof() {
            if self.eat(&TokenKind::RBracket) {
                return;
            }
            if self.at(&TokenKind::Semi)
                || self.at(&TokenKind::LBrace)
                || self.at_rbrace()
                || self.at(&TokenKind::AttrStart)
            {
                return;
            }
            self.bump();
        }
    }

    // ---------------------------------------------------------------------
    // 类型
    // ---------------------------------------------------------------------

    /// 解析一个类型引用。
    fn parse_type_ref(&mut self) -> PResult<TypeRef> {
        if self.at(&TokenKind::KwStruct) && self.peek_is(1, &TokenKind::LBrace) {
            return self.parse_inline_struct_type();
        }
        let first = self.expect_ident("类型名")?;
        if first == "str" && self.at(&TokenKind::LParen) {
            return self.parse_str_args().map(TypeRef::Str);
        }
        if let Some(prim) = Prim::parse_name(&first) {
            return Ok(TypeRef::Prim(prim));
        }
        if let Some(kind) = PtrKind::parse_name(&first) {
            let inner = if self.eat(&TokenKind::Lt) {
                let target = self.parse_type_ref()?;
                self.expect(&TokenKind::Gt, "`>`")?;
                Some(Box::new(target))
            } else {
                None
            };
            return Ok(TypeRef::Ptr { kind, inner });
        }
        let mut path = first;
        while self.at(&TokenKind::ColonColon) && self.ident_at(1).is_some() {
            self.bump();
            let segment = self.expect_ident("类型名段")?;
            path.push_str("::");
            path.push_str(&segment);
        }
        Ok(TypeRef::Named(path))
    }

    /// 匿名内联结构体 `struct { ... }`（02 §10 的 `eocd` 成员需要）。
    fn parse_inline_struct_type(&mut self) -> PResult<TypeRef> {
        self.expect(&TokenKind::KwStruct, "`struct`")?;
        let members = self.parse_member_list()?;
        Ok(TypeRef::InlineStruct(Box::new(StructDecl {
            name: String::new(),
            attrs: Vec::new(),
            members,
            size: None,
        })))
    }

    /// `str(enc="utf8", term=0)` 的参数列表。
    fn parse_str_args(&mut self) -> PResult<StrArgs> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut args = StrArgs {
            enc: None,
            term: None,
            size: None,
            max: None,
        };
        if self.eat(&TokenKind::RParen) {
            return Ok(args);
        }
        loop {
            let key = self.expect_ident("str 参数名")?;
            self.expect(&TokenKind::Eq, "`=`")?;
            match key.as_str() {
                "enc" => match self.current().kind.clone() {
                    TokenKind::Str(value) => {
                        self.bump();
                        args.enc = Some(value);
                    }
                    _ => {
                        return Err(self.err_here(format!(
                            "str 的 enc 参数需要字符串字面量，实际是 {}",
                            self.current().describe()
                        )));
                    }
                },
                "term" => args.term = Some(self.parse_expr()?),
                "size" => args.size = Some(self.parse_expr()?),
                "max" => args.max = Some(self.parse_expr()?),
                _ => {
                    return Err(
                        self.err_here(format!("未知 str 参数 `{key}`（只支持 enc/term/size/max）"))
                    );
                }
            }
            if self.eat(&TokenKind::Comma) {
                if self.at(&TokenKind::RParen) {
                    break;
                }
                continue;
            }
            break;
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        Ok(args)
    }

    // ---------------------------------------------------------------------
    // 声明
    // ---------------------------------------------------------------------

    /// `struct Name [: size] { members }`
    fn parse_struct_decl(&mut self, attrs: Vec<Attr>) -> PResult<StructDecl> {
        self.expect(&TokenKind::KwStruct, "`struct`")?;
        let name = self.expect_ident("结构体名")?;
        let size = if self.eat(&TokenKind::Colon) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let members = self.parse_member_list()?;
        let _ = self.eat(&TokenKind::Semi);
        Ok(StructDecl {
            name,
            attrs,
            members,
            size,
        })
    }

    /// `union Name [: size] { members }`
    fn parse_union_decl(&mut self, attrs: Vec<Attr>) -> PResult<UnionDecl> {
        self.expect(&TokenKind::KwUnion, "`union`")?;
        let name = self.expect_ident("联合体名")?;
        let size = if self.eat(&TokenKind::Colon) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let members = self.parse_member_list()?;
        let _ = self.eat(&TokenKind::Semi);
        Ok(UnionDecl {
            name,
            attrs,
            members,
            size,
        })
    }

    /// `enum Name [: repr] { Variant [= value], ... }`
    fn parse_enum_decl(&mut self, attrs: Vec<Attr>) -> PResult<EnumDecl> {
        self.expect(&TokenKind::KwEnum, "`enum`")?;
        let name = self.expect_ident("枚举名")?;
        let repr = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut variants = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let before = self.pos;
            let variant = self.parse_enum_variant();
            match variant {
                Ok(variant) => {
                    variants.push(variant);
                    if self.eat(&TokenKind::Comma) || self.eat(&TokenKind::Semi) {
                        continue;
                    }
                    if !self.at_rbrace() {
                        let error = self.err_here("枚举项之间需要 `,` 或 `;`");
                        self.record(error);
                    }
                }
                Err(error) => {
                    self.record(error);
                    if self.pos == before {
                        self.bump();
                    }
                    if !self.at_rbrace() && !matches!(self.current().kind, TokenKind::Ident(_)) {
                        self.recover_statement();
                    }
                }
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        let _ = self.eat(&TokenKind::Semi);
        Ok(EnumDecl {
            name,
            repr,
            attrs,
            variants,
        })
    }

    /// 一个枚举项。
    fn parse_enum_variant(&mut self) -> PResult<EnumVariant> {
        let attrs = self.parse_attrs();
        let name = self.expect_ident("枚举项名")?;
        let value = if self.eat(&TokenKind::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(EnumVariant { name, value, attrs })
    }

    /// `bitfield Name [: repr] [#[bits: ...]] { name : width; ... }`
    fn parse_bitfield_decl(&mut self, attrs: Vec<Attr>) -> PResult<BitfieldDecl> {
        self.expect(&TokenKind::KwBitfield, "`bitfield`")?;
        let name = self.expect_ident("位域名")?;
        let repr = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };
        // 02 §3.5 的示例把 `#[bits: lsb]` 写在 repr 与 `{` 之间；
        // 前置属性与这里读到的属性合并，二者都归 BitfieldDecl::attrs。
        let mut attrs = attrs;
        attrs.extend(self.parse_attrs());
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut members = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let member_attrs = self.parse_attrs();
            if self.at_rbrace() {
                break;
            }
            let member_name = if self.ident_at(0) == Some("_") {
                self.bump();
                None
            } else {
                Some(self.expect_ident("位域名或 `_`")?)
            };
            self.expect(&TokenKind::Colon, "`:`")?;
            let width = self.parse_expr()?;
            self.expect(&TokenKind::Semi, "`;`")?;
            members.push(BitfieldMember {
                name: member_name,
                width,
                attrs: member_attrs,
            });
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        let _ = self.eat(&TokenKind::Semi);
        Ok(BitfieldDecl {
            name,
            repr,
            attrs,
            members,
        })
    }

    /// `fn name(params) [-> ret] { body }`
    fn parse_fn_decl(&mut self, attrs: Vec<Attr>) -> PResult<FnDecl> {
        self.expect(&TokenKind::KwFn, "`fn`")?;
        let name = self.expect_ident("函数名")?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                let param_name = self.expect_ident("参数名")?;
                self.expect(&TokenKind::Colon, "`:`")?;
                let ty = self.parse_type_ref()?;
                params.push(Param {
                    name: param_name,
                    ty,
                });
                if self.eat(&TokenKind::Comma) {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        Ok(FnDecl {
            name,
            attrs,
            params,
            ret,
            body,
        })
    }

    /// `#pragma name [= value];`
    fn parse_pragma(&mut self) -> PResult<PragmaDecl> {
        self.expect(&TokenKind::Hash, "`#`")?;
        let word_span = self.current().span;
        let word = self.expect_ident("`pragma`")?;
        if word != "pragma" {
            return Err(ParseError::syntax(
                word_span,
                format!("期望 `pragma`，实际是 `{word}`"),
            ));
        }
        let name = self.expect_ident("pragma 名")?;
        let value = if self.eat(&TokenKind::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::Semi, "`;`")?;
        Ok(PragmaDecl {
            name,
            value,
            attrs: Vec::new(),
        })
    }

    // ---------------------------------------------------------------------
    // 成员
    // ---------------------------------------------------------------------

    /// 解析 `{ member* }`，消费两侧花括号。
    fn parse_member_list(&mut self) -> PResult<Vec<Member>> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut members = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let before = self.pos;
            match self.parse_member() {
                Ok(member) => members.push(member),
                Err(error) => {
                    self.record(error);
                    if self.pos == before {
                        self.bump();
                    }
                    // 缺失分号时当前 token 往往已经是下一个成员；此时直接重同步，
                    // 才能把同一结构体里的多个错误一次报全。
                    if !self.looks_like_member() && !self.at_rbrace() {
                        self.recover_statement();
                    }
                }
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(members)
    }

    /// 解析一个成员（含前置属性）。
    fn parse_member(&mut self) -> PResult<Member> {
        let attrs = self.parse_attrs();
        self.parse_member_after_attrs(attrs)
    }

    /// 已有属性时解析成员。
    fn parse_member_after_attrs(&mut self, mut attrs: Vec<Attr>) -> PResult<Member> {
        if self.eat(&TokenKind::KwBe) {
            attrs.push(Attr {
                name: "endian".to_owned(),
                args: vec!["big".to_owned()],
            });
        } else if self.eat(&TokenKind::KwLe) {
            attrs.push(Attr {
                name: "endian".to_owned(),
                args: vec!["little".to_owned()],
            });
        }

        if self.eat(&TokenKind::KwPadding) {
            self.expect(&TokenKind::LBracket, "`[`")?;
            let count = self.parse_expr()?;
            self.expect(&TokenKind::RBracket, "`]`")?;
            self.expect(&TokenKind::Semi, "`;`")?;
            attrs.push(Attr {
                name: "padding".to_owned(),
                args: Vec::new(),
            });
            return Ok(Member {
                ty: TypeRef::Prim(Prim::Byte),
                name: "_".to_owned(),
                array: Some(ArraySpec::Fixed(count)),
                at: None,
                cond: None,
                attrs,
            });
        }

        let ty = self.parse_type_ref()?;
        let name = if let Some(name) = self.ident_at(0) {
            let name = name.to_owned();
            self.bump();
            name
        } else if self.at(&TokenKind::LBracket)
            || self.at(&TokenKind::Semi)
            || self.at(&TokenKind::At)
            || self.at(&TokenKind::KwIf)
        {
            "_".to_owned()
        } else {
            return Err(self.err_here(format!("期望成员名，实际是 {}", self.current().describe())));
        };

        let mut array = None;
        if self.eat(&TokenKind::LBracket) {
            if self.eat(&TokenKind::RBracket) {
                array = Some(self.finish_until_clause(ArraySpec::Unsized)?);
            } else if self.ident_at(0) == Some("until") && self.peek_is(1, &TokenKind::LParen) {
                self.bump();
                self.bump();
                let term = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                self.expect(&TokenKind::RBracket, "`]`")?;
                array = Some(ArraySpec::Until(term));
            } else {
                let count = self.parse_expr()?;
                self.expect(&TokenKind::RBracket, "`]`")?;
                array = Some(self.finish_until_clause(ArraySpec::Fixed(count))?);
            }
        } else if self.ident_at(0) == Some("until") && self.peek_is(1, &TokenKind::LParen) {
            array = Some(self.finish_until_clause(ArraySpec::Unsized)?);
        }

        let at = if self.eat(&TokenKind::At) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let cond = if self.eat(&TokenKind::KwIf) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::Semi, "`;`")?;
        Ok(Member {
            ty,
            name,
            array,
            at,
            cond,
            attrs,
        })
    }

    /// 处理 `[] until(term)` 的后缀；没有 `until` 时保留原规格。
    fn finish_until_clause(&mut self, spec: ArraySpec) -> PResult<ArraySpec> {
        if self.ident_at(0) == Some("until") && self.peek_is(1, &TokenKind::LParen) {
            self.bump();
            self.bump();
            let term = self.parse_expr()?;
            self.expect(&TokenKind::RParen, "`)`")?;
            Ok(ArraySpec::Until(term))
        } else {
            Ok(spec)
        }
    }

    // ---------------------------------------------------------------------
    // 语句与块
    // ---------------------------------------------------------------------

    /// 解析 `{ stmt* }`，带递归深度保护。
    fn parse_block(&mut self) -> PResult<Block> {
        self.enter()?;
        let result = self.parse_block_inner();
        self.leave();
        result
    }

    /// 块解析主体（不含深度计数）。
    fn parse_block_inner(&mut self) -> PResult<Block> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            if self.eat(&TokenKind::Semi) {
                continue;
            }
            let before = self.pos;
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(error) => {
                    self.record(error);
                    if self.pos == before {
                        self.bump();
                    }
                    if !self.looks_like_stmt_start() && !self.at_rbrace() {
                        self.recover_statement();
                    }
                }
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(Block { stmts })
    }

    /// 解析一条语句。
    fn parse_stmt(&mut self) -> PResult<Stmt> {
        let attrs = self.parse_attrs();
        match self.current().kind.clone() {
            TokenKind::KwLet => Ok(Stmt::Let(self.parse_let_stmt()?)),
            TokenKind::KwIf => Ok(Stmt::If(self.parse_if_stmt()?)),
            TokenKind::KwMatch => Ok(Stmt::Match(self.parse_match_stmt()?)),
            TokenKind::KwFor => Ok(Stmt::For(self.parse_for_stmt()?)),
            TokenKind::KwWhile => Ok(Stmt::While(self.parse_while_stmt()?)),
            TokenKind::KwReturn => {
                self.bump();
                let value = if self.at(&TokenKind::Semi) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(&TokenKind::Semi, "`;`")?;
                Ok(Stmt::Return(value))
            }
            TokenKind::KwBreak => {
                self.bump();
                self.expect(&TokenKind::Semi, "`;`")?;
                Ok(Stmt::Break)
            }
            TokenKind::KwContinue => {
                self.bump();
                self.expect(&TokenKind::Semi, "`;`")?;
                Ok(Stmt::Continue)
            }
            _ if self.looks_like_member() => {
                Ok(Stmt::Member(self.parse_member_after_attrs(attrs)?))
            }
            _ => {
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::Semi, "`;`")?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    /// `let name [: ty] [= init];`
    fn parse_let_stmt(&mut self) -> PResult<LetStmt> {
        self.expect(&TokenKind::KwLet, "`let`")?;
        let name = self.expect_ident("变量名")?;
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type_ref()?)
        } else {
            None
        };
        let init = if self.eat(&TokenKind::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::Semi, "`;`")?;
        Ok(LetStmt { name, ty, init })
    }

    /// `if cond { ... } [else { ... } | else if ...]`
    fn parse_if_stmt(&mut self) -> PResult<IfStmt> {
        self.expect(&TokenKind::KwIf, "`if`")?;
        let cond = self.parse_expr()?;
        let then_branch = self.parse_block()?;
        let else_branch = if self.eat(&TokenKind::KwElse) {
            if self.at(&TokenKind::KwIf) {
                Some(ElseBranch::If(Box::new(self.parse_if_stmt()?)))
            } else {
                Some(ElseBranch::Block(self.parse_block()?))
            }
        } else {
            None
        };
        Ok(IfStmt {
            cond,
            then_branch,
            else_branch,
        })
    }

    /// `while cond { ... }`
    fn parse_while_stmt(&mut self) -> PResult<WhileStmt> {
        self.expect(&TokenKind::KwWhile, "`while`")?;
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(WhileStmt { cond, body })
    }

    /// C 风格 `for (init; cond; step) { ... }`
    fn parse_for_stmt(&mut self) -> PResult<ForStmt> {
        self.expect(&TokenKind::KwFor, "`for`")?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let init = if self.eat(&TokenKind::Semi) {
            None
        } else if self.at(&TokenKind::KwLet) {
            Some(Box::new(Stmt::Let(self.parse_let_stmt()?)))
        } else if self.looks_like_member() {
            let ty = self.parse_type_ref()?;
            let name = self.expect_ident("循环变量名")?;
            let value = if self.eat(&TokenKind::Eq) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            self.expect(&TokenKind::Semi, "`;`")?;
            Some(Box::new(Stmt::Let(LetStmt {
                name,
                ty: Some(ty),
                init: value,
            })))
        } else {
            let expr = self.parse_expr()?;
            self.expect(&TokenKind::Semi, "`;`")?;
            Some(Box::new(Stmt::Expr(expr)))
        };
        let cond = if self.eat(&TokenKind::Semi) {
            None
        } else {
            let cond = self.parse_expr()?;
            self.expect(&TokenKind::Semi, "`;`")?;
            Some(cond)
        };
        let step = if self.eat(&TokenKind::RParen) {
            None
        } else {
            let step = self.parse_expr()?;
            self.expect(&TokenKind::RParen, "`)`")?;
            Some(step)
        };
        let body = self.parse_block()?;
        Ok(ForStmt {
            init,
            cond,
            step,
            body,
        })
    }

    /// `match expr { pattern => { ... }, ... }`
    fn parse_match_stmt(&mut self) -> PResult<MatchStmt> {
        self.expect(&TokenKind::KwMatch, "`match`")?;
        let scrutinee = self.parse_expr()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut arms = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let _attrs = self.parse_attrs();
            if self.at_rbrace() {
                break;
            }
            let pattern = if self.ident_at(0) == Some("_") {
                self.bump();
                Pattern::Wildcard
            } else {
                Pattern::Expr(self.parse_expr()?)
            };
            self.expect(&TokenKind::FatArrow, "`=>`")?;
            let body = if self.at(&TokenKind::LBrace) {
                self.parse_block()?
            } else {
                let expr = self.parse_expr()?;
                Block {
                    stmts: vec![Stmt::Expr(expr)],
                }
            };
            arms.push(MatchArm { pattern, body });
            let _ = self.eat(&TokenKind::Comma);
            let _ = self.eat(&TokenKind::Semi);
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(MatchStmt { scrutinee, arms })
    }

    // ---------------------------------------------------------------------
    // 表达式（优先级按 C，从低到高）
    // ---------------------------------------------------------------------

    /// 表达式入口，带递归深度保护。
    fn parse_expr(&mut self) -> PResult<Expr> {
        self.enter()?;
        let result = self.parse_assignment();
        self.leave();
        result
    }

    /// 赋值（右结合）。
    fn parse_assignment(&mut self) -> PResult<Expr> {
        let target = self.parse_ternary()?;
        let op = match &self.current().kind {
            TokenKind::Eq => Some(AssignOp::Assign),
            TokenKind::PlusEq => Some(AssignOp::AddAssign),
            TokenKind::MinusEq => Some(AssignOp::SubAssign),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let value = self.parse_expr()?;
            Ok(Expr::Assign {
                op,
                target: Box::new(target),
                value: Box::new(value),
            })
        } else {
            Ok(target)
        }
    }

    /// 三元 `cond ? a : b`（右结合）。
    fn parse_ternary(&mut self) -> PResult<Expr> {
        let cond = self.parse_or()?;
        if self.eat(&TokenKind::Question) {
            let then_expr = self.parse_expr()?;
            self.expect(&TokenKind::Colon, "`:`")?;
            let else_expr = self.parse_expr()?;
            Ok(Expr::Ternary {
                cond: Box::new(cond),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            })
        } else {
            Ok(cond)
        }
    }

    /// 通用的左结合二元层。
    fn parse_binary<F, O>(&mut self, next: F, op_for: O) -> PResult<Expr>
    where
        F: Fn(&mut Self) -> PResult<Expr>,
        O: Fn(&TokenKind) -> Option<BinaryOp>,
    {
        let mut lhs = next(self)?;
        while let Some(op) = op_for(&self.current().kind) {
            self.bump();
            let rhs = next(self)?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    /// `||`
    fn parse_or(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_and, |kind| match kind {
            TokenKind::OrOr => Some(BinaryOp::Or),
            _ => None,
        })
    }

    /// `&&`
    fn parse_and(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_bitor, |kind| match kind {
            TokenKind::AndAnd => Some(BinaryOp::And),
            _ => None,
        })
    }

    /// `|`
    fn parse_bitor(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_bitxor, |kind| match kind {
            TokenKind::Pipe => Some(BinaryOp::BitOr),
            _ => None,
        })
    }

    /// `^`
    fn parse_bitxor(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_bitand, |kind| match kind {
            TokenKind::Caret => Some(BinaryOp::BitXor),
            _ => None,
        })
    }

    /// `&`
    fn parse_bitand(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_equality, |kind| match kind {
            TokenKind::Amp => Some(BinaryOp::BitAnd),
            _ => None,
        })
    }

    /// `==` / `!=`
    fn parse_equality(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_relational, |kind| match kind {
            TokenKind::EqEq => Some(BinaryOp::Eq),
            TokenKind::NotEq => Some(BinaryOp::Ne),
            _ => None,
        })
    }

    /// `<` / `<=` / `>` / `>=`
    fn parse_relational(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_shift, |kind| match kind {
            TokenKind::Lt => Some(BinaryOp::Lt),
            TokenKind::Le => Some(BinaryOp::Le),
            TokenKind::Gt => Some(BinaryOp::Gt),
            TokenKind::Ge => Some(BinaryOp::Ge),
            _ => None,
        })
    }

    /// `<<` / `>>`
    fn parse_shift(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_additive, |kind| match kind {
            TokenKind::Shl => Some(BinaryOp::Shl),
            TokenKind::Shr => Some(BinaryOp::Shr),
            _ => None,
        })
    }

    /// `+` / `-`
    fn parse_additive(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_multiplicative, |kind| match kind {
            TokenKind::Plus => Some(BinaryOp::Add),
            TokenKind::Minus => Some(BinaryOp::Sub),
            _ => None,
        })
    }

    /// `*` / `/` / `%`
    fn parse_multiplicative(&mut self) -> PResult<Expr> {
        self.parse_binary(Self::parse_unary, |kind| match kind {
            TokenKind::Star => Some(BinaryOp::Mul),
            TokenKind::Slash => Some(BinaryOp::Div),
            TokenKind::Percent => Some(BinaryOp::Rem),
            _ => None,
        })
    }

    /// 一元运算，迭代收集前缀运算符以避免长链递归。
    fn parse_unary(&mut self) -> PResult<Expr> {
        let mut ops = Vec::new();
        loop {
            let op = match self.current().kind {
                TokenKind::Minus => UnaryOp::Neg,
                TokenKind::Plus => UnaryOp::Pos,
                TokenKind::Bang => UnaryOp::Not,
                TokenKind::Tilde => UnaryOp::BitNot,
                _ => break,
            };
            self.bump();
            ops.push(op);
        }
        let mut expr = self.parse_power()?;
        for op in ops.into_iter().rev() {
            expr = Expr::Unary {
                op,
                expr: Box::new(expr),
            };
        }
        Ok(expr)
    }

    /// `**` 幂运算（右结合，高于一元）。
    fn parse_power(&mut self) -> PResult<Expr> {
        let mut bases = vec![self.parse_postfix()?];
        while self.eat(&TokenKind::StarStar) {
            bases.push(self.parse_postfix()?);
        }
        let mut expr = match bases.pop() {
            Some(expr) => expr,
            None => return Err(self.err_here("幂表达式缺少操作数")),
        };
        while let Some(base) = bases.pop() {
            expr = Expr::Binary {
                op: BinaryOp::Pow,
                lhs: Box::new(base),
                rhs: Box::new(expr),
            };
        }
        Ok(expr)
    }

    /// 后缀：调用、索引、成员访问。
    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.eat(&TokenKind::LParen) {
                let mut args = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if self.eat(&TokenKind::Comma) {
                            if self.at(&TokenKind::RParen) {
                                break;
                            }
                            continue;
                        }
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)`")?;
                expr = Expr::Call {
                    callee: Box::new(expr),
                    args,
                };
            } else if self.eat(&TokenKind::LBracket) {
                let index = self.parse_expr()?;
                self.expect(&TokenKind::RBracket, "`]`")?;
                expr = Expr::Index {
                    base: Box::new(expr),
                    index: Box::new(index),
                };
            } else if self.eat(&TokenKind::Dot) {
                let name = self.expect_ident("成员名")?;
                expr = Expr::Member {
                    base: Box::new(expr),
                    name,
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    /// 基本表达式：字面量、路径、`$` 内建、括号、`sizeof`/`len`。
    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.current().kind.clone() {
            TokenKind::Int { value, .. } => {
                self.bump();
                Ok(Expr::Literal(Literal::Int(value)))
            }
            TokenKind::Float(value) => {
                self.bump();
                Ok(Expr::Literal(Literal::Float(value)))
            }
            TokenKind::Str(value) => {
                self.bump();
                Ok(Expr::Literal(Literal::Str(value)))
            }
            TokenKind::Char(value) => {
                self.bump();
                Ok(Expr::Literal(Literal::Char(value)))
            }
            TokenKind::KwTrue => {
                self.bump();
                Ok(Expr::Literal(Literal::Bool(true)))
            }
            TokenKind::KwFalse => {
                self.bump();
                Ok(Expr::Literal(Literal::Bool(false)))
            }
            TokenKind::Ident(first) => {
                self.bump();
                if first == "sizeof" && self.at(&TokenKind::LParen) {
                    self.bump();
                    let ty = self.parse_type_ref()?;
                    self.expect(&TokenKind::RParen, "`)`")?;
                    return Ok(Expr::SizeOf(Box::new(ty)));
                }
                if first == "len" && self.at(&TokenKind::LParen) {
                    self.bump();
                    let value = self.parse_expr()?;
                    self.expect(&TokenKind::RParen, "`)`")?;
                    return Ok(Expr::Len(Box::new(value)));
                }
                // 02 §2 把 `null`/`inf` 列为字面量；它们不占用关键字，
                // 这里在标识符分支里识别，避免词法层增加保留字。
                if first == "null" {
                    return Ok(Expr::Literal(Literal::Null));
                }
                if first == "inf" {
                    return Ok(Expr::Literal(Literal::Float(f64::INFINITY)));
                }
                let mut path = first;
                while self.at(&TokenKind::ColonColon) && self.ident_at(1).is_some() {
                    self.bump();
                    let segment = self.expect_ident("路径段")?;
                    path.push_str("::");
                    path.push_str(&segment);
                }
                Ok(Expr::Path(path))
            }
            TokenKind::Dollar => {
                self.bump();
                match self.ident_at(0) {
                    Some("size") => {
                        self.bump();
                        Ok(Expr::FileSize)
                    }
                    Some("start") => {
                        self.bump();
                        Ok(Expr::StructStart)
                    }
                    Some("end") => {
                        self.bump();
                        Ok(Expr::StructEnd)
                    }
                    _ => Ok(Expr::Cursor),
                }
            }
            TokenKind::LParen => {
                self.bump();
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                Ok(expr)
            }
            _ => Err(self.err_here(format!("期望表达式，实际是 {}", self.current().describe()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn parse_ok(source: &str) -> Program {
        match parse_source(source) {
            Ok(program) => program,
            Err(diagnostics) => panic!("期望解析成功，实际诊断: {diagnostics:?}"),
        }
    }

    fn parse_err(source: &str) -> Vec<Diag> {
        match parse_source(source) {
            Ok(program) => panic!("期望解析失败，实际 AST: {program:?}"),
            Err(diagnostics) => diagnostics,
        }
    }

    fn first_struct(program: &Program) -> &StructDecl {
        match program.items.first() {
            Some(Item::Struct(decl)) => decl,
            other => panic!("第一个顶层项不是 struct: {other:?}"),
        }
    }

    fn first_fn(program: &Program) -> &FnDecl {
        match program.items.first() {
            Some(Item::Fn(decl)) => decl,
            other => panic!("第一个顶层项不是 fn: {other:?}"),
        }
    }

    #[test]
    fn parses_struct_members_arrays_offsets_conditions_and_attrs() {
        let source = r#"
            #[color = "red"]
            struct Header : 32 {
                u32 magic;
                u8 magic_bytes[4];
                u8 data[until(0)];
                u32 x @ 0x10;
                u32 y if x == 1;
                #[format = "hex"] u8 z;
                padding[4];
            }
        "#;
        let program = parse_ok(source);
        let decl = first_struct(&program);
        assert_eq!(decl.name, "Header");
        assert_eq!(decl.size, Some(Expr::Literal(Literal::Int(32))));
        assert_eq!(decl.attrs.len(), 1);
        assert_eq!(decl.attrs[0].name, "color");
        assert_eq!(decl.attrs[0].args, vec!["red".to_owned()]);
        assert_eq!(decl.members.len(), 7);

        assert_eq!(decl.members[0].name, "magic");
        assert_eq!(decl.members[0].array, None);
        assert!(matches!(
            decl.members[1].array,
            Some(ArraySpec::Fixed(Expr::Literal(Literal::Int(4))))
        ));
        assert!(matches!(decl.members[2].array, Some(ArraySpec::Until(_))));
        assert!(decl.members[3].at.is_some());
        assert!(decl.members[4].cond.is_some());
        assert_eq!(decl.members[5].attrs[0].name, "format");
        assert_eq!(decl.members[6].name, "_");
        assert_eq!(decl.members[6].attrs[0].name, "padding");
        assert_eq!(decl.members[6].ty, TypeRef::Prim(Prim::Byte));
    }

    #[test]
    fn parses_union_enum_and_bitfield() {
        let source = r#"
            union Packed : 8 { u32 a; u16 b; }
            enum Format : u16 { Unknown = 0x0000, Png = 0x8950, }
            bitfield Flags : u8 #[bits: lsb] { compressed : 1; _ : 6; }
        "#;
        let program = parse_ok(source);
        assert_eq!(program.items.len(), 3);
        match &program.items[0] {
            Item::Union(union) => {
                assert_eq!(union.name, "Packed");
                assert_eq!(union.size, Some(Expr::Literal(Literal::Int(8))));
                assert_eq!(union.members.len(), 2);
            }
            other => panic!("期望 union: {other:?}"),
        }
        match &program.items[1] {
            Item::Enum(enum_decl) => {
                assert_eq!(enum_decl.name, "Format");
                assert_eq!(enum_decl.repr, Some(TypeRef::Prim(Prim::U16)));
                assert_eq!(enum_decl.variants.len(), 2);
                assert_eq!(enum_decl.variants[1].name, "Png");
            }
            other => panic!("期望 enum: {other:?}"),
        }
        match &program.items[2] {
            Item::Bitfield(bitfield) => {
                assert_eq!(bitfield.name, "Flags");
                assert_eq!(bitfield.attrs[0].name, "bits");
                assert_eq!(bitfield.attrs[0].args, vec!["lsb".to_owned()]);
                assert_eq!(bitfield.members.len(), 2);
                assert_eq!(bitfield.members[1].name, None);
            }
            other => panic!("期望 bitfield: {other:?}"),
        }
    }

    #[test]
    fn parses_fn_params_return_and_all_statement_forms() {
        let source = r#"
            fn classify(a: u32, b: u8) -> u32 {
                let n = a + b;
                if n > 10 { return n; } else { return 0; }
                while n > 0 { break; }
                for (u32 i = 0; i < n; i = i + 1) { continue; }
                match n {
                    0 => { return 1; }
                    _ => { return 2; }
                }
            }
        "#;
        let program = parse_ok(source);
        let function = first_fn(&program);
        assert_eq!(function.name, "classify");
        assert_eq!(function.params.len(), 2);
        assert_eq!(function.params[1].ty, TypeRef::Prim(Prim::U8));
        assert_eq!(function.ret, Some(TypeRef::Prim(Prim::U32)));
        assert_eq!(function.body.stmts.len(), 5);
        assert!(matches!(function.body.stmts[0], Stmt::Let(_)));
        assert!(matches!(function.body.stmts[1], Stmt::If(_)));
        assert!(matches!(function.body.stmts[2], Stmt::While(_)));
        match &function.body.stmts[3] {
            Stmt::For(loop_stmt) => {
                assert!(matches!(loop_stmt.init.as_deref(), Some(Stmt::Let(_))));
                assert!(loop_stmt.cond.is_some());
                assert!(matches!(
                    loop_stmt.step,
                    Some(Expr::Assign {
                        op: AssignOp::Assign,
                        ..
                    })
                ));
            }
            other => panic!("期望 for: {other:?}"),
        }
        assert!(matches!(function.body.stmts[4], Stmt::Match(_)));
    }

    #[test]
    fn parses_match_paths_and_wildcard() {
        let source = r#"
            fn f(fmt: u16) -> u32 {
                match fmt {
                    Format::Png => { return 1; }
                    _ => { return 0; }
                }
            }
        "#;
        let program = parse_ok(source);
        let function = first_fn(&program);
        let Stmt::Match(match_stmt) = &function.body.stmts[0] else {
            panic!("期望 match 语句");
        };
        assert_eq!(match_stmt.arms.len(), 2);
        match &match_stmt.arms[0].pattern {
            Pattern::Expr(Expr::Path(path)) => assert_eq!(path, "Format::Png"),
            other => panic!("期望路径模式: {other:?}"),
        }
        assert_eq!(match_stmt.arms[1].pattern, Pattern::Wildcard);
    }

    #[test]
    fn parses_pragma_and_endian_prefixes() {
        let source = r#"
            #pragma endian = little;
            struct Mixed {
                be u32 big;
                le u16 small;
            }
        "#;
        let program = parse_ok(source);
        match &program.items[0] {
            Item::Pragma(pragma) => {
                assert_eq!(pragma.name, "endian");
                assert_eq!(pragma.value, Some(Expr::Path("little".to_owned())));
            }
            other => panic!("期望 pragma: {other:?}"),
        }
        let decl = match program.items.get(1) {
            Some(Item::Struct(decl)) => decl,
            other => panic!("第二个顶层项不是 struct: {other:?}"),
        };
        assert_eq!(decl.members[0].attrs[0].name, "endian");
        assert_eq!(decl.members[0].attrs[0].args, vec!["big".to_owned()]);
        assert_eq!(decl.members[1].attrs[0].args, vec!["little".to_owned()]);
    }

    #[test]
    fn parses_str_type_and_unsized_arrays() {
        let source = r#"
            struct Names {
                str(enc = "utf8", term = 0) name;
                u8 payload[];
                char tail[] until(0);
            }
        "#;
        let program = parse_ok(source);
        let decl = first_struct(&program);
        match &decl.members[0].ty {
            TypeRef::Str(args) => {
                assert_eq!(args.enc.as_deref(), Some("utf8"));
                assert!(args.term.is_some());
            }
            other => panic!("期望 str 类型: {other:?}"),
        }
        assert_eq!(decl.members[1].array, Some(ArraySpec::Unsized));
        assert!(matches!(decl.members[2].array, Some(ArraySpec::Until(_))));
    }

    #[test]
    fn parses_inline_struct_member() {
        let source = r#"
            struct Zip {
                struct { u32 signature; u16 version; } eocd @ $size - 22;
            }
        "#;
        let program = parse_ok(source);
        let decl = first_struct(&program);
        match &decl.members[0].ty {
            TypeRef::InlineStruct(inner) => {
                assert_eq!(inner.members.len(), 2);
                assert_eq!(inner.members[0].name, "signature");
            }
            other => panic!("期望内联结构体: {other:?}"),
        }
        assert!(decl.members[0].at.is_some());
    }

    #[test]
    fn parses_attribute_argument_forms() {
        let source = r#"
            #[assert(hdr.size == 4, "长度错误")]
            #[hidden]
            #[bits: lsb]
            #[ai:conf(0.8)]
            struct S { u8 x; }
        "#;
        let program = parse_ok(source);
        let decl = first_struct(&program);
        assert_eq!(decl.attrs.len(), 4);
        assert_eq!(decl.attrs[0].name, "assert");
        assert_eq!(decl.attrs[0].args.len(), 2);
        assert_eq!(decl.attrs[0].args[1], "长度错误");
        assert_eq!(decl.attrs[1].name, "hidden");
        assert!(decl.attrs[1].args.is_empty());
        assert_eq!(decl.attrs[2].name, "bits");
        assert_eq!(decl.attrs[2].args, vec!["lsb".to_owned()]);
        assert_eq!(decl.attrs[3].name, "ai:conf");
        assert_eq!(decl.attrs[3].args, vec!["0.8".to_owned()]);
    }

    #[test]
    fn expression_precedence_matches_c() {
        let source = r#"
            fn f() -> u32 {
                let x = 1 + 2 * 3 == 7 && true || false;
                return x;
            }
        "#;
        let program = parse_ok(source);
        let function = first_fn(&program);
        let Stmt::Let(let_stmt) = &function.body.stmts[0] else {
            panic!("期望 let");
        };
        let Some(init) = &let_stmt.init else {
            panic!("期望初值");
        };
        // 顶层应为 ||；左支为 &&；&& 的左支为 ==；== 的左支为 +，其右支为 *。
        let Expr::Binary {
            op: BinaryOp::Or,
            lhs,
            ..
        } = init
        else {
            panic!("顶层应为 ||: {init:?}");
        };
        let Expr::Binary {
            op: BinaryOp::And,
            lhs: and_lhs,
            ..
        } = lhs.as_ref()
        else {
            panic!("&& 层错误: {lhs:?}");
        };
        let Expr::Binary {
            op: BinaryOp::Eq,
            lhs: eq_lhs,
            ..
        } = and_lhs.as_ref()
        else {
            panic!("== 层错误: {and_lhs:?}");
        };
        let Expr::Binary {
            op: BinaryOp::Add,
            rhs: add_rhs,
            ..
        } = eq_lhs.as_ref()
        else {
            panic!("+ 层错误: {eq_lhs:?}");
        };
        assert!(matches!(
            add_rhs.as_ref(),
            Expr::Binary {
                op: BinaryOp::Mul,
                ..
            }
        ));
    }

    #[test]
    fn parses_unary_ternary_postfix_and_builtins() {
        let source = r#"
            fn f(data: u8) -> u32 {
                let a = -hdr.items[0].length + sizeof(u32) * len(data);
                let b = a > 0 ? a : -a;
                let c = 2 ** 3 ** 2;
                let d = null;
                let e = inf;
                return b + c;
            }
        "#;
        let program = parse_ok(source);
        let function = first_fn(&program);
        let Stmt::Let(first_let) = &function.body.stmts[0] else {
            panic!("期望 let");
        };
        let Some(Expr::Binary {
            op: BinaryOp::Add,
            lhs,
            ..
        }) = &first_let.init
        else {
            panic!("期望加法: {:?}", first_let.init);
        };
        assert!(matches!(lhs.as_ref(), Expr::Unary { .. }));
        let Stmt::Let(second_let) = &function.body.stmts[1] else {
            panic!("期望 let");
        };
        assert!(matches!(second_let.init, Some(Expr::Ternary { .. })));
        let Stmt::Let(third_let) = &function.body.stmts[2] else {
            panic!("期望 let");
        };
        assert!(matches!(
            third_let.init,
            Some(Expr::Binary {
                op: BinaryOp::Pow,
                ..
            })
        ));
        let literal_init = |index: usize| match &function.body.stmts[index] {
            Stmt::Let(let_stmt) => let_stmt.init.clone(),
            other => panic!("期望 let: {other:?}"),
        };
        assert_eq!(literal_init(3), Some(Expr::Literal(Literal::Null)));
        assert!(matches!(
            literal_init(4),
            Some(Expr::Literal(Literal::Float(value))) if value.is_infinite()
        ));
    }

    #[test]
    fn parses_dollar_builtins_and_sizes() {
        let source = r#"
            fn f() -> u32 {
                let a = $;
                let b = $size;
                let c = $start + $end;
                return a + b + c;
            }
        "#;
        let program = parse_ok(source);
        let function = first_fn(&program);
        let init = |index: usize| match &function.body.stmts[index] {
            Stmt::Let(let_stmt) => let_stmt.init.clone(),
            other => panic!("期望 let: {other:?}"),
        };
        assert_eq!(init(0), Some(Expr::Cursor));
        assert_eq!(init(1), Some(Expr::FileSize));
        assert!(matches!(
            init(2),
            Some(Expr::Binary {
                op: BinaryOp::Add,
                ..
            })
        ));
    }

    #[test]
    fn error_recovery_reports_at_least_three_errors() {
        let source = r#"
            fn broken() {
                let a = ;
                let b = ;
                let c = ;
            }
            struct AlsoBroken {
                u8 x
                u8 y
                u8 z
            }
        "#;
        let diagnostics = parse_err(source);
        assert!(
            diagnostics.len() >= 3,
            "一次至少报 3 个错，实际 {}: {diagnostics:?}",
            diagnostics.len()
        );
        assert!(diagnostics.iter().all(|diag| diag.range.is_some()));
        assert!(
            diagnostics
                .iter()
                .all(|diag| diag.severity == geode_core::diag::Severity::Error)
        );
    }

    #[test]
    fn missing_member_semicolons_are_all_reported() {
        let diagnostics = parse_err(
            r#"
            struct S {
                u8 a
                u8 b
                u8 c
            }
        "#,
        );
        assert!(
            diagnostics.len() >= 3,
            "三个缺失分号应各报一次，实际 {}: {diagnostics:?}",
            diagnostics.len()
        );
    }

    proptest! {
        #[test]
        fn arbitrary_source_never_panics(source in any::<String>()) {
            let _ = parse_source(&source);
        }
    }
}
