use super::*;

impl Checker {
    pub(super) fn check_assign_target(
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

    pub(super) fn block(&mut self, block: &Block, scope: &mut HashMap<String, usize>) -> Type {
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

    pub(super) fn check_match(
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
}
