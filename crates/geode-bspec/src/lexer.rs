//! BinSpec 词法分析器。
//!
//! DESIGN: 02-dsl-spec-draft.md §3–§6、22-bspec-toolchain.md §3.2。
//!
//! 词法层只认“最小的词”，不建符号表、不判断语法是否成立。注释与空白是 trivia，
//! 直接跳过；AST 与格式化器未来若需要注释，应在此暴露带 trivia 的扫描接口，而不是
//! 把注释塞进 token 流让 parser 处处跳过。
//!
//! **错误码说明**：`docs/errors.toml` 尚无词法专用 E4 码，词法错误暂复用 E4001；
//! 原因见 `diag.rs` 模块文档。

use crate::diag::{Diagnostics, E_LEX, diag_at};
use crate::span::SrcSpan;
use geode_core::diag::Diag;

/// 整数后缀。BinSpec 只允许无符号的 `u8` / `u32` 后缀。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntSuffix {
    /// `u8`
    U8,
    /// `u32`
    U32,
}

impl IntSuffix {
    /// 源码拼写。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
        }
    }
}

/// token 类型。
///
/// 运算符与分隔符是单位变体；标识符/字面量携带已解码的值，span 另存于 [`Token`]。
#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    /// 标识符；关键字已在词法层单独成类。
    Ident(String),
    /// 整数字面量（已去掉下划线），可带 `u8`/`u32` 后缀。
    Int {
        /// 数值；超出 u128 时报告词法错误并置 0。
        value: u128,
        /// 可选后缀。
        suffix: Option<IntSuffix>,
    },
    /// 浮点字面量。
    Float(f64),
    /// 字符串字面量（已解码转义，不含两侧引号）。
    Str(String),
    /// 字符字面量（已解码转义）。
    Char(char),
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `;`
    Semi,
    /// `,`
    Comma,
    /// `#[`（属性起始，作为一个 token，便于 parser 快速分派）
    AttrStart,
    /// `#`（`#pragma` 使用）
    Hash,
    /// `@`
    At,
    /// `$`
    Dollar,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `~`
    Tilde,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `==`
    EqEq,
    /// `!=`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    AndAnd,
    /// `||`
    OrOr,
    /// `!`
    Bang,
    /// `=`
    Eq,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `.`
    Dot,
    /// `::`
    ColonColon,
    /// `->`
    Arrow,
    /// `=>`（`match` 分支使用；02 §5 运算符表未列，但 §4.4 的 match 语法必需）
    FatArrow,
    /// `?`
    Question,
    /// `:`
    Colon,
    /// `**`（02 §5.2 的幂运算）
    StarStar,
    /// `struct`
    KwStruct,
    /// `union`
    KwUnion,
    /// `enum`
    KwEnum,
    /// `bitfield`
    KwBitfield,
    /// `fn`
    KwFn,
    /// `let`
    KwLet,
    /// `if`
    KwIf,
    /// `else`
    KwElse,
    /// `match`
    KwMatch,
    /// `for`
    KwFor,
    /// `while`
    KwWhile,
    /// `return`
    KwReturn,
    /// `break`
    KwBreak,
    /// `continue`
    KwContinue,
    /// `true`
    KwTrue,
    /// `false`
    KwFalse,
    /// `padding`
    KwPadding,
    /// `be`（大端字段前缀）
    KwBe,
    /// `le`（小端字段前缀）
    KwLe,
    /// 文件结束哨兵；span 为空，位于源码末尾。
    Eof,
}

impl TokenKind {
    /// 固定拼写；标识符与字面量返回 `None`。
    #[must_use]
    pub const fn fixed_text(&self) -> Option<&'static str> {
        Some(match self {
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::Semi => ";",
            Self::Comma => ",",
            Self::AttrStart => "#[",
            Self::Hash => "#",
            Self::At => "@",
            Self::Dollar => "$",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Percent => "%",
            Self::Amp => "&",
            Self::Pipe => "|",
            Self::Caret => "^",
            Self::Tilde => "~",
            Self::Shl => "<<",
            Self::Shr => ">>",
            Self::EqEq => "==",
            Self::NotEq => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::AndAnd => "&&",
            Self::OrOr => "||",
            Self::Bang => "!",
            Self::Eq => "=",
            Self::PlusEq => "+=",
            Self::MinusEq => "-=",
            Self::Dot => ".",
            Self::ColonColon => "::",
            Self::Arrow => "->",
            Self::FatArrow => "=>",
            Self::Question => "?",
            Self::Colon => ":",
            Self::StarStar => "**",
            Self::KwStruct => "struct",
            Self::KwUnion => "union",
            Self::KwEnum => "enum",
            Self::KwBitfield => "bitfield",
            Self::KwFn => "fn",
            Self::KwLet => "let",
            Self::KwIf => "if",
            Self::KwElse => "else",
            Self::KwMatch => "match",
            Self::KwFor => "for",
            Self::KwWhile => "while",
            Self::KwReturn => "return",
            Self::KwBreak => "break",
            Self::KwContinue => "continue",
            Self::KwTrue => "true",
            Self::KwFalse => "false",
            Self::KwPadding => "padding",
            Self::KwBe => "be",
            Self::KwLe => "le",
            Self::Ident(_)
            | Self::Int { .. }
            | Self::Float(_)
            | Self::Str(_)
            | Self::Char(_)
            | Self::Eof => return None,
        })
    }
}

