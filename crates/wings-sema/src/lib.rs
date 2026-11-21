use std::collections::{HashMap, HashSet};

use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};
use wings_syntax::ast::{
    AssignTarget, BinaryOperator, Block, Expr, ExprKind, InterpolatedPart, MatchArm, MatchPattern,
    Name, Statement, TypeExpr, TypeKind,
};
use wings_syntax::{Parse, parse};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Unit,
    IntList,
    Bytes,
    String,
    Struct(String),
    Pid,
    ActorPid(String),
    NullPid,
    RequestId,
    Function {
        parameters: Vec<(String, Type)>,
        result: Box<Type>,
    },
    Message {
        parameters: Vec<(String, Type)>,
        reply: Option<Box<Type>>,
        is_reply: bool,
    },
    Unknown,
}

impl Type {
    pub fn is_capability(&self) -> bool {
        matches!(
            self,
            Self::Pid | Self::ActorPid(_) | Self::NullPid | Self::RequestId
        )
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int => formatter.write_str("int"),
            Self::Bool => formatter.write_str("bool"),
            Self::Unit => formatter.write_str("()"),
            Self::IntList => formatter.write_str("[int]"),
            Self::Bytes => formatter.write_str("bytes"),
            Self::String => formatter.write_str("str"),
            Self::Struct(name) => formatter.write_str(name),
            Self::Pid => formatter.write_str("pid"),
            Self::ActorPid(name) => write!(formatter, "pid<{name}>"),
            Self::NullPid => formatter.write_str("null"),
            Self::RequestId => formatter.write_str("req"),
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
            Self::Message {
                parameters,
                reply,
                is_reply,
            } => {
                formatter.write_str("(")?;
                for (index, (name, ty)) in parameters.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{name}: {ty}")?;
                }
                formatter.write_str(")")?;
                if *is_reply {
                    formatter.write_str(" (reply)")
                } else if let Some(ty) = reply {
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
    Actor,
    Struct,
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
    pub actor_states: Vec<Vec<(String, Type)>>,
    pub target_actors: HashMap<(usize, usize), String>,
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
        self.symbols
            .iter()
            .filter(move |symbol| symbol.kind == SymbolKind::Actor || symbol.scope.contains(offset))
    }
}

fn is_pascal_case(text: &str) -> bool {
    text.as_bytes().first().is_some_and(u8::is_ascii_uppercase) && !text.contains('_')
}

fn is_snake_case(text: &str) -> bool {
    !text.chars().any(|ch| ch.is_ascii_uppercase())
}

type HandlerTriple = (Vec<(String, Type, Span)>, Option<Type>);

fn check_acyclic_function_depths(edges: &[Vec<usize>]) -> Vec<Option<usize>> {
    // 0 = unvisited, 1 = visiting, 2 = done
    let n = edges.len();
    let mut state = vec![0u8; n];
    let mut memo: Vec<Option<usize>> = vec![None; n];

    fn dfs(
        u: usize,
        edges: &[Vec<usize>],
        state: &mut [u8],
        memo: &mut [Option<usize>],
    ) -> Option<usize> {
        if state[u] == 1 {
            return None;
        }
        if state[u] == 2 {
            return memo[u];
        }
        state[u] = 1;
        let mut max_depth = 1usize;
        let mut reaches_cycle = false;
        for &v in &edges[u] {
            match dfs(v, edges, state, memo) {
                Some(child_depth) => max_depth = max_depth.max(1 + child_depth),
                None => reaches_cycle = true,
            }
        }
        state[u] = 2;
        memo[u] = (!reaches_cycle).then_some(max_depth);
        memo[u]
    }

    for i in 0..n {
        if state[i] == 0 {
            dfs(i, edges, &mut state, &mut memo);
        }
    }
    memo
}

pub fn analyze(text: &str) -> Analysis {
    let parsed = parse(text);
    let file_span = Span::new(0, text.len());
    let mut checker = Checker {
        diagnostics: parsed.diagnostics.clone(),
        symbols: Vec::new(),
        references: Vec::new(),
        actor_symbols: HashMap::new(),
        actor_handlers: HashMap::new(),
        actor_structs: HashMap::new(),
        target_actors: HashMap::new(),
        current_actor: String::new(),
        functions: HashMap::new(),
        current_call_targets: Vec::new(),
        handlers: HashMap::new(),
        effects_allowed: false,
        structs: HashMap::new(),
    };
    let mut actor_names = HashMap::new();
    for actor in &parsed.program.actors {
        if !actor.name.text.is_empty() && !is_pascal_case(&actor.name.text) {
            checker.error("T020", "actor names must use PascalCase", actor.name.span);
        }
        let before = checker.symbols.len();
        checker.bind(
            &actor.name,
            SymbolKind::Actor,
            Type::ActorPid(actor.name.text.clone()),
            file_span,
            &mut actor_names,
        );
        if checker.symbols.len() != before {
            checker
                .actor_symbols
                .insert(actor.name.text.clone(), before);
        }
    }

    // Validate top-level shared structs
    checker.structs.clear();
    if parsed.program.structs.len() > 32 {
        checker.error("T019", "top-level struct count exceeds 32", file_span);
    }
    for decl in &parsed.program.structs {
        checker.register_struct_decl(decl, file_span);
    }
    let global_structs = checker.structs.clone();

    // Pre-pass: resolve per-actor structs and handler signatures across all actors
    let mut precomputed_handlers: Vec<(HashMap<String, usize>, Vec<HandlerTriple>)> =
        Vec::with_capacity(parsed.program.actors.len());

    for actor in &parsed.program.actors {
        checker.current_actor = actor.name.text.clone();
        checker.structs = global_structs.clone();

        let synthetic_state_schemas = usize::from(actor.states.len() >= 2);
        if actor.states.len() > 8 {
            checker.error(
                "T001",
                "an actor can declare at most 8 state bindings; group extra fields in a struct",
                actor.name.span,
            );
        }
        if global_structs.len() + actor.structs.len() + synthetic_state_schemas > 32 {
            checker.error(
                "T019",
                "actor struct schema count exceeds 32",
                actor.name.span,
            );
        }
        for decl in &actor.structs {
            checker.register_struct_decl(decl, actor.span);
        }
        checker
            .actor_structs
            .insert(actor.name.text.clone(), checker.structs.clone());

        let mut handler_names = HashMap::new();
        let mut handler_types = Vec::new();
        let mut handler_map = HashMap::new();
        for handler in &actor.handlers {
            if !handler.name.text.is_empty() && !is_snake_case(&handler.name.text) {
                checker.error(
                    "T020",
                    "handler names must use snake_case",
                    handler.name.span,
                );
            }
            let mut parameters = Vec::new();
            if handler.parameters.len() > 64 {
                checker.error(
                    "T016",
                    "too many parameters: message exceeds 64 parameters",
                    handler.name.span,
                );
            }
            for parameter in &handler.parameters {
                if !parameter.name.text.is_empty() && !is_snake_case(&parameter.name.text) {
                    checker.error(
                        "T020",
                        "parameter names must use snake_case",
                        parameter.name.span,
                    );
                }
                parameters.push((
                    parameter.name.text.clone(),
                    checker.resolve_type(&parameter.ty),
                    parameter.ty.span,
                ));
            }
            let reply = handler
                .reply_type
                .as_ref()
                .map(|ty| (checker.resolve_type(ty), ty.span));

            if handler.is_reply {
                if let Some((_, span)) = &reply {
                    checker.error(
                        "T018",
                        "'on reply' completion handlers cannot return a reply value",
                        *span,
                    );
                }
                let valid_sig = parameters.len() == 3
                    && parameters[0].1 == Type::RequestId
                    && parameters[1].1 == Type::Int
                    && matches!(
                        parameters[2].1,
                        Type::Int
                            | Type::Bool
                            | Type::Unit
                            | Type::IntList
                            | Type::Bytes
                            | Type::String
                            | Type::Struct(_)
                            | Type::Pid
                            | Type::ActorPid(_)
                    );
                if !valid_sig {
                    checker.error(
                        "T018",
                        "'on reply' requires (id: req, status: int, value: <reply_type>)",
                        handler.name.span,
                    );
                } else if let Type::Struct(name) = &parameters[2].1
                    && !checker.expandable_struct(name)
                {
                    checker.error(
                        "T011",
                        "struct completion expansion exceeds 64 total nodes",
                        parameters[2].2,
                    );
                }
            } else {
                for (_, ty, span) in &parameters {
                    if *ty == Type::RequestId {
                        checker.error(
                            "T011",
                            format!(
                                "{ty} is not supported in message parameters (use 'on reply' for request completions)"
                            ),
                            *span,
                        );
                    }
                }
                let mut total_nodes = 0usize;
                for (_, ty, span) in &parameters {
                    if let Some(nodes) = checker.message_nodes(ty) {
                        total_nodes += nodes;
                        if total_nodes > 64 {
                            checker.error(
                                "T016",
                                "message struct expansion exceeds 64 total nodes",
                                *span,
                            );
                            break;
                        }
                    }
                }
            }

            if let Some((ref ty, span)) = reply {
                if let Type::Struct(name) = ty {
                    if !checker.expandable_struct(name) {
                        checker.error(
                            "T011",
                            "struct reply expansion exceeds 64 total nodes",
                            span,
                        );
                    }
                } else if *ty == Type::RequestId {
                    checker.error(
                        "T011",
                        format!("{ty} is not supported as a message reply"),
                        span,
                    );
                }
            }

            let message_ty = Type::Message {
                parameters: parameters
                    .iter()
                    .map(|(name, ty, _)| (name.clone(), ty.clone()))
                    .collect(),
                reply: reply.as_ref().map(|(ty, _)| Box::new(ty.clone())),
                is_reply: handler.is_reply,
            };
            let before = checker.symbols.len();
            checker.bind(
                &handler.name,
                SymbolKind::Handler,
                message_ty,
                actor.span,
                &mut handler_names,
            );
            if checker.symbols.len() != before {
                handler_map.insert(handler.name.text.clone(), before);
            }
            handler_types.push((parameters, reply.map(|(ty, _)| ty)));
        }
        if !actor.name.text.is_empty() && !checker.actor_handlers.contains_key(&actor.name.text) {
            checker
                .actor_handlers
                .insert(actor.name.text.clone(), handler_map.clone());
        }
        precomputed_handlers.push((handler_map, handler_types));
    }

    let mut actor_states = Vec::with_capacity(parsed.program.actors.len());

    for (actor, (handler_map, handler_types)) in
        parsed.program.actors.iter().zip(precomputed_handlers)
    {
        checker.current_actor = actor.name.text.clone();
        checker.functions.clear();
        checker.handlers = handler_map;
        checker.effects_allowed = false;
        checker.structs = checker
            .actor_structs
            .get(&actor.name.text)
            .cloned()
            .unwrap_or_else(|| global_structs.clone());

        // Pass 1: register all function signatures up-front (supporting forward & recursive calls)
        let mut fn_entries = Vec::new();
        for function in &actor.functions {
            if !function.name.text.is_empty() && !is_snake_case(&function.name.text) {
                checker.error(
                    "T020",
                    "function names must use snake_case",
                    function.name.span,
                );
            }
            let parameters: Vec<_> = function
                .parameters
                .iter()
                .map(|parameter| {
                    if !parameter.name.text.is_empty() && !is_snake_case(&parameter.name.text) {
                        checker.error(
                            "T020",
                            "parameter names must use snake_case",
                            parameter.name.span,
                        );
                    }
                    (
                        parameter.name.text.clone(),
                        checker.resolve_type(&parameter.ty),
                    )
                })
                .collect();
            let result = function
                .result
                .as_ref()
                .map_or(Type::Unit, |ty| checker.resolve_type(ty));
            if is_reserved_name(&function.name.text)
                || checker.functions.contains_key(&function.name.text)
                || checker.structs.contains_key(&function.name.text)
                || checker.actor_symbols.contains_key(&function.name.text)
            {
                checker.error(
                    "T004",
                    format!(
                        "duplicate or reserved function name '{}'",
                        function.name.text
                    ),
                    function.name.span,
                );
                continue;
            }
            let fn_idx = fn_entries.len();
            let symbol = checker.symbols.len();
            checker.symbols.push(Symbol {
                name: function.name.text.clone(),
                kind: SymbolKind::Function,
                ty: Type::Function {
                    parameters: parameters.clone(),
                    result: Box::new(result.clone()),
                },
                span: function.name.span,
                scope: actor.span,
            });
            checker
                .functions
                .insert(function.name.text.clone(), (symbol, fn_idx));
            fn_entries.push((function, parameters, result));
        }

        // Pass 2: check all function bodies and collect call-graph edges
        let mut call_edges = vec![Vec::new(); fn_entries.len()];
        for (fn_idx, (function, parameters, result)) in fn_entries.iter().enumerate() {
            let mut scope = HashMap::new();
            for (parameter, (_, ty)) in function.parameters.iter().zip(parameters) {
                checker.bind(
                    &parameter.name,
                    SymbolKind::Parameter,
                    ty.clone(),
                    function.body.span,
                    &mut scope,
                );
            }
            checker.current_call_targets.clear();
            let actual = checker.block(&function.body, &mut scope);
            checker.require(
                &actual,
                result,
                function
                    .body
                    .tail
                    .as_ref()
                    .map_or(function.body.span, |tail| tail.span),
            );
            call_edges[fn_idx] = std::mem::take(&mut checker.current_call_targets);
        }

        for (fn_idx, depth_opt) in check_acyclic_function_depths(&call_edges)
            .into_iter()
            .enumerate()
        {
            if let Some(depth) = depth_opt
                && depth > 32
            {
                checker.error(
                    "T017",
                    "function call chain exceeds 32",
                    fn_entries[fn_idx].0.name.span,
                );
            }
        }

        let mut state_scope = HashMap::new();
        let mut resolved_states = Vec::with_capacity(actor.states.len());
        for state in &actor.states {
            if !state.name.text.is_empty() && !is_snake_case(&state.name.text) {
                checker.error("T020", "state names must use snake_case", state.name.span);
            }
            let actual = checker.expression(&state.default, &state_scope);
            let declared = match &state.ty {
                Some(ty_expr) => {
                    let decl = checker.resolve_type(ty_expr);
                    checker.require(&actual, &decl, state.default.span);
                    decl
                }
                None => {
                    if matches!(actual, Type::Unknown | Type::NullPid) {
                        checker.error(
                            "T002",
                            "cannot infer state type; add an explicit ': type' annotation (e.g. ': pid<Actor>')",
                            state.name.span,
                        );
                        Type::Unknown
                    } else {
                        actual
                    }
                }
            };
            if declared == Type::RequestId {
                checker.error(
                    "T018",
                    "req capabilities cannot be stored in actor state",
                    state.ty.as_ref().map_or(state.default.span, |ty| ty.span),
                );
            }
            checker.bind(
                &state.name,
                SymbolKind::State,
                declared.clone(),
                actor.span,
                &mut state_scope,
            );
            resolved_states.push((state.name.text.clone(), declared));
        }
        actor_states.push(resolved_states);

        checker.effects_allowed = true;
        for (handler, (parameters, reply)) in actor.handlers.iter().zip(handler_types) {
            let mut scope = state_scope.clone();
            for (parameter, (_, ty, _)) in handler.parameters.iter().zip(parameters) {
                checker.bind(
                    &parameter.name,
                    SymbolKind::Parameter,
                    ty,
                    handler.body.span,
                    &mut scope,
                );
            }
            let actual = checker.block(&handler.body, &mut scope);
            let span = handler
                .body
                .tail
                .as_ref()
                .map_or(handler.body.span, |tail| tail.span);
            checker.require(&actual, &reply.unwrap_or(Type::Unit), span);
        }
    }

    Analysis {
        parsed,
        diagnostics: checker.diagnostics,
        symbols: checker.symbols,
        references: checker.references,
        actor_states,
        target_actors: checker.target_actors,
    }
}

fn is_reserved_name(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "bool"
            | "str"
            | "bytes"
            | "pid"
            | "req"
            | "unit"
            | "box"
            | "gc"
            | "self"
            | "spawn"
            | "null"
            | "send_after"
            | "monitor"
    )
}

