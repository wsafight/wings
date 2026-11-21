use crate::ast::{
    Actor, AssignTarget, BinaryOperator, Block, Expr, ExprKind, Function, Handler,
    InterpolatedPart, MatchArm, MatchPattern, Name, Parameter, Program, State, Statement,
    StructDecl, TypeExpr, TypeKind,
};
use crate::string::RawSegment;
use crate::{Token, TokenKind};
use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};

pub fn parse(
    text: &str,
    tokens: &[Token],
    diagnostics: Vec<Diagnostic>,
) -> (Program, Vec<Diagnostic>) {
    let filtered: Vec<&Token> = tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let mut parser = Parser {
        text,
        tokens: &filtered,
        cursor: 0,
        expression_nodes: 0,
        block_depth: 0,
        diagnostics,
    };
    let mut program = Program::default();
    while !parser.at(TokenKind::Eof) {
        if parser.at(TokenKind::Actor) {
            program.actors.push(parser.actor());
        } else if parser.at(TokenKind::Struct) {
            program.structs.push(parser.struct_decl());
        } else if parser.at(TokenKind::Semicolon) {
            parser.bump();
        } else {
            parser.error("expected 'actor' or 'struct'");
            parser.bump();
        }
    }
    parser.diagnostics.truncate(MAX_DIAGNOSTICS);
    (program, parser.diagnostics)
}

struct Parser<'source, 'tokens> {
    text: &'source str,
    tokens: &'tokens [&'tokens Token],
    cursor: usize,
    expression_nodes: usize,
    block_depth: usize,
    diagnostics: Vec<Diagnostic>,
}