/// 一个带源码区间的 token。
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    /// token 类型。
    pub kind: TokenKind,
    /// 该 token 在源码里的半开字节区间。
    pub span: SrcSpan,
}

impl Token {
    /// 构造 token。
    #[must_use]
    pub const fn new(kind: TokenKind, span: SrcSpan) -> Self {
        Self { kind, span }
    }

    /// 适合诊断文案的简短描述，例如 ``标识符 `foo` ``、`` `;` ``。
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.kind {
            TokenKind::Ident(name) => format!("标识符 `{name}`"),
            TokenKind::Int { .. } => "整数字面量".to_owned(),
            TokenKind::Float(_) => "浮点字面量".to_owned(),
            TokenKind::Str(_) => "字符串字面量".to_owned(),
            TokenKind::Char(_) => "字符字面量".to_owned(),
            TokenKind::Eof => "文件结束".to_owned(),
            other => match other.fixed_text() {
                Some(text) => format!("`{text}`"),
                None => "token".to_owned(),
            },
        }
    }

    /// token 的“近似源码文本”。
    ///
    /// 用于 `#[attr(...)]` 这种按 token 拼接参数的场合：字符串会去掉引号，
    /// 数字按数值重新格式化，不保证与源码逐字节相同。
    #[must_use]
    pub fn text(&self) -> String {
        match &self.kind {
            TokenKind::Ident(name) => name.clone(),
            TokenKind::Int { value, suffix } => match suffix {
                Some(suffix) => format!("{value}{}", suffix.as_str()),
                None => value.to_string(),
            },
            TokenKind::Float(value) => value.to_string(),
            TokenKind::Str(value) => value.clone(),
            TokenKind::Char(value) => value.to_string(),
            TokenKind::Eof => String::new(),
            other => other.fixed_text().unwrap_or_default().to_owned(),
        }
    }
}

/// 词法分析器。
///
/// 用法：`Lexer::new(src).tokenize()`。一次扫描收集所有词法错误后统一返回，
/// 不在第一个错误处停下——编辑器需要一次看到全部红波浪线。
pub struct Lexer<'a> {
    /// 源文本。
    src: &'a str,
    /// 当前字节偏移，始终落在 UTF-8 字符边界。
    pos: usize,
    /// 已产出的 token。
    tokens: Vec<Token>,
    /// 已收集的诊断。
    diagnostics: Diagnostics,
}

