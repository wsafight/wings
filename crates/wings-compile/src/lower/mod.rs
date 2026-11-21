use std::collections::{BTreeMap, BTreeSet, HashMap};

use wings_abi::ValueType;
use wings_ir::{
    Comparison, ExternalActor, ExternalHandler, Function, Handler, Instruction, MAX_OPERATIONS,
    Operation, Operator, Procedure, Program, Register,
};
use wings_sema::{Analysis, Type};
use wings_syntax::ast::{
    BinaryOperator, Block, Expr, ExprKind, InterpolatedPart, MatchArm, MatchPattern, Name,
    Statement, StructDecl, TypeExpr, TypeKind,
};

use crate::check;

mod expr;
mod stmt;

use stmt::{collect_assigned_block, collect_assigned_expr};

fn collect_actor_records(
    global_structs: &[StructDecl],
    actor_structs: &[StructDecl],
) -> Result<Vec<wings_abi::RecordSchema>, String> {
    let mut records = Vec::new();
    for decl in global_structs.iter().chain(actor_structs.iter()) {
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
    Ok(records)
}

fn expand_external_param(
    ty: ValueType,
    name: &str,
    expanded: bool,
    records: &[wings_abi::RecordSchema],
    parameters: &mut Vec<(String, ValueType)>,
    nodes: &mut usize,
) -> Result<(), String> {
    *nodes += 1;
    if *nodes > 64 {
        return Err("message struct expansion exceeds 64 total nodes".into());
    }
    if let ValueType::RecordRef(schema) = ty {
        let fields = records[usize::from(schema)].fields.clone();
        for (field, field_ty) in fields {
            expand_external_param(field_ty, &field, expanded, records, parameters, nodes)?;
        }
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
    }
    Ok(())
}

fn build_external_actors(analysis: &Analysis) -> Result<Vec<ExternalActor>, String> {
    let global_structs = &analysis.parsed.program.structs;
    let mut out = Vec::with_capacity(analysis.parsed.program.actors.len());
    for actor in &analysis.parsed.program.actors {
        let records = collect_actor_records(global_structs, &actor.structs)?;
        let mut handlers = Vec::with_capacity(actor.handlers.len());
        for handler in &actor.handlers {
            let source_params: Vec<(String, ValueType)> = handler
                .parameters
                .iter()
                .map(|p| Ok((p.name.text.clone(), resolve_value_type(&p.ty, &records)?)))
                .collect::<Result<_, String>>()?;
            let expanded = source_params
                .iter()
                .any(|(_, ty)| matches!(ty, ValueType::RecordRef(_)));
            let is_record_completion = handler.is_reply
                && source_params.len() == 3
                && source_params[0].1 == ValueType::RequestId
                && source_params[1].1 == ValueType::Int
                && matches!(source_params[2].1, ValueType::RecordRef(_));
            let mut parameters = Vec::new();
            let mut nodes = 0usize;
            for (idx, (name, ty)) in source_params.iter().enumerate() {
                if is_record_completion && idx == 2 {
                    nodes = 0;
                }
                expand_external_param(*ty, name, expanded, &records, &mut parameters, &mut nodes)?;
            }
            let reply = handler
                .reply_type
                .as_ref()
                .map(|ty| resolve_value_type(ty, &records))
                .transpose()?;
            handlers.push(ExternalHandler {
                name: handler.name.text.clone(),
                parameters,
                reply,
            });
        }
        out.push(ExternalActor {
            name: actor.name.text.clone(),
            records,
            handlers,
        });
    }
    Ok(out)
}

pub fn lower(text: &str) -> Result<Program, String> {
    let analysis = check(text);
    if !analysis.diagnostics.is_empty() {
        return Err(crate::render_diagnostics("<source>", text, &analysis));
    }
    if analysis.parsed.program.actors.len() != 1 {
        return Err("scalar AOT requires exactly one actor per source file".into());
    }
    lower_actor(&analysis, 0, &[])
}

pub fn lower_app(text: &str) -> Result<Vec<Program>, String> {
    let analysis = check(text);
    if !analysis.diagnostics.is_empty() {
        return Err(crate::render_diagnostics("<source>", text, &analysis));
    }
    let count = analysis.parsed.program.actors.len();
    if !(1..=32).contains(&count) {
        return Err("application bundle requires 1..=32 actors".into());
    }
    let external_actors = build_external_actors(&analysis)?;
    let mut programs = Vec::with_capacity(count);
    for idx in 0..count {
        programs.push(lower_actor(&analysis, idx, &external_actors)?);
    }
    Ok(programs)
}

fn lower_actor(
    analysis: &Analysis,
    actor_idx: usize,
    external_actors: &[ExternalActor],
) -> Result<Program, String> {
    let actor = &analysis.parsed.program.actors[actor_idx];
    let resolved_states = &analysis.actor_states[actor_idx];
    let mut records = collect_actor_records(&analysis.parsed.program.structs, &actor.structs)?;

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
        let mut lowerer = Lowerer {
            actor_name: actor.name.text.as_str(),
            external_actors,
            target_actors: Some(&analysis.target_actors),
            functions: functions.clone(),
            records: &records,
            handlers: handler_ids.clone(),
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
        external_actors: external_actors.to_vec(),
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
    actor_name: &'program str,
    external_actors: &'program [ExternalActor],
    target_actors: Option<&'program HashMap<(usize, usize), String>>,
    records: &'program [wings_abi::RecordSchema],
    bindings: BTreeMap<String, Register>,
    instructions: Vec<Instruction>,
    functions: HashMap<&'program str, (usize, ValueType)>,
    expression_depth: usize,
    handlers: HashMap<&'program str, usize>,
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
        if carried_keys.is_empty() {
            let dummy = self.push(ValueType::Unit, Operation::Constant(0))?;
            let header = self.push(ValueType::Unit, Operation::Loop(vec![dummy]))?;
            let cond = emit_cond(self)?;
            self.push(ValueType::Unit, Operation::While(cond))?;
            emit_body(self)?;
            self.push(ValueType::Unit, Operation::EndLoop(vec![header]))?;
            self.bindings = outer;
            return Ok(());
        }

        let initials: Vec<Register> = carried_keys.iter().map(|k| self.bindings[k]).collect();
        let phi_types: Vec<ValueType> = initials
            .iter()
            .map(|&reg| self.instructions[reg].ty)
            .collect();
        let header = self.push(phi_types[0], Operation::Loop(initials))?;
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
        let after = self.push(phi_types[0], Operation::EndLoop(next_vals))?;
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

        let mut then_vec = Vec::with_capacity(1 + mutated_keys.len());
        then_vec.push(then_val);
        for key in mutated_keys {
            then_vec.push(then_bindings[key]);
        }
        let phi_types: Vec<ValueType> = then_vec
            .iter()
            .map(|&reg| self.instructions[reg].ty)
            .collect();
        self.push(ValueType::Unit, Operation::Else(then_vec))?;

        self.bindings = outer.clone();
        let else_val = emit_else(self)?;
        let mut else_vec = Vec::with_capacity(1 + mutated_keys.len());
        else_vec.push(else_val);
        for key in mutated_keys {
            else_vec.push(self.bindings[key]);
        }

        let merged_base = self.push(phi_types[0], Operation::EndIf(else_vec))?;
        for (idx, &ty) in phi_types.iter().enumerate().skip(1) {
            self.push(ty, Operation::PhiValue(idx))?;
        }
        self.bindings = outer;
        for (idx, key) in mutated_keys.iter().enumerate() {
            self.bindings.insert(key.clone(), merged_base + 1 + idx);
        }
        Ok(merged_base)
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
        Type::IntList => Ok(ValueType::ListRef),
        Type::Bytes => Ok(ValueType::BytesRef),
        Type::String => Ok(ValueType::StringRef),
        Type::Pid | Type::ActorPid(_) | Type::NullPid => Ok(ValueType::Pid),
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
        TypeKind::ActorPid(_) => Ok(ValueType::Pid),
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
                struct Saved { value: int }
                struct A { text: str, values: [int] }
                struct B { a: A, empty: Empty, saved: Saved, data: bytes, flag: bool, unit: () }
                state n = 0
                fn make() -> B => B {
                    a: A { text: "中", values: [] },
                    empty: Empty {},
                    saved: Saved { value: 7 },
                    data: b"",
                    flag: true,
                    unit: (),
                }
                on store(value: B) { gc(); n = value.a.text.len() }
                on start { self ! store(make()) }
                on query(value: B) -> int { value.saved.value; 1 }
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
                    ValueType::Int,
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
                4
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
