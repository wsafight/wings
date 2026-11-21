use std::collections::{HashMap, HashSet};

use wings_source::{Diagnostic, MAX_DIAGNOSTICS, Span};
use wings_syntax::ast::{
    AssignTarget, BinaryOperator, Block, Expr, ExprKind, InterpolatedPart, MatchArm, MatchPattern,
    Name, Statement, TypeExpr, TypeKind,
};
use wings_syntax::{Parse, parse};

mod expr;
mod stmt;
mod types;

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
