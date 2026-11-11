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
