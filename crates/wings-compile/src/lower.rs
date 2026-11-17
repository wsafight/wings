use std::collections::HashMap;

use wings_abi::ValueType;
use wings_ir::{
    Comparison, Function, Handler, Instruction, MAX_OPERATIONS, Operation, Operator, Procedure,
    Program, Register,
};
use wings_syntax::ast::{BinaryOperator, Expr, ExprKind, Statement};

use crate::check;

pub fn lower(text: &str) -> Result<Program, String> {
    let analysis = check(text);
    if !analysis.diagnostics.is_empty() {
        return Err(crate::render_diagnostics("<source>", text, &analysis));
    }
    let [process] = analysis.parsed.program.processes.as_slice() else {
        return Err("scalar AOT requires exactly one process per source file".into());
    };
    let mut records = Vec::new();
    for record in &process.records {
        let fields = record
            .fields
            .iter()
            .map(|field| {
                Ok((
                    field.name.text.clone(),
                    named_type(&field.ty.text, &records)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        records.push(wings_abi::RecordSchema {
            name: record.name.text.clone(),
            fields,
        });
    }
    let state = &process.states[0];
    let state_type = named_type(&state.ty.text, &records)?;
    let functions: HashMap<_, _> = process
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            Ok((
                function.name.text.as_str(),
                (index, named_type(&function.result.text, &records)?),
            ))
        })
        .collect::<Result<_, String>>()?;
    let mut procedures = Vec::new();
    for function in &process.functions {
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
                    named_type(&parameter.ty.text, &records)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        for (index, (name, ty)) in parameters.iter().enumerate() {
            let register = lowerer.push(*ty, Operation::Argument(index))?;
            lowerer.bindings.insert(name.clone(), register);
        }
        lowerer.statements(&function.body.statements, "", state)?;
        let reply = match &function.body.tail {
            Some(tail) => lowerer.expression(tail)?,
            None => lowerer.push(ValueType::Unit, Operation::Constant(0))?,
        };
        procedures.push(Procedure {
            name: function.name.text.clone(),
            parameters,
            result: named_type(&function.result.text, &records)?,
            function: Function {
                instructions: lowerer.instructions,
                state,
                reply,
            },
        });
    }
    let handler_ids: HashMap<_, _> = process
        .handlers
        .iter()
        .enumerate()
        .map(|(index, handler)| (handler.name.text.as_str(), index))
        .collect();
    let mut initializer = Lowerer {
        functions: functions.clone(),
        records: &records,
        handlers: handler_ids.clone(),
        ..Lowerer::default()
    };
    let initial_state = initializer.expression(&state.default)?;
    let initial_reply = initializer.push(ValueType::Unit, Operation::Constant(0))?;
    let initializer = Function {
        instructions: initializer.instructions,
        state: initial_state,
        reply: initial_reply,
    };
    let mut handlers = Vec::new();
    for handler in &process.handlers {
        if handler.parameters.len() > 64 {
            return Err("too many parameters: message exceeds 64 source parameters".into());
        }
        let mut lowerer = Lowerer {
            functions: functions.clone(),
            records: &records,
            handlers: handler_ids.clone(),
            ..Lowerer::default()
        };
        let mut candidate = lowerer.push(state_type, Operation::State)?;
        lowerer.bindings.insert(state.name.text.clone(), candidate);
        let source_parameters = handler
            .parameters
            .iter()
            .map(|parameter| {
                Ok((
                    parameter.name.text.clone(),
                    named_type(&parameter.ty.text, &records)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let expanded = source_parameters
            .iter()
            .any(|(_, ty)| matches!(ty, ValueType::RecordRef(_)));
        let mut parameters = Vec::new();
        let mut nodes = 0;
        for (name, ty) in &source_parameters {
            let register =
                lowerer.message_parameter(*ty, name, expanded, &mut parameters, &mut nodes)?;
            lowerer.bindings.insert(name.clone(), register);
        }
        candidate = lowerer.statements(&handler.body.statements, &state.name.text, candidate)?;
        let reply = match &handler.body.tail {
            Some(tail) => lowerer.expression(tail)?,
            None => lowerer.push(ValueType::Unit, Operation::Constant(0))?,
        };
        handlers.push(Handler {
            name: handler.name.text.clone(),
            parameters,
            reply: handler
                .reply_type
                .as_ref()
                .map(|ty| named_type(&ty.text, &records))
                .transpose()?,
            function: Function {
                instructions: lowerer.instructions,
                state: candidate,
                reply,
            },
        });
    }
    let program = Program {
        records,
        name: process.name.text.clone(),
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
    bindings: HashMap<String, Register>,
    instructions: Vec<Instruction>,
    functions: HashMap<&'program str, (usize, ValueType)>,
    expression_depth: usize,
    handlers: HashMap<&'program str, usize>,
}

impl Lowerer<'_> {
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
            return Err("message record expansion exceeds 64 total nodes".into());
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
            return Err("message record expansion exceeds 64 total nodes".into());
        }
        if let ValueType::RecordRef(schema) = self.instructions[value].ty {
            let fields = self.records[usize::from(schema)].fields.clone();
            for (field, (_, ty)) in fields.into_iter().enumerate() {
                let field = self.push(ty, Operation::RecordField { value, field })?;
                self.message_argument(field, flattened, nodes)?;
            }
        } else {
            flattened.push(value);
        }
        Ok(())
    }

    fn statements(
        &mut self,
        statements: &[Statement],
        state_name: &str,
        mut candidate: Register,
    ) -> Result<Register, String> {
        for statement in statements {
            match statement {
                Statement::Assign { target, value } => {
                    candidate = self.expression(value)?;
                    self.bindings.insert(target.text.clone(), candidate);
                }
                Statement::Let { name, value, .. } => {
                    let register = self.expression(value)?;
                    self.bindings.insert(name.text.clone(), register);
                }
                Statement::Expr(value) => {
                    self.expression(value)?;
                }
                Statement::While { condition, body } => {
                    let outer = self.bindings.clone();
                    let state_type = self.instructions[candidate].ty;
                    let header = self.push(state_type, Operation::Loop(candidate))?;
                    self.bindings.insert(state_name.to_owned(), header);
                    let condition = self.expression(condition)?;
                    self.push(ValueType::Unit, Operation::While(condition))?;
                    let next = self.statements(&body.statements, state_name, header)?;
                    if let Some(tail) = &body.tail {
                        self.expression(tail)?;
                    }
                    candidate = self.push(state_type, Operation::EndLoop(next))?;
                    self.bindings = outer;
                    self.bindings.insert(state_name.to_owned(), candidate);
                }
            }
        }
        Ok(candidate)
    }

    fn push(&mut self, ty: ValueType, operation: Operation) -> Result<Register, String> {
        if self.instructions.len() >= MAX_OPERATIONS {
            return Err("scalar AOT exceeds 4096 operations per entry".into());
        }
        let register = self.instructions.len();
        self.instructions.push(Instruction { ty, operation });
        Ok(register)
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
            ExprKind::Boolean(value) => {
                self.push(ValueType::Bool, Operation::Constant(i64::from(*value)))
            }
            ExprKind::Unit => self.push(ValueType::Unit, Operation::Constant(0)),
            ExprKind::Name(name) => self
                .bindings
                .get(&name.text)
                .copied()
                .ok_or_else(|| format!("unresolved scalar binding {}", name.text)),
            ExprKind::Field { value, name } => {
                let value = self.expression(value)?;
                let ValueType::RecordRef(schema) = self.instructions[value].ty else {
                    return Err("expected named record".into());
                };
                let (field, (_, ty)) = self.records[usize::from(schema)]
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, (field, _))| field == &name.text)
                    .ok_or("unknown record field")?;
                self.push(*ty, Operation::RecordField { value, field })
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
                if callee.text == "request" {
                    let target = self.expression(&arguments[0])?;
                    let ExprKind::Name(message) = &arguments[1].kind else {
                        return Err("expected request message".into());
                    };
                    let ExprKind::Name(completion) = &arguments[2].kind else {
                        return Err("expected request completion".into());
                    };
                    let handler = self.handlers[message.text.as_str()];
                    let callback = self.handlers[completion.text.as_str()];
                    let timeout = self.expression(&arguments[3])?;
                    let arguments = arguments[4..]
                        .iter()
                        .map(|argument| self.expression(argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    let arguments = self.message_arguments(arguments)?;
                    return self.push(
                        ValueType::RequestId,
                        Operation::Request {
                            target,
                            handler,
                            callback,
                            timeout,
                            arguments,
                        },
                    );
                }
                if callee.text == "cancel_request" {
                    let value = self.expression(&arguments[0])?;
                    return self.push(ValueType::Unit, Operation::CancelRequest(value));
                }
                if matches!(callee.text.as_str(), "self_pid" | "spawn_self") {
                    return self.push(
                        ValueType::Pid,
                        if callee.text == "self_pid" {
                            Operation::SelfPid
                        } else {
                            Operation::SpawnSelf
                        },
                    );
                }
                if matches!(callee.text.as_str(), "send_self" | "send") {
                    let (target, arguments) = if callee.text == "send" {
                        (Some(self.expression(&arguments[0])?), &arguments[1..])
                    } else {
                        (None, arguments.as_slice())
                    };
                    let ExprKind::Name(name) = &arguments[0].kind else {
                        return Err("expected static self-send target".into());
                    };
                    let handler = *self
                        .handlers
                        .get(name.text.as_str())
                        .ok_or("unknown self-send target")?;
                    let arguments = arguments[1..]
                        .iter()
                        .map(|argument| self.expression(argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    let arguments = self.message_arguments(arguments)?;
                    return self.push(
                        ValueType::Unit,
                        match target {
                            Some(target) => Operation::Send {
                                target,
                                handler,
                                arguments,
                            },
                            None => Operation::SendSelf { handler, arguments },
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
                    "record_int" => {
                        let value = self.expression(&arguments[0])?;
                        self.push(ValueType::Ref, Operation::RecordInt(value))
                    }
                    "read_int" => {
                        let value = self.expression(&arguments[0])?;
                        self.push(ValueType::Int, Operation::ReadInt(value))
                    }
                    "collect" => self.push(ValueType::Unit, Operation::Collect),
                    "string_from_bytes" | "string_bytes" | "string_len" | "concat_strings"
                    | "fill_bytes" | "bytes_len" | "byte_at" | "replace_byte" | "read_i64_be"
                    | "replace_i64_be" => {
                        let values = arguments
                            .iter()
                            .map(|argument| self.expression(argument))
                            .collect::<Result<Vec<_>, _>>()?;
                        let (ty, operation) = match callee.text.as_str() {
                            "string_from_bytes" => {
                                (ValueType::StringRef, Operation::StringFromBytes(values[0]))
                            }
                            "string_bytes" => {
                                (ValueType::BytesRef, Operation::StringBytes(values[0]))
                            }
                            "string_len" => (ValueType::Int, Operation::StringLen(values[0])),
                            "concat_strings" => (
                                ValueType::StringRef,
                                Operation::ConcatStrings(values[0], values[1]),
                            ),
                            "fill_bytes" => (
                                ValueType::BytesRef,
                                Operation::FillBytes(values[0], values[1]),
                            ),
                            "bytes_len" => (ValueType::Int, Operation::BytesLen(values[0])),
                            "byte_at" => (ValueType::Int, Operation::ByteAt(values[0], values[1])),
                            "read_i64_be" => {
                                (ValueType::Int, Operation::ReadI64Be(values[0], values[1]))
                            }
                            "replace_i64_be" => (
                                ValueType::BytesRef,
                                Operation::ReplaceI64Be([values[0], values[1], values[2]]),
                            ),
                            _ => (
                                ValueType::BytesRef,
                                Operation::ReplaceByte([values[0], values[1], values[2]]),
                            ),
                        };
                        self.push(ty, operation)
                    }
                    "empty_ints" => self.push(ValueType::ListRef, Operation::EmptyInts),
                    "prepend_int" => {
                        let value = self.expression(&arguments[0])?;
                        let tail = self.expression(&arguments[1])?;
                        self.push(ValueType::ListRef, Operation::PrependInt(value, tail))
                    }
                    "head_int" | "tail_ints" | "is_empty_ints" => {
                        let value = self.expression(&arguments[0])?;
                        let (ty, operation) = match callee.text.as_str() {
                            "head_int" => (ValueType::Int, Operation::HeadInt(value)),
                            "tail_ints" => (ValueType::ListRef, Operation::TailInts(value)),
                            _ => (ValueType::Bool, Operation::IsEmptyInts(value)),
                        };
                        self.push(ty, operation)
                    }
                    _ => Err("unsupported builtin call".into()),
                }
            }
            ExprKind::Negate(value) => {
                let register = self.expression(value)?;
                self.push(ValueType::Int, Operation::Negate(register))
            }
            ExprKind::If {
                condition,
                then_value,
                else_value,
            } => {
                let condition = self.expression(condition)?;
                self.push(ValueType::Unit, Operation::If(condition))?;
                let then_value = self.expression(then_value)?;
                self.push(ValueType::Unit, Operation::Else(then_value))?;
                let else_value = self.expression(else_value)?;
                self.push(
                    self.instructions[then_value].ty,
                    Operation::EndIf(else_value),
                )
            }
            ExprKind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                if self.instructions[left].ty == ValueType::StringRef
                    && matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual)
                {
                    let equal = self.push(ValueType::Bool, Operation::EqualStrings(left, right))?;
                    return if matches!(operator, BinaryOperator::Equal) {
                        Ok(equal)
                    } else {
                        let false_value = self.push(ValueType::Bool, Operation::Constant(0))?;
                        self.push(
                            ValueType::Bool,
                            Operation::Compare(Comparison::Equal, equal, false_value),
                        )
                    };
                }
                let operation = match operator {
                    BinaryOperator::Add => Operation::Binary(Operator::Add, left, right),
                    BinaryOperator::Subtract => Operation::Binary(Operator::Subtract, left, right),
                    BinaryOperator::Multiply => Operation::Binary(Operator::Multiply, left, right),
                    BinaryOperator::Divide => Operation::Binary(Operator::Divide, left, right),
                    BinaryOperator::Equal => Operation::Compare(Comparison::Equal, left, right),
                    BinaryOperator::NotEqual => {
                        Operation::Compare(Comparison::NotEqual, left, right)
                    }
                    BinaryOperator::Less => Operation::Compare(Comparison::Less, left, right),
                    BinaryOperator::LessEqual => {
                        Operation::Compare(Comparison::LessEqual, left, right)
                    }
                    BinaryOperator::Greater => Operation::Compare(Comparison::Greater, left, right),
                    BinaryOperator::GreaterEqual => {
                        Operation::Compare(Comparison::GreaterEqual, left, right)
                    }
                };
                self.push(
                    if matches!(operation, Operation::Compare(..)) {
                        ValueType::Bool
                    } else {
                        ValueType::Int
                    },
                    operation,
                )
            }
            ExprKind::Error => Err("cannot lower an invalid expression".into()),
        }
    }
}

fn named_type(name: &str, records: &[wings_abi::RecordSchema]) -> Result<ValueType, String> {
    if let Some(index) = records.iter().position(|record| record.name == name) {
        return Ok(ValueType::RecordRef(index as u16));
    }
    value_type(name)
}

fn value_type(name: &str) -> Result<ValueType, String> {
    match name {
        "Int" => Ok(ValueType::Int),
        "Bool" => Ok(ValueType::Bool),
        "Unit" => Ok(ValueType::Unit),
        "RecordInt" => Ok(ValueType::Ref),
        "IntList" => Ok(ValueType::ListRef),
        "Bytes" => Ok(ValueType::BytesRef),
        "String" => Ok(ValueType::StringRef),
        "Pid" => Ok(ValueType::Pid),
        "RequestId" => Ok(ValueType::RequestId),
        _ => Err(format!("unsupported scalar type {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_message_parameters_expand_to_typed_leaves_and_rebuild_independent_records() {
        let program = lower("process P { record Empty {} record A { text: String, values: IntList } record B { a: A, empty: Empty, saved: RecordInt, data: Bytes, flag: Bool, unit: Unit } state n: Int = 0; fn make() -> B { B(A(\"中\", empty_ints()), Empty(), record_int(7), fill_bytes(0, 0), true, ()) } on Store(value: B) { collect(); n = string_len(value.a.text) } on Start { send_self(Store, make()) } on Query(value: B) -> Int { value.saved; 1 } on Done(id: RequestId, status: Int, result: Int) {} on Ask { request(self_pid(), Query, Done, 1000, make()); } }").unwrap();
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
        assert_eq!(
            start
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction.operation, Operation::Call { .. }))
                .count(),
            1
        );
        assert!(start.instructions.iter().any(|instruction| matches!(&instruction.operation, Operation::SendSelf { arguments, .. } if arguments.len() == 6)));
        let mut forged = program;
        forged.handlers[0].parameters[0].1 = ValueType::RecordRef(0);
        assert!(forged.validate().is_err());
    }

    #[test]
    fn named_message_expansion_counts_empty_records_and_repeated_subtrees() {
        for count in [63, 64, 65] {
            let parameters = (0..count)
                .map(|index| format!("value_{index}: Empty"))
                .collect::<Vec<_>>()
                .join(",");
            let arguments = vec!["Empty()"; count].join(",");
            let source = format!(
                "process P {{ record Empty {{}} state n: Int = 0; on Store({parameters}) {{ n = n + 1 }} on Start {{ send_self(Store, {arguments}) }} }}"
            );
            assert_eq!(lower(&source).is_ok(), count <= 64, "{count}");
        }
        for suffix in ["", ", extra: Int", ", extra: Empty", ", extra: A"] {
            let parameters = (0..7)
                .map(|index| format!("value_{index}: A"))
                .collect::<Vec<_>>()
                .join(",");
            let source = format!(
                "process P {{ record Empty {{}} record A {{ a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int, h: Int }} state n: Int = 0; on Store({parameters}{suffix}) {{}} }}"
            );
            assert_eq!(lower(&source).is_ok(), suffix != ", extra: A", "{suffix}");
        }
        let mut records = "record A0 {}".to_owned();
        for index in 1..32 {
            records.push_str(&format!(
                " record A{index} {{ left: A{}, right: A{} }}",
                index - 1,
                index - 1
            ));
        }
        assert!(
            lower(&format!(
                "process P {{ {records} state n: Int = 0; on Store(value: A31) {{}} }}"
            ))
            .unwrap_err()
            .contains("64 total nodes")
        );
    }

    #[test]
    fn string_equality_lowers_to_content_intrinsic_instead_of_reference_comparison() {
        let program = lower("process P { state text: String = \"中\"; on Equal -> Bool { text == \"中\" } on Different -> Bool { text != \"中\" } }").unwrap();
        for handler in &program.handlers {
            assert!(
                handler
                    .function
                    .instructions
                    .iter()
                    .any(|instruction| matches!(
                        instruction.operation,
                        Operation::EqualStrings(_, _)
                    ))
            );
            assert!(handler.function.instructions.iter().all(|instruction| {
                match instruction.operation {
                    Operation::Compare(_, left, right) => {
                        !handler.function.instructions[left].ty.is_reference()
                            && !handler.function.instructions[right].ty.is_reference()
                    }
                    _ => true,
                }
            }));
        }
    }

    #[test]
    fn named_records_preserve_nominal_types_fields_and_cross_frame_roots() {
        let source = "process P { record A { value: String } record B { left: A, right: Int } state data: B = B(A(\"中😀\"), 7); fn read(value: B) -> String { value.left.value } on Read -> Int { let saved = read(data); collect(); string_len(saved) + data.right } }";
        let program = lower(source).unwrap();
        assert_eq!(program.records.len(), 2);
        assert_eq!(program.state, ValueType::RecordRef(1));
        assert_eq!(
            program.functions[0].parameters[0].1,
            ValueType::RecordRef(1)
        );
        let instructions = &program.functions[0].function.instructions;
        assert!(
            instructions
                .iter()
                .any(|instruction| matches!(instruction.operation, Operation::RecordField { .. }))
        );
        for invalid in [
            "process P { record A { value: Int } record B { value: Int } state n: A = B(1); }",
            "process P { record A { value: A } state n: Int = 0; }",
            "process P { record A { value: B } record B {} state n: Int = 0; }",
            "process P { record A { value: Pid } state n: Int = 0; }",
            "process P { record A { value: Int, value: Int } state n: Int = 0; }",
            "process P { record A { value: Int } state n: A = A(true); }",
            "process P { record A { value: Int } state n: A = A(); }",
            "process P { record A { value: Int } state n: A = A(1); on Read -> Int { n.missing } }",
            "process P { record A {} state n: Int = 0; on Read -> A { A() } }",
            "process P { record A {} state n: A = A(); on Read -> Bool { n == n } }",
            "process P { record collect {} state n: Int = 0; }",
        ] {
            assert!(lower(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn named_record_source_limits_are_inclusive() {
        let records = (0..32)
            .map(|index| format!("record Shape{index} {{}} "))
            .collect::<String>();
        let source = format!("process P {{ {records} state n: Shape31 = Shape31(); }}");
        assert_eq!(lower(&source).unwrap().records.len(), 32);
        assert!(lower(&source.replace("state n:", "record Extra {} state n:")).is_err());
        let fields = (0..8)
            .map(|index| format!("field{index}: Int"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            "process P {{ record Wide {{ {fields} }} state n: Wide = Wide(0,1,2,3,4,5,6,7); on Read -> Int {{ n.field7 }} }}"
        );
        lower(&source).unwrap();
        assert!(lower(&source.replace("field7: Int", "field7: Int, extra: Int")).is_err());
    }

    #[test]
    fn string_literals_lower_without_per_byte_ir_and_keep_managed_roots() {
        let source = format!(
            "process P {{ state text: String = \"{}\"; fn copy(value: String) -> String {{ concat_strings(value, \"😀\") }} on Read -> Int {{ let saved = copy(text); collect(); string_len(saved) }} }}",
            "a".repeat(65536)
        );
        let program = lower(&source).unwrap();
        assert!(program.initializer.instructions.len() < 4);
        assert!(
            matches!(&program.initializer.instructions[0].operation, Operation::StringLiteral(value) if value.len()==65536)
        );
        assert_eq!(program.initializer.instructions[0].ty, ValueType::StringRef);
        let function = &program.handlers[0].function;
        let collect = function
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.operation, Operation::Collect))
            .unwrap();
        assert!(!function.root_plan().at[collect].is_empty());
        program.validate().unwrap();
    }

    #[test]
    fn strings_have_distinct_types_and_cross_frame_gc_roots() {
        let program = lower("process P { fn copy(value: String) -> String { concat_strings(value, value) } state data: String = string_from_bytes(fill_bytes(0, 0)); on Read -> Int { let next = copy(data); collect(); string_len(next) + bytes_len(string_bytes(data)) } }").unwrap();
        assert_eq!(program.state, ValueType::StringRef);
        assert_eq!(program.functions[0].result, ValueType::StringRef);
        let function = &program.handlers[0].function;
        let collection = function
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.operation, Operation::Collect))
            .unwrap();
        assert_eq!(function.root_plan().at[collection].len(), 2);
        program.validate().unwrap();
    }

    #[test]
    fn string_messages_preserve_schema_types_and_cross_copy_roots() {
        let program = lower("process P { state count: Int = 0; on Store(value: String, data: Bytes) { count = string_len(value) + bytes_len(data) } on Count(value: String, data: Bytes) -> Int { collect(); string_len(value) + bytes_len(data) } on Done(id: RequestId, status: Int, value: Int) { count = value } on Start { let child = spawn_self(); send(child, Store, \"中😀\", fill_bytes(0, 0)); send_self(Store, \"\", fill_bytes(0, 0)); let pending = request(child, Count, Done, 100, \"a\", fill_bytes(0, 0)); } }").unwrap();
        assert_eq!(program.handlers[0].parameters[0].1, ValueType::StringRef);
        assert_eq!(program.handlers[0].parameters[1].1, ValueType::BytesRef);
        program.validate().unwrap();
    }

    #[test]
    fn strings_cannot_bypass_message_reply_or_bytes_type_boundaries() {
        for source in [
            "process P { state count: Int = 0; on Set(value: String) { count = string_len(value) } on Wrong { send_self(Set, fill_bytes(0, 0)) } }",
            "process P { state count: Int = 0; on Read -> String { fill_bytes(0, 0) } }",
            "process P { state data: String = fill_bytes(0, 0); }",
            "process P { state data: String = string_from_bytes(fill_bytes(0, 0)); on Read -> Bool { data < data } }",
            "process P { state data: String = string_from_bytes(fill_bytes(0, 0)); on Read -> Int { byte_at(data, 0) } }",
        ] {
            assert!(lower(source).is_err(), "{source}");
        }
    }

    #[test]
    fn byte_values_lower_as_references_with_last_use_roots() {
        let program = lower("process P { fn changed(value: Bytes) -> Bytes { replace_byte(value, 0, 5) } state data: Bytes = fill_bytes(4, 0); on Read -> Int { let next = changed(data); collect(); bytes_len(next) + byte_at(data, 0) } }").unwrap();
        assert_eq!(program.state, ValueType::BytesRef);
        let function = &program.handlers[0].function;
        let collection = function
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.operation, Operation::Collect))
            .unwrap();
        assert_eq!(function.root_plan().at[collection].len(), 2);
        assert!(
            program.functions[0]
                .function
                .instructions
                .iter()
                .any(|instruction| matches!(instruction.operation, Operation::ReplaceByte(_)))
        );
    }

    #[test]
    fn managed_functions_evaluate_arguments_once_and_do_not_expand_call_graph() {
        let program = lower("process P { state n: Int = 0; fn twice(value: RecordInt) -> Int { read_int(value) + read_int(value) } on Get -> Int { let value = 7; twice(record_int(value)) + value } }").unwrap();
        assert_eq!(
            program.handlers[0]
                .function
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction.operation, Operation::RecordInt(_)))
                .count(),
            1
        );
        let mut definitions = String::from("fn f0(x: Int) -> Int { x }");
        for index in 1..14 {
            definitions.push_str(&format!(
                " fn f{index}(x: Int) -> Int {{ f{}(x); f{}(x) }}",
                index - 1,
                index - 1
            ));
        }
        let text =
            format!("process P {{ state n: Int = 0; {definitions} on Get -> Int {{ f13(n) }} }}");
        let program = lower(&text).unwrap();
        assert_eq!(program.functions.len(), 14);
        assert_eq!(program.functions[13].function.instructions.len(), 4);
        assert_eq!(program.handlers[0].function.instructions.len(), 2);
        let mut definitions = String::from("fn f0(x: Int) -> Int { x }");
        for index in 1..32 {
            definitions.push_str(&format!(
                " fn f{index}(x: Int) -> Int {{ f{}(x) }}",
                index - 1
            ));
        }
        let text = format!(
            "process P {{ state n: Int = 0; {definitions} on Get -> Int {{ f31(f31(n)) }} }}"
        );
        let program = lower(&text).unwrap();
        assert_eq!(program.handlers[0].function.instructions.len(), 3);
        let mut definitions = String::from("fn f0(x: Int) -> Int { x }");
        for index in 1..32 {
            definitions.push_str(&format!(
                " fn f{index}(x: Int) -> Int {{ {}f{}(x) }}",
                "-".repeat(16),
                index - 1
            ));
        }
        let text =
            format!("process P {{ state n: Int = 0; {definitions} on Get -> Int {{ f31(n) }} }}");
        let program = lower(&text).unwrap();
        assert_eq!(program.functions[31].function.instructions.len(), 19);
    }

    #[test]
    fn record_state_while_lowers_to_rooted_header_and_exit() {
        let program = lower("process P { state n: RecordInt = record_int(0); on Count(limit: Int) -> Int { while read_int(n) < limit { n = record_int(read_int(n) + 1) }; read_int(n) } }").unwrap();
        let function = &program.handlers[0].function;
        let plan = function.root_plan();
        let header = function
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.operation, Operation::Loop(_)))
            .unwrap();
        assert_eq!(function.instructions[header].ty, ValueType::Ref);
        assert!(plan.at[header].contains(&header));
        assert!(!plan.at[header].contains(&0));
        assert!(plan.at[function.instructions.len()].contains(&function.state));
    }

    #[test]
    fn record_conditionals_lower_to_typed_merges_and_path_roots() {
        let program = lower(
            "process P { state saved: RecordInt = if true { record_int(7) } else { record_int(9) }; on Get(flag: Bool) -> Int { let selected = if flag { saved } else { record_int(11) }; collect(); read_int(selected) } }",
        ).unwrap();
        program.validate().unwrap();
        for function in [&program.initializer, &program.handlers[0].function] {
            let plan = function.root_plan();
            let (index, _) = function
                .instructions
                .iter()
                .enumerate()
                .find(|(_, instruction)| matches!(instruction.operation, Operation::EndIf(_)))
                .unwrap();
            assert_eq!(function.instructions[index].ty, ValueType::Ref);
            assert!(plan.at[index].contains(&index));
        }
    }

    #[test]
    fn lowers_while_as_state_carried_header_condition_and_backedge() {
        let program = lower("process P { state n: Int = 0; on Count(limit: Int) -> Int { while n < limit { n = n + 1 }; n } }").unwrap();
        let function = &program.handlers[0].function;
        assert!(matches!(
            function.instructions[2].operation,
            Operation::Loop(0)
        ));
        assert!(matches!(
            function.instructions[4].operation,
            Operation::While(3)
        ));
        assert!(matches!(
            function.instructions[7].operation,
            Operation::EndLoop(6)
        ));
        assert_eq!((function.state, function.reply), (7, 7));
        program.validate().unwrap();
    }

    #[test]
    fn lowers_conditionals_as_lazy_structured_operations() {
        let program = lower("process P { state n: Int = 0; on Get(flag: Bool) -> Int { if flag { 1 } else { 1 / 0 } } }").unwrap();
        let function = &program.handlers[0].function;
        assert!(matches!(
            function.instructions[2].operation,
            Operation::If(1)
        ));
        assert!(matches!(
            function.instructions[4].operation,
            Operation::Else(3)
        ));
        assert!(matches!(
            function.instructions[8].operation,
            Operation::EndIf(7)
        ));
        assert_eq!(function.reply, 8);
        program.validate().unwrap();
    }

    #[test]
    fn lowers_persistent_state_without_exposing_reference_messages() {
        let program = lower("process P { state n: RecordInt = record_int(7); on Set(value: Int) -> Int { n = record_int(value); collect(); read_int(n) } on Get -> Int { collect(); read_int(n) } }").unwrap();
        assert_eq!(program.state, ValueType::Ref);
        assert_eq!(program.initializer.root_plan().last_uses[1], 3);
        let function = &program.handlers[0].function;
        assert_eq!(function.instructions[0].ty, ValueType::Ref);
        assert_eq!(
            function.root_plan().last_uses[function.state],
            function.instructions.len()
        );
        program.validate().unwrap();
    }

    #[test]
    fn lowers_temporary_records_in_source_evaluation_order() {
        let program = lower("process P { state n: Int = 0; on Get -> Int { let record: RecordInt = record_int(n + 1); collect(); read_int(record) } }").unwrap();
        let instructions = &program.handlers[0].function.instructions;
        assert!(matches!(instructions[3].operation, Operation::RecordInt(2)));
        assert_eq!(instructions[3].ty, ValueType::Ref);
        assert!(matches!(instructions[4].operation, Operation::Collect));
        assert!(matches!(instructions[5].operation, Operation::ReadInt(3)));
        assert_eq!(program.handlers[0].function.root_plan().capacity, 1);
    }

    fn instruction(program: &Program, register: Register) -> &Instruction {
        &program.handlers[0].function.instructions[register]
    }

    #[test]
    fn lowers_constant_initializer_with_separate_unit_reply() {
        let program = lower("process P { state count: Int = 42 }").unwrap();
        assert_eq!(program.name, "P");
        assert_eq!(program.state, ValueType::Int);
        assert!(program.handlers.is_empty());
        assert!(matches!(
            program.initializer.instructions[program.initializer.state].operation,
            Operation::Constant(42)
        ));
        assert_eq!(
            program.initializer.instructions[program.initializer.reply].ty,
            ValueType::Unit
        );
    }

    #[test]
    fn lowers_bool_and_unit_literals_with_canonical_encodings() {
        for (source, ty, value) in [
            (
                "process P { state flag: Bool = true; on Get -> Bool { false } }",
                ValueType::Bool,
                0,
            ),
            (
                "process P { state unit: Unit = (); on Get -> Unit { () } }",
                ValueType::Unit,
                0,
            ),
        ] {
            let program = lower(source).unwrap();
            let reply = instruction(&program, program.handlers[0].function.reply);
            assert_eq!(reply.ty, ty);
            assert!(matches!(reply.operation, Operation::Constant(encoded) if encoded == value));
            program.validate().unwrap();
        }
    }

    #[test]
    fn state_updates_rebind_subsequent_reads_without_rewriting_old_locals() {
        let program = lower("process P { state count: Int = 0; on Update(delta: Int) -> Int { let old = count; count = count + delta; count = count + old; old } }").unwrap();
        let function = &program.handlers[0].function;
        assert!(matches!(
            function.instructions[2].operation,
            Operation::Binary(Operator::Add, 0, 1)
        ));
        assert!(matches!(
            function.instructions[3].operation,
            Operation::Binary(Operator::Add, 2, 0)
        ));
        assert_eq!(function.state, 3);
        assert_eq!(function.reply, 0);
    }

    #[test]
    fn immutable_local_aliases_reuse_registers() {
        let program = lower("process P { state count: Int = 0; on Get -> Int { let first = count; let second = first; second } }").unwrap();
        assert_eq!(program.handlers[0].function.instructions.len(), 1);
        assert_eq!(program.handlers[0].function.state, 0);
        assert_eq!(program.handlers[0].function.reply, 0);
    }

    #[test]
    fn byte_i64_lowering_keeps_order_types_and_source_roots() {
        let program = lower("process P { state data: Bytes = fill_bytes(16, 0); on Set(value: Int) -> Int { data = replace_i64_be(data, 1, value); read_i64_be(data, 1) } }").unwrap();
        let function = &program.handlers[0].function;
        let replaced = function
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.operation, Operation::ReplaceI64Be(_)))
            .unwrap();
        let read = function
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.operation, Operation::ReadI64Be(_, _)))
            .unwrap();
        assert!(replaced < read);
        assert_eq!(function.instructions[replaced].ty, ValueType::BytesRef);
        assert_eq!(function.instructions[read].ty, ValueType::Int);
        assert!(function.root_plan().at[replaced].contains(&0));
        assert!(function.root_plan().at[read].contains(&replaced));
    }

    #[test]
    fn unused_expression_statements_are_not_removed() {
        let program =
            lower("process P { state count: Int = 0; on Get -> Int { 1 / 0; count } }").unwrap();
        assert!(
            program.handlers[0]
                .function
                .instructions
                .iter()
                .any(|instruction| matches!(
                    instruction.operation,
                    Operation::Binary(Operator::Divide, _, _)
                ))
        );
        assert_eq!(program.handlers[0].function.reply, 0);
    }

    #[test]
    fn lowers_all_arithmetic_operators_and_negation_in_dependency_order() {
        let program = lower("process P { state count: Int = 1 + 2; on Calc(x: Int) -> Int { let a = -x; let b = a + count; let c = b - 2; let d = c * 3; d / 4 } }").unwrap();
        let instructions = &program.handlers[0].function.instructions;
        assert!(matches!(instructions[2].operation, Operation::Negate(1)));
        assert!(matches!(
            instructions[3].operation,
            Operation::Binary(Operator::Add, 2, 0)
        ));
        assert!(matches!(
            instructions[5].operation,
            Operation::Binary(Operator::Subtract, 3, 4)
        ));
        assert!(matches!(
            instructions[7].operation,
            Operation::Binary(Operator::Multiply, 5, 6)
        ));
        assert!(matches!(
            instructions[9].operation,
            Operation::Binary(Operator::Divide, 7, 8)
        ));
        assert_eq!(program.handlers[0].function.reply, 9);
        program.validate().unwrap();
    }

    #[test]
    fn parameter_order_and_types_are_preserved() {
        let program = lower("process P { state count: Int = 0; on Select(value: Int, flag: Bool, unit: Unit) -> Bool { flag } }").unwrap();
        assert_eq!(
            program.handlers[0].parameters,
            vec![
                ("value".into(), ValueType::Int),
                ("flag".into(), ValueType::Bool),
                ("unit".into(), ValueType::Unit)
            ]
        );
        for index in 0..3 {
            assert!(
                matches!(instruction(&program, index + 1).operation, Operation::Argument(argument) if argument == index)
            );
        }
        assert_eq!(program.handlers[0].function.reply, 2);
    }

    #[test]
    fn preserves_the_difference_between_one_way_and_explicit_unit_reply() {
        let program =
            lower("process P { state count: Int = 0; on Notify {} on Ack -> Unit {} }").unwrap();
        assert_eq!(program.handlers[0].reply, None);
        assert_eq!(program.handlers[1].reply, Some(ValueType::Unit));
        for handler in &program.handlers {
            assert_eq!(
                handler.function.instructions[handler.function.reply].ty,
                ValueType::Unit
            );
        }
    }

    #[test]
    fn rejects_empty_and_multiple_processes_after_static_check() {
        for source in [
            "",
            "process A { state count: Int = 0 } process B { state count: Int = 0 }",
        ] {
            assert!(check(source).diagnostics.is_empty());
            assert!(lower(source).unwrap_err().contains("exactly one process"));
        }
    }

    #[test]
    fn rejects_invalid_source_before_accessing_state_or_lowering() {
        for source in [
            "process P {}",
            "process P { state a: Int = 0; state b: Int = 0 }",
            "process P { state count: Int = false }",
            "process P { state count: Int = 0; on Get -> Int { missing } }",
        ] {
            let error = lower(source).unwrap_err();
            assert!(error.contains("<source>:"), "{error}");
            assert!(error.contains("error["), "{error}");
        }
    }

    #[test]
    fn lowering_operation_limit_is_inclusive() {
        // State read + 4094 discarded constants + synthesized Unit = 4096.
        let source = format!(
            "process P {{ state count: Int = 0; on Work {{ {} }} }}",
            "0;".repeat(MAX_OPERATIONS - 2)
        );
        assert!(check(&source).diagnostics.is_empty());
        let program = lower(&source).unwrap();
        assert_eq!(
            program.handlers[0].function.instructions.len(),
            MAX_OPERATIONS
        );
        let source = format!(
            "process P {{ state count: Int = 0; on Work {{ {} }} }}",
            "0;".repeat(MAX_OPERATIONS - 1)
        );
        assert!(check(&source).diagnostics.is_empty());
        assert!(
            lower(&source)
                .unwrap_err()
                .contains("exceeds 4096 operations")
        );
    }

    #[test]
    fn lowering_rejects_excessive_handler_and_parameter_counts() {
        let handlers = (0..129)
            .map(|index| format!("on H{index} {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!("process P {{ state count: Int = 0; {handlers} }}");
        assert!(check(&source).diagnostics.is_empty());
        assert!(lower(&source).unwrap_err().contains("too many handlers"));
        let parameters = (0..65)
            .map(|index| format!("p{index}: Int"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("process P {{ state count: Int = 0; on H({parameters}) {{}} }}");
        assert!(check(&source).diagnostics.is_empty());
        assert!(lower(&source).unwrap_err().contains("too many parameters"));
    }
}
