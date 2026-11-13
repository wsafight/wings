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
    IntList,
    Pid,
    RequestId,
    Function {
        parameters: Vec<(String, Type)>,
        result: Box<Type>,
    },
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
            Self::IntList => formatter.write_str("IntList"),
            Self::Pid => formatter.write_str("Pid"),
            Self::RequestId => formatter.write_str("RequestId"),
            Self::Function { parameters, result } => {
                formatter.write_str("(")?;
                for (index, (name, ty)) in parameters.iter().enumerate() {
                    if index != 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{name}: {ty}")?;
                }
                write!(formatter, ") -> {result}")
            }
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
    Function,
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
        functions: HashMap::new(),
        in_function: false,
        function_depth: 1,
        handlers: HashMap::new(),
        effects_allowed: false,
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
        checker.functions.clear();
        checker.handlers.clear();
        checker.effects_allowed = false;
        for function in &process.functions {
            let parameters: Vec<_> = function
                .parameters
                .iter()
                .map(|parameter| {
                    (
                        parameter.name.text.clone(),
                        checker.declared_type(&parameter.ty),
                    )
                })
                .collect();
            let result = checker.declared_type(&function.result);
            if matches!(
                function.name.text.as_str(),
                "record_int"
                    | "read_int"
                    | "collect"
                    | "empty_ints"
                    | "prepend_int"
                    | "head_int"
                    | "tail_ints"
                    | "is_empty_ints"
                    | "send_self"
                    | "self_pid"
                    | "spawn_self"
                    | "send"
                    | "request"
                    | "cancel_request"
            ) || checker.functions.contains_key(&function.name.text)
            {
                checker.error(
                    "T004",
                    "duplicate or reserved function name",
                    function.name.span,
                );
                continue;
            }
            let mut scope = HashMap::new();
            for (parameter, (_, ty)) in function.parameters.iter().zip(&parameters) {
                checker.bind(
                    &parameter.name,
                    SymbolKind::Parameter,
                    ty.clone(),
                    function.body.span,
                    &mut scope,
                );
            }
            checker.in_function = true;
            checker.function_depth = 1;
            let actual = checker.block(&function.body, &mut scope);
            checker.in_function = false;
            checker.require(
                &actual,
                &result,
                function
                    .body
                    .tail
                    .as_ref()
                    .map_or(function.body.span, |tail| tail.span),
            );
            let depth = checker.function_depth;
            if depth > 32 {
                checker.error("T017", "function call chain exceeds 32", function.name.span);
            }
            let symbol = checker.symbols.len();
            checker.symbols.push(Symbol {
                name: function.name.text.clone(),
                kind: SymbolKind::Function,
                ty: Type::Function {
                    parameters,
                    result: Box::new(result),
                },
                span: function.name.span,
                scope: process.span,
            });
            checker
                .functions
                .insert(function.name.text.clone(), (symbol, depth));
        }
        let mut state_scope = HashMap::new();
        for state in &process.states {
            let declared = checker.declared_type(&state.ty);
            if matches!(declared, Type::Pid | Type::RequestId) {
                checker.error(
                    "T018",
                    "Pid/RequestId state is not supported",
                    state.ty.span,
                );
            }
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
                        if !matches!(ty, Type::RequestId | Type::RecordInt) {
                            checker.external_type(&ty, parameter.ty.span);
                        }
                        ty
                    })
                })
                .collect();
            if parameters.iter().any(|(_, ty)| *ty == Type::RequestId)
                && !(handler.reply_type.is_none()
                    && parameters.iter().map(|(_, ty)| ty).eq([
                        &Type::RequestId,
                        &Type::Int,
                        &Type::Int,
                    ]))
            {
                checker.error(
                    "T011",
                    "RequestId is only supported in completion event signatures",
                    handler.name.span,
                );
            }
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
            if let Some(&symbol) = handler_names.get(&handler.name.text) {
                checker.handlers.insert(handler.name.text.clone(), symbol);
            }
        }
        checker.effects_allowed = true;
        for handler in &process.handlers {
            let parameters: Vec<_> = handler
                .parameters
                .iter()
                .map(|parameter| checker.declared_type(&parameter.ty))
                .collect();
            let reply = handler
                .reply_type
                .as_ref()
                .map(|name| checker.declared_type(name));
            let mut scope = state_scope.clone();
            for (parameter, ty) in handler.parameters.iter().zip(parameters) {
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
    functions: HashMap<String, (usize, usize)>,
    in_function: bool,
    function_depth: usize,
    handlers: HashMap<String, usize>,
    effects_allowed: bool,
}

impl Checker {
    fn external_type(&mut self, ty: &Type, span: Span) {
        if matches!(
            ty,
            Type::RecordInt | Type::IntList | Type::Pid | Type::RequestId
        ) {
            self.error(
                "T011",
                format!("{ty} is not supported in message fields or replies"),
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
            "IntList" => Type::IntList,
            "Pid" => Type::Pid,
            "RequestId" => Type::RequestId,
            "" => Type::Unknown,
            _ => {
                self.error(
                    "T002",
                    format!(
                        "unknown type '{}'; supported types are Int, Bool, Unit, RecordInt and IntList",
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
                Statement::While { condition, body } => {
                    if self.in_function {
                        self.error(
                            "T017",
                            "while is not supported in functions",
                            condition.span,
                        );
                    }
                    let condition_type = self.expression(condition, scope);
                    self.require(&condition_type, &Type::Bool, condition.span);
                    let result = self.block(body, &mut scope.clone());
                    self.require(&result, &Type::Unit, body.span);
                }
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
                if callee.text == "request" {
                    if !self.effects_allowed {
                        self.error(
                            "T018",
                            "request is only supported in handlers",
                            expression.span,
                        );
                    }
                    if scope.contains_key(&callee.text) {
                        self.error("T012", "bindings are not callable", callee.span);
                    }
                    if arguments.len() < 4 {
                        self.error(
                            "T013",
                            "request requires pid, message, completion and timeout",
                            expression.span,
                        );
                        return Type::RequestId;
                    }
                    let actual = self.expression(&arguments[0], scope);
                    self.require(&actual, &Type::Pid, arguments[0].span);
                    let target = self.message_target(&arguments[1], scope);
                    let callback = self.message_target(&arguments[2], scope);
                    let timeout = self.expression(&arguments[3], scope);
                    self.require(&timeout, &Type::Int, arguments[3].span);
                    let types: Vec<_> = arguments[4..]
                        .iter()
                        .map(|argument| self.expression(argument, scope))
                        .collect();
                    if let Some(Type::Message { parameters, reply }) = target {
                        if reply.as_deref() != Some(&Type::Int) {
                            self.error(
                                "T018",
                                "request requires an Int reply handler",
                                arguments[1].span,
                            );
                        }
                        if parameters.len() != types.len() {
                            self.error("T013", "request argument count mismatch", expression.span);
                        }
                        for ((actual, (_, expected)), value) in
                            types.iter().zip(&parameters).zip(&arguments[4..])
                        {
                            self.require(actual, expected, value.span);
                        }
                    }
                    if !matches!(callback, Some(Type::Message { ref parameters, reply: None }) if parameters.iter().map(|(_, ty)| ty).eq([&Type::RequestId, &Type::Int, &Type::Int]))
                    {
                        self.error(
                            "T018",
                            "request completion requires one-way (RequestId, Int, Int)",
                            arguments[2].span,
                        );
                    }
                    return Type::RequestId;
                }
                if callee.text == "cancel_request" {
                    if !self.effects_allowed {
                        self.error(
                            "T018",
                            "cancel_request is only supported in handlers",
                            expression.span,
                        );
                    }
                    if scope.contains_key(&callee.text) {
                        self.error("T012", "bindings are not callable", callee.span);
                    }
                    if arguments.len() != 1 {
                        self.error(
                            "T013",
                            "cancel_request requires one RequestId",
                            expression.span,
                        );
                    }
                    for argument in arguments {
                        let actual = self.expression(argument, scope);
                        self.require(&actual, &Type::RequestId, argument.span);
                    }
                    return Type::Unit;
                }
                if matches!(callee.text.as_str(), "self_pid" | "spawn_self") {
                    if !self.effects_allowed {
                        self.error(
                            "T018",
                            "process effects are only supported in handlers",
                            expression.span,
                        );
                    }
                    if scope.contains_key(&callee.text) {
                        self.error("T012", "bindings are not callable", callee.span);
                    }
                    for argument in arguments {
                        self.expression(argument, scope);
                    }
                    if !arguments.is_empty() {
                        self.error(
                            "T013",
                            "process identity or creation expects no arguments",
                            expression.span,
                        );
                    }
                    return Type::Pid;
                }
                if matches!(callee.text.as_str(), "send_self" | "send") {
                    if !self.effects_allowed {
                        self.error(
                            "T018",
                            "send effects are only supported in handlers",
                            expression.span,
                        );
                    }
                    if scope.contains_key(&callee.text) {
                        self.error("T012", "bindings are not callable", callee.span);
                    }
                    let message_arguments = if callee.text == "send" {
                        if let Some((pid, rest)) = arguments.split_first() {
                            let actual = self.expression(pid, scope);
                            self.require(&actual, &Type::Pid, pid.span);
                            rest
                        } else {
                            &[]
                        }
                    } else {
                        arguments.as_slice()
                    };
                    let Some((target, values)) = message_arguments.split_first() else {
                        self.error(
                            "T013",
                            "send_self requires a static message name",
                            expression.span,
                        );
                        return Type::Unit;
                    };
                    let ExprKind::Name(name) = &target.kind else {
                        self.error(
                            "T018",
                            "send_self requires a static message name",
                            target.span,
                        );
                        return Type::Unit;
                    };
                    if scope.contains_key(&name.text) {
                        self.error(
                            "T018",
                            "send_self target is shadowed by a binding",
                            name.span,
                        );
                    }
                    let types: Vec<_> = values
                        .iter()
                        .map(|value| self.expression(value, scope))
                        .collect();
                    let Some(&symbol) = self.handlers.get(&name.text) else {
                        self.error("T018", "unknown self-send message", name.span);
                        return Type::Unit;
                    };
                    self.references.push(Reference {
                        span: name.span,
                        symbol,
                    });
                    let Type::Message { parameters, reply } = self.symbols[symbol].ty.clone()
                    else {
                        unreachable!()
                    };
                    if reply.is_some() {
                        self.error("T018", "send_self requires a one-way handler", name.span);
                    }
                    if parameters.iter().any(|(_, ty)| *ty == Type::RequestId) {
                        self.error(
                            "T018",
                            "completion events cannot be sent directly",
                            name.span,
                        );
                    }
                    if types.len() != parameters.len() {
                        self.error("T013", "send_self argument count mismatch", expression.span);
                    }
                    for ((actual, (_, expected)), value) in
                        types.iter().zip(&parameters).zip(values)
                    {
                        self.require(actual, expected, value.span);
                    }
                    return Type::Unit;
                }
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
                if let Some(&(symbol, depth)) = self.functions.get(&callee.text) {
                    self.references.push(Reference {
                        span: callee.span,
                        symbol,
                    });
                    self.function_depth = self.function_depth.max(depth + 1);
                    let Type::Function { parameters, result } = self.symbols[symbol].ty.clone()
                    else {
                        unreachable!()
                    };
                    if types.len() != parameters.len() {
                        self.error(
                            "T013",
                            format!(
                                "{} expects {} argument(s), found {}",
                                callee.text,
                                parameters.len(),
                                types.len()
                            ),
                            expression.span,
                        );
                    }
                    for ((actual, (_, expected)), argument) in
                        types.iter().zip(&parameters).zip(arguments)
                    {
                        self.require(actual, expected, argument.span);
                    }
                    return *result;
                }
                let (expected, result) = match callee.text.as_str() {
                    "record_int" => (vec![Type::Int], Type::RecordInt),
                    "read_int" => (vec![Type::RecordInt], Type::Int),
                    "collect" => (vec![], Type::Unit),
                    "empty_ints" => (vec![], Type::IntList),
                    "prepend_int" => (vec![Type::Int, Type::IntList], Type::IntList),
                    "head_int" => (vec![Type::IntList], Type::Int),
                    "tail_ints" => (vec![Type::IntList], Type::IntList),
                    "is_empty_ints" => (vec![Type::IntList], Type::Bool),
                    _ => {
                        self.error(
                            "T012",
                            format!("unsupported builtin '{}'", callee.text),
                            callee.span,
                        );
                        return Type::Unknown;
                    }
                };
                let count = expected.len();
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
                for ((actual, expected), argument) in types.iter().zip(&expected).zip(arguments) {
                    self.require(actual, expected, argument.span);
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
                    if matches!(
                        left_type,
                        Type::RecordInt | Type::IntList | Type::Pid | Type::RequestId
                    ) || matches!(
                        right_type,
                        Type::RecordInt | Type::IntList | Type::Pid | Type::RequestId
                    ) {
                        self.error(
                            "T015",
                            "record references and process capabilities cannot be compared",
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

    fn message_target(
        &mut self,
        expression: &Expr,
        scope: &HashMap<String, usize>,
    ) -> Option<Type> {
        let ExprKind::Name(name) = &expression.kind else {
            self.error("T018", "expected static message name", expression.span);
            return None;
        };
        if scope.contains_key(&name.text) {
            self.error("T018", "message target is shadowed", name.span);
        }
        let Some(&symbol) = self.handlers.get(&name.text) else {
            self.error("T018", "unknown request message", name.span);
            return None;
        };
        self.references.push(Reference {
            span: name.span,
            symbol,
        });
        Some(self.symbols[symbol].ty.clone())
    }
}
