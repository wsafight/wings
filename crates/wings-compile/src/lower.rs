use std::collections::HashMap;

use wings_abi::ValueType;
use wings_ir::{
    Comparison, Function, Handler, Instruction, MAX_OPERATIONS, Operation, Operator, Program,
    Register,
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
    let state = &process.states[0];
    let state_type = value_type(&state.ty.text)?;
    let functions: HashMap<_, _> = process
        .functions
        .iter()
        .map(|function| (function.name.text.as_str(), function))
        .collect();
    let mut initializer = Lowerer {
        functions: functions.clone(),
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
        let mut lowerer = Lowerer {
            functions: functions.clone(),
            ..Lowerer::default()
        };
        let mut candidate = lowerer.push(state_type, Operation::State)?;
        lowerer.bindings.insert(state.name.text.clone(), candidate);
        let parameters = handler
            .parameters
            .iter()
            .map(|parameter| Ok((parameter.name.text.clone(), value_type(&parameter.ty.text)?)))
            .collect::<Result<Vec<_>, String>>()?;
        for (index, (name, ty)) in parameters.iter().enumerate() {
            let register = lowerer.push(*ty, Operation::Argument(index))?;
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
                .map(|ty| value_type(&ty.text))
                .transpose()?,
            function: Function {
                instructions: lowerer.instructions,
                state: candidate,
                reply,
            },
        });
    }
    let program = Program {
        name: process.name.text.clone(),
        state: state_type,
        initializer,
        handlers,
    };
    program.validate()?;
    Ok(program)
}

#[derive(Default)]
struct Lowerer<'program> {
    bindings: HashMap<String, Register>,
    instructions: Vec<Instruction>,
    functions: HashMap<&'program str, &'program wings_syntax::ast::Function>,
    call_depth: usize,
    expression_depth: usize,
}

impl Lowerer<'_> {
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
            ExprKind::Boolean(value) => {
                self.push(ValueType::Bool, Operation::Constant(i64::from(*value)))
            }
            ExprKind::Unit => self.push(ValueType::Unit, Operation::Constant(0)),
            ExprKind::Name(name) => self
                .bindings
                .get(&name.text)
                .copied()
                .ok_or_else(|| format!("unresolved scalar binding {}", name.text)),
            ExprKind::Call { callee, arguments } => {
                if let Some(function) = self.functions.get(callee.text.as_str()).copied() {
                    if self.call_depth == 32 {
                        return Err("function call chain exceeds 32".into());
                    }
                    let values = arguments
                        .iter()
                        .map(|argument| self.expression(argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.call_depth += 1;
                    self.push(ValueType::Unit, Operation::Constant(0))?;
                    let outer = std::mem::take(&mut self.bindings);
                    for (parameter, register) in function.parameters.iter().zip(values) {
                        self.bindings.insert(parameter.name.text.clone(), register);
                    }
                    for statement in &function.body.statements {
                        match statement {
                            Statement::Let { name, value, .. } => {
                                let register = self.expression(value)?;
                                self.bindings.insert(name.text.clone(), register);
                            }
                            Statement::Expr(value) => {
                                self.expression(value)?;
                            }
                            _ => return Err("functions cannot assign state or loop".into()),
                        }
                    }
                    let result = match &function.body.tail {
                        Some(tail) => self.expression(tail),
                        None => self.push(ValueType::Unit, Operation::Constant(0)),
                    };
                    self.bindings = outer;
                    self.call_depth -= 1;
                    return result;
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

fn value_type(name: &str) -> Result<ValueType, String> {
    match name {
        "Int" => Ok(ValueType::Int),
        "Bool" => Ok(ValueType::Bool),
        "Unit" => Ok(ValueType::Unit),
        "RecordInt" => Ok(ValueType::Ref),
        _ => Err(format!("unsupported scalar type {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_functions_evaluate_arguments_once_restore_bindings_and_bound_empty_expansion() {
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
        let error = lower(&text).unwrap_err();
        assert!(error.contains("4096"), "{error}");
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
        assert_eq!(program.handlers[0].function.instructions.len(), 65);
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
        assert!(
            lower(&text)
                .unwrap_err()
                .contains("expression depth exceeds 256")
        );
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
