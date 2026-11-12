use std::collections::HashMap;

use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};
use wings_syntax::ast::{BinaryOperator, Block, Expr, ExprKind, Name, Statement};
use wings_syntax::{Parse, parse};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Unit,
    RecordInt,
    Message {
        parameters: Vec<(String, Type)>,
        reply: Option<Box<Type>>,
    },
    Unknown,
}

impl std::fmt::Display for Type {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int => formatter.write_str("Int"),
            Self::Bool => formatter.write_str("Bool"),
            Self::Unit => formatter.write_str("Unit"),
            Self::RecordInt => formatter.write_str("RecordInt"),
            Self::Message { parameters, reply } => {
                formatter.write_str("(")?;
                for (index, (name, ty)) in parameters.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{name}: {ty}")?;
                }
                formatter.write_str(")")?;
                if let Some(ty) = reply {
                    write!(formatter, " -> {ty}")
                } else {
                    formatter.write_str(" (one-way)")
                }
            }
            Self::Unknown => formatter.write_str("unknown"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Process,
    Handler,
    Parameter,
    State,
    Local,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub ty: Type,
    pub span: Span,
    pub scope: Span,
}

#[derive(Clone, Debug)]
pub struct Reference {
    pub span: Span,
    pub symbol: usize,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub parsed: Parse,
    pub diagnostics: Vec<Diagnostic>,
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
}

impl Analysis {
    pub fn symbol_at(&self, offset: usize) -> Option<&Symbol> {
        if let Some(reference) = self
            .references
            .iter()
            .find(|reference| reference.span.contains(offset))
        {
            return self.symbols.get(reference.symbol);
        }
        self.symbols
            .iter()
            .find(|symbol| symbol.span.contains(offset))
    }

    pub fn visible_symbols(&self, offset: usize) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().filter(move |symbol| {
            symbol.kind == SymbolKind::Process || symbol.scope.contains(offset)
        })
    }
}

pub fn analyze(text: &str) -> Analysis {
    let parsed = parse(text);
    let mut checker = Checker {
        diagnostics: parsed.diagnostics.clone(),
        symbols: Vec::new(),
        references: Vec::new(),
    };
    let mut process_names = HashMap::new();
    for process in &parsed.program.processes {
        checker.bind(
            &process.name,
            SymbolKind::Process,
            Type::Unknown,
            process.span,
            &mut process_names,
        );
        if process.states.len() != 1 {
            checker.error(
                "T001",
                "a process must declare exactly one state",
                process.name.span,
            );
        }
        let mut state_scope = HashMap::new();
        for state in &process.states {
            let declared = checker.declared_type(&state.ty);
            let actual = checker.expression(&state.default, &HashMap::new());
            checker.require(&actual, &declared, state.default.span);
            checker.bind(
                &state.name,
                SymbolKind::State,
                declared,
                process.span,
                &mut state_scope,
            );
        }
        let mut handler_names = HashMap::new();
        for handler in &process.handlers {
            if !handler.name.text.is_empty()
                && !handler
                    .name
                    .text
                    .chars()
                    .next()
                    .is_some_and(char::is_uppercase)
            {
                checker.error(
                    "T010",
                    "message names must start with an uppercase letter",
                    handler.name.span,
                );
            }
            let parameters: Vec<_> = handler
                .parameters
                .iter()
                .map(|parameter| {
                    (parameter.name.text.clone(), {
                        let ty = checker.declared_type(&parameter.ty);
                        checker.external_type(&ty, parameter.ty.span);
                        ty
                    })
                })
                .collect();
            let reply = handler.reply_type.as_ref().map(|name| {
                let ty = checker.declared_type(name);
                checker.external_type(&ty, name.span);
                ty
            });
            checker.bind(
                &handler.name,
                SymbolKind::Handler,
                Type::Message {
                    parameters: parameters.clone(),
                    reply: reply.clone().map(Box::new),
                },
                process.span,
                &mut handler_names,
            );
            let mut scope = state_scope.clone();
            for (parameter, (_, ty)) in handler.parameters.iter().zip(parameters) {
                checker.bind(
                    &parameter.name,
                    SymbolKind::Parameter,
                    ty,
                    handler.body.span,
                    &mut scope,
                );
            }
            let result = checker.block(&handler.body, &mut scope);
            let span = handler
                .body
                .tail
                .as_ref()
                .map_or(handler.body.span, |tail| tail.span);
            checker.require(&result, &reply.unwrap_or(Type::Unit), span);
        }
    }
    Analysis {
        parsed,
        diagnostics: checker.diagnostics,
        symbols: checker.symbols,
        references: checker.references,
    }
}

struct Checker {
    diagnostics: Vec<Diagnostic>,
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
}

