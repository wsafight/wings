use std::collections::HashMap;

use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};
use wings_syntax::ast::{Block, Expr, ExprKind, Name, Statement};
use wings_syntax::{Parse, parse};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Unit,
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
                    (
                        parameter.name.text.clone(),
                        checker.declared_type(&parameter.ty),
                    )
                })
                .collect();
            let reply = handler
                .reply_type
                .as_ref()
                .map(|ty| checker.declared_type(ty));
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
            "" => Type::Unknown,
            _ => {
                self.error(
                    "T002",
                    format!(
                        "unknown type '{}'; supported types are Int, Bool and Unit",
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
            ExprKind::Negate(value) => {
                let actual = self.expression(value, scope);
                self.require(&actual, &Type::Int, value.span);
                Type::Int
            }
            ExprKind::Binary { left, right, .. } => {
                let left_type = self.expression(left, scope);
                let right_type = self.expression(right, scope);
                self.require(&left_type, &Type::Int, left.span);
                self.require(&right_type, &Type::Int, right.span);
                Type::Int
            }
            ExprKind::Error => Type::Unknown,
        }
    }
}