impl<'source, 'tokens> Parser<'source, 'tokens> {
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
        self.error_at(message, self.current().span);
    }

    fn error_at(&mut self, message: &str, span: Span) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics
                .push(Diagnostic::error("S003", message, span));
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
            TokenKind::Struct
                | TokenKind::On
                | TokenKind::Fn
                | TokenKind::State
                | TokenKind::Actor
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

    fn actor(&mut self) -> Actor {
        let start = self.bump().start;
        let name = self.name();
        self.expect(TokenKind::LeftBrace, "expected '{' after actor name");
        let mut states = Vec::new();
        let mut structs = Vec::new();
        let mut handlers = Vec::new();
        let mut functions = Vec::new();
        while !matches!(
            self.current().kind,
            TokenKind::RightBrace | TokenKind::Actor | TokenKind::Eof
        ) {
            match self.current().kind {
                TokenKind::Struct => structs.push(self.struct_decl()),
                TokenKind::State => states.push(self.state()),
                TokenKind::On => handlers.push(self.handler()),
                TokenKind::Fn => {
                    let function = self.function();
                    if functions.len() == 64 {
                        self.error_at("actor function count exceeds 64", function.name.span);
                    } else {
                        functions.push(function);
                    }
                }
                TokenKind::Semicolon => {
                    self.bump();
                }
                _ => {
                    self.error("expected 'struct', 'state', 'fn' or 'on' inside actor");
                    self.bump();
                }
            }
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after actor")
            .end;
        Actor {
            name,
            structs,
            states,
            handlers,
            functions,
            span: Span::new(start, end),
        }
    }

    fn parse_type(&mut self) -> TypeExpr {
        let start = self.current().span.start;
        match self.current().kind {
            TokenKind::LeftBracket => {
                self.bump();
                let inner = self.parse_type();
                let end = self
                    .expect(
                        TokenKind::RightBracket,
                        "expected ']' after list element type",
                    )
                    .end;
                TypeExpr {
                    kind: TypeKind::List(Box::new(inner)),
                    span: Span::new(start, end),
                }
            }
            TokenKind::LeftParen => {
                self.bump();
                let end = self
                    .expect(TokenKind::RightParen, "expected ')' for unit type '()'")
                    .end;
                TypeExpr {
                    kind: TypeKind::Unit,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Identifier => {
                let ident = self.name();
                if ident.text == "box" && self.at(TokenKind::Less) {
                    self.bump();
                    let inner = self.parse_type();
                    let end = self
                        .expect(TokenKind::Greater, "expected '>' after boxed type")
                        .end;
                    TypeExpr {
                        kind: TypeKind::Boxed(Box::new(inner)),
                        span: Span::new(start, end),
                    }
                } else if ident.text == "pid" && self.at(TokenKind::Less) {
                    self.bump();
                    let actor = self.name();
                    let end = self
                        .expect(
                            TokenKind::Greater,
                            "expected '>' after actor name in pid<Actor>",
                        )
                        .end;
                    TypeExpr {
                        kind: TypeKind::ActorPid(actor),
                        span: Span::new(start, end),
                    }
                } else {
                    let span = ident.span;
                    TypeExpr {
                        kind: TypeKind::Named(ident),
                        span,
                    }
                }
            }
            _ => {
                self.error("expected a type");
                let span = Span::new(start, start);
                TypeExpr {
                    kind: TypeKind::Error,
                    span,
                }
            }
        }
    }

    fn struct_decl(&mut self) -> StructDecl {
        let start = self.bump().start;
        let name = self.name();
        self.expect(TokenKind::LeftBrace, "expected '{' after struct name");
        let mut fields = Vec::new();
        while !self.at(TokenKind::RightBrace) && !self.boundary() {
            let previous = self.cursor;
            let field_name = self.name();
            self.expect(TokenKind::Colon, "expected ':' after struct field name");
            let ty = self.parse_type();
            fields.push(Parameter {
                name: field_name,
                ty,
            });
            if self.cursor == previous {
                self.bump();
                break;
            }
            if self.at(TokenKind::Comma) || self.at(TokenKind::Semicolon) {
                self.bump();
            } else if !self.line_break() && !self.at(TokenKind::RightBrace) {
                break;
            }
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after struct fields")
            .end;
        StructDecl {
            name,
            fields,
            span: Span::new(start, end),
        }
    }

    fn state(&mut self) -> State {
        self.bump();
        let name = self.name();
        let ty = if self.at(TokenKind::Colon) {
            self.bump();
            Some(self.parse_type())
        } else {
            None
        };
        self.expect(TokenKind::Equal, "expected '=' before initial state");
        self.expression_nodes = 0;
        let default = self.expression(0, false, true);
        self.separator();
        State { name, ty, default }
    }

    fn parameter_list(&mut self) -> Vec<Parameter> {
        let mut parameters = Vec::new();
        if self.at(TokenKind::LeftParen) {
            self.bump();
            while !self.at(TokenKind::RightParen)
                && !self.at(TokenKind::LeftBrace)
                && !self.at(TokenKind::FatArrow)
                && !self.at(TokenKind::Arrow)
                && !self.boundary()
            {
                let previous = self.cursor;
                let name = self.name();
                self.expect(TokenKind::Colon, "expected ':' after parameter name");
                let ty = self.parse_type();
                parameters.push(Parameter { name, ty });
                if self.cursor == previous || !self.at(TokenKind::Comma) {
                    break;
                }
                self.bump();
            }
            self.expect(TokenKind::RightParen, "expected ')' after parameters");
        }
        parameters
    }

    fn callable_body(&mut self) -> Block {
        if self.at(TokenKind::FatArrow) {
            let arrow = self.bump();
            self.expression_nodes = 0;
            if self.at(TokenKind::LeftBrace) {
                return self.block();
            }
            let value = self.expression(0, false, true);
            if !self.line_break()
                && (self.at(TokenKind::Equal)
                    || Self::compound_assign_op(self.current().kind).is_some())
            {
                let stmt = self.finish_assign(value);
                let end = match &stmt {
                    Statement::Assign { value, .. } => value.span.end.max(arrow.end),
                    _ => arrow.end,
                };
                self.separator();
                Block {
                    statements: vec![stmt],
                    tail: None,
                    span: Span::new(arrow.start, end),
                }
            } else {
                let end = value.span.end.max(arrow.end);
                self.separator();
                Block {
                    statements: Vec::new(),
                    tail: Some(Box::new(value)),
                    span: Span::new(arrow.start, end),
                }
            }
        } else {
            self.block()
        }
    }

    fn handler(&mut self) -> Handler {
        let start = self.bump().start;
        let is_reply = self.at(TokenKind::Identifier)
            && &self.text[self.current().span.start..self.current().span.end] == "reply"
            && self.next_is(TokenKind::Identifier);
        if is_reply {
            self.bump();
        }
        let name = self.name();
        let parameters = self.parameter_list();
        let reply_type = if self.at(TokenKind::Arrow) {
            self.bump();
            Some(self.parse_type())
        } else {
            None
        };
        let body = self.callable_body();
        let span = Span::new(start, body.span.end);
        Handler {
            is_reply,
            name,
            parameters,
            reply_type,
            body,
            span,
        }
    }

    fn function(&mut self) -> Function {
        let start = self.bump().start;
        let name = self.name();
        let parameters = self.parameter_list();
        if parameters.len() > 64 {
            self.error_at("function parameter count exceeds 64", name.span);
        }
        let result = if self.at(TokenKind::Arrow) {
            self.bump();
            Some(self.parse_type())
        } else {
            None
        };
        let body = self.callable_body();
        let span = Span::new(start, body.span.end);
        Function {
            name,
            parameters,
            result,
            body,
            span,
        }
    }

    fn compound_assign_op(kind: TokenKind) -> Option<BinaryOperator> {
        match kind {
            TokenKind::PlusEqual => Some(BinaryOperator::Add),
            TokenKind::MinusEqual => Some(BinaryOperator::Subtract),
            TokenKind::StarEqual => Some(BinaryOperator::Multiply),
            TokenKind::SlashEqual => Some(BinaryOperator::Divide),
            TokenKind::PercentEqual => Some(BinaryOperator::Remainder),
            _ => None,
        }
    }

    fn extract_assign_target(&mut self, expr: Expr) -> Option<AssignTarget> {
        let span = expr.span;
        let mut fields = Vec::new();
        let mut cursor = expr;
        loop {
            match cursor.kind {
                ExprKind::Name(base) => {
                    fields.reverse();
                    return Some(AssignTarget { base, fields, span });
                }
                ExprKind::Field { value, name } => {
                    fields.push(name);
                    cursor = *value;
                }
                _ => {
                    self.error_at("invalid assignment target", span);
                    return None;
                }
            }
        }
    }

    fn finish_assign(&mut self, lhs: Expr) -> Statement {
        let operator = Self::compound_assign_op(self.current().kind);
        self.bump();
        let value = self.expression(0, false, true);
        if let Some(target) = self.extract_assign_target(lhs) {
            Statement::Assign {
                target,
                operator,
                value,
            }
        } else {
            Statement::Expr(value)
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
            .expect(TokenKind::LeftBrace, "expected '{' before block")
            .start;
        let mut statements = Vec::new();
        let mut tail = None;
        while !self.boundary() {
            if self.at(TokenKind::Semicolon) {
                self.bump();
                continue;
            }
            let previous = self.cursor;
            self.expression_nodes = 0;
            if self.at(TokenKind::While) {
                self.bump();
                let condition = self.expression(0, true, false);
                let body = self.block();
                statements.push(Statement::While { condition, body });
                if self.at(TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            } else if self.at(TokenKind::For) {
                let for_start = self.bump().start;
                let binding = self.name();
                self.expect(TokenKind::In, "expected 'in' in for loop");
                let first = self.expression(0, true, false);
                if self.at(TokenKind::DotDot) || self.at(TokenKind::DotDotEqual) {
                    let inclusive = self.at(TokenKind::DotDotEqual);
                    self.bump();
                    let end = self.expression(0, true, false);
                    let body = self.block();
                    let span = Span::new(for_start, body.span.end);
                    statements.push(Statement::ForRange {
                        binding,
                        start: first,
                        end,
                        inclusive,
                        body,
                        span,
                    });
                } else {
                    let body = self.block();
                    let span = Span::new(for_start, body.span.end);
                    statements.push(Statement::ForList {
                        binding,
                        list: first,
                        body,
                        span,
                    });
                }
                if self.at(TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            } else if self.at(TokenKind::Let) {
                self.bump();
                let name = self.name();
                let ty = if self.at(TokenKind::Colon) {
                    self.bump();
                    Some(self.parse_type())
                } else {
                    None
                };
                self.expect(TokenKind::Equal, "expected '=' in let binding");
                let value = self.expression(0, false, true);
                statements.push(Statement::Let { name, ty, value });
            } else {
                let value = self.expression(0, false, true);
                if !self.line_break()
                    && (self.at(TokenKind::Equal)
                        || Self::compound_assign_op(self.current().kind).is_some())
                {
                    let stmt = self.finish_assign(value);
                    statements.push(stmt);
                } else if self.boundary() {
                    tail = Some(Box::new(value));
                    break;
                } else {
                    let is_block_like =
                        matches!(value.kind, ExprKind::If { .. } | ExprKind::Match { .. });
                    statements.push(Statement::Expr(value));
                    if is_block_like {
                        if self.at(TokenKind::Semicolon) {
                            self.bump();
                        }
                        continue;
                    }
                }
            }
            if self.cursor == previous {
                self.bump();
            }
            self.separator();
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after block")
            .end;
        self.block_depth -= 1;
        Block {
            statements,
            tail,
            span: Span::new(start, end),
        }
    }

    fn arguments(&mut self) -> (Vec<Expr>, Span) {
        self.bump();
        let mut arguments = Vec::new();
        while !self.at(TokenKind::RightParen) && !self.boundary() {
            let previous = self.cursor;
            arguments.push(self.expression(0, true, true));
            if self.cursor == previous || self.expression_nodes > 256 {
                break;
            }
            if !self.at(TokenKind::Comma) {
                break;
            }
            self.bump();
        }
        let end = self.expect(TokenKind::RightParen, "expected ')' after arguments");
        (arguments, end)
    }

    fn ask_call(&mut self, receiver: Expr) -> Expr {
        let start = receiver.span.start;
        self.bump(); // '('
        let message = self.name();
        let arguments = if self.at(TokenKind::LeftParen) && !self.line_break() {
            self.arguments().0
        } else {
            Vec::new()
        };
        let mut timeout: Option<Expr> = None;
        let mut then: Option<Name> = None;
        while self.at(TokenKind::Comma) && !self.boundary() {
            self.bump();
            if self.at(TokenKind::RightParen) {
                break;
            }
            if self.at(TokenKind::Identifier) && self.next_is(TokenKind::Colon) {
                let label = self.name();
                self.bump(); // ':'
                match label.text.as_str() {
                    "timeout" => {
                        if timeout.is_some() {
                            self.error_at("duplicate 'timeout' argument in ask", label.span);
                        }
                        timeout = Some(self.expression(0, true, true));
                    }
                    "then" => {
                        if then.is_some() {
                            self.error_at("duplicate 'then' argument in ask", label.span);
                        }
                        then = Some(self.name());
                    }
                    _ => {
                        self.error_at(
                            "unknown ask option; expected 'timeout' or 'then'",
                            label.span,
                        );
                        self.expression(0, true, true);
                    }
                }
            } else if timeout.is_none() {
                timeout = Some(self.expression(0, true, true));
            } else if then.is_none() {
                then = Some(self.name());
            } else {
                self.error("unexpected extra argument in ask");
                self.expression(0, true, true);
            }
        }
        let close = self.expect(TokenKind::RightParen, "expected ')' after ask arguments");
        let span = Span::new(start, close.end);
        let timeout = timeout.unwrap_or_else(|| {
            self.error_at("ask requires 'timeout: <ms>'", span);
            Expr {
                kind: ExprKind::Error,
                span,
            }
        });
        let then = then.unwrap_or_else(|| {
            self.error_at("ask requires 'then: <reply_handler>'", span);
            Name {
                text: String::new(),
                span,
            }
        });
        Expr {
            kind: ExprKind::Ask {
                target: Box::new(receiver),
                message,
                arguments,
                timeout: Box::new(timeout),
                then,
            },
            span,
        }
    }

    fn expression(&mut self, minimum: u8, multiline: bool, allow_struct_init: bool) -> Expr {
        self.expression_nodes += 1;
        if self.expression_nodes > 256 {
            self.error("expression exceeds the 256-node frontend limit");
            return Expr {
                kind: ExprKind::Error,
                span: self.current().span,
            };
        }
        let mut left = self.primary(multiline, allow_struct_init);
        loop {
            if self.expression_nodes > 256 || (!multiline && self.line_break()) {
                break;
            }
            if self.at(TokenKind::Dot) {
                self.bump();
                let name = self.name();
                if self.at(TokenKind::LeftParen) && !self.line_break() {
                    if name.text == "ask" {
                        left = self.ask_call(left);
                    } else {
                        let (arguments, close) = self.arguments();
                        let span = Span::new(left.span.start, close.end);
                        left = Expr {
                            kind: ExprKind::MethodCall {
                                receiver: Box::new(left),
                                method: name,
                                arguments,
                            },
                            span,
                        };
                    }
                } else {
                    let span = Span::new(left.span.start, name.span.end);
                    left = Expr {
                        kind: ExprKind::Field {
                            value: Box::new(left),
                            name,
                        },
                        span,
                    };
                }
                self.expression_nodes += 1;
                continue;
            }
            if self.at(TokenKind::PipeGreater) {
                if 1 < minimum {
                    break;
                }
                self.bump();
                let right = self.expression(2, multiline, allow_struct_init);
                let span = Span::new(left.span.start, right.span.end);
                left = match right.kind {
                    ExprKind::Name(callee) => Expr {
                        kind: ExprKind::Call {
                            callee,
                            arguments: vec![left],
                        },
                        span,
                    },
                    ExprKind::Call {
                        callee,
                        mut arguments,
                    } => {
                        arguments.insert(0, left);
                        Expr {
                            kind: ExprKind::Call { callee, arguments },
                            span,
                        }
                    }
                    ExprKind::Error => Expr {
                        kind: ExprKind::Error,
                        span,
                    },
                    _ => {
                        self.error_at("expected function name or call after '|>'", right.span);
                        Expr {
                            kind: ExprKind::Error,
                            span,
                        }
                    }
                };
                continue;
            }
            if self.at(TokenKind::Bang) {
                if 2 < minimum {
                    break;
                }
                self.bump();
                let message = self.name();
                let (arguments, end) = if self.at(TokenKind::LeftParen) && !self.line_break() {
                    let (args, close) = self.arguments();
                    (args, close.end)
                } else {
                    (Vec::new(), message.span.end)
                };
                let span = Span::new(left.span.start, end);
                left = Expr {
                    kind: ExprKind::Send {
                        target: Box::new(left),
                        message,
                        arguments,
                    },
                    span,
                };
                self.expression_nodes += 1;
                continue;
            }
            let (operator, precedence) = match self.current().kind {
                TokenKind::OrOr => (BinaryOperator::Or, 3),
                TokenKind::AndAnd => (BinaryOperator::And, 4),
                TokenKind::EqualEqual => (BinaryOperator::Equal, 5),
                TokenKind::NotEqual => (BinaryOperator::NotEqual, 5),
                TokenKind::Less => (BinaryOperator::Less, 5),
                TokenKind::LessEqual => (BinaryOperator::LessEqual, 5),
                TokenKind::Greater => (BinaryOperator::Greater, 5),
                TokenKind::GreaterEqual => (BinaryOperator::GreaterEqual, 5),
                TokenKind::Plus => (BinaryOperator::Add, 6),
                TokenKind::Minus => (BinaryOperator::Subtract, 6),
                TokenKind::Star => (BinaryOperator::Multiply, 7),
                TokenKind::Slash => (BinaryOperator::Divide, 7),
                TokenKind::Percent => (BinaryOperator::Remainder, 7),
                _ => break,
            };
            if precedence < minimum {
                break;
            }
            self.bump();
            let right = self.expression(precedence + 1, multiline, allow_struct_init);
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

    fn struct_init(&mut self, start: usize, name: Name) -> Expr {
        self.bump(); // '{'
        let mut base = None;
        let mut fields = Vec::new();
        while !self.at(TokenKind::RightBrace) && !self.boundary() {
            let previous = self.cursor;
            if self.at(TokenKind::DotDot) {
                self.bump();
                if base.is_some() {
                    self.error("duplicate '..' base in struct literal");
                }
                base = Some(Box::new(self.expression(0, true, true)));
                if self.at(TokenKind::Comma) {
                    self.bump();
                }
                break;
            } else {
                let field_name = self.name();
                let value = if self.at(TokenKind::Colon) {
                    self.bump();
                    self.expression(0, true, true)
                } else {
                    Expr {
                        span: field_name.span,
                        kind: ExprKind::Name(field_name.clone()),
                    }
                };
                fields.push((field_name, value));
            }
            if self.cursor == previous || self.expression_nodes > 256 {
                break;
            }
            if !self.at(TokenKind::Comma) {
                break;
            }
            self.bump();
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after struct literal")
            .end;
        Expr {
            kind: ExprKind::StructInit { name, base, fields },
            span: Span::new(start, end),
        }
    }

    fn list_literal(&mut self, start: usize) -> Expr {
        self.bump();
        let mut elements = Vec::new();
        let mut tail = None;
        while !self.at(TokenKind::RightBracket) && !self.boundary() {
            let previous = self.cursor;
            if self.at(TokenKind::DotDot) {
                self.bump();
                tail = Some(Box::new(self.expression(0, true, true)));
                if self.at(TokenKind::Comma) {
                    self.bump();
                }
                break;
            }
            elements.push(self.expression(0, true, true));
            if self.cursor == previous || self.expression_nodes > 256 {
                break;
            }
            if !self.at(TokenKind::Comma) {
                break;
            }
            self.bump();
        }
        let end = self
            .expect(TokenKind::RightBracket, "expected ']' after list literal")
            .end;
        Expr {
            kind: ExprKind::List { elements, tail },
            span: Span::new(start, end),
        }
    }

    fn match_pattern(&mut self) -> MatchPattern {
        let start = self.current().span.start;
        match self.current().kind {
            TokenKind::Identifier
                if &self.text[self.current().span.start..self.current().span.end] == "_" =>
            {
                MatchPattern::Wildcard(self.bump())
            }
            TokenKind::True | TokenKind::False => {
                let val = self.at(TokenKind::True);
                MatchPattern::Boolean(val, self.bump())
            }
            TokenKind::Integer => {
                let span = self.bump();
                match self.integer(&self.text[span.start..span.end], span) {
                    ExprKind::Integer(v) => MatchPattern::Integer(v, span),
                    _ => MatchPattern::Wildcard(span),
                }
            }
            TokenKind::Minus if self.next_is(TokenKind::Integer) => {
                let minus = self.bump();
                let num = self.bump();
                let span = Span::new(minus.start, num.end);
                match self.integer(&format!("-{}", &self.text[num.start..num.end]), span) {
                    ExprKind::Integer(v) => MatchPattern::Integer(v, span),
                    _ => MatchPattern::Wildcard(span),
                }
            }
            TokenKind::String => {
                let span = self.bump();
                match crate::string::decode(&self.text[span.start..span.end]) {
                    Ok(s) => MatchPattern::String(s, span),
                    Err(msg) => {
                        if self.diagnostics.len() < MAX_DIAGNOSTICS {
                            self.diagnostics.push(Diagnostic::error("S005", msg, span));
                        }
                        MatchPattern::Wildcard(span)
                    }
                }
            }
            TokenKind::LeftBracket => {
                self.bump();
                if self.at(TokenKind::RightBracket) {
                    let end = self.bump().end;
                    MatchPattern::EmptyList(Span::new(start, end))
                } else {
                    let head = self.name();
                    self.expect(TokenKind::Comma, "expected ',' after list head pattern");
                    self.expect(TokenKind::DotDot, "expected '..' before list tail pattern");
                    let tail = self.name();
                    if self.at(TokenKind::Comma) {
                        self.bump();
                    }
                    let end = self
                        .expect(TokenKind::RightBracket, "expected ']' after list pattern")
                        .end;
                    MatchPattern::ConsList {
                        head,
                        tail,
                        span: Span::new(start, end),
                    }
                }
            }
            _ => {
                self.error("expected a match pattern ('_', literal, '[]', or '[head, ..tail]')");
                let span = self.bump();
                MatchPattern::Wildcard(span)
            }
        }
    }

    fn match_expr(&mut self, start: usize) -> Expr {
        self.bump(); // 'match'
        let scrutinee = self.expression(0, true, false);
        self.expect(TokenKind::LeftBrace, "expected '{' after match scrutinee");
        let mut arms = Vec::new();
        while !self.at(TokenKind::RightBrace) && !self.boundary() {
            if self.at(TokenKind::Comma) || self.at(TokenKind::Semicolon) {
                self.bump();
                continue;
            }
            let arm_start = self.current().span.start;
            let previous = self.cursor;
            let pattern = self.match_pattern();
            let arrow = self.expect(TokenKind::FatArrow, "expected '=>' after match pattern");
            let body = if self.at(TokenKind::LeftBrace) {
                self.block()
            } else {
                let value = self.expression(0, false, true);
                if !self.line_break()
                    && (self.at(TokenKind::Equal)
                        || Self::compound_assign_op(self.current().kind).is_some())
                {
                    let stmt = self.finish_assign(value);
                    let end = match &stmt {
                        Statement::Assign { value, .. } => value.span.end.max(arrow.end),
                        _ => arrow.end,
                    };
                    Block {
                        statements: vec![stmt],
                        tail: None,
                        span: Span::new(arrow.start, end),
                    }
                } else {
                    let end = value.span.end.max(arrow.end);
                    Block {
                        statements: Vec::new(),
                        tail: Some(Box::new(value)),
                        span: Span::new(arrow.start, end),
                    }
                }
            };
            let arm_span = Span::new(arm_start, body.span.end);
            arms.push(MatchArm {
                pattern,
                body,
                span: arm_span,
            });
            if self.cursor == previous || self.expression_nodes > 256 {
                break;
            }
            if self.at(TokenKind::Comma) || self.at(TokenKind::Semicolon) {
                self.bump();
            } else if !self.line_break() && !self.at(TokenKind::RightBrace) {
                self.error("expected ',' or newline between match arms");
                break;
            }
        }
        let end = self
            .expect(TokenKind::RightBrace, "expected '}' after match arms")
            .end;
        Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            span: Span::new(start, end),
        }
    }

    fn parse_string_or_interp(&mut self, span: Span, is_fstring: bool) -> ExprKind {
        let raw = &self.text[span.start..span.end];
        let segments = match crate::string::split_interpolated(raw, span.start, is_fstring) {
            Ok(None) => match crate::string::decode(raw) {
                Ok(value) => return ExprKind::String(value),
                Err(message) => {
                    if self.diagnostics.len() < MAX_DIAGNOSTICS {
                        self.diagnostics
                            .push(Diagnostic::error("S005", message, span));
                    }
                    return ExprKind::Error;
                }
            },
            Ok(Some(segments)) => segments,
            Err((message, err_span)) => {
                if self.diagnostics.len() < MAX_DIAGNOSTICS {
                    self.diagnostics
                        .push(Diagnostic::error("S005", message, err_span));
                }
                return ExprKind::Error;
            }
        };

        if segments.is_empty() {
            return ExprKind::String(String::new());
        }
        if segments.len() == 1
            && let RawSegment::Literal(lit) = &segments[0]
        {
            return ExprKind::String(lit.clone());
        }

        let mut parts = Vec::with_capacity(segments.len());
        for segment in segments {
            match segment {
                RawSegment::Literal(lit) => parts.push(InterpolatedPart::Literal(lit)),
                RawSegment::Expr {
                    slice,
                    offset,
                    span: hole_span,
                } => {
                    let (mut sub_tokens, mut sub_diags) = crate::lex(slice);
                    for tok in &mut sub_tokens {
                        tok.span = Span::new(tok.span.start + offset, tok.span.end + offset);
                    }
                    for diag in &mut sub_diags {
                        diag.span = Span::new(diag.span.start + offset, diag.span.end + offset);
                        if self.diagnostics.len() < MAX_DIAGNOSTICS {
                            self.diagnostics.push(diag.clone());
                        }
                    }
                    let refs: Vec<&Token> = sub_tokens
                        .iter()
                        .filter(|tok| !tok.kind.is_trivia())
                        .collect();
                    let mut sub = Parser {
                        text: self.text,
                        tokens: &refs,
                        cursor: 0,
                        expression_nodes: self.expression_nodes,
                        block_depth: self.block_depth,
                        diagnostics: Vec::new(),
                    };
                    let expr = sub.expression(0, true, true);
                    if !sub.at(TokenKind::Eof) {
                        sub.error_at("unexpected tokens after interpolated expression", hole_span);
                    }
                    self.expression_nodes = sub.expression_nodes;
                    for diag in sub.diagnostics {
                        if self.diagnostics.len() < MAX_DIAGNOSTICS {
                            self.diagnostics.push(diag);
                        }
                    }
                    parts.push(InterpolatedPart::Expr(expr));
                }
            }
        }
        ExprKind::InterpolatedString(parts)
    }

    fn if_expr(&mut self, start: usize) -> Expr {
        self.bump(); // 'if'
        let condition = self.expression(0, true, false);
        let then_branch = self.block();
        let (else_branch, end) = if self.at(TokenKind::Else) {
            self.bump();
            if self.at(TokenKind::If) {
                self.expression_nodes += 1;
                if self.expression_nodes > 256 {
                    self.error("expression exceeds the 256-node frontend limit");
                    let err_span = self.current().span;
                    (
                        Some(Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(Expr {
                                kind: ExprKind::Error,
                                span: err_span,
                            })),
                            span: err_span,
                        }),
                        err_span.end,
                    )
                } else {
                    let nested = self.if_expr(self.current().span.start);
                    let nested_span = nested.span;
                    (
                        Some(Block {
                            statements: Vec::new(),
                            tail: Some(Box::new(nested)),
                            span: nested_span,
                        }),
                        nested_span.end,
                    )
                }
            } else {
                let block = self.block();
                let block_end = block.span.end;
                (Some(block), block_end)
            }
        } else {
            (None, then_branch.span.end)
        };
        Expr {
            kind: ExprKind::If {
                condition: Box::new(condition),
                then_branch,
                else_branch,
            },
            span: Span::new(start, end),
        }
    }

    fn primary(&mut self, multiline: bool, allow_struct_init: bool) -> Expr {
        let start = self.current().span.start;
        let mut span = self.current().span;
        let kind = match self.current().kind {
            TokenKind::If => return self.if_expr(start),
            TokenKind::Match => return self.match_expr(start),
            TokenKind::LeftBracket => return self.list_literal(start),
            TokenKind::SelfKw => {
                self.bump();
                ExprKind::SelfPid
            }
            TokenKind::Spawn => {
                self.bump();
                self.expect(TokenKind::LeftParen, "expected '(' after 'spawn'");
                let target = if !self.at(TokenKind::RightParen) && !self.boundary() {
                    Some(self.name())
                } else {
                    None
                };
                let end = self
                    .expect(TokenKind::RightParen, "expected ')' after 'spawn('")
                    .end;
                span = Span::new(start, end);
                ExprKind::Spawn(target)
            }
            TokenKind::Integer => {
                self.bump();
                self.integer(&self.text[span.start..span.end], span)
            }
            TokenKind::String => {
                self.bump();
                self.parse_string_or_interp(span, false)
            }
            TokenKind::FormatString => {
                self.bump();
                self.parse_string_or_interp(span, true)
            }
            TokenKind::Bytes => {
                self.bump();
                match crate::string::decode_bytes(&self.text[span.start..span.end]) {
                    Ok(bytes) => ExprKind::Bytes(bytes),
                    Err(message) => {
                        if self.diagnostics.len() < MAX_DIAGNOSTICS {
                            self.diagnostics
                                .push(Diagnostic::error("S005", message, span));
                        }
                        ExprKind::Error
                    }
                }
            }
            TokenKind::Bang => {
                self.bump();
                let value = self.expression(8, multiline, allow_struct_init);
                span = Span::new(start, value.span.end);
                ExprKind::Not(Box::new(value))
            }
            TokenKind::Minus => {
                self.bump();
                if self.at(TokenKind::Integer) && !self.next_is(TokenKind::Dot) {
                    let number = self.bump();
                    span = Span::new(start, number.end);
                    self.integer(&format!("-{}", &self.text[number.start..number.end]), span)
                } else {
                    let value = self.expression(8, multiline, allow_struct_init);
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
                if self.at(TokenKind::ColonColon) && !self.line_break() {
                    self.bump();
                    let method = self.name();
                    let (arguments, close) = if self.at(TokenKind::LeftParen) {
                        self.arguments()
                    } else {
                        self.error("expected '(' after static path");
                        (Vec::new(), method.span)
                    };
                    span = Span::new(start, close.end);
                    ExprKind::StaticCall {
                        namespace: callee,
                        method,
                        arguments,
                    }
                } else if self.at(TokenKind::LeftParen) && !self.line_break() {
                    let (arguments, close) = self.arguments();
                    span = Span::new(start, close.end);
                    ExprKind::Call { callee, arguments }
                } else if allow_struct_init
                    && self.at(TokenKind::LeftBrace)
                    && !self.line_break()
                    && callee
                        .text
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_uppercase)
                {
                    return self.struct_init(start, callee);
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
                    let value = self.expression(0, true, true);
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
                        TokenKind::RightParen
                            | TokenKind::RightBracket
                            | TokenKind::Semicolon
                            | TokenKind::Comma
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
        let parsed = if text.contains('_') {
            let cleaned: String = text.chars().filter(|&ch| ch != '_').collect();
            cleaned.parse::<i64>()
        } else {
            text.parse::<i64>()
        };
        match parsed {
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