impl Checker {
    fn external_type(&mut self, ty: &Type, span: Span) {
        if *ty == Type::RecordInt {
            self.error(
                "T011",
                "RecordInt is not supported in message fields or replies",
                span,
            );
        }
    }
    fn error(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics
                .push(Diagnostic::error(code, message, span));
        }
    }

    fn declared_type(&mut self, name: &Name) -> Type {
        match name.text.as_str() {
            "Int" => Type::Int,
            "Bool" => Type::Bool,
            "Unit" => Type::Unit,
            "RecordInt" => Type::RecordInt,
            "" => Type::Unknown,
            _ => {
                self.error(
                    "T002",
                    format!(
                        "unknown type '{}'; supported types are Int, Bool, Unit and RecordInt",
                        name.text
                    ),
                    name.span,
                );
                Type::Unknown
            }
        }
    }

    fn bind(
        &mut self,
        name: &Name,
        kind: SymbolKind,
        ty: Type,
        scope_span: Span,
        scope: &mut HashMap<String, usize>,
    ) {
        if name.text.is_empty() {
            return;
        }
        if scope.contains_key(&name.text) {
            self.error("T004", format!("duplicate name '{}'", name.text), name.span);
            return;
        }
        scope.insert(name.text.clone(), self.symbols.len());
        self.symbols.push(Symbol {
            name: name.text.clone(),
            kind,
            ty,
            span: name.span,
            scope: scope_span,
        });
    }

    fn resolve(&mut self, name: &Name, scope: &HashMap<String, usize>) -> Option<usize> {
        if let Some(&symbol) = scope.get(&name.text) {
            self.references.push(Reference {
                span: name.span,
                symbol,
            });
            Some(symbol)
        } else {
            self.error("T006", format!("unknown name '{}'", name.text), name.span);
            None
        }
    }

    fn require(&mut self, actual: &Type, expected: &Type, span: Span) {
        if !matches!(actual, Type::Unknown)
            && !matches!(expected, Type::Unknown)
            && actual != expected
        {
            self.error("T005", format!("expected {expected}, found {actual}"), span);
        }
    }

    fn block(&mut self, block: &Block, scope: &mut HashMap<String, usize>) -> Type {
        for statement in &block.statements {
            match statement {
                Statement::Assign { target, value } => {
                    let actual = self.expression(value, scope);
                    if let Some(symbol) = self.resolve(target, scope) {
                        if self.symbols[symbol].kind != SymbolKind::State {
                            self.error("T008", "only state bindings can be assigned", target.span);
                        } else {
                            self.require(&actual, &self.symbols[symbol].ty.clone(), value.span);
                        }
                    }
                }
                Statement::Let { name, ty, value } => {
                    let actual = self.expression(value, scope);
                    let declared = ty.as_ref().map(|ty| self.declared_type(ty));
                    if let Some(declared) = &declared {
                        self.require(&actual, declared, value.span);
                    }
                    self.bind(
                        name,
                        SymbolKind::Local,
                        declared.unwrap_or(actual),
                        Span::new(value.span.end, block.span.end),
                        scope,
                    );
                }
                Statement::Expr(value) => {
                    self.expression(value, scope);
                }
            }
        }
        block
            .tail
            .as_ref()
            .map_or(Type::Unit, |tail| self.expression(tail, scope))
    }

    fn expression(&mut self, expression: &Expr, scope: &HashMap<String, usize>) -> Type {
        match &expression.kind {
            ExprKind::Integer(_) => Type::Int,
            ExprKind::Boolean(_) => Type::Bool,
            ExprKind::Unit => Type::Unit,
            ExprKind::Name(name) => self
                .resolve(name, scope)
                .map_or(Type::Unknown, |symbol| self.symbols[symbol].ty.clone()),
            ExprKind::Call { callee, arguments } => {
                let types: Vec<Type> = arguments
                    .iter()
                    .map(|argument| self.expression(argument, scope))
                    .collect();
                if scope.contains_key(&callee.text) {
                    self.resolve(callee, scope);
                    self.error(
                        "T012",
                        "bindings are not callable; this name shadows a builtin",
                        callee.span,
                    );
                    return Type::Unknown;
                }
                let (expected, result) = match callee.text.as_str() {
                    "record_int" => (Some(Type::Int), Type::RecordInt),
                    "read_int" => (Some(Type::RecordInt), Type::Int),
                    "collect" => (None, Type::Unit),
                    _ => {
                        self.error(
                            "T012",
                            format!("unsupported builtin '{}'", callee.text),
                            callee.span,
                        );
                        return Type::Unknown;
                    }
                };
                let count = usize::from(expected.is_some());
                if arguments.len() != count {
                    self.error(
                        "T013",
                        format!(
                            "{} expects {count} argument(s), found {}",
                            callee.text,
                            arguments.len()
                        ),
                        expression.span,
                    );
                }
                if let (Some(expected), Some(actual), Some(argument)) =
                    (expected, types.first(), arguments.first())
                {
                    self.require(actual, &expected, argument.span);
                }
                result
            }
            ExprKind::Negate(value) => {
                let actual = self.expression(value, scope);
                self.require(&actual, &Type::Int, value.span);
                Type::Int
            }
            ExprKind::If {
                condition,
                then_value,
                else_value,
            } => {
                let condition_type = self.expression(condition, scope);
                self.require(&condition_type, &Type::Bool, condition.span);
                let then_type = self.expression(then_value, scope);
                let else_type = self.expression(else_value, scope);
                self.require(&else_type, &then_type, else_value.span);
                if then_type == Type::RecordInt || else_type == Type::RecordInt {
                    self.error(
                        "T014",
                        "if expressions currently require scalar results",
                        expression.span,
                    );
                }
                then_type
            }
            ExprKind::Binary {
                operator,
                left,
                right,
            } => {
                let left_type = self.expression(left, scope);
                let right_type = self.expression(right, scope);
                if matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual) {
                    self.require(&right_type, &left_type, right.span);
                    if left_type == Type::RecordInt || right_type == Type::RecordInt {
                        self.error(
                            "T015",
                            "record references cannot be compared",
                            expression.span,
                        );
                    }
                    return Type::Bool;
                }
                self.require(&left_type, &Type::Int, left.span);
                self.require(&right_type, &Type::Int, right.span);
                if matches!(
                    operator,
                    BinaryOperator::Less
                        | BinaryOperator::LessEqual
                        | BinaryOperator::Greater
                        | BinaryOperator::GreaterEqual
                ) {
                    Type::Bool
                } else {
                    Type::Int
                }
            }
            ExprKind::Error => Type::Unknown,
        }
    }
}
