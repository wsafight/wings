use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Identifier,
    Integer,
    String,
    FormatString,
    Bytes,
    Actor,
    Struct,
    State,
    On,
    Fn,
    Let,
    If,
    Else,
    While,
    For,
    In,
    Match,
    SelfKw,
    Spawn,
    True,
    False,
    Colon,
    ColonColon,
    Dot,
    DotDot,
    DotDotEqual,
    Comma,
    Equal,
    EqualEqual,
    FatArrow,
    NotEqual,
    Bang,
    AndAnd,
    OrOr,
    PipeGreater,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Arrow,
    Semicolon,
    LeftBrace,
    RightBrace,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Plus,
    PlusEqual,
    Minus,
    MinusEqual,
    Star,
    StarEqual,
    Slash,
    SlashEqual,
    Percent,
    PercentEqual,
    Whitespace,
    Comment,
    Invalid,
    Eof,
}

impl TokenKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, Self::Whitespace | Self::Comment)
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

fn scan_string_body(
    text: &str,
    mut offset: usize,
    is_fstring: bool,
    allow_dollar_interp: bool,
) -> (usize, bool) {
    let bytes = text.as_bytes();
    let mut escaped = false;
    let mut brace_depth = 0usize;
    let mut in_nested_string = false;
    let mut nested_escaped = false;
    while let Some(next) = text[offset..].chars().next() {
        if matches!(next, '\n' | '\r') {
            return (offset, false);
        }
        let step = next.len_utf8();
        if brace_depth > 0 {
            offset += step;
            if in_nested_string {
                if nested_escaped {
                    nested_escaped = false;
                } else if next == '\\' {
                    nested_escaped = true;
                } else if next == '"' {
                    in_nested_string = false;
                }
            } else if next == '"' {
                in_nested_string = true;
            } else if next == '{' {
                brace_depth += 1;
            } else if next == '}' {
                brace_depth -= 1;
            }
            continue;
        }
        if escaped {
            escaped = false;
            offset += step;
            continue;
        }
        if next == '\\' {
            escaped = true;
            offset += step;
            continue;
        }
        if next == '"' {
            offset += step;
            return (offset, true);
        }
        if is_fstring && next == '{' {
            if bytes.get(offset + 1) == Some(&b'{') {
                offset += 2;
                continue;
            }
            brace_depth = 1;
            offset += 1;
            continue;
        }
        if allow_dollar_interp && next == '$' && bytes.get(offset + 1) == Some(&b'{') {
            brace_depth = 1;
            offset += 2;
            continue;
        }
        offset += step;
    }
    (offset, false)
}

