use std::collections::HashMap;

use wings_abi::ValueType;
use wings_ir::{
    Function, Handler, Instruction, MAX_OPERATIONS, Operation, Operator, Program, Register,
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
    let mut initializer = Lowerer::default();
    let initial_state = initializer.expression(&state.default)?;
    let initial_reply = initializer.push(ValueType::Unit, Operation::Constant(0))?;
    let initializer = Function {
        instructions: initializer.instructions,
        state: initial_state,
        reply: initial_reply,
    };
    let mut handlers = Vec::new();
    for handler in &process.handlers {
        let mut lowerer = Lowerer::default();
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
        for statement in &handler.body.statements {
            match statement {
                Statement::Assign { target, value } => {
                    candidate = lowerer.expression(value)?;
                    lowerer.bindings.insert(target.text.clone(), candidate);
                }
                Statement::Let { name, value, .. } => {
                    let register = lowerer.expression(value)?;
                    lowerer.bindings.insert(name.text.clone(), register);
                }
                Statement::Expr(value) => {
                    lowerer.expression(value)?;
                }
            }
        }
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
struct Lowerer {
    bindings: HashMap<String, Register>,
    instructions: Vec<Instruction>,
}

impl Lowerer {
    fn push(&mut self, ty: ValueType, operation: Operation) -> Result<Register, String> {
        if self.instructions.len() >= MAX_OPERATIONS {
            return Err("scalar AOT exceeds 4096 operations per entry".into());
        }
        let register = self.instructions.len();
        self.instructions.push(Instruction { ty, operation });
        Ok(register)
    }

    fn expression(&mut self, expression: &Expr) -> Result<Register, String> {
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
            ExprKind::Negate(value) => {
                let register = self.expression(value)?;
                self.push(ValueType::Int, Operation::Negate(register))
            }
            ExprKind::Binary {
                operator,
                left,
                right,
            } => {
                let left = self.expression(left)?;
                let right = self.expression(right)?;
                let operator = match operator {
                    BinaryOperator::Add => Operator::Add,
                    BinaryOperator::Subtract => Operator::Subtract,
                    BinaryOperator::Multiply => Operator::Multiply,
                    BinaryOperator::Divide => Operator::Divide,
                };
                self.push(ValueType::Int, Operation::Binary(operator, left, right))
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
        _ => Err(format!("unsupported scalar type {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
