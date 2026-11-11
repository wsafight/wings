use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Identifier,
    Integer,
    Process,
    State,
    On,
    Let,
    True,
    False,
    Colon,
    Comma,
    Equal,
    Arrow,
    Semicolon,
    LeftBrace,
    RightBrace,
    LeftParen,
    RightParen,
    Plus,
    Minus,
    Star,
    Slash,
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

pub fn lex(text: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut diagnostics = Vec::new();
    let mut offset = 0;
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
        } else if character == '#' {
            while let Some(next) = text[offset..].chars().next() {
                if next == '\n' {
                    break;
                }
                offset += next.len_utf8();
            }
            TokenKind::Comment
        } else if character.is_ascii_digit() {
            while text.as_bytes().get(offset).is_some_and(u8::is_ascii_digit) {
                offset += 1;
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
                "process" => TokenKind::Process,
                "state" => TokenKind::State,
                "on" => TokenKind::On,
                "let" => TokenKind::Let,
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                _ => TokenKind::Identifier,
            }
        } else {
            match character {
                ':' => TokenKind::Colon,
                ',' => TokenKind::Comma,
                '=' => TokenKind::Equal,
                ';' => TokenKind::Semicolon,
                '{' => TokenKind::LeftBrace,
                '}' => TokenKind::RightBrace,
                '(' => TokenKind::LeftParen,
                ')' => TokenKind::RightParen,
                '+' => TokenKind::Plus,
                '-' if text.as_bytes().get(offset) == Some(&b'>') => {
                    offset += 1;
                    TokenKind::Arrow
                }
                '-' => TokenKind::Minus,
                '*' => TokenKind::Star,
                '/' => TokenKind::Slash,
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
