use super::*;

impl Lowerer<'_> {
    pub(super) fn apply_binary_op(
        &mut self,
        operator: BinaryOperator,
        left: Register,
        right: Register,
    ) -> Result<Register, String> {
        if self.instructions[left].ty == ValueType::StringRef {
            match operator {
                BinaryOperator::Add => {
                    return self.push(ValueType::StringRef, Operation::ConcatStrings(left, right));
                }
                BinaryOperator::Equal => {
                    return self.push(ValueType::Bool, Operation::EqualStrings(left, right));
                }
                BinaryOperator::NotEqual => {
                    let equal = self.push(ValueType::Bool, Operation::EqualStrings(left, right))?;
                    let false_value = self.push(ValueType::Bool, Operation::Constant(0))?;
                    return self.push(
                        ValueType::Bool,
                        Operation::Compare(Comparison::Equal, equal, false_value),
                    );
                }
                _ => return Err("unsupported string binary operator".into()),
            }
        }
        let operation = match operator {
            BinaryOperator::Add => Operation::Binary(Operator::Add, left, right),
            BinaryOperator::Subtract => Operation::Binary(Operator::Subtract, left, right),
            BinaryOperator::Multiply => Operation::Binary(Operator::Multiply, left, right),
            BinaryOperator::Divide => Operation::Binary(Operator::Divide, left, right),
            BinaryOperator::Remainder => Operation::Binary(Operator::Remainder, left, right),
            BinaryOperator::Equal => Operation::Compare(Comparison::Equal, left, right),
            BinaryOperator::NotEqual => Operation::Compare(Comparison::NotEqual, left, right),
            BinaryOperator::Less => Operation::Compare(Comparison::Less, left, right),
            BinaryOperator::LessEqual => Operation::Compare(Comparison::LessEqual, left, right),
            BinaryOperator::Greater => Operation::Compare(Comparison::Greater, left, right),
            BinaryOperator::GreaterEqual => {
                Operation::Compare(Comparison::GreaterEqual, left, right)
            }
            BinaryOperator::And | BinaryOperator::Or => unreachable!("short-circuit handled early"),
        };
        let ty = if matches!(operation, Operation::Compare(..)) {
            ValueType::Bool
        } else {
            ValueType::Int
        };
        self.push(ty, operation)
    }

    pub(super) fn lower_deep_assign(
        &mut self,
        base_reg: Register,
        fields: &[Name],
        operator: Option<BinaryOperator>,
        rhs_expr: &Expr,
    ) -> Result<Register, String> {
        if fields.is_empty() {
            let rhs = self.expression(rhs_expr)?;
            return match operator {
                None => Ok(rhs),
                Some(op) => self.apply_binary_op(op, base_reg, rhs),
            };
        }
        let ValueType::RecordRef(schema) = self.instructions[base_reg].ty else {
            return Err("expected struct value in field assignment".into());
        };
        let schema_fields = self.records[usize::from(schema)].fields.clone();
        let (target_idx, (_, target_ty)) = schema_fields
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == &fields[0].text)
            .ok_or("unknown struct field in assignment")?;
        let target_ty = *target_ty;

        let new_child = if fields.len() == 1 && operator.is_none() {
            self.expression(rhs_expr)?
        } else {
            let old_child = self.push(
                target_ty,
                Operation::RecordField {
                    value: base_reg,
                    field: target_idx,
                },
            )?;
            self.lower_deep_assign(old_child, &fields[1..], operator, rhs_expr)?
        };

        let mut new_args = Vec::with_capacity(schema_fields.len());
        for (idx, (_, f_ty)) in schema_fields.into_iter().enumerate() {
            if idx == target_idx {
                new_args.push(new_child);
            } else {
                new_args.push(self.push(
                    f_ty,
                    Operation::RecordField {
                        value: base_reg,
                        field: idx,
                    },
                )?);
            }
        }
        self.push(
            ValueType::RecordRef(schema),
            Operation::RecordNew {
                schema,
                arguments: new_args,
            },
        )
    }

    pub(super) fn statements(&mut self, statements: &[Statement]) -> Result<(), String> {
        for statement in statements {
            match statement {
                Statement::Let { name, value, .. } => {
                    let register = self.expression(value)?;
                    if name.text != "_" && !name.text.is_empty() {
                        self.bindings.insert(name.text.clone(), register);
                    }
                }
                Statement::Assign {
                    target,
                    operator,
                    value,
                } => {
                    let current = self.bindings[&target.base.text];
                    let updated =
                        self.lower_deep_assign(current, &target.fields, *operator, value)?;
                    self.bindings.insert(target.base.text.clone(), updated);
                }
                Statement::Expr(value) => {
                    self.expression(value)?;
                }
                Statement::While { condition, body } => {
                    let mut mutated = BTreeSet::new();
                    collect_assigned_expr(condition, &BTreeSet::new(), &mut mutated);
                    collect_assigned_block(body, &mut BTreeSet::new(), &mut mutated);
                    let carried: Vec<String> = mutated
                        .into_iter()
                        .filter(|k| self.bindings.contains_key(k))
                        .collect();
                    self.lower_loop_with(
                        &carried,
                        |this| this.expression(condition),
                        |this| this.unit_block(body),
                    )?;
                }
                Statement::ForRange {
                    binding,
                    start,
                    end,
                    inclusive,
                    body,
                    ..
                } => {
                    let start_reg = self.expression(start)?;
                    let end_reg = self.expression(end)?;
                    let cur_key = self.fresh_temp("for_cur");
                    self.bindings.insert(cur_key.clone(), start_reg);

                    let mut mutated = BTreeSet::new();
                    let mut shadowed = BTreeSet::new();
                    if binding.text != "_" && !binding.text.is_empty() {
                        shadowed.insert(binding.text.clone());
                    }
                    collect_assigned_block(body, &mut shadowed, &mut mutated);
                    let mut carried: Vec<String> = mutated
                        .into_iter()
                        .filter(|k| self.bindings.contains_key(k))
                        .collect();
                    carried.push(cur_key.clone());

                    if *inclusive {
                        let init_done = self.push(
                            ValueType::Bool,
                            Operation::Compare(Comparison::Greater, start_reg, end_reg),
                        )?;
                        let done_key = self.fresh_temp("for_done");
                        self.bindings.insert(done_key.clone(), init_done);
                        carried.push(done_key.clone());
                        carried.sort();

                        self.lower_loop_with(
                            &carried,
                            |this| {
                                let done = this.bindings[&done_key];
                                let zero = this.push(ValueType::Bool, Operation::Constant(0))?;
                                this.push(
                                    ValueType::Bool,
                                    Operation::Compare(Comparison::Equal, done, zero),
                                )
                            },
                            |this| {
                                let cur = this.bindings[&cur_key];
                                let saved_outer = this.bindings.get(&binding.text).copied();
                                if binding.text != "_" && !binding.text.is_empty() {
                                    this.bindings.insert(binding.text.clone(), cur);
                                }
                                this.unit_block(body)?;
                                if let Some(prev) = saved_outer {
                                    this.bindings.insert(binding.text.clone(), prev);
                                } else {
                                    this.bindings.remove(&binding.text);
                                }
                                let at_end = this.push(
                                    ValueType::Bool,
                                    Operation::Compare(Comparison::Equal, cur, end_reg),
                                )?;
                                let step_keys = vec![cur_key.clone(), done_key.clone()];
                                this.lower_branch_with(
                                    at_end,
                                    &step_keys,
                                    |b| {
                                        let t = b.push(ValueType::Bool, Operation::Constant(1))?;
                                        b.bindings.insert(done_key.clone(), t);
                                        b.push(ValueType::Unit, Operation::Constant(0))
                                    },
                                    |b| {
                                        let one = b.push(ValueType::Int, Operation::Constant(1))?;
                                        let next = b.push(
                                            ValueType::Int,
                                            Operation::Binary(Operator::Add, cur, one),
                                        )?;
                                        b.bindings.insert(cur_key.clone(), next);
                                        b.push(ValueType::Unit, Operation::Constant(0))
                                    },
                                )?;
                                Ok(())
                            },
                        )?;
                        self.bindings.remove(&done_key);
                    } else {
                        carried.sort();
                        self.lower_loop_with(
                            &carried,
                            |this| {
                                let cur = this.bindings[&cur_key];
                                this.push(
                                    ValueType::Bool,
                                    Operation::Compare(Comparison::Less, cur, end_reg),
                                )
                            },
                            |this| {
                                let cur = this.bindings[&cur_key];
                                let saved_outer = this.bindings.get(&binding.text).copied();
                                if binding.text != "_" && !binding.text.is_empty() {
                                    this.bindings.insert(binding.text.clone(), cur);
                                }
                                this.unit_block(body)?;
                                if let Some(prev) = saved_outer {
                                    this.bindings.insert(binding.text.clone(), prev);
                                } else {
                                    this.bindings.remove(&binding.text);
                                }
                                let one = this.push(ValueType::Int, Operation::Constant(1))?;
                                let next = this.push(
                                    ValueType::Int,
                                    Operation::Binary(Operator::Add, cur, one),
                                )?;
                                this.bindings.insert(cur_key.clone(), next);
                                Ok(())
                            },
                        )?;
                    }
                    self.bindings.remove(&cur_key);
                }
                Statement::ForList {
                    binding,
                    list,
                    body,
                    ..
                } => {
                    let list_reg = self.expression(list)?;
                    let cursor_key = self.fresh_temp("for_list");
                    self.bindings.insert(cursor_key.clone(), list_reg);

                    let mut mutated = BTreeSet::new();
                    let mut shadowed = BTreeSet::new();
                    if binding.text != "_" && !binding.text.is_empty() {
                        shadowed.insert(binding.text.clone());
                    }
                    collect_assigned_block(body, &mut shadowed, &mut mutated);
                    let mut carried: Vec<String> = mutated
                        .into_iter()
                        .filter(|k| self.bindings.contains_key(k))
                        .collect();
                    carried.push(cursor_key.clone());
                    carried.sort();

                    self.lower_loop_with(
                        &carried,
                        |this| {
                            let cur = this.bindings[&cursor_key];
                            let empty = this.push(ValueType::Bool, Operation::IsEmptyInts(cur))?;
                            let zero = this.push(ValueType::Bool, Operation::Constant(0))?;
                            this.push(
                                ValueType::Bool,
                                Operation::Compare(Comparison::Equal, empty, zero),
                            )
                        },
                        |this| {
                            let cur = this.bindings[&cursor_key];
                            let head = this.push(ValueType::Int, Operation::HeadInt(cur))?;
                            let tail = this.push(ValueType::ListRef, Operation::TailInts(cur))?;
                            this.bindings.insert(cursor_key.clone(), tail);
                            let saved_outer = this.bindings.get(&binding.text).copied();
                            if binding.text != "_" && !binding.text.is_empty() {
                                this.bindings.insert(binding.text.clone(), head);
                            }
                            this.unit_block(body)?;
                            if let Some(prev) = saved_outer {
                                this.bindings.insert(binding.text.clone(), prev);
                            } else {
                                this.bindings.remove(&binding.text);
                            }
                            Ok(())
                        },
                    )?;
                    self.bindings.remove(&cursor_key);
                }
            }
        }
        Ok(())
    }

    pub(super) fn lower_match_arms(
        &mut self,
        scrutinee: Register,
        arms: &[MatchArm],
    ) -> Result<Register, String> {
        let [first, rest @ ..] = arms else {
            return Err("empty match arms".into());
        };
        if rest.is_empty() || matches!(first.pattern, MatchPattern::Wildcard(_)) {
            return self.lower_match_arm_body(scrutinee, first);
        }

        let cond = match &first.pattern {
            MatchPattern::Wildcard(_) => unreachable!(),
            MatchPattern::Null(_) => {
                let target = self.push(ValueType::Pid, Operation::NullPid)?;
                self.push(
                    ValueType::Bool,
                    Operation::Compare(Comparison::Equal, scrutinee, target),
                )?
            }
            MatchPattern::Integer(val, _) => {
                let target = self.push(ValueType::Int, Operation::Constant(*val))?;
                self.push(
                    ValueType::Bool,
                    Operation::Compare(Comparison::Equal, scrutinee, target),
                )?
            }
            MatchPattern::Boolean(val, _) => {
                let target = self.push(ValueType::Bool, Operation::Constant(i64::from(*val)))?;
                self.push(
                    ValueType::Bool,
                    Operation::Compare(Comparison::Equal, scrutinee, target),
                )?
            }
            MatchPattern::String(lit, _) => {
                let target =
                    self.push(ValueType::StringRef, Operation::StringLiteral(lit.clone()))?;
                self.push(ValueType::Bool, Operation::EqualStrings(scrutinee, target))?
            }
            MatchPattern::EmptyList(_) => {
                self.push(ValueType::Bool, Operation::IsEmptyInts(scrutinee))?
            }
            MatchPattern::ConsList { .. } => {
                let empty = self.push(ValueType::Bool, Operation::IsEmptyInts(scrutinee))?;
                let zero = self.push(ValueType::Bool, Operation::Constant(0))?;
                self.push(
                    ValueType::Bool,
                    Operation::Compare(Comparison::Equal, empty, zero),
                )?
            }
        };

        let mut mutated = BTreeSet::new();
        collect_assigned_arm(first, &mut mutated);
        for arm in rest {
            collect_assigned_arm(arm, &mut mutated);
        }
        let mutated_keys: Vec<String> = mutated
            .into_iter()
            .filter(|k| self.bindings.contains_key(k))
            .collect();

        self.lower_branch_with(
            cond,
            &mutated_keys,
            |this| this.lower_match_arm_body(scrutinee, first),
            |this| this.lower_match_arms(scrutinee, rest),
        )
    }

    pub(super) fn lower_match_arm_body(
        &mut self,
        scrutinee: Register,
        arm: &MatchArm,
    ) -> Result<Register, String> {
        if let MatchPattern::ConsList { head, tail, .. } = &arm.pattern {
            let saved_head = self.bindings.get(&head.text).copied();
            let saved_tail = self.bindings.get(&tail.text).copied();
            if head.text != "_" && !head.text.is_empty() {
                let h = self.push(ValueType::Int, Operation::HeadInt(scrutinee))?;
                self.bindings.insert(head.text.clone(), h);
            }
            if tail.text != "_" && !tail.text.is_empty() {
                let t = self.push(ValueType::ListRef, Operation::TailInts(scrutinee))?;
                self.bindings.insert(tail.text.clone(), t);
            }
            let res = self.block(&arm.body)?;
            if let Some(prev) = saved_head {
                self.bindings.insert(head.text.clone(), prev);
            } else {
                self.bindings.remove(&head.text);
            }
            if let Some(prev) = saved_tail {
                self.bindings.insert(tail.text.clone(), prev);
            } else {
                self.bindings.remove(&tail.text);
            }
            Ok(res)
        } else {
            self.block(&arm.body)
        }
    }
}

