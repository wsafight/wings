use super::*;

impl Checker {
    pub(super) fn error(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics
                .push(Diagnostic::error(code, message, span));
        }
    }

    pub(super) fn register_struct_decl(
        &mut self,
        decl: &wings_syntax::ast::StructDecl,
        scope_span: Span,
    ) {
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

    pub(super) fn check_cross_actor_struct(&mut self, target_actor: &str, ty: &Type, span: Span) {
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

    pub(super) fn expandable_struct(&self, root: &str) -> bool {
        pub(super) fn visit(structs: &StructMap, name: &str, nodes: &mut usize) -> bool {
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

    pub(super) fn message_nodes(&self, ty: &Type) -> Option<usize> {
        pub(super) fn visit(structs: &StructMap, ty: &Type, nodes: &mut usize) -> bool {
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

    pub(super) fn resolve_type(&mut self, ty_expr: &TypeExpr) -> Type {
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

    pub(super) fn bind(
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

    pub(super) fn resolve(&mut self, name: &Name, scope: &HashMap<String, usize>) -> Option<usize> {
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

    pub(super) fn types_compatible(&self, actual: &Type, expected: &Type) -> bool {
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

    pub(super) fn require(&mut self, actual: &Type, expected: &Type, span: Span) {
        if !self.types_compatible(actual, expected) {
            self.error("T005", format!("expected {expected}, found {actual}"), span);
        }
    }
}