pub fn lex(text: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut diagnostics = Vec::new();
    let mut offset = 0;
    let bytes = text.as_bytes();
    while offset < text.len() {
        let start = offset;
        let character = text[offset..].chars().next().unwrap();
        offset += character.len_utf8();
        let kind = if character.is_whitespace() {
            while let Some(next) = text[offset..].chars().next() {
                if !next.is_whitespace() {
                    break;
                }
                offset += next.len_utf8();
            }
            TokenKind::Whitespace
        } else if character == '"' {
            let (end, closed) = scan_string_body(text, offset, false, true);
            offset = end;
            if !closed && diagnostics.len() < MAX_DIAGNOSTICS {
                diagnostics.push(Diagnostic::error(
                    "S005",
                    "unterminated string literal",
                    Span::new(start, offset),
                ));
            }
            TokenKind::String
        } else if (character == 'b' || character == 'f') && bytes.get(offset) == Some(&b'"') {
            offset += 1;
            let is_fstring = character == 'f';
            let (end, closed) = scan_string_body(text, offset, is_fstring, false);
            offset = end;
            if !closed && diagnostics.len() < MAX_DIAGNOSTICS {
                diagnostics.push(Diagnostic::error(
                    "S005",
                    if is_fstring {
                        "unterminated format string literal"
                    } else {
                        "unterminated byte literal"
                    },
                    Span::new(start, offset),
                ));
            }
            if is_fstring {
                TokenKind::FormatString
            } else {
                TokenKind::Bytes
            }
        } else if character == '#' || (character == '/' && bytes.get(offset) == Some(&b'/')) {
            while let Some(next) = text[offset..].chars().next() {
                if next == '\n' {
                    break;
                }
                offset += next.len_utf8();
            }
            TokenKind::Comment
        } else if character.is_ascii_digit() {
            while let Some(&byte) = bytes.get(offset) {
                if byte.is_ascii_digit() {
                    offset += 1;
                } else if byte == b'_' && bytes.get(offset + 1).is_some_and(u8::is_ascii_digit) {
                    offset += 2;
                } else {
                    break;
                }
            }
            TokenKind::Integer
        } else if character == '_' || character.is_alphabetic() {
            while let Some(next) = text[offset..].chars().next() {
                if next != '_' && !next.is_alphanumeric() {
                    break;
                }
                offset += next.len_utf8();
            }
            match &text[start..offset] {
                "actor" => TokenKind::Actor,
                "struct" => TokenKind::Struct,
                "state" => TokenKind::State,
                "on" => TokenKind::On,
                "fn" => TokenKind::Fn,
                "let" => TokenKind::Let,
                "if" => TokenKind::If,
                "else" => TokenKind::Else,
                "while" => TokenKind::While,
                "for" => TokenKind::For,
                "in" => TokenKind::In,
                "match" => TokenKind::Match,
                "self" => TokenKind::SelfKw,
                "spawn" => TokenKind::Spawn,
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                _ => TokenKind::Identifier,
            }
        } else {
            match character {
                ':' if bytes.get(offset) == Some(&b':') => {
                    offset += 1;
                    TokenKind::ColonColon
                }
                ':' => TokenKind::Colon,
                '.' if bytes.get(offset) == Some(&b'.') && bytes.get(offset + 1) == Some(&b'=') => {
                    offset += 2;
                    TokenKind::DotDotEqual
                }
                '.' if bytes.get(offset) == Some(&b'.') => {
                    offset += 1;
                    TokenKind::DotDot
                }
                '.' => TokenKind::Dot,
                ',' => TokenKind::Comma,
                '=' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::EqualEqual
                }
                '=' if bytes.get(offset) == Some(&b'>') => {
                    offset += 1;
                    TokenKind::FatArrow
                }
                '=' => TokenKind::Equal,
                '!' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::NotEqual
                }
                '!' => TokenKind::Bang,
                '&' if bytes.get(offset) == Some(&b'&') => {
                    offset += 1;
                    TokenKind::AndAnd
                }
                '|' if bytes.get(offset) == Some(&b'|') => {
                    offset += 1;
                    TokenKind::OrOr
                }
                '|' if bytes.get(offset) == Some(&b'>') => {
                    offset += 1;
                    TokenKind::PipeGreater
                }
                '<' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::LessEqual
                }
                '>' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::GreaterEqual
                }
                '<' => TokenKind::Less,
                '>' => TokenKind::Greater,
                ';' => TokenKind::Semicolon,
                '{' => TokenKind::LeftBrace,
                '}' => TokenKind::RightBrace,
                '(' => TokenKind::LeftParen,
                ')' => TokenKind::RightParen,
                '[' => TokenKind::LeftBracket,
                ']' => TokenKind::RightBracket,
                '+' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::PlusEqual
                }
                '+' => TokenKind::Plus,
                '-' if bytes.get(offset) == Some(&b'>') => {
                    offset += 1;
                    TokenKind::Arrow
                }
                '-' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::MinusEqual
                }
                '-' => TokenKind::Minus,
                '*' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::StarEqual
                }
                '*' => TokenKind::Star,
                '/' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::SlashEqual
                }
                '/' => TokenKind::Slash,
                '%' if bytes.get(offset) == Some(&b'=') => {
                    offset += 1;
                    TokenKind::PercentEqual
                }
                '%' => TokenKind::Percent,
                _ => {
                    if diagnostics.len() < MAX_DIAGNOSTICS {
                        diagnostics.push(Diagnostic::error(
                            "S001",
                            format!("unexpected character {character:?}"),
                            Span::new(start, offset),
                        ));
                    }
                    TokenKind::Invalid
                }
            }
        };
        tokens.push(Token {
            kind,
            span: Span::new(start, offset),
        });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(text.len(), text.len()),
    });
    (tokens, diagnostics)
}