pub(super) fn collect_assigned_block(
    block: &Block,
    shadowed: &mut BTreeSet<String>,
    out: &mut BTreeSet<String>,
) {
    let mut local_shadow = shadowed.clone();
    for statement in &block.statements {
        match statement {
            Statement::Let { name, value, .. } => {
                collect_assigned_expr(value, &local_shadow, out);
                if name.text != "_" && !name.text.is_empty() {
                    local_shadow.insert(name.text.clone());
                }
            }
            Statement::Assign { target, value, .. } => {
                collect_assigned_expr(value, &local_shadow, out);
                if !local_shadow.contains(&target.base.text) {
                    out.insert(target.base.text.clone());
                }
            }
            Statement::Expr(expr) => collect_assigned_expr(expr, &local_shadow, out),
            Statement::While { condition, body } => {
                collect_assigned_expr(condition, &local_shadow, out);
                collect_assigned_block(body, &mut local_shadow, out);
            }
            Statement::ForRange {
                binding,
                start,
                end,
                body,
                ..
            } => {
                collect_assigned_expr(start, &local_shadow, out);
                collect_assigned_expr(end, &local_shadow, out);
                let mut inner_shadow = local_shadow.clone();
                if binding.text != "_" && !binding.text.is_empty() {
                    inner_shadow.insert(binding.text.clone());
                }
                collect_assigned_block(body, &mut inner_shadow, out);
            }
            Statement::ForList {
                binding,
                list,
                body,
                ..
            } => {
                collect_assigned_expr(list, &local_shadow, out);
                let mut inner_shadow = local_shadow.clone();
                if binding.text != "_" && !binding.text.is_empty() {
                    inner_shadow.insert(binding.text.clone());
                }
                collect_assigned_block(body, &mut inner_shadow, out);
            }
        }
    }
    if let Some(tail) = &block.tail {
        collect_assigned_expr(tail, &local_shadow, out);
    }
}

