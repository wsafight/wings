use crate::ast::{
    BinaryOperator, Block, Expr, ExprKind, Function, Handler, Name, Parameter, Process, Program,
    Record, State, Statement,
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
        block_depth: 0,
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
    block_depth: usize,
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
        self.record_declaration()
            || matches!(
                self.current().kind,
                TokenKind::On
                    | TokenKind::Fn
                    | TokenKind::State
                    | TokenKind::Process
                    | TokenKind::RightBrace
                    | TokenKind::Eof
            )
    }

    fn record_declaration(&self) -> bool {
        self.at(TokenKind::Identifier)
            && &self.text[self.current().span.start..self.current().span.end] == "record"
            && self.next_is(TokenKind::Identifier)
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
        let mut records = Vec::new();
        let mut handlers = Vec::new();
        let mut functions = Vec::new();
        while !matches!(
            self.current().kind,
            TokenKind::RightBrace | TokenKind::Process | TokenKind::Eof
        ) {
            match self.current().kind {
                TokenKind::Identifier if self.record_declaration() => records.push(self.record()),
                TokenKind::State => states.push(self.state()),
                TokenKind::On => handlers.push(self.handler()),
                TokenKind::Fn => {
                    let function = self.function();
                    if functions.len() == 64 {
                        self.error("process function count exceeds 64");
                    } else {
                        functions.push(function);
                    }
                }
                _ => {
                    self.error("expected state, 'fn' function or 'on' handler");
                    self.bump();
                }
            }
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after process")
            .end;
        Process {
            name,
            records,
            states,
            handlers,
            functions,
            span: Span::new(start, end),
        }
    }

    fn record(&mut self) -> Record {
        let start = self.bump().start;
        let name = self.name();
        self.expect(TokenKind::LeftBrace, "expected '{' after record name");
        let mut fields = Vec::new();
        while self.at(TokenKind::Identifier) {
            let name = self.name();
            self.expect(TokenKind::Colon, "expected ':' after field name");
            let ty = self.name();
            fields.push(Parameter { name, ty });
            if !self.at(TokenKind::Comma) {
                break;
            }
            self.bump();
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after record fields")
            .end;
        Record {
            name,
            fields,
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

    fn function(&mut self) -> Function {
        let start = self.current().span.start;
        let has_parameters = self
            .tokens
            .get(self.cursor + 2)
            .is_some_and(|token| token.kind == TokenKind::LeftParen);
        let signature = self.handler();
        if !has_parameters {
            self.error("function parameters require parentheses");
        }
        let result = signature.reply_type.unwrap_or_else(|| {
            self.error("function return type requires '-> Type'");
            Name {
                text: String::new(),
                span: signature.name.span,
            }
        });
        if signature.parameters.len() > 64 {
            self.error("function parameter count exceeds 64");
        }
        Function {
            name: signature.name,
            parameters: signature.parameters,
            result,
            body: signature.body,
            span: Span::new(start, signature.span.end),
        }
    }

    fn block(&mut self) -> Block {
        if self.block_depth == 64 {
            self.error("block nesting exceeds the 64-level frontend limit");
            let start = self.current().span.start;
            if self.at(TokenKind::LeftBrace) {
                self.bump();
                let mut depth = 1;
                while depth != 0 && !self.at(TokenKind::Eof) {
                    match self.current().kind {
                        TokenKind::LeftBrace => depth += 1,
                        TokenKind::RightBrace => depth -= 1,
                        _ => {}
                    }
                    self.bump();
                }
            }
            return Block {
                statements: Vec::new(),
                tail: None,
                span: Span::new(start, self.current().span.start),
            };
        }
        self.block_depth += 1;
        let start = self
            .expect(TokenKind::LeftBrace, "expected '{' before handler body")
            .start;
        let mut statements = Vec::new();
        let mut tail = None;
        while !self.boundary() {
            let previous = self.cursor;
            self.expression_nodes = 0;
            if self.at(TokenKind::While) {
                self.bump();
                let condition = self.expression(0, true);
                let body = self.block();
                statements.push(Statement::While { condition, body });
            } else if self.at(TokenKind::Let) {
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
        self.block_depth -= 1;
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
            if self.at(TokenKind::Dot) {
                self.bump();
                let name = self.name();
                let span = Span::new(left.span.start, name.span.end);
                left = Expr {
                    kind: ExprKind::Field {
                        value: Box::new(left),
                        name,
                    },
                    span,
                };
                self.expression_nodes += 1;
                continue;
            }
            let (operator, precedence) = match self.current().kind {
                TokenKind::EqualEqual => (BinaryOperator::Equal, 1),
                TokenKind::NotEqual => (BinaryOperator::NotEqual, 1),
                TokenKind::Less => (BinaryOperator::Less, 1),
                TokenKind::LessEqual => (BinaryOperator::LessEqual, 1),
                TokenKind::Greater => (BinaryOperator::Greater, 1),
                TokenKind::GreaterEqual => (BinaryOperator::GreaterEqual, 1),
                TokenKind::Plus => (BinaryOperator::Add, 2),
                TokenKind::Minus => (BinaryOperator::Subtract, 2),
                TokenKind::Star => (BinaryOperator::Multiply, 3),
                TokenKind::Slash => (BinaryOperator::Divide, 3),
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

    fn conditional_arm(&mut self) -> Expr {
        if !self.at(TokenKind::LeftBrace) {
            self.error("expected '{' before conditional arm");
            return Expr {
                kind: ExprKind::Error,
                span: self.current().span,
            };
        }
        self.bump();
        let value = self.expression(0, true);
        if !self.at(TokenKind::RightBrace) {
            self.error("expected one expression followed by '}' in conditional arm");
            let mut depth = 0;
            while !matches!(
                self.current().kind,
                TokenKind::On | TokenKind::State | TokenKind::Process | TokenKind::Eof
            ) {
                if self.at(TokenKind::RightBrace) {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if self.at(TokenKind::LeftBrace) {
                    depth += 1;
                }
                self.bump();
            }
        }
        self.expect(TokenKind::RightBrace, "expected '}' after conditional arm");
        value
    }

    fn primary(&mut self, multiline: bool) -> Expr {
        let start = self.current().span.start;
        let mut span = self.current().span;
        let kind = match self.current().kind {
            TokenKind::If => {
                self.bump();
                let condition = self.expression(0, true);
                let then_value = self.conditional_arm();
                if !self.at(TokenKind::Else) {
                    self.error("expected 'else' after if expression");
                    return Expr {
                        kind: ExprKind::Error,
                        span: Span::new(start, then_value.span.end),
                    };
                }
                self.bump();
                let else_value = self.conditional_arm();
                let end = self.tokens[self.cursor.saturating_sub(1)]
                    .span
                    .end
                    .max(else_value.span.end);
                span = Span::new(start, end);
                ExprKind::If {
                    condition: Box::new(condition),
                    then_value: Box::new(then_value),
                    else_value: Box::new(else_value),
                }
            }
            TokenKind::Integer => {
                self.bump();
                self.integer(&self.text[span.start..span.end], span)
            }
            TokenKind::String => {
                self.bump();
                match crate::string::decode(&self.text[span.start..span.end]) {
                    Ok(value) => ExprKind::String(value),
                    Err(message) => {
                        if self.diagnostics.len() < MAX_DIAGNOSTICS {
                            self.diagnostics
                                .push(Diagnostic::error("S005", message, span));
                        }
                        ExprKind::Error
                    }
                }
            }
            TokenKind::Minus => {
                self.bump();
                if self.at(TokenKind::Integer) {
                    let number = self.bump();
                    span = Span::new(start, number.end);
                    self.integer(&format!("-{}", &self.text[number.start..number.end]), span)
                } else {
                    let value = self.expression(4, multiline);
                    span = Span::new(start, value.span.end);
                    ExprKind::Negate(Box::new(value))
                }
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(TokenKind::True);
                self.bump();
                ExprKind::Boolean(value)
            }
            TokenKind::Identifier => {
                let callee = self.name();
                if self.at(TokenKind::LeftParen) && !self.line_break() {
                    self.bump();
                    let mut arguments = Vec::new();
                    while !self.at(TokenKind::RightParen) && !self.boundary() {
                        let previous = self.cursor;
                        arguments.push(self.expression(0, true));
                        if self.cursor == previous || self.expression_nodes > 256 {
                            break;
                        }
                        if !self.at(TokenKind::Comma) {
                            break;
                        }
                        self.bump();
                    }
                    span = Span::new(
                        start,
                        self.expect(TokenKind::RightParen, "expected ')' after call arguments")
                            .end,
                    );
                    ExprKind::Call { callee, arguments }
                } else {
                    ExprKind::Name(callee)
                }
            }
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
