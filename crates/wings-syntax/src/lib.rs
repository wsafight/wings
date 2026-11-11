pub mod ast;
mod lexer;
mod parser;

pub use lexer::{Token, TokenKind, lex};
use wings_source::{Diagnostic, Span};

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Parse {
    pub program: ast::Program,
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(text: &str) -> Parse {
    if text.len() > MAX_SOURCE_BYTES {
        return Parse {
            program: ast::Program::default(),
            tokens: Vec::new(),
            diagnostics: vec![Diagnostic::error(
                "S002",
                "source exceeds the 1 MiB frontend limit",
                Span::new(0, 0),
            )],
        };
    }
    let (tokens, diagnostics) = lex(text);
    let (program, diagnostics) = parser::parse(text, &tokens, diagnostics);
    Parse {
        program,
        tokens,
        diagnostics,
    }
}