pub(super) fn collect_assigned_arm(arm: &MatchArm, out: &mut BTreeSet<String>) {
    let mut shadowed = BTreeSet::new();
    if let MatchPattern::ConsList { head, tail, .. } = &arm.pattern {
        if head.text != "_" && !head.text.is_empty() {
            shadowed.insert(head.text.clone());
        }
        if tail.text != "_" && !tail.text.is_empty() {
            shadowed.insert(tail.text.clone());
        }
    }
    collect_assigned_block(&arm.body, &mut shadowed, out);
}

pub(super) fn collect_assigned_expr(
    expr: &Expr,
    shadowed: &BTreeSet<String>,
    out: &mut BTreeSet<String>,
) {
    match &expr.kind {
        ExprKind::InterpolatedString(parts) => {
            for part in parts {
                if let InterpolatedPart::Expr(inner) = part {
                    collect_assigned_expr(inner, shadowed, out);
                }
            }
        }
        ExprKind::List { elements, tail } => {
            for elem in elements {
                collect_assigned_expr(elem, shadowed, out);
            }
            if let Some(t) = tail {
                collect_assigned_expr(t, shadowed, out);
            }
        }
        ExprKind::StructInit { base, fields, .. } => {
            for (_, val) in fields {
                collect_assigned_expr(val, shadowed, out);
            }
            if let Some(b) = base {
                collect_assigned_expr(b, shadowed, out);
            }
        }
        ExprKind::Field { value, .. } | ExprKind::Negate(value) | ExprKind::Not(value) => {
            collect_assigned_expr(value, shadowed, out);
        }
        ExprKind::MethodCall {
            receiver,
            arguments,
            ..
        } => {
            collect_assigned_expr(receiver, shadowed, out);
            for arg in arguments {
                collect_assigned_expr(arg, shadowed, out);
            }
        }
        ExprKind::StaticCall { arguments, .. } | ExprKind::Call { arguments, .. } => {
            for arg in arguments {
                collect_assigned_expr(arg, shadowed, out);
            }
        }
        ExprKind::Send {
            target, arguments, ..
        } => {
            collect_assigned_expr(target, shadowed, out);
            for arg in arguments {
                collect_assigned_expr(arg, shadowed, out);
            }
        }
        ExprKind::SendAfter {
            target,
            delay,
            arguments,
            ..
        } => {
            collect_assigned_expr(target, shadowed, out);
            collect_assigned_expr(delay, shadowed, out);
            for arg in arguments {
                collect_assigned_expr(arg, shadowed, out);
            }
        }
        ExprKind::Monitor { target, .. } => {
            collect_assigned_expr(target, shadowed, out);
        }
        ExprKind::Ask {
            target,
            arguments,
            timeout,
            ..
        } => {
            collect_assigned_expr(target, shadowed, out);
            collect_assigned_expr(timeout, shadowed, out);
            for arg in arguments {
                collect_assigned_expr(arg, shadowed, out);
            }
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_assigned_expr(condition, shadowed, out);
            collect_assigned_block(then_branch, &mut shadowed.clone(), out);
            if let Some(else_blk) = else_branch {
                collect_assigned_block(else_blk, &mut shadowed.clone(), out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_assigned_expr(scrutinee, shadowed, out);
            for arm in arms {
                let mut arm_shadow = shadowed.clone();
                if let MatchPattern::ConsList { head, tail, .. } = &arm.pattern {
                    if head.text != "_" && !head.text.is_empty() {
                        arm_shadow.insert(head.text.clone());
                    }
                    if tail.text != "_" && !tail.text.is_empty() {
                        arm_shadow.insert(tail.text.clone());
                    }
                }
                collect_assigned_block(&arm.body, &mut arm_shadow, out);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_assigned_expr(left, shadowed, out);
            collect_assigned_expr(right, shadowed, out);
        }
        _ => {}
    }
}