impl<'a> Lexer<'a> {
    /// 为源码创建词法分析器。
    #[must_use]
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            tokens: Vec::new(),
            diagnostics: Diagnostics::new(),
        }
    }

    /// 扫描全部 token（末尾追加 [`TokenKind::Eof`]）。
    ///
    /// # Errors
    /// 任一字符非法、字符串/字符字面量未闭合、块注释未闭合或数字格式错误时，
    /// 返回按源码顺序排列的全部诊断；每条诊断都带 [`SrcSpan`]。
    pub fn tokenize(mut self) -> Result<Vec<Token>, Vec<Diag>> {
        self.scan_all();
        if self.diagnostics.has_errors() {
            Err(self.diagnostics.into_vec())
        } else {
            Ok(self.tokens)
        }
    }

    /// 主扫描循环。
    fn scan_all(&mut self) {
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() {
                self.pos = self.pos.saturating_add(1);
                continue;
            }
            if byte == b'/' && self.peek_at(1) == Some(b'/') {
                self.skip_line_comment();
                continue;
            }
            if byte == b'/' && self.peek_at(1) == Some(b'*') {
                self.skip_block_comment();
                continue;
            }
            if byte.is_ascii_alphabetic() || byte == b'_' {
                self.lex_ident();
                continue;
            }
            if byte.is_ascii_digit() {
                self.lex_number();
                continue;
            }
            if byte == b'"' {
                self.lex_string();
                continue;
            }
            if byte == b'\'' {
                self.lex_char();
                continue;
            }
            self.lex_punct();
        }
        let end = self.offset();
        self.tokens
            .push(Token::new(TokenKind::Eof, SrcSpan::empty(end)));
    }

    /// 当前字节。
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    /// 向前第 `n` 个字节（`n=0` 即当前字节）。
    fn peek_at(&self, n: usize) -> Option<u8> {
        self.pos
            .checked_add(n)
            .and_then(|index| self.src.as_bytes().get(index))
            .copied()
    }

    /// 当前字符（用于处理非 ASCII 文本）。
    fn peek_char(&self) -> Option<char> {
        self.src.get(self.pos..)?.chars().next()
    }

    /// 当前偏移转 `u32`（超过上限饱和；源文件不可能真的有 4 GiB）。
    fn offset(&self) -> u32 {
        u32::try_from(self.pos).unwrap_or(u32::MAX)
    }

    /// 从 `start` 到当前偏移的区间。
    fn span_from(&self, start: u32) -> SrcSpan {
        SrcSpan::new(start, self.offset())
    }

    /// 记录一条词法错误。
    fn error(&mut self, span: SrcSpan, detail: impl Into<String>) {
        self.diagnostics.push(diag_at(E_LEX, span, detail));
    }

    /// 推入一个 token。
    fn push(&mut self, kind: TokenKind, start: u32) {
        self.tokens.push(Token::new(kind, self.span_from(start)));
    }

    /// 跳过 `//` 行注释，不吞换行符。
    fn skip_line_comment(&mut self) {
        while let Some(byte) = self.peek() {
            if byte == b'\n' {
                break;
            }
            self.pos = self.pos.saturating_add(1);
        }
    }

    /// 跳过可嵌套的 `/* ... */` 块注释；未闭合时报告错误。
    fn skip_block_comment(&mut self) {
        let start = self.offset();
        self.pos = self.pos.saturating_add(2);
        let mut depth = 1_u32;
        while depth > 0 {
            let Some(byte) = self.peek() else {
                self.error(self.span_from(start), "块注释未闭合：缺少 `*/`");
                return;
            };
            if byte == b'/' && self.peek_at(1) == Some(b'*') {
                depth = depth.saturating_add(1);
                self.pos = self.pos.saturating_add(2);
            } else if byte == b'*' && self.peek_at(1) == Some(b'/') {
                depth = depth.saturating_sub(1);
                self.pos = self.pos.saturating_add(2);
            } else {
                self.advance_char();
            }
        }
    }

    /// 前进一个完整字符（非法字节按 1 字节前进，保证不死循环）。
    fn advance_char(&mut self) {
        match self.peek_char() {
            Some(ch) => self.pos = self.pos.saturating_add(ch.len_utf8()),
            None => self.pos = self.pos.saturating_add(1),
        }
    }

    /// 扫描标识符或关键字。
    fn lex_ident(&mut self) {
        let start = self.offset();
        self.pos = self.pos.saturating_add(1);
        while let Some(byte) = self.peek() {
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                self.pos = self.pos.saturating_add(1);
            } else {
                break;
            }
        }
        let text = self.src.get(start as usize..self.pos).unwrap_or_default();
        let kind = match text {
            "struct" => TokenKind::KwStruct,
            "union" => TokenKind::KwUnion,
            "enum" => TokenKind::KwEnum,
            "bitfield" => TokenKind::KwBitfield,
            "fn" => TokenKind::KwFn,
            "let" => TokenKind::KwLet,
            "if" => TokenKind::KwIf,
            "else" => TokenKind::KwElse,
            "match" => TokenKind::KwMatch,
            "for" => TokenKind::KwFor,
            "while" => TokenKind::KwWhile,
            "return" => TokenKind::KwReturn,
            "break" => TokenKind::KwBreak,
            "continue" => TokenKind::KwContinue,
            "true" => TokenKind::KwTrue,
            "false" => TokenKind::KwFalse,
            "padding" => TokenKind::KwPadding,
            "be" => TokenKind::KwBe,
            "le" => TokenKind::KwLe,
            _ => TokenKind::Ident(text.to_owned()),
        };
        self.push(kind, start);
    }

    /// 扫描数字：十进制/0x/0b/0o、下划线分隔、可选整数后缀，或浮点。
    fn lex_number(&mut self) {
        let start = self.offset();
        let mut radix = 10_u32;
        let mut bad_digit = false;
        if self.peek() == Some(b'0') {
            match self.peek_at(1) {
                Some(b'x' | b'X') => {
                    radix = 16;
                    self.pos = self.pos.saturating_add(2);
                }
                Some(b'b' | b'B') => {
                    radix = 2;
                    self.pos = self.pos.saturating_add(2);
                }
                Some(b'o' | b'O') => {
                    radix = 8;
                    self.pos = self.pos.saturating_add(2);
                }
                _ => {}
            }
        }

        let mut digits = String::new();
        let mut last_was_digit = false;
        while let Some(byte) = self.peek() {
            if let Some(value) = digit_value(byte, radix) {
                let _ = value;
                digits.push(char::from(byte));
                last_was_digit = true;
                self.pos = self.pos.saturating_add(1);
            } else if byte == b'_' {
                let next_is_digit = self
                    .peek_at(1)
                    .and_then(|next| digit_value(next, radix))
                    .is_some();
                if !last_was_digit || !next_is_digit {
                    bad_digit = true;
                }
                last_was_digit = false;
                self.pos = self.pos.saturating_add(1);
            } else if radix != 10 && byte.is_ascii_digit() {
                // 例如 0b102：2 不是二进制数字，但属于这个数字字面量。
                bad_digit = true;
                last_was_digit = false;
                self.pos = self.pos.saturating_add(1);
            } else {
                break;
            }
        }

        if digits.is_empty() {
            self.error(self.span_from(start), "数字字面量缺少有效数字");
            self.push(
                TokenKind::Int {
                    value: 0,
                    suffix: None,
                },
                start,
            );
            return;
        }

        let mut is_float = false;
        if radix == 10 {
            // 小数点后必须跟数字，避免把 `1.foo` 的成员访问误吞成浮点。
            if self.peek() == Some(b'.')
                && self.peek_at(1).is_some_and(|next| next.is_ascii_digit())
            {
                is_float = true;
                self.pos = self.pos.saturating_add(1);
                self.scan_decimal_digits(&mut digits, &mut bad_digit, &mut last_was_digit);
            }
            // 指数：e/E [sign] digit。
            if matches!(self.peek(), Some(b'e' | b'E')) {
                let after_e = self.peek_at(1);
                let after_sign = self.peek_at(2);
                let signed = matches!(after_e, Some(b'+' | b'-'))
                    && after_sign.is_some_and(|next| next.is_ascii_digit());
                let unsigned = after_e.is_some_and(|next| next.is_ascii_digit());
                if signed || unsigned {
                    is_float = true;
                    self.pos = self.pos.saturating_add(1);
                    if matches!(self.peek(), Some(b'+' | b'-')) {
                        self.pos = self.pos.saturating_add(1);
                    }
                    self.scan_decimal_digits(&mut digits, &mut bad_digit, &mut last_was_digit);
                }
            }
        }

        // 后缀（整数 u8/u32；浮点不允许后缀）。
        let suffix_start = self.offset();
        while let Some(byte) = self.peek() {
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                self.pos = self.pos.saturating_add(1);
            } else {
                break;
            }
        }
        let suffix_text = self
            .src
            .get(suffix_start as usize..self.pos)
            .unwrap_or_default();
        let suffix = if suffix_text.is_empty() {
            None
        } else if is_float {
            self.error(
                self.span_from(suffix_start),
                format!("浮点字面量不支持后缀 `{suffix_text}`"),
            );
            None
        } else {
            match suffix_text {
                "u8" => Some(IntSuffix::U8),
                "u32" => Some(IntSuffix::U32),
                _ => {
                    self.error(
                        self.span_from(suffix_start),
                        format!("未知整数后缀 `{suffix_text}`（只支持 u8/u32）"),
                    );
                    None
                }
            }
        };

        if is_float {
            let text = self.literal_text(start);
            match text.parse::<f64>() {
                Ok(value) if value.is_finite() => self.push(TokenKind::Float(value), start),
                _ => {
                    self.error(self.span_from(start), "浮点字面量格式无效或超出范围");
                    self.push(TokenKind::Float(0.0), start);
                }
            }
            return;
        }

        if bad_digit {
            self.error(
                self.span_from(start),
                "数字字面量包含非法数字或下划线位置错误",
            );
        }
        let value = if bad_digit {
            0
        } else {
            match u128::from_str_radix(&digits, radix) {
                Ok(value) => value,
                Err(_) => {
                    self.error(self.span_from(start), "整数字面量超出 u128 范围");
                    0
                }
            }
        };
        self.push(TokenKind::Int { value, suffix }, start);
    }

    /// 扫描小数部分/指数部分的十进制数字，沿用整体的 `digits` 缓冲。
    fn scan_decimal_digits(
        &mut self,
        digits: &mut String,
        bad_digit: &mut bool,
        last_was_digit: &mut bool,
    ) {
        let mut any = false;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() {
                digits.push(char::from(byte));
                any = true;
                *last_was_digit = true;
                self.pos = self.pos.saturating_add(1);
            } else if byte == b'_' {
                let next_is_digit = self.peek_at(1).is_some_and(|next| next.is_ascii_digit());
                if !*last_was_digit || !next_is_digit {
                    *bad_digit = true;
                }
                *last_was_digit = false;
                self.pos = self.pos.saturating_add(1);
            } else {
                break;
            }
        }
        if !any {
            *bad_digit = true;
        }
    }

    /// 取 `[start, pos)` 的源码文本并去掉下划线。
    fn literal_text(&self, start: u32) -> String {
        match self.src.get(start as usize..self.pos) {
            Some(text) => text.replace('_', ""),
            None => String::new(),
        }
    }

    /// 扫描字符串字面量。
    fn lex_string(&mut self) {
        let start = self.offset();
        self.pos = self.pos.saturating_add(1);
        let mut value = String::new();
        loop {
            let Some(ch) = self.peek_char() else {
                self.error(self.span_from(start), "字符串字面量未闭合：缺少 `\"`");
                return;
            };
            match ch {
                '"' => {
                    self.pos = self.pos.saturating_add(1);
                    self.push(TokenKind::Str(value), start);
                    return;
                }
                '\n' | '\r' => {
                    self.error(self.span_from(start), "字符串字面量未闭合：缺少 `\"`");
                    return;
                }
                '\\' => {
                    let escape_start = self.offset();
                    self.pos = self.pos.saturating_add(1);
                    match self.peek_char() {
                        Some('n') => {
                            value.push('\n');
                            self.pos = self.pos.saturating_add(1);
                        }
                        Some('t') => {
                            value.push('\t');
                            self.pos = self.pos.saturating_add(1);
                        }
                        Some('\\') => {
                            value.push('\\');
                            self.pos = self.pos.saturating_add(1);
                        }
                        Some('"') => {
                            value.push('"');
                            self.pos = self.pos.saturating_add(1);
                        }
                        Some('x') => {
                            self.pos = self.pos.saturating_add(1);
                            let mut code = 0_u32;
                            let mut digits = 0_u8;
                            while digits < 2 {
                                let Some(byte) = self.peek() else {
                                    break;
                                };
                                let Some(digit) = digit_value(byte, 16) else {
                                    break;
                                };
                                code = code.saturating_mul(16).saturating_add(digit);
                                digits = digits.saturating_add(1);
                                self.pos = self.pos.saturating_add(1);
                            }
                            if digits == 2 {
                                value.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                            } else {
                                self.error(
                                    self.span_from(escape_start),
                                    "`\\x` 转义需要两位十六进制数字",
                                );
                            }
                        }
                        Some('u') => {
                            self.pos = self.pos.saturating_add(1);
                            let mut code = 0_u32;
                            let mut digits = 0_u8;
                            let mut valid = self.peek_char() == Some('{');
                            if valid {
                                self.pos = self.pos.saturating_add(1);
                                while let Some(ch) = self.peek_char() {
                                    if ch == '}' {
                                        break;
                                    }
                                    match ch.to_digit(16) {
                                        Some(digit) if digits < 6 => {
                                            code = code.saturating_mul(16).saturating_add(digit);
                                            digits = digits.saturating_add(1);
                                            self.pos = self.pos.saturating_add(1);
                                        }
                                        _ => {
                                            valid = false;
                                            self.pos = self.pos.saturating_add(ch.len_utf8());
                                        }
                                    }
                                }
                                if self.peek_char() == Some('}') {
                                    self.pos = self.pos.saturating_add(1);
                                } else {
                                    valid = false;
                                }
                            }
                            match (valid && digits > 0, char::from_u32(code)) {
                                (true, Some(decoded)) => value.push(decoded),
                                _ => self.error(
                                    self.span_from(escape_start),
                                    "`\\u{...}` 转义需要 1–6 位十六进制 Unicode 码点",
                                ),
                            }
                        }
                        Some(other) => {
                            self.error(
                                self.span_from(escape_start),
                                format!("未知转义序列 `\\{other}`"),
                            );
                            value.push(other);
                            self.pos = self.pos.saturating_add(other.len_utf8());
                        }
                        None => {
                            self.error(self.span_from(start), "字符串字面量未闭合：缺少 `\"`");
                            return;
                        }
                    }
                }
                _ => {
                    value.push(ch);
                    self.pos = self.pos.saturating_add(ch.len_utf8());
                }
            }
        }
    }

    /// 扫描字符字面量。
    fn lex_char(&mut self) {
        let start = self.offset();
        self.pos = self.pos.saturating_add(1);
        let Some(ch) = self.peek_char() else {
            self.error(self.span_from(start), "字符字面量未闭合：缺少 `'`");
            return;
        };
        if ch == '\'' {
            self.error(self.span_from(start), "字符字面量不能为空");
            self.pos = self.pos.saturating_add(1);
            self.push(TokenKind::Char('\0'), start);
            return;
        }
        let value = if ch == '\\' {
            let escape_start = self.offset();
            self.pos = self.pos.saturating_add(1);
            match self.peek_char() {
                Some('n') => {
                    self.pos = self.pos.saturating_add(1);
                    '\n'
                }
                Some('t') => {
                    self.pos = self.pos.saturating_add(1);
                    '\t'
                }
                Some('\\') => {
                    self.pos = self.pos.saturating_add(1);
                    '\\'
                }
                Some('\'') => {
                    self.pos = self.pos.saturating_add(1);
                    '\''
                }
                Some('"') => {
                    self.pos = self.pos.saturating_add(1);
                    '"'
                }
                Some(other) => {
                    self.error(
                        self.span_from(escape_start),
                        format!("未知转义序列 `\\{other}`"),
                    );
                    self.pos = self.pos.saturating_add(other.len_utf8());
                    other
                }
                None => {
                    self.error(self.span_from(start), "字符字面量未闭合：缺少 `'`");
                    return;
                }
            }
        } else {
            self.pos = self.pos.saturating_add(ch.len_utf8());
            ch
        };

        match self.peek_char() {
            Some('\'') => {
                self.pos = self.pos.saturating_add(1);
                self.push(TokenKind::Char(value), start);
            }
            Some('\n' | '\r') | None => {
                self.error(self.span_from(start), "字符字面量未闭合：缺少 `'`");
            }
            Some(_) => {
                // 多字符字面量：吞到闭合引号（或行尾）后报一个错，避免级联。
                while let Some(next) = self.peek_char() {
                    if next == '\'' || next == '\n' || next == '\r' {
                        break;
                    }
                    self.pos = self.pos.saturating_add(next.len_utf8());
                }
                if self.peek_char() == Some('\'') {
                    self.pos = self.pos.saturating_add(1);
                }
                self.error(self.span_from(start), "字符字面量只能包含一个字符");
                self.push(TokenKind::Char(value), start);
            }
        }
    }

    /// 扫描运算符/分隔符；非法字符在此报告。
    fn lex_punct(&mut self) {
        let start = self.offset();
        let byte = match self.peek() {
            Some(byte) => byte,
            None => return,
        };
        let two = |lexer: &mut Self, kind: TokenKind| {
            lexer.pos = lexer.pos.saturating_add(2);
            lexer.push(kind, start);
        };
        let one = |lexer: &mut Self, kind: TokenKind| {
            lexer.pos = lexer.pos.saturating_add(1);
            lexer.push(kind, start);
        };
        match byte {
            b'(' => one(self, TokenKind::LParen),
            b')' => one(self, TokenKind::RParen),
            b'{' => one(self, TokenKind::LBrace),
            b'}' => one(self, TokenKind::RBrace),
            b'[' => one(self, TokenKind::LBracket),
            b']' => one(self, TokenKind::RBracket),
            b';' => one(self, TokenKind::Semi),
            b',' => one(self, TokenKind::Comma),
            b'@' => one(self, TokenKind::At),
            b'$' => one(self, TokenKind::Dollar),
            b'?' => one(self, TokenKind::Question),
            b'~' => one(self, TokenKind::Tilde),
            b'#' => {
                if self.peek_at(1) == Some(b'[') {
                    two(self, TokenKind::AttrStart);
                } else {
                    one(self, TokenKind::Hash);
                }
            }
            b'+' => {
                if self.peek_at(1) == Some(b'=') {
                    two(self, TokenKind::PlusEq);
                } else {
                    one(self, TokenKind::Plus);
                }
            }
            b'-' => match self.peek_at(1) {
                Some(b'=') => two(self, TokenKind::MinusEq),
                Some(b'>') => two(self, TokenKind::Arrow),
                _ => one(self, TokenKind::Minus),
            },
            b'*' => {
                if self.peek_at(1) == Some(b'*') {
                    two(self, TokenKind::StarStar);
                } else {
                    one(self, TokenKind::Star);
                }
            }
            b'/' => one(self, TokenKind::Slash),
            b'%' => one(self, TokenKind::Percent),
            b'&' => {
                if self.peek_at(1) == Some(b'&') {
                    two(self, TokenKind::AndAnd);
                } else {
                    one(self, TokenKind::Amp)
                }
            }
            b'|' => {
                if self.peek_at(1) == Some(b'|') {
                    two(self, TokenKind::OrOr);
                } else {
                    one(self, TokenKind::Pipe)
                }
            }
            b'^' => one(self, TokenKind::Caret),
            b'<' => match self.peek_at(1) {
                Some(b'=') => two(self, TokenKind::Le),
                Some(b'<') => two(self, TokenKind::Shl),
                _ => one(self, TokenKind::Lt),
            },
            b'>' => match self.peek_at(1) {
                Some(b'=') => two(self, TokenKind::Ge),
                Some(b'>') => two(self, TokenKind::Shr),
                _ => one(self, TokenKind::Gt),
            },
            b'=' => match self.peek_at(1) {
                Some(b'=') => two(self, TokenKind::EqEq),
                Some(b'>') => two(self, TokenKind::FatArrow),
                _ => one(self, TokenKind::Eq),
            },
            b'!' => {
                if self.peek_at(1) == Some(b'=') {
                    two(self, TokenKind::NotEq);
                } else {
                    one(self, TokenKind::Bang)
                }
            }
            b'.' => one(self, TokenKind::Dot),
            b':' => {
                if self.peek_at(1) == Some(b':') {
                    two(self, TokenKind::ColonColon);
                } else {
                    one(self, TokenKind::Colon)
                }
            }
            _ => {
                let ch = match self.peek_char() {
                    Some(ch) => ch,
                    None => return,
                };
                self.error(
                    self.span_from(start),
                    format!("非法字符 `{ch}`（U+{:04X}）", u32::from(ch)),
                );
                self.advance_char();
            }
        }
    }
}