type StructMap = HashMap<String, (usize, Vec<(String, Type)>)>;

struct Checker {
    diagnostics: Vec<Diagnostic>,
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
    actor_symbols: HashMap<String, usize>,
    actor_handlers: HashMap<String, HashMap<String, usize>>,
    actor_structs: HashMap<String, StructMap>,
    target_actors: HashMap<(usize, usize), String>,
    current_actor: String,
    functions: HashMap<String, (usize, usize)>,
    current_call_targets: Vec<usize>,
    handlers: HashMap<String, usize>,
    effects_allowed: bool,
    structs: StructMap,
}

impl Checker {
    fn error(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics
                .push(Diagnostic::error(code, message, span));
        }
    }

    fn register_struct_decl(&mut self, decl: &wings_syntax::ast::StructDecl, scope_span: Span) {
        if !decl.name.text.is_empty() && !is_pascal_case(&decl.name.text) {
            self.error("T020", "struct names must use PascalCase", decl.name.span);
        }
        if self.structs.contains_key(&decl.name.text)
            || self.actor_symbols.contains_key(&decl.name.text)
            || is_reserved_name(&decl.name.text)
        {
            self.error(
                "T004",
                format!("duplicate or reserved struct name '{}'", decl.name.text),
                decl.name.span,
            );
            return;
        }
        if decl.fields.len() > 8 {
            self.error("T019", "struct field count exceeds 8", decl.span);
        }
        let mut field_names = HashSet::new();
        let mut fields = Vec::new();
        for field in &decl.fields {
            if !field.name.text.is_empty() && !is_snake_case(&field.name.text) {
                self.error(
                    "T020",
                    "struct field names must use snake_case",
                    field.name.span,
                );
            }
            if !field_names.insert(field.name.text.clone()) {
                self.error("T004", "duplicate struct field", field.name.span);
            }
            let ty = self.resolve_type(&field.ty);
            if ty == Type::RequestId {
                self.error(
                    "T019",
                    "request capabilities (req) cannot be stored in struct fields",
                    field.ty.span,
                );
            }
            fields.push((field.name.text.clone(), ty));
        }
        let sym = self.symbols.len();
        self.symbols.push(Symbol {
            name: decl.name.text.clone(),
            kind: SymbolKind::Struct,
            ty: Type::Struct(decl.name.text.clone()),
            span: decl.name.span,
            scope: scope_span,
        });
        self.structs.insert(decl.name.text.clone(), (sym, fields));
    }

    fn check_cross_actor_struct(&mut self, target_actor: &str, ty: &Type, span: Span) {
        if target_actor == self.current_actor {
            return;
        }
        if let Type::Struct(struct_name) = ty {
            let caller_fields = self.structs.get(struct_name).map(|(_, f)| f);
            let target_fields = self
                .actor_structs
                .get(target_actor)
                .and_then(|s| s.get(struct_name))
                .map(|(_, f)| f);
            match (caller_fields, target_fields) {
                (Some(cf), Some(tf)) if cf == tf => {
                    let fields_clone = cf.clone();
                    for (_, field_ty) in &fields_clone {
                        self.check_cross_actor_struct(target_actor, field_ty, span);
                    }
                }
                _ => {
                    self.error(
                        "T019",
                        format!(
                            "cross-actor message struct '{struct_name}' must be declared as a shared top-level struct"
                        ),
                        span,
                    );
                }
            }
        }
    }

    fn expandable_struct(&self, root: &str) -> bool {
        fn visit(structs: &StructMap, name: &str, nodes: &mut usize) -> bool {
            *nodes += 1;
            if *nodes > 64 {
                return false;
            }
            let Some((_, fields)) = structs.get(name) else {
                return false;
            };
            for (_, ty) in fields {
                if let Type::Struct(nested) = ty {
                    if !visit(structs, nested, nodes) {
                        return false;
                    }
                } else {
                    *nodes += 1;
                    if *nodes > 64 {
                        return false;
                    }
                }
            }
            true
        }
        let mut nodes = 0;
        visit(&self.structs, root, &mut nodes)
    }

    fn message_nodes(&self, ty: &Type) -> Option<usize> {
        fn visit(structs: &StructMap, ty: &Type, nodes: &mut usize) -> bool {
            *nodes += 1;
            if *nodes > 64 {
                return false;
            }
            if let Type::Struct(name) = ty {
                let Some((_, fields)) = structs.get(name) else {
                    return false;
                };
                for (_, field_ty) in fields {
                    if !visit(structs, field_ty, nodes) {
                        return false;
                    }
                }
            }
            true
        }
        let mut nodes = 0;
        visit(&self.structs, ty, &mut nodes).then_some(nodes)
    }

    fn resolve_type(&mut self, ty_expr: &TypeExpr) -> Type {
        match &ty_expr.kind {
            TypeKind::Unit => Type::Unit,
            TypeKind::List(inner) => {
                let elem = self.resolve_type(inner);
                if !matches!(elem, Type::Int | Type::Unknown) {
                    self.error("T002", "only [int] lists are supported", ty_expr.span);
                }
                Type::IntList
            }
            TypeKind::ActorPid(actor_name) => {
                if actor_name.text.is_empty() {
                    Type::Unknown
                } else if let Some(&sym) = self.actor_symbols.get(&actor_name.text) {
                    self.references.push(Reference {
                        span: actor_name.span,
                        symbol: sym,
                    });
                    Type::ActorPid(actor_name.text.clone())
                } else {
                    self.error(
                        "T002",
                        format!(
                            "unknown actor '{}' in pid<{}>",
                            actor_name.text, actor_name.text
                        ),
                        actor_name.span,
                    );
                    Type::Unknown
                }
            }
            TypeKind::Named(name) => match name.text.as_str() {
                "int" => Type::Int,
                "bool" => Type::Bool,
                "str" => Type::String,
                "bytes" => Type::Bytes,
                "pid" => Type::Pid,
                "req" => Type::RequestId,
                "unit" => Type::Unit,
                "" => Type::Unknown,
                _ if self.structs.contains_key(&name.text) => {
                    let sym = self.structs[&name.text].0;
                    self.references.push(Reference {
                        span: name.span,
                        symbol: sym,
                    });
                    Type::Struct(name.text.clone())
                }
                _ => {
                    self.error(
                        "T002",
                        format!(
                            "unknown type '{}'; built-in types are int, bool, (), str, bytes, [int], pid and req",
                            name.text
                        ),
                        name.span,
                    );
                    Type::Unknown
                }
            },
            TypeKind::Error => Type::Unknown,
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
        if name.text.is_empty() || name.text == "_" {
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

    fn types_compatible(&self, actual: &Type, expected: &Type) -> bool {
        if matches!(actual, Type::Unknown) || matches!(expected, Type::Unknown) {
            return true;
        }
        if actual == expected {
            return true;
        }
        match (actual, expected) {
            (Type::NullPid, Type::Pid | Type::ActorPid(_)) => true,
            (Type::ActorPid(_), Type::Pid) => true,
            (Type::Pid, Type::ActorPid(name)) => {
                !self.current_actor.is_empty() && name == &self.current_actor
            }
            _ => false,
        }
    }

    fn require(&mut self, actual: &Type, expected: &Type, span: Span) {
        if !self.types_compatible(actual, expected) {
            self.error("T005", format!("expected {expected}, found {actual}"), span);
        }
    }

    fn check_assign_target(
        &mut self,
        target: &AssignTarget,
        scope: &HashMap<String, usize>,
    ) -> Type {
        let Some(symbol) = self.resolve(&target.base, scope) else {
            return Type::Unknown;
        };
        if !matches!(
            self.symbols[symbol].kind,
            SymbolKind::State | SymbolKind::Local
        ) {
            self.error(
                "T008",
                "only 'state' and 'let' bindings can be assigned",
                target.base.span,
            );
            return Type::Unknown;
        }
        let mut current_ty = self.symbols[symbol].ty.clone();
        for field in &target.fields {
            if let Type::Struct(struct_name) = &current_ty
                && let Some((_, fields)) = self.structs.get(struct_name)
                && let Some((_, field_ty)) = fields.iter().find(|(f, _)| f == &field.text)
            {
                current_ty = field_ty.clone();
            } else {
                self.error("T019", "unknown field or non-struct target", field.span);
                return Type::Unknown;
            }
        }
        current_ty
    }

    fn block(&mut self, block: &Block, scope: &mut HashMap<String, usize>) -> Type {
        for statement in &block.statements {
            match statement {
                Statement::While { condition, body } => {
                    let condition_type = self.expression(condition, scope);
                    self.require(&condition_type, &Type::Bool, condition.span);
                    let result = self.block(body, &mut scope.clone());
                    self.require(&result, &Type::Unit, body.span);
                }
                Statement::ForRange {
                    binding,
                    start,
                    end,
                    body,
                    ..
                } => {
                    if !binding.text.is_empty()
                        && binding.text != "_"
                        && !is_snake_case(&binding.text)
                    {
                        self.error(
                            "T020",
                            "loop variable names must use snake_case",
                            binding.span,
                        );
                    }
                    let start_ty = self.expression(start, scope);
                    self.require(&start_ty, &Type::Int, start.span);
                    let end_ty = self.expression(end, scope);
                    self.require(&end_ty, &Type::Int, end.span);
                    let mut inner_scope = scope.clone();
                    if binding.text != "_" && !binding.text.is_empty() {
                        inner_scope.remove(&binding.text);
                        self.bind(
                            binding,
                            SymbolKind::Local,
                            Type::Int,
                            body.span,
                            &mut inner_scope,
                        );
                    }
                    let result = self.block(body, &mut inner_scope);
                    self.require(&result, &Type::Unit, body.span);
                }
                Statement::ForList {
                    binding,
                    list,
                    body,
                    ..
                } => {
                    if !binding.text.is_empty()
                        && binding.text != "_"
                        && !is_snake_case(&binding.text)
                    {
                        self.error(
                            "T020",
                            "loop variable names must use snake_case",
                            binding.span,
                        );
                    }
                    let list_ty = self.expression(list, scope);
                    self.require(&list_ty, &Type::IntList, list.span);
                    let mut inner_scope = scope.clone();
                    if binding.text != "_" && !binding.text.is_empty() {
                        inner_scope.remove(&binding.text);
                        self.bind(
                            binding,
                            SymbolKind::Local,
                            Type::Int,
                            body.span,
                            &mut inner_scope,
                        );
                    }
                    let result = self.block(body, &mut inner_scope);
                    self.require(&result, &Type::Unit, body.span);
                }
                Statement::Assign {
                    target,
                    operator,
                    value,
                } => {
                    let target_ty = self.check_assign_target(target, scope);
                    let actual = self.expression(value, scope);
                    match operator {
                        None => self.require(&actual, &target_ty, value.span),
                        Some(BinaryOperator::Add) => {
                            if matches!(target_ty, Type::Int | Type::String) {
                                self.require(&actual, &target_ty, value.span);
                            } else if !matches!(target_ty, Type::Unknown) {
                                self.error(
                                    "T005",
                                    format!("'+=' requires int or str, found {target_ty}"),
                                    target.span,
                                );
                            }
                        }
                        Some(_) => {
                            self.require(&target_ty, &Type::Int, target.span);
                            self.require(&actual, &Type::Int, value.span);
                        }
                    }
                }
                Statement::Let { name, ty, value } => {
                    if !name.text.is_empty() && name.text != "_" && !is_snake_case(&name.text) {
                        self.error(
                            "T020",
                            "local variable names must use snake_case",
                            name.span,
                        );
                    }
                    let actual = self.expression(value, scope);
                    let declared = ty.as_ref().map(|ty_expr| self.resolve_type(ty_expr));
                    let bound_ty = if let Some(declared) = declared {
                        self.require(&actual, &declared, value.span);
                        declared
                    } else if actual == Type::NullPid {
                        self.error(
                            "T002",
                            "cannot infer pid type from 'null'; add ': pid' or ': pid<Actor>'",
                            name.span,
                        );
                        Type::Unknown
                    } else {
                        actual
                    };
                    self.bind(
                        name,
                        SymbolKind::Local,
                        bound_ty,
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

    fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        scope: &HashMap<String, usize>,
    ) -> Type {
        let scrutinee_ty = self.expression(scrutinee, scope);
        if !matches!(
            scrutinee_ty,
            Type::Int
                | Type::Bool
                | Type::String
                | Type::IntList
                | Type::Pid
                | Type::ActorPid(_)
                | Type::Unknown
        ) {
            self.error(
                "T005",
                format!("match supports int, bool, str, [int] and pid, found {scrutinee_ty}"),
                scrutinee.span,
            );
        }
        if arms.is_empty() {
            self.error("T005", "match requires at least one arm", span);
            return Type::Unknown;
        }
        let mut expected_result: Option<Type> = None;
        let mut has_wildcard = false;
        let mut has_true = false;
        let mut has_false = false;
        let mut has_empty = false;
        let mut has_cons = false;

        for arm in arms {
            if has_wildcard {
                self.error("T005", "unreachable match arm after '_'", arm.span);
            }
            let mut arm_scope = scope.clone();
            match &arm.pattern {
                MatchPattern::Wildcard(_) => {
                    has_wildcard = true;
                }
                MatchPattern::Null(pat_span) => {
                    if !matches!(scrutinee_ty, Type::Pid | Type::ActorPid(_) | Type::Unknown) {
                        self.error(
                            "T005",
                            format!("expected pid for 'null' pattern, found {scrutinee_ty}"),
                            *pat_span,
                        );
                    }
                }
                MatchPattern::Integer(_, pat_span) => {
                    self.require(&Type::Int, &scrutinee_ty, *pat_span);
                }
                MatchPattern::Boolean(val, pat_span) => {
                    self.require(&Type::Bool, &scrutinee_ty, *pat_span);
                    if *val {
                        has_true = true;
                    } else {
                        has_false = true;
                    }
                }
                MatchPattern::String(_, pat_span) => {
                    self.require(&Type::String, &scrutinee_ty, *pat_span);
                }
                MatchPattern::EmptyList(pat_span) => {
                    self.require(&Type::IntList, &scrutinee_ty, *pat_span);
                    has_empty = true;
                }
                MatchPattern::ConsList {
                    head,
                    tail,
                    span: pat_span,
                } => {
                    self.require(&Type::IntList, &scrutinee_ty, *pat_span);
                    has_cons = true;
                    if head.text != "_" && !head.text.is_empty() {
                        if !is_snake_case(&head.text) {
                            self.error("T020", "pattern bindings must use snake_case", head.span);
                        }
                        arm_scope.remove(&head.text);
                        self.bind(
                            head,
                            SymbolKind::Local,
                            Type::Int,
                            arm.body.span,
                            &mut arm_scope,
                        );
                    }
                    if tail.text != "_" && !tail.text.is_empty() {
                        if !is_snake_case(&tail.text) {
                            self.error("T020", "pattern bindings must use snake_case", tail.span);
                        }
                        if tail.text == head.text {
                            self.error("T004", "duplicate pattern binding", tail.span);
                        } else {
                            arm_scope.remove(&tail.text);
                            self.bind(
                                tail,
                                SymbolKind::Local,
                                Type::IntList,
                                arm.body.span,
                                &mut arm_scope,
                            );
                        }
                    }
                }
            }
            let arm_ty = self.block(&arm.body, &mut arm_scope);
            if let Some(expected) = &mut expected_result {
                if *expected == Type::NullPid && matches!(arm_ty, Type::Pid | Type::ActorPid(_)) {
                    *expected = arm_ty;
                } else {
                    let target = expected.clone();
                    self.require(&arm_ty, &target, arm.body.span);
                }
            } else {
                expected_result = Some(arm_ty);
            }
        }

        let exhaustive = has_wildcard
            || match scrutinee_ty {
                Type::Bool => has_true && has_false,
                Type::IntList => has_empty && has_cons,
                Type::Unknown => true,
                _ => false,
            };
        if !exhaustive {
            self.error(
                "T005",
                "non-exhaustive match; add missing cases or a '_' fallback arm",
                span,
            );
        }
        expected_result.unwrap_or(Type::Unknown)
    }

    fn expression(&mut self, expression: &Expr, scope: &HashMap<String, usize>) -> Type {
        match &expression.kind {
            ExprKind::Integer(_) => Type::Int,
            ExprKind::String(_) => Type::String,
            ExprKind::InterpolatedString(parts) => {
                for part in parts {
                    if let InterpolatedPart::Expr(inner) = part {
                        let ty = self.expression(inner, scope);
                        if !matches!(ty, Type::String | Type::Int | Type::Bool | Type::Unknown) {
                            self.error(
                                "T005",
                                format!(
                                    "string interpolation supports str, int and bool, found {ty}"
                                ),
                                inner.span,
                            );
                        }
                    }
                }
                Type::String
            }
            ExprKind::Bytes(_) => Type::Bytes,
            ExprKind::Boolean(_) => Type::Bool,
            ExprKind::Unit => Type::Unit,
            ExprKind::Null => Type::NullPid,
            ExprKind::SelfPid => {
                if !self.effects_allowed {
                    self.error(
                        "T018",
                        "'self' is only supported in handlers",
                        expression.span,
                    );
                }
                Type::Pid
            }
            ExprKind::Spawn(target_actor) => {
                if !self.effects_allowed {
                    self.error(
                        "T018",
                        "'spawn()' is only supported in handlers",
                        expression.span,
                    );
                }
                match target_actor {
                    None => Type::Pid,
                    Some(actor_name) => {
                        if actor_name.text.is_empty() {
                            Type::Unknown
                        } else if let Some(&sym) = self.actor_symbols.get(&actor_name.text) {
                            self.references.push(Reference {
                                span: actor_name.span,
                                symbol: sym,
                            });
                            Type::ActorPid(actor_name.text.clone())
                        } else {
                            self.error(
                                "T002",
                                format!("unknown actor '{}'", actor_name.text),
                                actor_name.span,
                            );
                            Type::Unknown
                        }
                    }
                }
            }
            ExprKind::Name(name) => self
                .resolve(name, scope)
                .map_or(Type::Unknown, |symbol| self.symbols[symbol].ty.clone()),
            ExprKind::List { elements, tail } => {
                for elem in elements {
                    let ty = self.expression(elem, scope);
                    self.require(&ty, &Type::Int, elem.span);
                }
                if let Some(tail_expr) = tail {
                    let ty = self.expression(tail_expr, scope);
                    self.require(&ty, &Type::IntList, tail_expr.span);
                }
                Type::IntList
            }
            ExprKind::StructInit { name, base, fields } => {
                let Some((sym, schema)) = self.structs.get(&name.text).cloned() else {
                    self.error("T002", format!("unknown struct '{}'", name.text), name.span);
                    for (_, val) in fields {
                        self.expression(val, scope);
                    }
                    if let Some(b) = base {
                        self.expression(b, scope);
                    }
                    return Type::Unknown;
                };
                self.references.push(Reference {
                    span: name.span,
                    symbol: sym,
                });
                let result_ty = Type::Struct(name.text.clone());
                if let Some(base_expr) = base {
                    let base_ty = self.expression(base_expr, scope);
                    self.require(&base_ty, &result_ty, base_expr.span);
                }
                let mut seen = HashSet::new();
                for (field_name, field_val) in fields {
                    let val_ty = self.expression(field_val, scope);
                    if !seen.insert(field_name.text.clone()) {
                        self.error(
                            "T004",
                            format!("duplicate field '{}' in struct literal", field_name.text),
                            field_name.span,
                        );
                        continue;
                    }
                    if let Some((_, expected_ty)) =
                        schema.iter().find(|(f, _)| f == &field_name.text)
                    {
                        self.require(&val_ty, expected_ty, field_val.span);
                    } else {
                        self.error(
                            "T019",
                            format!(
                                "unknown field '{}' on struct '{}'",
                                field_name.text, name.text
                            ),
                            field_name.span,
                        );
                    }
                }
                if base.is_none() {
                    for (schema_field, _) in &schema {
                        if !seen.contains(schema_field) {
                            self.error(
                                "T013",
                                format!(
                                    "missing field '{}' in '{}' literal",
                                    schema_field, name.text
                                ),
                                expression.span,
                            );
                        }
                    }
                }
                result_ty
            }
            ExprKind::Field { value, name } => {
                let ty = self.expression(value, scope);
                if let Type::Struct(ref struct_name) = ty {
                    if let Some((_, fields)) = self.structs.get(struct_name)
                        && let Some((_, field_ty)) = fields.iter().find(|(f, _)| f == &name.text)
                    {
                        return field_ty.clone();
                    }
                }
                if !matches!(ty, Type::Unknown) {
                    self.error("T019", "unknown field or non-struct value", name.span);
                }
                Type::Unknown
            }
            ExprKind::StaticCall {
                namespace,
                method,
                arguments,
            } => {
                let arg_types: Vec<Type> = arguments
                    .iter()
                    .map(|arg| self.expression(arg, scope))
                    .collect();
                match (namespace.text.as_str(), method.text.as_str()) {
                    ("bytes", "fill") => {
                        if arguments.len() != 2 {
                            self.error(
                                "T013",
                                format!(
                                    "bytes::fill expects 2 arguments (len, byte), found {}",
                                    arguments.len()
                                ),
                                expression.span,
                            );
                        }
                        for (ty, arg) in arg_types.iter().zip(arguments) {
                            self.require(ty, &Type::Int, arg.span);
                        }
                        Type::Bytes
                    }
                    _ => {
                        self.error(
                            "T012",
                            format!(
                                "unknown static function '{}::{}'",
                                namespace.text, method.text
                            ),
                            expression.span,
                        );
                        Type::Unknown
                    }
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                arguments,
            } => {
                let recv_ty = self.expression(receiver, scope);
                let arg_types: Vec<Type> = arguments
                    .iter()
                    .map(|arg| self.expression(arg, scope))
                    .collect();
                if matches!(recv_ty, Type::Unknown) {
                    return Type::Unknown;
                }
                let signature: Option<(&[Type], Type)> = match (&recv_ty, method.text.as_str()) {
                    (Type::Int, "to_str") => Some((&[], Type::String)),
                    (Type::Bool, "to_str") => Some((&[], Type::String)),
                    (Type::IntList, "is_empty") => Some((&[], Type::Bool)),
                    (Type::IntList, "head") => Some((&[], Type::Int)),
                    (Type::IntList, "tail") => Some((&[], Type::IntList)),
                    (Type::IntList, "push") => Some((&[Type::Int], Type::IntList)),
                    (Type::Bytes, "len") => Some((&[], Type::Int)),
                    (Type::Bytes, "is_empty") => Some((&[], Type::Bool)),
                    (Type::Bytes, "at") => Some((&[Type::Int], Type::Int)),
                    (Type::Bytes, "set") => Some((&[Type::Int, Type::Int], Type::Bytes)),
                    (Type::Bytes, "i64_at") => Some((&[Type::Int], Type::Int)),
                    (Type::Bytes, "set_i64") => Some((&[Type::Int, Type::Int], Type::Bytes)),
                    (Type::Bytes, "to_str") => Some((&[], Type::String)),
                    (Type::String, "len") => Some((&[], Type::Int)),
                    (Type::String, "is_empty") => Some((&[], Type::Bool)),
                    (Type::String, "at") => Some((&[Type::Int], Type::Int)),
                    (Type::String, "slice") => Some((&[Type::Int, Type::Int], Type::String)),
                    (Type::String, "bytes") => Some((&[], Type::Bytes)),
                    (Type::Pid | Type::ActorPid(_) | Type::NullPid, "is_null") => {
                        Some((&[], Type::Bool))
                    }
                    (Type::RequestId, "cancel") => {
                        if !self.effects_allowed {
                            self.error(
                                "T018",
                                "'cancel()' is only supported in handlers",
                                expression.span,
                            );
                        }
                        Some((&[], Type::Unit))
                    }
                    _ => None,
                };
                let Some((expected_args, ret_ty)) = signature else {
                    self.error(
                        "T012",
                        format!("unknown method '{}' on type {recv_ty}", method.text),
                        method.span,
                    );
                    return Type::Unknown;
                };
                if arguments.len() != expected_args.len() {
                    self.error(
                        "T013",
                        format!(
                            "method '{}' expects {} argument(s), found {}",
                            method.text,
                            expected_args.len(),
                            arguments.len()
                        ),
                        expression.span,
                    );
                }
                for ((actual, expected), arg) in arg_types.iter().zip(expected_args).zip(arguments)
                {
                    self.require(actual, expected, arg.span);
                }
                ret_ty
            }
            ExprKind::Send {
                target,
                message,
                arguments,
            } => {
                if !self.effects_allowed {
                    self.error(
                        "T018",
                        "message send ('!') is only supported in handlers",
                        expression.span,
                    );
                }
                let target_ty = self.expression(target, scope);
                let resolved_target = match &target_ty {
                    Type::Pid => Some(self.current_actor.clone()),
                    Type::ActorPid(name) => Some(name.clone()),
                    Type::Unknown => None,
                    other => {
                        self.error("T005", format!("expected pid, found {other}"), target.span);
                        None
                    }
                };
                let arg_types: Vec<Type> = arguments
                    .iter()
                    .map(|arg| self.expression(arg, scope))
                    .collect();
                if let Some(target_actor) = resolved_target {
                    self.target_actors.insert(
                        (expression.span.start, expression.span.end),
                        target_actor.clone(),
                    );
                    let msg_sym = if target_actor == self.current_actor {
                        self.handlers.get(&message.text).copied()
                    } else {
                        self.actor_handlers
                            .get(&target_actor)
                            .and_then(|m| m.get(&message.text))
                            .copied()
                    };
                    if let Some(sym) = msg_sym {
                        self.references.push(Reference {
                            span: message.span,
                            symbol: sym,
                        });
                        if let Type::Message {
                            parameters,
                            reply,
                            is_reply,
                        } = self.symbols[sym].ty.clone()
                        {
                            if is_reply {
                                self.error(
                                    "T018",
                                    "'on reply' completion handlers cannot be sent directly",
                                    message.span,
                                );
                            } else if reply.is_some() {
                                self.error(
                                    "T018",
                                    "'!' requires a one-way handler; use '.ask(...)' for request/reply handlers",
                                    message.span,
                                );
                            }
                            if arguments.len() != parameters.len() {
                                self.error(
                                    "T013",
                                    format!(
                                        "message '{}' expects {} argument(s), found {}",
                                        message.text,
                                        parameters.len(),
                                        arguments.len()
                                    ),
                                    expression.span,
                                );
                            }
                            for ((actual, (_, expected)), arg) in
                                arg_types.iter().zip(&parameters).zip(arguments)
                            {
                                self.require(actual, expected, arg.span);
                                self.check_cross_actor_struct(&target_actor, expected, arg.span);
                            }
                        }
                    } else if !message.text.is_empty() {
                        self.error(
                            "T018",
                            format!("unknown message handler '{}'", message.text),
                            message.span,
                        );
                    }
                }
                Type::Unit
            }
            ExprKind::SendAfter {
                target,
                delay,
                message,
                arguments,
            } => {
                if !self.effects_allowed {
                    self.error(
                        "T018",
                        "'send_after(...)' is only supported in handlers",
                        expression.span,
                    );
                }
                let target_ty = self.expression(target, scope);
                let resolved_target = match &target_ty {
                    Type::Pid => Some(self.current_actor.clone()),
                    Type::ActorPid(name) => Some(name.clone()),
                    Type::Unknown => None,
                    other => {
                        self.error("T005", format!("expected pid, found {other}"), target.span);
                        None
                    }
                };
                let delay_ty = self.expression(delay, scope);
                self.require(&delay_ty, &Type::Int, delay.span);
                let arg_types: Vec<Type> = arguments
                    .iter()
                    .map(|arg| self.expression(arg, scope))
                    .collect();
                if let Some(target_actor) = resolved_target {
                    self.target_actors.insert(
                        (expression.span.start, expression.span.end),
                        target_actor.clone(),
                    );
                    let msg_sym = if target_actor == self.current_actor {
                        self.handlers.get(&message.text).copied()
                    } else {
                        self.actor_handlers
                            .get(&target_actor)
                            .and_then(|m| m.get(&message.text))
                            .copied()
                    };
                    if let Some(sym) = msg_sym {
                        self.references.push(Reference {
                            span: message.span,
                            symbol: sym,
                        });
                        if let Type::Message {
                            parameters,
                            reply,
                            is_reply,
                        } = self.symbols[sym].ty.clone()
                        {
                            if is_reply {
                                self.error(
                                    "T018",
                                    "'on reply' completion handlers cannot be scheduled with send_after",
                                    message.span,
                                );
                            } else if reply.is_some() {
                                self.error(
                                    "T018",
                                    "'send_after' requires a one-way handler",
                                    message.span,
                                );
                            }
                            if arguments.len() != parameters.len() {
                                self.error(
                                    "T013",
                                    format!(
                                        "message '{}' expects {} argument(s), found {}",
                                        message.text,
                                        parameters.len(),
                                        arguments.len()
                                    ),
                                    expression.span,
                                );
                            }
                            for ((actual, (_, expected)), arg) in
                                arg_types.iter().zip(&parameters).zip(arguments)
                            {
                                self.require(actual, expected, arg.span);
                                self.check_cross_actor_struct(&target_actor, expected, arg.span);
                            }
                        }
                    } else if !message.text.is_empty() {
                        self.error(
                            "T018",
                            format!("unknown message handler '{}'", message.text),
                            message.span,
                        );
                    }
                }
                Type::Unit
            }
            ExprKind::Monitor { target, handler } => {
                if !self.effects_allowed {
                    self.error(
                        "T018",
                        "'monitor(...)' is only supported in handlers",
                        expression.span,
                    );
                }
                let target_ty = self.expression(target, scope);
                if !matches!(target_ty, Type::Pid | Type::ActorPid(_) | Type::Unknown) {
                    self.error(
                        "T005",
                        format!("expected pid, found {target_ty}"),
                        target.span,
                    );
                }
                if let Some(&sym) = self.handlers.get(&handler.text) {
                    self.references.push(Reference {
                        span: handler.span,
                        symbol: sym,
                    });
                    if let Type::Message {
                        parameters,
                        reply,
                        is_reply,
                    } = self.symbols[sym].ty.clone()
                    {
                        if is_reply || reply.is_some() || parameters.len() != 2 {
                            self.error(
                                "T018",
                                "monitor handler must be a one-way 'on <name>(dead: pid, reason: int)' handler",
                                handler.span,
                            );
                        } else {
                            if !matches!(target_ty, Type::Unknown) {
                                self.require(&target_ty, &parameters[0].1, handler.span);
                            }
                            self.require(&parameters[1].1, &Type::Int, handler.span);
                        }
                    }
                } else if !handler.text.is_empty() {
                    self.error(
                        "T018",
                        format!("unknown monitor handler '{}'", handler.text),
                        handler.span,
                    );
                }
                Type::Unit
            }
            ExprKind::Ask {
                target,
                message,
                arguments,
                timeout,
                then,
            } => {
                if !self.effects_allowed {
                    self.error(
                        "T018",
                        "'.ask(...)' is only supported in handlers",
                        expression.span,
                    );
                }
                let target_ty = self.expression(target, scope);
                let resolved_target = match &target_ty {
                    Type::Pid => Some(self.current_actor.clone()),
                    Type::ActorPid(name) => Some(name.clone()),
                    Type::Unknown => None,
                    other => {
                        self.error("T005", format!("expected pid, found {other}"), target.span);
                        None
                    }
                };
                let timeout_ty = self.expression(timeout, scope);
                self.require(&timeout_ty, &Type::Int, timeout.span);
                let arg_types: Vec<Type> = arguments
                    .iter()
                    .map(|arg| self.expression(arg, scope))
                    .collect();

                let mut target_reply: Option<Type> = None;
                if let Some(target_actor) = resolved_target {
                    self.target_actors.insert(
                        (expression.span.start, expression.span.end),
                        target_actor.clone(),
                    );
                    let msg_sym = if target_actor == self.current_actor {
                        self.handlers.get(&message.text).copied()
                    } else {
                        self.actor_handlers
                            .get(&target_actor)
                            .and_then(|m| m.get(&message.text))
                            .copied()
                    };
                    if let Some(msg_sym) = msg_sym {
                        self.references.push(Reference {
                            span: message.span,
                            symbol: msg_sym,
                        });
                        if let Type::Message {
                            parameters,
                            reply,
                            is_reply,
                        } = self.symbols[msg_sym].ty.clone()
                        {
                            if is_reply || reply.is_none() {
                                self.error(
                                    "T018",
                                    "'.ask(...)' requires a handler that returns '-> type'",
                                    message.span,
                                );
                            } else {
                                if let Some(ref rep_ty) = reply {
                                    self.check_cross_actor_struct(
                                        &target_actor,
                                        rep_ty,
                                        message.span,
                                    );
                                }
                                target_reply = reply.map(|b| *b);
                            }
                            if arguments.len() != parameters.len() {
                                self.error(
                                    "T013",
                                    format!(
                                        "message '{}' expects {} argument(s), found {}",
                                        message.text,
                                        parameters.len(),
                                        arguments.len()
                                    ),
                                    expression.span,
                                );
                            }
                            for ((actual, (_, expected)), arg) in
                                arg_types.iter().zip(&parameters).zip(arguments)
                            {
                                self.require(actual, expected, arg.span);
                                self.check_cross_actor_struct(&target_actor, expected, arg.span);
                            }
                        }
                    } else if !message.text.is_empty() {
                        self.error(
                            "T018",
                            format!("unknown request message '{}'", message.text),
                            message.span,
                        );
                    }
                }

                if let Some(&then_sym) = self.handlers.get(&then.text) {
                    self.references.push(Reference {
                        span: then.span,
                        symbol: then_sym,
                    });
                    if let Type::Message {
                        parameters,
                        is_reply,
                        ..
                    } = self.symbols[then_sym].ty.clone()
                    {
                        if !is_reply || parameters.len() != 3 {
                            self.error(
                                "T018",
                                "'then:' handler must be declared with 'on reply'",
                                then.span,
                            );
                        } else if let Some(expected_reply) = &target_reply {
                            self.require(&parameters[2].1, expected_reply, then.span);
                        }
                    }
                } else if !then.text.is_empty() {
                    self.error(
                        "T018",
                        format!("unknown completion handler '{}'", then.text),
                        then.span,
                    );
                }
                Type::RequestId
            }
            ExprKind::Call { callee, arguments } => {
                if scope.contains_key(&callee.text) {
                    for argument in arguments {
                        self.expression(argument, scope);
                    }
                    self.error("T012", "local/state bindings are not callable", callee.span);
                    return Type::Unknown;
                }
                let arg_types: Vec<Type> = arguments
                    .iter()
                    .map(|arg| self.expression(arg, scope))
                    .collect();

                if let Some((sym, schema)) = self.structs.get(&callee.text).cloned() {
                    self.references.push(Reference {
                        span: callee.span,
                        symbol: sym,
                    });
                    if arguments.len() != schema.len() {
                        self.error(
                            "T013",
                            format!(
                                "struct '{}' expects {} field(s), found {}",
                                callee.text,
                                schema.len(),
                                arguments.len()
                            ),
                            expression.span,
                        );
                    }
                    for ((actual, (_, expected)), arg) in
                        arg_types.iter().zip(&schema).zip(arguments)
                    {
                        self.require(actual, expected, arg.span);
                    }
                    return Type::Struct(callee.text.clone());
                }

                if let Some(&(sym, fn_idx)) = self.functions.get(&callee.text) {
                    self.references.push(Reference {
                        span: callee.span,
                        symbol: sym,
                    });
                    self.current_call_targets.push(fn_idx);
                    let Type::Function { parameters, result } = self.symbols[sym].ty.clone() else {
                        return Type::Unknown;
                    };
                    if arguments.len() != parameters.len() {
                        self.error(
                            "T013",
                            format!(
                                "{} expects {} argument(s), found {}",
                                callee.text,
                                parameters.len(),
                                arguments.len()
                            ),
                            expression.span,
                        );
                    }
                    for ((actual, (_, expected)), arg) in
                        arg_types.iter().zip(&parameters).zip(arguments)
                    {
                        self.require(actual, expected, arg.span);
                    }
                    return *result;
                }

                match callee.text.as_str() {
                    "gc" => {
                        if !arguments.is_empty() {
                            self.error("T013", "gc() takes no arguments", expression.span);
                        }
                        Type::Unit
                    }
                    _ => {
                        self.error(
                            "T012",
                            format!("unknown function '{}'", callee.text),
                            callee.span,
                        );
                        Type::Unknown
                    }
                }
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition_type = self.expression(condition, scope);
                self.require(&condition_type, &Type::Bool, condition.span);
                let then_type = self.block(then_branch, &mut scope.clone());
                if let Some(else_block) = else_branch {
                    let else_type = self.block(else_block, &mut scope.clone());
                    if then_type == Type::NullPid
                        && matches!(else_type, Type::Pid | Type::ActorPid(_))
                    {
                        else_type
                    } else {
                        self.require(&else_type, &then_type, else_block.span);
                        then_type
                    }
                } else {
                    self.require(&then_type, &Type::Unit, then_branch.span);
                    Type::Unit
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.check_match(scrutinee, arms, expression.span, scope)
            }
            ExprKind::Negate(value) => {
                let actual = self.expression(value, scope);
                self.require(&actual, &Type::Int, value.span);
                Type::Int
            }
            ExprKind::Not(value) => {
                let actual = self.expression(value, scope);
                self.require(&actual, &Type::Bool, value.span);
                Type::Bool
            }
            ExprKind::Binary {
                operator,
                left,
                right,
            } => {
                let left_type = self.expression(left, scope);
                let right_type = self.expression(right, scope);
                match operator {
                    BinaryOperator::And | BinaryOperator::Or => {
                        self.require(&left_type, &Type::Bool, left.span);
                        self.require(&right_type, &Type::Bool, right.span);
                        Type::Bool
                    }
                    BinaryOperator::Equal | BinaryOperator::NotEqual => {
                        if left_type == Type::NullPid
                            && matches!(right_type, Type::Pid | Type::ActorPid(_))
                        {
                            // null == pid is valid
                        } else {
                            self.require(&right_type, &left_type, right.span);
                        }
                        if matches!(
                            left_type,
                            Type::Struct(_) | Type::IntList | Type::Bytes | Type::RequestId
                        ) || matches!(
                            right_type,
                            Type::Struct(_) | Type::IntList | Type::Bytes | Type::RequestId
                        ) {
                            self.error(
                                "T015",
                                "heap references and request capabilities cannot be compared with == or !=",
                                expression.span,
                            );
                        }
                        Type::Bool
                    }
                    BinaryOperator::Add => {
                        if left_type == Type::String {
                            self.require(&right_type, &Type::String, right.span);
                            Type::String
                        } else {
                            self.require(&left_type, &Type::Int, left.span);
                            self.require(&right_type, &Type::Int, right.span);
                            Type::Int
                        }
                    }
                    BinaryOperator::Less
                    | BinaryOperator::LessEqual
                    | BinaryOperator::Greater
                    | BinaryOperator::GreaterEqual => {
                        self.require(&left_type, &Type::Int, left.span);
                        self.require(&right_type, &Type::Int, right.span);
                        Type::Bool
                    }
                    BinaryOperator::Subtract
                    | BinaryOperator::Multiply
                    | BinaryOperator::Divide
                    | BinaryOperator::Remainder => {
                        self.require(&left_type, &Type::Int, left.span);
                        self.require(&right_type, &Type::Int, right.span);
                        Type::Int
                    }
                }
            }
            ExprKind::Error => Type::Unknown,
        }
    }
}
