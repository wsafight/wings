use super::*;

impl Checker {
    pub(super) fn expression(&mut self, expression: &Expr, scope: &HashMap<String, usize>) -> Type {
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
