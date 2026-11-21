use super::*;

impl Lowerer<'_> {
    pub(super) fn expression(&mut self, expression: &Expr) -> Result<Register, String> {
        if self.expression_depth == 256 {
            return Err("expanded expression depth exceeds 256".into());
        }
        self.expression_depth += 1;
        let result = self.expression_inner(expression);
        self.expression_depth -= 1;
        result
    }

    fn expression_inner(&mut self, expression: &Expr) -> Result<Register, String> {
        match &expression.kind {
            ExprKind::Integer(value) => self.push(ValueType::Int, Operation::Constant(*value)),
            ExprKind::String(value) => self.push(
                ValueType::StringRef,
                Operation::StringLiteral(value.clone()),
            ),
            ExprKind::InterpolatedString(parts) => {
                let mut part_regs = Vec::with_capacity(parts.len());
                for part in parts {
                    let str_reg = match part {
                        InterpolatedPart::Literal(lit) => {
                            self.push(ValueType::StringRef, Operation::StringLiteral(lit.clone()))?
                        }
                        InterpolatedPart::Expr(inner) => {
                            let val = self.expression(inner)?;
                            match self.instructions[val].ty {
                                ValueType::StringRef => val,
                                ValueType::Int => {
                                    self.push(ValueType::StringRef, Operation::IntToString(val))?
                                }
                                ValueType::Bool => {
                                    self.push(ValueType::StringRef, Operation::BoolToString(val))?
                                }
                                _ => return Err("unsupported interpolated value type".into()),
                            }
                        }
                    };
                    part_regs.push(str_reg);
                }
                let Some((&first, rest)) = part_regs.split_first() else {
                    return self.push(
                        ValueType::StringRef,
                        Operation::StringLiteral(String::new()),
                    );
                };
                let mut acc = first;
                for &next in rest {
                    acc = self.push(ValueType::StringRef, Operation::ConcatStrings(acc, next))?;
                }
                Ok(acc)
            }
            ExprKind::Bytes(bytes) => {
                self.push(ValueType::BytesRef, Operation::BytesLiteral(bytes.clone()))
            }
            ExprKind::Boolean(value) => {
                self.push(ValueType::Bool, Operation::Constant(i64::from(*value)))
            }
            ExprKind::Unit => self.push(ValueType::Unit, Operation::Constant(0)),
            ExprKind::Null => self.push(ValueType::Pid, Operation::NullPid),
            ExprKind::SelfPid => self.push(ValueType::Pid, Operation::SelfPid),
            ExprKind::Spawn(target_actor) => match target_actor {
                None => self.push(ValueType::Pid, Operation::SpawnSelf),
                Some(name) if name.text == self.actor_name => {
                    self.push(ValueType::Pid, Operation::SpawnSelf)
                }
                Some(name) => {
                    let idx = self
                        .external_actors
                        .iter()
                        .position(|ext| ext.name == name.text)
                        .ok_or_else(|| {
                            format!(
                                "cross-actor spawn('{}') requires multi-actor build ('wings-compile build-app')",
                                name.text
                            )
                        })?;
                    self.push(ValueType::Pid, Operation::SpawnActor(idx))
                }
            },
            ExprKind::Name(name) => Ok(self.bindings[&name.text]),
            ExprKind::List { elements, tail } => {
                let mut elem_regs = Vec::with_capacity(elements.len());
                for elem in elements {
                    elem_regs.push(self.expression(elem)?);
                }
                let mut acc = match tail {
                    Some(tail_expr) => self.expression(tail_expr)?,
                    None => self.push(ValueType::ListRef, Operation::EmptyInts)?,
                };
                for &elem in elem_regs.iter().rev() {
                    acc = self.push(ValueType::ListRef, Operation::PrependInt(elem, acc))?;
                }
                Ok(acc)
            }
            ExprKind::StructInit { name, base, fields } => {
                let schema = self
                    .records
                    .iter()
                    .position(|r| r.name == name.text)
                    .ok_or("unknown struct schema")?;
                let mut evaluated_fields = HashMap::with_capacity(fields.len());
                for (field_name, field_expr) in fields {
                    let reg = self.expression(field_expr)?;
                    evaluated_fields.insert(field_name.text.as_str(), reg);
                }
                let base_reg = match base {
                    Some(base_expr) => Some(self.expression(base_expr)?),
                    None => None,
                };
                let schema_fields = self.records[schema].fields.clone();
                let mut arguments = Vec::with_capacity(schema_fields.len());
                for (idx, (field_name, field_ty)) in schema_fields.into_iter().enumerate() {
                    if let Some(&reg) = evaluated_fields.get(field_name.as_str()) {
                        arguments.push(reg);
                    } else if let Some(base_val) = base_reg {
                        arguments.push(self.push(
                            field_ty,
                            Operation::RecordField {
                                value: base_val,
                                field: idx,
                            },
                        )?);
                    } else {
                        return Err(format!("missing field '{field_name}' in struct literal"));
                    }
                }
                self.push(
                    ValueType::RecordRef(schema as u16),
                    Operation::RecordNew {
                        schema: schema as u16,
                        arguments,
                    },
                )
            }
            ExprKind::Field { value, name } => {
                let value = self.expression(value)?;
                let ValueType::RecordRef(schema) = self.instructions[value].ty else {
                    return Err("expected struct value".into());
                };
                let (field, (_, ty)) = self.records[usize::from(schema)]
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, (field, _))| field == &name.text)
                    .ok_or("unknown struct field")?;
                self.push(*ty, Operation::RecordField { value, field })
            }
            ExprKind::StaticCall {
                namespace,
                method,
                arguments,
            } => match (namespace.text.as_str(), method.text.as_str()) {
                ("bytes", "fill") => {
                    let len = self.expression(&arguments[0])?;
                    let byte = self.expression(&arguments[1])?;
                    self.push(ValueType::BytesRef, Operation::FillBytes(len, byte))
                }
                _ => Err("unsupported static call".into()),
            },
            ExprKind::MethodCall {
                receiver,
                method,
                arguments,
            } => {
                let recv = self.expression(receiver)?;
                let recv_ty = self.instructions[recv].ty;
                let args: Vec<Register> = arguments
                    .iter()
                    .map(|a| self.expression(a))
                    .collect::<Result<_, _>>()?;
                match (recv_ty, method.text.as_str()) {
                    (ValueType::Int, "to_str") => {
                        self.push(ValueType::StringRef, Operation::IntToString(recv))
                    }
                    (ValueType::Bool, "to_str") => {
                        self.push(ValueType::StringRef, Operation::BoolToString(recv))
                    }
                    (ValueType::ListRef, "is_empty") => {
                        self.push(ValueType::Bool, Operation::IsEmptyInts(recv))
                    }
                    (ValueType::ListRef, "head") => {
                        self.push(ValueType::Int, Operation::HeadInt(recv))
                    }
                    (ValueType::ListRef, "tail") => {
                        self.push(ValueType::ListRef, Operation::TailInts(recv))
                    }
                    (ValueType::ListRef, "push") => {
                        self.push(ValueType::ListRef, Operation::PrependInt(args[0], recv))
                    }
                    (ValueType::BytesRef, "len") => {
                        self.push(ValueType::Int, Operation::BytesLen(recv))
                    }
                    (ValueType::BytesRef, "is_empty") => {
                        let len = self.push(ValueType::Int, Operation::BytesLen(recv))?;
                        let zero = self.push(ValueType::Int, Operation::Constant(0))?;
                        self.push(
                            ValueType::Bool,
                            Operation::Compare(Comparison::Equal, len, zero),
                        )
                    }
                    (ValueType::BytesRef, "at") => {
                        self.push(ValueType::Int, Operation::ByteAt(recv, args[0]))
                    }
                    (ValueType::BytesRef, "set") => self.push(
                        ValueType::BytesRef,
                        Operation::ReplaceByte([recv, args[0], args[1]]),
                    ),
                    (ValueType::BytesRef, "i64_at") => {
                        self.push(ValueType::Int, Operation::ReadI64Be(recv, args[0]))
                    }
                    (ValueType::BytesRef, "set_i64") => self.push(
                        ValueType::BytesRef,
                        Operation::ReplaceI64Be([recv, args[0], args[1]]),
                    ),
                    (ValueType::BytesRef, "to_str") => {
                        self.push(ValueType::StringRef, Operation::StringFromBytes(recv))
                    }
                    (ValueType::StringRef, "len") => {
                        self.push(ValueType::Int, Operation::StringLen(recv))
                    }
                    (ValueType::StringRef, "is_empty") => {
                        let len = self.push(ValueType::Int, Operation::StringLen(recv))?;
                        let zero = self.push(ValueType::Int, Operation::Constant(0))?;
                        self.push(
                            ValueType::Bool,
                            Operation::Compare(Comparison::Equal, len, zero),
                        )
                    }
                    (ValueType::StringRef, "at") => {
                        self.push(ValueType::Int, Operation::StringAt(recv, args[0]))
                    }
                    (ValueType::StringRef, "slice") => self.push(
                        ValueType::StringRef,
                        Operation::StringSlice([recv, args[0], args[1]]),
                    ),
                    (ValueType::StringRef, "bytes") => {
                        self.push(ValueType::BytesRef, Operation::StringBytes(recv))
                    }
                    (ValueType::Pid, "is_null") => {
                        let null_reg = self.push(ValueType::Pid, Operation::NullPid)?;
                        self.push(
                            ValueType::Bool,
                            Operation::Compare(Comparison::Equal, recv, null_reg),
                        )
                    }
                    (ValueType::RequestId, "cancel") => {
                        self.push(ValueType::Unit, Operation::CancelRequest(recv))
                    }
                    _ => Err(format!("unsupported method '{}'", method.text)),
                }
            }
            ExprKind::Send {
                target,
                message,
                arguments,
            } => {
                let target_actor = self
                    .target_actors
                    .and_then(|map| map.get(&(expression.span.start, expression.span.end)))
                    .map(String::as_str)
                    .unwrap_or(self.actor_name);
                if target_actor == self.actor_name {
                    let handler = *self
                        .handlers
                        .get(message.text.as_str())
                        .ok_or("unknown send target")?;
                    if matches!(target.kind, ExprKind::SelfPid) {
                        let arg_regs: Vec<Register> = arguments
                            .iter()
                            .map(|a| self.expression(a))
                            .collect::<Result<_, _>>()?;
                        let flattened = self.message_arguments(arg_regs)?;
                        self.push(
                            ValueType::Unit,
                            Operation::SendSelf {
                                handler,
                                arguments: flattened,
                            },
                        )
                    } else {
                        let target_reg = self.expression(target)?;
                        let arg_regs: Vec<Register> = arguments
                            .iter()
                            .map(|a| self.expression(a))
                            .collect::<Result<_, _>>()?;
                        let flattened = self.message_arguments(arg_regs)?;
                        self.push(
                            ValueType::Unit,
                            Operation::Send {
                                target: target_reg,
                                handler,
                                arguments: flattened,
                            },
                        )
                    }
                } else {
                    let (actor_idx, ext_actor) = self
                        .external_actors
                        .iter()
                        .enumerate()
                        .find(|(_, ext)| ext.name == target_actor)
                        .ok_or("unknown external send actor")?;
                    let handler = ext_actor
                        .handlers
                        .iter()
                        .position(|h| h.name == message.text)
                        .ok_or("unknown external send handler")?;
                    let target_reg = self.expression(target)?;
                    let arg_regs: Vec<Register> = arguments
                        .iter()
                        .map(|a| self.expression(a))
                        .collect::<Result<_, _>>()?;
                    let flattened = self.message_arguments(arg_regs)?;
                    self.push(
                        ValueType::Unit,
                        Operation::SendActor {
                            target: target_reg,
                            actor: actor_idx,
                            handler,
                            arguments: flattened,
                        },
                    )
                }
            }
            ExprKind::SendAfter {
                target,
                delay,
                message,
                arguments,
            } => {
                let target_actor = self
                    .target_actors
                    .and_then(|map| map.get(&(expression.span.start, expression.span.end)))
                    .map(String::as_str)
                    .unwrap_or(self.actor_name);
                if target_actor == self.actor_name {
                    let handler = *self
                        .handlers
                        .get(message.text.as_str())
                        .ok_or("unknown send_after target")?;
                    if matches!(target.kind, ExprKind::SelfPid) {
                        let delay_reg = self.expression(delay)?;
                        let arg_regs: Vec<Register> = arguments
                            .iter()
                            .map(|a| self.expression(a))
                            .collect::<Result<_, _>>()?;
                        let flattened = self.message_arguments(arg_regs)?;
                        self.push(
                            ValueType::Unit,
                            Operation::SendAfterSelf {
                                handler,
                                delay: delay_reg,
                                arguments: flattened,
                            },
                        )
                    } else {
                        let target_reg = self.expression(target)?;
                        let delay_reg = self.expression(delay)?;
                        let arg_regs: Vec<Register> = arguments
                            .iter()
                            .map(|a| self.expression(a))
                            .collect::<Result<_, _>>()?;
                        let flattened = self.message_arguments(arg_regs)?;
                        self.push(
                            ValueType::Unit,
                            Operation::SendAfter {
                                target: target_reg,
                                handler,
                                delay: delay_reg,
                                arguments: flattened,
                            },
                        )
                    }
                } else {
                    let (actor_idx, ext_actor) = self
                        .external_actors
                        .iter()
                        .enumerate()
                        .find(|(_, ext)| ext.name == target_actor)
                        .ok_or("unknown external send_after actor")?;
                    let handler = ext_actor
                        .handlers
                        .iter()
                        .position(|h| h.name == message.text)
                        .ok_or("unknown external send_after handler")?;
                    let target_reg = self.expression(target)?;
                    let delay_reg = self.expression(delay)?;
                    let arg_regs: Vec<Register> = arguments
                        .iter()
                        .map(|a| self.expression(a))
                        .collect::<Result<_, _>>()?;
                    let flattened = self.message_arguments(arg_regs)?;
                    self.push(
                        ValueType::Unit,
                        Operation::SendAfterActor {
                            target: target_reg,
                            actor: actor_idx,
                            handler,
                            delay: delay_reg,
                            arguments: flattened,
                        },
                    )
                }
            }
            ExprKind::Monitor { target, handler } => {
                let target_reg = self.expression(target)?;
                let handler_idx = *self
                    .handlers
                    .get(handler.text.as_str())
                    .ok_or("unknown monitor handler")?;
                self.push(
                    ValueType::Unit,
                    Operation::Monitor {
                        target: target_reg,
                        handler: handler_idx,
                    },
                )
            }
            ExprKind::Ask {
                target,
                message,
                arguments,
                timeout,
                then,
            } => {
                let target_actor = self
                    .target_actors
                    .and_then(|map| map.get(&(expression.span.start, expression.span.end)))
                    .map(String::as_str)
                    .unwrap_or(self.actor_name);
                let target_reg = self.expression(target)?;
                let callback = *self
                    .handlers
                    .get(then.text.as_str())
                    .ok_or("unknown completion handler")?;
                let timeout_reg = self.expression(timeout)?;
                let arg_regs: Vec<Register> = arguments
                    .iter()
                    .map(|a| self.expression(a))
                    .collect::<Result<_, _>>()?;
                let flattened = self.message_arguments(arg_regs)?;
                if target_actor == self.actor_name {
                    let handler = *self
                        .handlers
                        .get(message.text.as_str())
                        .ok_or("unknown request message")?;
                    self.push(
                        ValueType::RequestId,
                        Operation::Request {
                            target: target_reg,
                            handler,
                            callback,
                            timeout: timeout_reg,
                            arguments: flattened,
                        },
                    )
                } else {
                    let (actor_idx, ext_actor) = self
                        .external_actors
                        .iter()
                        .enumerate()
                        .find(|(_, ext)| ext.name == target_actor)
                        .ok_or("unknown external request actor")?;
                    let handler = ext_actor
                        .handlers
                        .iter()
                        .position(|h| h.name == message.text)
                        .ok_or("unknown external request handler")?;
                    self.push(
                        ValueType::RequestId,
                        Operation::RequestActor {
                            target: target_reg,
                            actor: actor_idx,
                            handler,
                            callback,
                            timeout: timeout_reg,
                            arguments: flattened,
                        },
                    )
                }
            }
            ExprKind::Call { callee, arguments } => {
                if let Some(schema) = self
                    .records
                    .iter()
                    .position(|record| record.name == callee.text)
                {
                    let arguments = arguments
                        .iter()
                        .map(|value| self.expression(value))
                        .collect::<Result<Vec<_>, _>>()?;
                    return self.push(
                        ValueType::RecordRef(schema as u16),
                        Operation::RecordNew {
                            schema: schema as u16,
                            arguments,
                        },
                    );
                }
                if let Some((function, result)) = self.functions.get(callee.text.as_str()).copied()
                {
                    let values = arguments
                        .iter()
                        .map(|argument| self.expression(argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    return self.push(
                        result,
                        Operation::Call {
                            function,
                            arguments: values,
                        },
                    );
                }
                match callee.text.as_str() {
                    "gc" => self.push(ValueType::Unit, Operation::Collect),
                    _ => Err(format!("unsupported builtin call '{}'", callee.text)),
                }
            }
            ExprKind::Negate(value) => {
                let register = self.expression(value)?;
                self.push(ValueType::Int, Operation::Negate(register))
            }
            ExprKind::Not(value) => {
                let register = self.expression(value)?;
                let zero = self.push(ValueType::Bool, Operation::Constant(0))?;
                self.push(
                    ValueType::Bool,
                    Operation::Compare(Comparison::Equal, register, zero),
                )
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let cond = self.expression(condition)?;
                let mut mutated = BTreeSet::new();
                collect_assigned_block(then_branch, &mut BTreeSet::new(), &mut mutated);
                if let Some(else_blk) = else_branch {
                    collect_assigned_block(else_blk, &mut BTreeSet::new(), &mut mutated);
                }
                let mutated_keys: Vec<String> = mutated
                    .into_iter()
                    .filter(|k| self.bindings.contains_key(k))
                    .collect();
                self.lower_branch_with(
                    cond,
                    &mutated_keys,
                    |this| this.block(then_branch),
                    |this| match else_branch {
                        Some(else_blk) => this.block(else_blk),
                        None => this.push(ValueType::Unit, Operation::Constant(0)),
                    },
                )
            }
            ExprKind::Match { scrutinee, arms } => {
                let scrut = self.expression(scrutinee)?;
                self.lower_match_arms(scrut, arms)
            }
            ExprKind::Binary {
                operator,
                left,
                right,
            } => {
                if matches!(operator, BinaryOperator::And | BinaryOperator::Or) {
                    let left_reg = self.expression(left)?;
                    let mut mutated = BTreeSet::new();
                    collect_assigned_expr(right, &BTreeSet::new(), &mut mutated);
                    let mutated_keys: Vec<String> = mutated
                        .into_iter()
                        .filter(|k| self.bindings.contains_key(k))
                        .collect();
                    if *operator == BinaryOperator::And {
                        return self.lower_branch_with(
                            left_reg,
                            &mutated_keys,
                            |this| this.expression(right),
                            |this| this.push(ValueType::Bool, Operation::Constant(0)),
                        );
                    } else {
                        return self.lower_branch_with(
                            left_reg,
                            &mutated_keys,
                            |this| this.push(ValueType::Bool, Operation::Constant(1)),
                            |this| this.expression(right),
                        );
                    }
                }
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                self.apply_binary_op(*operator, left, right)
            }
            ExprKind::Error => Err("cannot lower an invalid expression".into()),
        }
    }
}