/// 某字节在给定进制下的数值。
fn digit_value(byte: u8, radix: u32) -> Option<u32> {
    let value = match byte {
        b'0'..=b'9' => u32::from(byte.saturating_sub(b'0')),
        b'a'..=b'f' => u32::from(byte.saturating_sub(b'a')).saturating_add(10),
        b'A'..=b'F' => u32::from(byte.saturating_sub(b'A')).saturating_add(10),
        _ => return None,
    };
    if value < radix { Some(value) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn lex(src: &str) -> Vec<Token> {
        match Lexer::new(src).tokenize() {
            Ok(tokens) => tokens,
            Err(diags) => panic!("期望词法成功，实际错误: {diags:?}"),
        }
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).into_iter().map(|token| token.kind).collect()
    }

    #[test]
    fn keywords_are_recognized() {
        let source = "struct union enum bitfield fn let if else match for while return break \
                      continue true false padding be le";
        let expected = vec![
            TokenKind::KwStruct,
            TokenKind::KwUnion,
            TokenKind::KwEnum,
            TokenKind::KwBitfield,
            TokenKind::KwFn,
            TokenKind::KwLet,
            TokenKind::KwIf,
            TokenKind::KwElse,
            TokenKind::KwMatch,
            TokenKind::KwFor,
            TokenKind::KwWhile,
            TokenKind::KwReturn,
            TokenKind::KwBreak,
            TokenKind::KwContinue,
            TokenKind::KwTrue,
            TokenKind::KwFalse,
            TokenKind::KwPadding,
            TokenKind::KwBe,
            TokenKind::KwLe,
            TokenKind::Eof,
        ];
        assert_eq!(kinds(source), expected);
    }

    #[test]
    fn integers_support_bases_underscores_and_suffix() {
        let tokens = lex("0 42 1_000 0xDEAD_BEEF 0b1010_0101 0o17 7u8 9u32");
        let values: Vec<(u128, Option<IntSuffix>)> = tokens
            .iter()
            .filter_map(|token| match token.kind {
                TokenKind::Int { value, suffix } => Some((value, suffix)),
                _ => None,
            })
            .collect();
        assert_eq!(
            values,
            vec![
                (0, None),
                (42, None),
                (1000, None),
                (0xDEAD_BEEF, None),
                (0b1010_0101, None),
                (0o17, None),
                (7, Some(IntSuffix::U8)),
                (9, Some(IntSuffix::U32)),
            ]
        );
    }

    #[test]
    fn floats_cover_decimal_and_exponent_forms() {
        let tokens = lex("1.5 0.25 2e3 1.2E-2 3_000.5");
        let values: Vec<f64> = tokens
            .iter()
            .filter_map(|token| match token.kind {
                TokenKind::Float(value) => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(values, vec![1.5, 0.25, 2000.0, 0.012, 3000.5]);
    }

    #[test]
    fn strings_and_chars_decode_escapes() {
        let tokens = lex(r#""a\n\t\\\"" "\x41\u{1F600}" '\n' 'x' '\''"#);
        assert_eq!(tokens[0].kind, TokenKind::Str("a\n\t\\\"".to_owned()));
        assert_eq!(tokens[1].kind, TokenKind::Str("A😀".to_owned()));
        assert_eq!(tokens[2].kind, TokenKind::Char('\n'));
        assert_eq!(tokens[3].kind, TokenKind::Char('x'));
        assert_eq!(tokens[4].kind, TokenKind::Char('\''));
    }

    #[test]
    fn nested_comments_are_trivia() {
        let tokens = lex("a /* outer /* inner */ still outer */ b // tail\n c");
        let idents: Vec<String> = tokens
            .into_iter()
            .filter_map(|token| match token.kind {
                TokenKind::Ident(name) => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(idents, vec!["a", "b", "c"]);
    }

    #[test]
    fn every_operator_and_delimiter_is_lexed() {
        let source = "( ) { } [ ] ; , + - * / % & | ^ ~ << >> == != < <= > >= && || ! = += -= \
                      . :: -> => ? : ** @ $ # #[";
        let actual = kinds(source);
        let expected = vec![
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::Semi,
            TokenKind::Comma,
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::Percent,
            TokenKind::Amp,
            TokenKind::Pipe,
            TokenKind::Caret,
            TokenKind::Tilde,
            TokenKind::Shl,
            TokenKind::Shr,
            TokenKind::EqEq,
            TokenKind::NotEq,
            TokenKind::Lt,
            TokenKind::Le,
            TokenKind::Gt,
            TokenKind::Ge,
            TokenKind::AndAnd,
            TokenKind::OrOr,
            TokenKind::Bang,
            TokenKind::Eq,
            TokenKind::PlusEq,
            TokenKind::MinusEq,
            TokenKind::Dot,
            TokenKind::ColonColon,
            TokenKind::Arrow,
            TokenKind::FatArrow,
            TokenKind::Question,
            TokenKind::Colon,
            TokenKind::StarStar,
            TokenKind::At,
            TokenKind::Dollar,
            TokenKind::Hash,
            TokenKind::AttrStart,
            TokenKind::Eof,
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn every_token_span_matches_its_source_slice() {
        let source = "struct X { u8 magic[4]; } // 注释\n";
        let tokens = lex(source);
        for token in &tokens {
            let start = token.span.start as usize;
            let end = token.span.end as usize;
            assert!(source.is_char_boundary(start));
            assert!(source.is_char_boundary(end));
            assert!(start <= end);
        }
        let first = &tokens[0];
        assert_eq!(
            &source[first.span.start as usize..first.span.end as usize],
            "struct"
        );
    }

    #[test]
    fn unclosed_constructs_report_spanned_errors() {
        let cases = [
            ("\"abc", "字符串"),
            ("/* a /* b */", "块注释"),
            ("'ab'", "字符字面量"),
            ("§", "非法字符"),
        ];
        for (source, needle) in cases {
            let diagnostics = Lexer::new(source).tokenize().expect_err("必须报错");
            assert!(!diagnostics.is_empty());
            for diag in diagnostics {
                assert_eq!(diag.code, E_LEX);
                assert!(diag.range.is_some(), "错误必须带源码区间");
                assert!(diag.detail.contains(needle), "诊断细节: {}", diag.detail);
            }
        }
    }

    #[test]
    fn malformed_numbers_report_errors_but_keep_scanning() {
        let diagnostics = Lexer::new("0b102 0x 1_ 12q")
            .tokenize()
            .expect_err("必须报错");
        assert!(diagnostics.len() >= 3);
        assert!(diagnostics.iter().all(|diag| diag.range.is_some()));
    }

    proptest! {
        #[test]
        fn arbitrary_input_never_panics(source in any::<String>()) {
            let _ = Lexer::new(&source).tokenize();
        }

        #[test]
        fn token_spans_are_monotonic_and_in_bounds(
            source in proptest::collection::vec(
                prop_oneof![
                    Just('a'), Just('Z'), Just('_'), Just('0'), Just('1'), Just('u'),
                    Just(' '), Just('\n'), Just('\t'), Just('('), Just(')'), Just('{'),
                    Just('}'), Just('['), Just(']'), Just(';'), Just(','), Just('+'),
                    Just('-'), Just('*'), Just('/'), Just('%'), Just('&'), Just('|'),
                    Just('^'), Just('~'), Just('<'), Just('>'), Just('='), Just('!'),
                    Just('.'), Just(':'), Just('?'), Just('@'), Just('$'), Just('#'),
                    Just('"'), Just('\''), Just('\\'), Just('é'), Just('你'),
                ],
                0..80,
            )
        ) {
            let source: String = source.into_iter().collect();
            let Ok(tokens) = Lexer::new(&source).tokenize() else {
                return Ok(());
            };
            let mut previous_end = 0_u32;
            for token in &tokens {
                prop_assert!(token.span.start >= previous_end);
                prop_assert!(token.span.end <= source.len() as u32);
                prop_assert!(source.is_char_boundary(token.span.start as usize));
                prop_assert!(source.is_char_boundary(token.span.end as usize));
                previous_end = token.span.end;
            }
            prop_assert_eq!(tokens.last().map(|token| token.kind.clone()), Some(TokenKind::Eof));
        }
    }
}
