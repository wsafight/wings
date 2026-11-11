use crate::ast::{
    BinaryOperator, Block, Expr, ExprKind, Handler, Name, Parameter, Process, Program, State,
    Statement,
};
use crate::{Token, TokenKind};
use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};

pub fn parse(
    text: &str,
    tokens: &[Token],
    diagnostics: Vec<Diagnostic>,
) -> (Program, Vec<Diagnostic>) {
    let mut parser = Parser {
        text,
        tokens: tokens
            .iter()
            .filter(|token| !token.kind.is_trivia())
            .collect(),
        cursor: 0,
        expression_nodes: 0,
        diagnostics,
    };
    let mut program = Program::default();
    while !parser.at(TokenKind::Eof) {
        if parser.at(TokenKind::Process) {
            program.processes.push(parser.process());
        } else {
            parser.error("expected 'process'");
            parser.bump();
        }
    }
    parser.diagnostics.truncate(MAX_DIAGNOSTICS);
    (program, parser.diagnostics)
}

struct Parser<'source> {
    text: &'source str,
    tokens: Vec<&'source Token>,
    cursor: usize,
    expression_nodes: usize,
    diagnostics: Vec<Diagnostic>,
}

impl Parser<'_> {
    fn current(&self) -> &Token {
        self.tokens[self.cursor]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.current().kind == kind
    }

    fn next_is(&self, kind: TokenKind) -> bool {
        self.tokens
            .get(self.cursor + 1)
            .is_some_and(|token| token.kind == kind)
    }

    fn line_break(&self) -> bool {
        self.cursor > 0
            && self.text[self.tokens[self.cursor - 1].span.end..self.current().span.start]
                .contains(['\n', '\r'])
    }

    fn bump(&mut self) -> Span {
        let span = self.current().span;
        if !self.at(TokenKind::Eof) {
            self.cursor += 1;
        }
        span
    }

    fn error(&mut self, message: &str) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics
                .push(Diagnostic::error("S003", message, self.current().span));
        }
    }

    fn expect(&mut self, kind: TokenKind, message: &str) -> Span {
        if self.at(kind) {
            self.bump()
        } else {
            self.error(message);
            Span::new(self.current().span.start, self.current().span.start)
        }
    }

    fn name(&mut self) -> Name {
        if self.at(TokenKind::Identifier) {
            let span = self.bump();
            Name {
                text: self.text[span.start..span.end].to_owned(),
                span,
            }
        } else {
            self.error("expected a name");
            Name {
                text: String::new(),
                span: Span::new(self.current().span.start, self.current().span.start),
            }
        }
    }

    fn boundary(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::On
                | TokenKind::State
                | TokenKind::Process
                | TokenKind::RightBrace
                | TokenKind::Eof
        )
    }

    fn separator(&mut self) {
        if self.at(TokenKind::Semicolon) {
            self.bump();
        } else if !self.line_break() && !self.boundary() {
            self.error("expected a newline or ';' between statements");
            self.recover_statement();
        }
    }

    fn recover_statement(&mut self) {
        while !self.boundary() && !self.at(TokenKind::Semicolon) && !self.line_break() {
            self.bump();
        }
        if self.at(TokenKind::Semicolon) {
            self.bump();
        }
    }

    fn process(&mut self) -> Process {
        let start = self.bump().start;
        let name = self.name();
        self.expect(TokenKind::LeftBrace, "expected '{' after process name");
        let mut states = Vec::new();
        let mut handlers = Vec::new();
        while !matches!(
            self.current().kind,
            TokenKind::RightBrace | TokenKind::Process | TokenKind::Eof
        ) {
            match self.current().kind {
                TokenKind::State => states.push(self.state()),
                TokenKind::On => handlers.push(self.handler()),
                _ => {
                    self.error("expected state declaration or 'on' handler");
                    self.bump();
                }
            }
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after process")
            .end;
        Process {
            name,
            states,
            handlers,
            span: Span::new(start, end),
        }
    }

    fn state(&mut self) -> State {
        self.bump();
        let name = self.name();
        self.expect(TokenKind::Colon, "expected ':' after state name");
        let ty = self.name();
        self.expect(TokenKind::Equal, "expected '=' before initial state");
        self.expression_nodes = 0;
        let default = self.expression(0, false);
        self.separator();
        State { name, ty, default }
    }

    fn handler(&mut self) -> Handler {
        let start = self.bump().start;
        let name = self.name();
        let mut parameters = Vec::new();
        if self.at(TokenKind::LeftParen) {
            self.bump();
            while !self.at(TokenKind::RightParen)
                && !self.at(TokenKind::LeftBrace)
                && !self.at(TokenKind::Arrow)
                && !self.boundary()
            {
                let previous = self.cursor;
                let name = self.name();
                self.expect(TokenKind::Colon, "expected ':' after parameter name");
                let ty = self.name();
                parameters.push(Parameter { name, ty });
                if self.cursor == previous || !self.at(TokenKind::Comma) {
                    break;
                }
                self.bump();
            }
            self.expect(TokenKind::RightParen, "expected ')' after parameters");
        }
        let reply_type = if self.at(TokenKind::Arrow) {
            self.bump();
            Some(self.name())
        } else {
            None
        };
        let body = self.block();
        let span = Span::new(start, body.span.end);
        Handler {
            name,
            parameters,
            reply_type,
            body,
            span,
        }
    }

    fn block(&mut self) -> Block {
        let start = self
            .expect(TokenKind::LeftBrace, "expected '{' before handler body")
            .start;
        let mut statements = Vec::new();
        let mut tail = None;
        while !self.boundary() {
            let previous = self.cursor;
            self.expression_nodes = 0;
            if self.at(TokenKind::Let) {
                self.bump();
                let name = self.name();
                let ty = if self.at(TokenKind::Colon) {
                    self.bump();
                    Some(self.name())
                } else {
                    None
                };
                self.expect(TokenKind::Equal, "expected '=' in local binding");
                let value = self.expression(0, false);
                statements.push(Statement::Let { name, ty, value });
            } else if self.at(TokenKind::Identifier) && self.next_is(TokenKind::Equal) {
                let target = self.name();
                self.bump();
                let value = self.expression(0, false);
                statements.push(Statement::Assign { target, value });
            } else {
                let value = self.expression(0, false);
                if self.boundary() {
                    tail = Some(value);
                    break;
                }
                statements.push(Statement::Expr(value));
            }
            if self.cursor == previous {
                self.bump();
            }
            self.separator();
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after handler body")
            .end;
        Block {
            statements,
            tail,
            span: Span::new(start, end),
        }
    }

    fn expression(&mut self, minimum: u8, multiline: bool) -> Expr {
        self.expression_nodes += 1;
        if self.expression_nodes > 256 {
            self.error("expression exceeds the 256-node frontend limit");
            return Expr {
                kind: ExprKind::Error,
                span: self.current().span,
            };
        }
        let mut left = self.primary(multiline);
        loop {
            if self.expression_nodes > 256 || (!multiline && self.line_break()) {
                break;
            }
            let (operator, precedence) = match self.current().kind {
                TokenKind::Plus => (BinaryOperator::Add, 1),
                TokenKind::Minus => (BinaryOperator::Subtract, 1),
                TokenKind::Star => (BinaryOperator::Multiply, 2),
                TokenKind::Slash => (BinaryOperator::Divide, 2),
                _ => break,
            };
            if precedence < minimum {
                break;
            }
            self.bump();
            let right = self.expression(precedence + 1, multiline);
            let span = Span::new(left.span.start, right.span.end);
            left = Expr {
                kind: ExprKind::Binary {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }
        left
    }

    fn primary(&mut self, multiline: bool) -> Expr {
        let start = self.current().span.start;
        let mut span = self.current().span;
        let kind = match self.current().kind {
            TokenKind::Integer => {
                self.bump();
                self.integer(&self.text[span.start..span.end], span)
            }
            TokenKind::Minus => {
                self.bump();
                if self.at(TokenKind::Integer) {
                    let number = self.bump();
                    span = Span::new(start, number.end);
                    self.integer(&format!("-{}", &self.text[number.start..number.end]), span)
                } else {
                    let value = self.expression(3, multiline);
                    span = Span::new(start, value.span.end);
                    ExprKind::Negate(Box::new(value))
                }
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(TokenKind::True);
                self.bump();
                ExprKind::Boolean(value)
            }
            TokenKind::Identifier => ExprKind::Name(self.name()),
            TokenKind::LeftParen => {
                self.bump();
                if self.at(TokenKind::RightParen) {
                    span = Span::new(start, self.bump().end);
                    ExprKind::Unit
                } else {
                    let value = self.expression(0, true);
                    let end = self
                        .expect(TokenKind::RightParen, "expected ')' after expression")
                        .end;
                    return Expr {
                        span: Span::new(start, end),
                        ..value
                    };
                }
            }
            _ => {
                self.error("expected an expression");
                if !self.boundary()
                    && !matches!(
                        self.current().kind,
                        TokenKind::RightParen | TokenKind::Semicolon | TokenKind::Comma
                    )
                {
                    self.bump();
                }
                ExprKind::Error
            }
        };
        Expr { kind, span }
    }

    fn integer(&mut self, text: &str, span: Span) -> ExprKind {
        match text.parse::<i64>() {
            Ok(value) => ExprKind::Integer(value),
            Err(_) => {
                if self.diagnostics.len() < MAX_DIAGNOSTICS {
                    self.diagnostics.push(Diagnostic::error(
                        "S004",
                        "integer literal is outside the signed 64-bit range",
                        span,
                    ));
                }
                ExprKind::Error
            }
        }
    }
}
