use std::collections::{BTreeMap, BTreeSet, HashMap};

use wings_abi::ValueType;
use wings_ir::{
    Comparison, Function, Handler, Instruction, MAX_OPERATIONS, Operation, Operator, Procedure,
    Program, Register,
};
use wings_sema::Type;
use wings_syntax::ast::{
    BinaryOperator, Block, Expr, ExprKind, InterpolatedPart, MatchArm, MatchPattern, Name,
    Statement, TypeExpr, TypeKind,
};

use crate::check;

pub fn lower(text: &str) -> Result<Program, String> {
    let analysis = check(text);
    if !analysis.diagnostics.is_empty() {
        return Err(crate::render_diagnostics("<source>", text, &analysis));
    }
    let [actor] = analysis.parsed.program.actors.as_slice() else {
        return Err("scalar AOT requires exactly one actor per source file".into());
    };
    let resolved_states = &analysis.actor_states[0];

    let mut records = Vec::new();
    for decl in &actor.structs {
        let fields = decl
            .fields
            .iter()
            .map(|field| {
                Ok((
                    field.name.text.clone(),
                    resolve_value_type(&field.ty, &records)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        records.push(wings_abi::RecordSchema {
            name: decl.name.text.clone(),
            fields,
        });
    }

    let state_fields: Vec<(String, ValueType)> = resolved_states
        .iter()
        .map(|(name, ty)| Ok((name.clone(), sema_type_to_value_type(ty, &records)?)))
        .collect::<Result<_, String>>()?;

    let (state_type, multi_state_schema) = match state_fields.as_slice() {
        [] => (ValueType::Unit, None),
        [(_, ty)] => (*ty, None),
        many => {
            let schema_idx = records.len() as u16;
            records.push(wings_abi::RecordSchema {
                name: "$State".into(),
                fields: many.to_vec(),
            });
            (ValueType::RecordRef(schema_idx), Some(schema_idx))
        }
    };

    let functions: HashMap<&str, (usize, ValueType)> = actor
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            let ret = match &function.result {
                Some(ty) => resolve_value_type(ty, &records)?,
                None => ValueType::Unit,
            };
            Ok((function.name.text.as_str(), (index, ret)))
        })
        .collect::<Result<_, String>>()?;

    let mut procedures = Vec::new();
    for function in &actor.functions {
        let mut lowerer = Lowerer {
            functions: functions.clone(),
            records: &records,
            in_function: true,
            state_name: None,
            ..Lowerer::default()
        };
        let state = lowerer.push(ValueType::Unit, Operation::Constant(0))?;
        let parameters = function
            .parameters
            .iter()
            .map(|parameter| {
                Ok((
                    parameter.name.text.clone(),
                    resolve_value_type(&parameter.ty, &records)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        for (index, (name, ty)) in parameters.iter().enumerate() {
            let register = lowerer.push(*ty, Operation::Argument(index))?;
            if name != "_" {
                lowerer.bindings.insert(name.clone(), register);
            }
        }
        let reply = lowerer.block(&function.body)?;
        let result_ty = match &function.result {
            Some(ty) => resolve_value_type(ty, &records)?,
            None => ValueType::Unit,
        };
        procedures.push(Procedure {
            name: function.name.text.clone(),
            parameters,
            result: result_ty,
            function: Function {
                instructions: lowerer.instructions,
                state,
                reply,
            },
        });
    }

    let handler_ids: HashMap<&str, usize> = actor
        .handlers
        .iter()
        .enumerate()
        .map(|(index, handler)| (handler.name.text.as_str(), index))
        .collect();

    let mut init_lowerer = Lowerer {
        functions: functions.clone(),
        records: &records,
        handlers: handler_ids.clone(),
        in_function: false,
        state_name: None,
        ..Lowerer::default()
    };
    let initial_state = match (actor.states.as_slice(), multi_state_schema) {
        ([], None) => init_lowerer.push(ValueType::Unit, Operation::Constant(0))?,
        ([single], None) => init_lowerer.expression(&single.default)?,
        (many, Some(schema)) => {
            let mut init_args = Vec::with_capacity(many.len());
            for state_decl in many {
                let reg = init_lowerer.expression(&state_decl.default)?;
                if state_decl.name.text != "_" {
                    init_lowerer
                        .bindings
                        .insert(state_decl.name.text.clone(), reg);
                }
                init_args.push(reg);
            }
            init_lowerer.push(
                ValueType::RecordRef(schema),
                Operation::RecordNew {
                    schema,
                    arguments: init_args,
                },
            )?
        }
        _ => unreachable!(),
    };
    let initial_reply = init_lowerer.push(ValueType::Unit, Operation::Constant(0))?;
    let initializer = Function {
        instructions: init_lowerer.instructions,
        state: initial_state,
        reply: initial_reply,
    };

    let mut handlers = Vec::new();
    for handler in &actor.handlers {
        if handler.parameters.len() > 64 {
            return Err("too many parameters: message exceeds 64 source parameters".into());
        }
        let single_state_name = if state_fields.len() == 1 {
            Some(state_fields[0].0.clone())
        } else {
            None
        };
        let mut lowerer = Lowerer {
            functions: functions.clone(),
            records: &records,
            handlers: handler_ids.clone(),
            in_function: false,
            state_name: single_state_name,
            ..Lowerer::default()
        };
        let raw_state = lowerer.push(state_type, Operation::State)?;
        let mut initial_state_regs = Vec::new();
        match (state_fields.as_slice(), multi_state_schema) {
            ([], None) => {}
            ([(name, _)], None) => {
                lowerer.bindings.insert(name.clone(), raw_state);
            }
            (many, Some(_)) => {
                for (field_idx, (name, ty)) in many.iter().enumerate() {
                    let field_reg = lowerer.push(
                        *ty,
                        Operation::RecordField {
                            value: raw_state,
                            field: field_idx,
                        },
                    )?;
                    lowerer.bindings.insert(name.clone(), field_reg);
                    initial_state_regs.push(field_reg);
                }
            }
            _ => unreachable!(),
        }

        let source_parameters = handler
            .parameters
            .iter()
            .map(|parameter| {
                Ok((
                    parameter.name.text.clone(),
                    resolve_value_type(&parameter.ty, &records)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let expanded = source_parameters
            .iter()
            .any(|(_, ty)| matches!(ty, ValueType::RecordRef(_)));
        let completion = if handler.is_reply && source_parameters.len() == 3 {
            match source_parameters[2].1 {
                ValueType::RecordRef(index)
                    if source_parameters[0].1 == ValueType::RequestId
                        && source_parameters[1].1 == ValueType::Int =>
                {
                    Some(ValueType::RecordRef(index))
                }
                _ => None,
            }
        } else {
            None
        };

        let mut parameters = Vec::new();
        let mut nodes = 0;
        for (index, (name, ty)) in source_parameters.iter().enumerate() {
            if completion.is_some() && index == 2 {
                nodes = 0;
            }
            let register =
                lowerer.message_parameter(*ty, name, expanded, &mut parameters, &mut nodes)?;
            if name != "_" {
                lowerer.bindings.insert(name.clone(), register);
            }
        }

        let reply = lowerer.block(&handler.body)?;

        let final_state = match (state_fields.as_slice(), multi_state_schema) {
            ([], None) => raw_state,
            ([(name, _)], None) => lowerer.bindings[name],
            (many, Some(schema)) => {
                let current_regs: Vec<Register> = many
                    .iter()
                    .map(|(name, _)| lowerer.bindings[name])
                    .collect();
                if current_regs == initial_state_regs {
                    raw_state
                } else {
                    lowerer.push(
                        ValueType::RecordRef(schema),
                        Operation::RecordNew {
                            schema,
                            arguments: current_regs,
                        },
                    )?
                }
            }
            _ => unreachable!(),
        };

        handlers.push(Handler {
            name: handler.name.text.clone(),
            parameters,
            reply: handler
                .reply_type
                .as_ref()
                .map(|ty| resolve_value_type(ty, &records))
                .transpose()?,
            completion,
            function: Function {
                instructions: lowerer.instructions,
                state: final_state,
                reply,
            },
        });
    }

    let program = Program {
        records,
        name: actor.name.text.clone(),
        state: state_type,
        initializer,
        functions: procedures,
        handlers,
    };
    program.validate()?;
    Ok(program)
}

#[derive(Default)]
struct Lowerer<'program> {
    records: &'program [wings_abi::RecordSchema],
    bindings: BTreeMap<String, Register>,
    instructions: Vec<Instruction>,
    functions: HashMap<&'program str, (usize, ValueType)>,
    expression_depth: usize,
    handlers: HashMap<&'program str, usize>,
    in_function: bool,
    state_name: Option<String>,
    temp_counter: usize,
}

impl Lowerer<'_> {
    fn fresh_temp(&mut self, prefix: &str) -> String {
        let id = self.temp_counter;
        self.temp_counter += 1;
        format!("${prefix}_{id}")
    }

    fn message_parameter(
        &mut self,
        ty: ValueType,
        name: &str,
        expanded: bool,
        parameters: &mut Vec<(String, ValueType)>,
        nodes: &mut usize,
    ) -> Result<Register, String> {
        *nodes += 1;
        if *nodes > 64 {
            return Err("message struct expansion exceeds 64 total nodes".into());
        }
        if let ValueType::RecordRef(schema) = ty {
            let fields = self.records[usize::from(schema)].fields.clone();
            let mut arguments = Vec::new();
            for (field, ty) in fields {
                arguments.push(self.message_parameter(ty, &field, expanded, parameters, nodes)?);
            }
            self.push(ty, Operation::RecordNew { schema, arguments })
        } else {
            let index = parameters.len();
            parameters.push((
                if expanded {
                    format!("field_{index}")
                } else {
                    name.into()
                },
                ty,
            ));
            self.push(ty, Operation::Argument(index))
        }
    }

    fn message_arguments(&mut self, arguments: Vec<Register>) -> Result<Vec<Register>, String> {
        let mut flattened = Vec::new();
        let mut nodes = 0;
        for value in arguments {
            self.message_argument(value, &mut flattened, &mut nodes)?;
        }
        Ok(flattened)
    }

    fn message_argument(
        &mut self,
        value: Register,
        flattened: &mut Vec<Register>,
        nodes: &mut usize,
    ) -> Result<(), String> {
        *nodes += 1;
        if *nodes > 64 {
            return Err("message struct expansion exceeds 64 total nodes".into());
        }
        if let ValueType::RecordRef(schema) = self.instructions[value].ty {
            let fields = self.records[usize::from(schema)].fields.clone();
            for (field, (_, ty)) in fields.into_iter().enumerate() {
                let field_reg = self.push(ty, Operation::RecordField { value, field })?;
                self.message_argument(field_reg, flattened, nodes)?;
            }
        } else {
            flattened.push(value);
        }
        Ok(())
    }

    fn push(&mut self, ty: ValueType, operation: Operation) -> Result<Register, String> {
        if self.instructions.len() >= MAX_OPERATIONS {
            return Err("scalar AOT exceeds 4096 operations per entry".into());
        }
        let register = self.instructions.len();
        self.instructions.push(Instruction { ty, operation });
        Ok(register)
    }

    fn block(&mut self, block: &Block) -> Result<Register, String> {
        let outer_keys: BTreeSet<String> = self.bindings.keys().cloned().collect();
        self.statements(&block.statements)?;
        let result = match &block.tail {
            Some(tail) => self.expression(tail)?,
            None => self.push(ValueType::Unit, Operation::Constant(0))?,
        };
        self.bindings.retain(|key, _| outer_keys.contains(key));
        Ok(result)
    }

    fn unit_block(&mut self, block: &Block) -> Result<(), String> {
        let outer_keys: BTreeSet<String> = self.bindings.keys().cloned().collect();
        self.statements(&block.statements)?;
        if let Some(tail) = &block.tail {
            self.expression(tail)?;
        }
        self.bindings.retain(|key, _| outer_keys.contains(key));
        Ok(())
    }

    fn lower_loop_with<FCond, FBody>(
        &mut self,
        carried_keys: &[String],
        mut emit_cond: FCond,
        mut emit_body: FBody,
    ) -> Result<(), String>
    where
        FCond: FnMut(&mut Self) -> Result<Register, String>,
        FBody: FnMut(&mut Self) -> Result<(), String>,
    {
        let outer = self.bindings.clone();
        if !self.in_function
            && let Some(ref single_state) = self.state_name.clone()
            && (carried_keys.is_empty()
                || (carried_keys.len() == 1 && &carried_keys[0] == single_state))
        {
            let init = self.bindings[single_state];
            let state_ty = self.instructions[init].ty;
            let header = self.push(state_ty, Operation::Loop(init))?;
            self.bindings.insert(single_state.clone(), header);
            let cond = emit_cond(self)?;
            self.push(ValueType::Unit, Operation::While(cond))?;
            emit_body(self)?;
            let next = self.bindings[single_state];
            let after = self.push(state_ty, Operation::EndLoop(next))?;
            self.bindings = outer;
            self.bindings.insert(single_state.clone(), after);
            return Ok(());
        }

        if carried_keys.is_empty() {
            let dummy = self.push(ValueType::Unit, Operation::Constant(0))?;
            let header = self.push(ValueType::Unit, Operation::LoopValues(vec![dummy]))?;
            let cond = emit_cond(self)?;
            self.push(ValueType::Unit, Operation::While(cond))?;
            emit_body(self)?;
            self.push(ValueType::Unit, Operation::EndLoopValues(vec![header]))?;
            self.bindings = outer;
            return Ok(());
        }

        let initials: Vec<Register> = carried_keys.iter().map(|k| self.bindings[k]).collect();
        let phi_types: Vec<ValueType> = initials
            .iter()
            .map(|&reg| self.instructions[reg].ty)
            .collect();
        let header = self.push(phi_types[0], Operation::LoopValues(initials))?;
        for (idx, &ty) in phi_types.iter().enumerate().skip(1) {
            self.push(ty, Operation::PhiValue(idx))?;
        }
        for (idx, key) in carried_keys.iter().enumerate() {
            self.bindings.insert(key.clone(), header + idx);
        }

        let cond = emit_cond(self)?;
        self.push(ValueType::Unit, Operation::While(cond))?;
        emit_body(self)?;

        let next_vals: Vec<Register> = carried_keys.iter().map(|k| self.bindings[k]).collect();
        let after = self.push(phi_types[0], Operation::EndLoopValues(next_vals))?;
        for (idx, &ty) in phi_types.iter().enumerate().skip(1) {
            self.push(ty, Operation::PhiValue(idx))?;
        }
        self.bindings = outer;
        for (idx, key) in carried_keys.iter().enumerate() {
            self.bindings.insert(key.clone(), after + idx);
        }
        Ok(())
    }

    fn lower_branch_with<FThen, FElse>(
        &mut self,
        condition: Register,
        mutated_keys: &[String],
        mut emit_then: FThen,
        mut emit_else: FElse,
    ) -> Result<Register, String>
    where
        FThen: FnMut(&mut Self) -> Result<Register, String>,
        FElse: FnMut(&mut Self) -> Result<Register, String>,
    {
        let outer = self.bindings.clone();
        self.push(ValueType::Unit, Operation::If(condition))?;

        self.bindings = outer.clone();
        let then_val = emit_then(self)?;
        let then_bindings = self.bindings.clone();
        let result_ty = self.instructions[then_val].ty;

        if mutated_keys.is_empty() {
            self.push(ValueType::Unit, Operation::Else(then_val))?;
            self.bindings = outer.clone();
            let else_val = emit_else(self)?;
            self.bindings = outer;
            return self.push(result_ty, Operation::EndIf(else_val));
        }

        if mutated_keys.len() == 1 && result_ty == ValueType::Unit {
            let k0 = &mutated_keys[0];
            let then_k0 = then_bindings[k0];
            let k0_ty = self.instructions[then_k0].ty;
            self.push(ValueType::Unit, Operation::Else(then_k0))?;
            self.bindings = outer.clone();
            let _else_val = emit_else(self)?;
            let else_k0 = self.bindings[k0];
            let merged_k0 = self.push(k0_ty, Operation::EndIf(else_k0))?;
            self.bindings = outer;
            self.bindings.insert(k0.clone(), merged_k0);
            return self.push(ValueType::Unit, Operation::Constant(0));
        }

        let mut then_vec = Vec::with_capacity(1 + mutated_keys.len());
        then_vec.push(then_val);
        for key in mutated_keys {
            then_vec.push(then_bindings[key]);
        }
        let phi_types: Vec<ValueType> = then_vec
            .iter()
            .map(|&reg| self.instructions[reg].ty)
            .collect();
        self.push(ValueType::Unit, Operation::ElseValues(then_vec))?;

        self.bindings = outer.clone();
        let else_val = emit_else(self)?;
        let mut else_vec = Vec::with_capacity(1 + mutated_keys.len());
        else_vec.push(else_val);
        for key in mutated_keys {
            else_vec.push(self.bindings[key]);
        }

        let merged_base = self.push(phi_types[0], Operation::EndIfValues(else_vec))?;
        for (idx, &ty) in phi_types.iter().enumerate().skip(1) {
            self.push(ty, Operation::PhiValue(idx))?;
        }
        self.bindings = outer;
        for (idx, key) in mutated_keys.iter().enumerate() {
            self.bindings.insert(key.clone(), merged_base + 1 + idx);
        }
        Ok(merged_base)
    }

    fn apply_binary_op(
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

    fn lower_deep_assign(
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

    fn statements(&mut self, statements: &[Statement]) -> Result<(), String> {
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

    fn lower_match_arms(
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

    fn lower_match_arm_body(
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

    fn expression(&mut self, expression: &Expr) -> Result<Register, String> {
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
            ExprKind::SelfPid => self.push(ValueType::Pid, Operation::SelfPid),
            ExprKind::Spawn => self.push(ValueType::Pid, Operation::SpawnSelf),
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
                    (ValueType::Int, "box") => {
                        self.push(ValueType::Ref, Operation::RecordInt(recv))
                    }
                    (ValueType::Bool, "to_str") => {
                        self.push(ValueType::StringRef, Operation::BoolToString(recv))
                    }
                    (ValueType::Ref, "get") => self.push(ValueType::Int, Operation::ReadInt(recv)),
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
            }
            ExprKind::Ask {
                target,
                message,
                arguments,
                timeout,
                then,
            } => {
                let target_reg = self.expression(target)?;
                let handler = *self
                    .handlers
                    .get(message.text.as_str())
                    .ok_or("unknown request message")?;
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
                    "box" => {
                        let val = self.expression(&arguments[0])?;
                        self.push(ValueType::Ref, Operation::RecordInt(val))
                    }
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

fn collect_assigned_block(
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

fn collect_assigned_arm(arm: &MatchArm, out: &mut BTreeSet<String>) {
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

fn collect_assigned_expr(expr: &Expr, shadowed: &BTreeSet<String>, out: &mut BTreeSet<String>) {
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

fn sema_type_to_value_type(
    ty: &Type,
    records: &[wings_abi::RecordSchema],
) -> Result<ValueType, String> {
    match ty {
        Type::Int => Ok(ValueType::Int),
        Type::Bool => Ok(ValueType::Bool),
        Type::Unit => Ok(ValueType::Unit),
        Type::BoxInt => Ok(ValueType::Ref),
        Type::IntList => Ok(ValueType::ListRef),
        Type::Bytes => Ok(ValueType::BytesRef),
        Type::String => Ok(ValueType::StringRef),
        Type::Pid => Ok(ValueType::Pid),
        Type::RequestId => Ok(ValueType::RequestId),
        Type::Struct(name) => records
            .iter()
            .position(|r| &r.name == name)
            .map(|idx| ValueType::RecordRef(idx as u16))
            .ok_or_else(|| format!("unknown struct schema '{name}'")),
        _ => Err(format!("unsupported value type {ty}")),
    }
}

fn resolve_value_type(
    ty_expr: &TypeExpr,
    records: &[wings_abi::RecordSchema],
) -> Result<ValueType, String> {
    match &ty_expr.kind {
        TypeKind::Unit => Ok(ValueType::Unit),
        TypeKind::List(_) => Ok(ValueType::ListRef),
        TypeKind::Boxed(_) => Ok(ValueType::Ref),
        TypeKind::Named(name) => match name.text.as_str() {
            "int" => Ok(ValueType::Int),
            "bool" => Ok(ValueType::Bool),
            "str" => Ok(ValueType::StringRef),
            "bytes" => Ok(ValueType::BytesRef),
            "pid" => Ok(ValueType::Pid),
            "req" => Ok(ValueType::RequestId),
            "unit" => Ok(ValueType::Unit),
            other => records
                .iter()
                .position(|r| r.name == other)
                .map(|idx| ValueType::RecordRef(idx as u16))
                .ok_or_else(|| format!("unsupported scalar type {other}")),
        },
        TypeKind::Error => Err("cannot lower invalid type".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_message_parameters_expand_to_typed_leaves_and_rebuild_independent_records() {
        let program = lower(
            r#"
            actor P {
                struct Empty {}
                struct A { text: str, values: [int] }
                struct B { a: A, empty: Empty, saved: box<int>, data: bytes, flag: bool, unit: () }
                state n = 0
                fn make() -> B => B {
                    a: A { text: "中", values: [] },
                    empty: Empty {},
                    saved: box(7),
                    data: b"",
                    flag: true,
                    unit: (),
                }
                on store(value: B) { gc(); n = value.a.text.len() }
                on start { self ! store(make()) }
                on query(value: B) -> int { value.saved; 1 }
                on reply done(id: req, status: int, result: int) {}
                on ask_it { let _ = self.ask(query(make()), timeout: 1_000, then: done) }
            }
            "#,
        )
        .unwrap();
        for index in [0, 2] {
            assert_eq!(
                program.handlers[index]
                    .parameters
                    .iter()
                    .map(|(_, ty)| *ty)
                    .collect::<Vec<_>>(),
                vec![
                    ValueType::StringRef,
                    ValueType::ListRef,
                    ValueType::Ref,
                    ValueType::BytesRef,
                    ValueType::Bool,
                    ValueType::Unit
                ]
            );
            assert_eq!(
                program.handlers[index]
                    .function
                    .instructions
                    .iter()
                    .filter(|instruction| matches!(
                        instruction.operation,
                        Operation::RecordNew { .. }
                    ))
                    .count(),
                3
            );
        }
        let start = &program.handlers[1].function;
        assert!(start.instructions.iter().any(|instruction| matches!(
            &instruction.operation,
            Operation::SendSelf { arguments, .. } if arguments.len() == 6
        )));
    }

    #[test]
    fn multi_state_and_loops_and_deep_assign_validate_cleanly() {
        let program = lower(
            r#"
            actor Demo {
                struct Inner { count: int, label: str }
                struct Outer { inner: Inner, ok: bool }
                state count = 0
                state tag = "init"
                state data = Outer { inner: Inner { count: 1, label: "a" }, ok: true }

                fn sum_range(limit: int) -> int {
                    let total = 0
                    for i in 1..=limit {
                        if i % 2 == 0 {
                            total += i
                        }
                    }
                    total
                }

                on step(n: int) -> str {
                    count += sum_range(n)
                    data.inner.count += 1
                    data.inner.label += "!"
                    tag = f"{data.inner.label}:{count}"
                    tag
                }

                on read_only -> int => count
            }
            "#,
        )
        .unwrap();
        program.validate().unwrap();
        assert_eq!(program.records.len(), 3); // Inner, Outer, $State
        // read_only handler does not allocate a new $State record!
        assert!(
            !program.handlers[1]
                .function
                .instructions
                .iter()
                .any(|inst| matches!(inst.operation, Operation::RecordNew { .. }))
        );
    }

    #[test]
    fn rejects_empty_and_multiple_actors_after_static_check() {
        for source in [
            "",
            "actor A { state count = 0 } actor B { state count = 0 }",
        ] {
            assert!(check(source).diagnostics.is_empty());
            assert!(lower(source).unwrap_err().contains("exactly one actor"));
        }
    }

    #[test]
    fn lowering_operation_limit_is_inclusive() {
        let source = format!(
            "actor P {{ state count = 0; on work {{ {} }} }}",
            "0;".repeat(MAX_OPERATIONS - 2)
        );
        assert!(check(&source).diagnostics.is_empty());
        let program = lower(&source).unwrap();
        assert_eq!(
            program.handlers[0].function.instructions.len(),
            MAX_OPERATIONS
        );
        let source = format!(
            "actor P {{ state count = 0; on work {{ {} }} }}",
            "0;".repeat(MAX_OPERATIONS - 1)
        );
        assert!(check(&source).diagnostics.is_empty());
        assert!(
            lower(&source)
                .unwrap_err()
                .contains("exceeds 4096 operations")
        );
    }
}
