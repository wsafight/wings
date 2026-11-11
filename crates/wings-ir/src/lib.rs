use wings_abi::ValueType;

pub const MAX_OPERATIONS: usize = 4096;
pub const MAX_HANDLERS: usize = 128;
pub const MAX_PARAMETERS: usize = 64;

pub type Register = usize;

#[derive(Clone, Copy, Debug)]
pub enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Clone, Debug)]
pub enum Operation {
    Constant(i64),
    State,
    Argument(usize),
    Negate(Register),
    Binary(Operator, Register, Register),
}

#[derive(Clone, Debug)]
pub struct Instruction {
    pub ty: ValueType,
    pub operation: Operation,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub instructions: Vec<Instruction>,
    pub state: Register,
    pub reply: Register,
}

#[derive(Clone, Debug)]
pub struct Handler {
    pub name: String,
    pub parameters: Vec<(String, ValueType)>,
    pub reply: Option<ValueType>,
    pub function: Function,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub state: ValueType,
    pub initializer: Function,
    pub handlers: Vec<Handler>,
}

impl Program {
    pub fn validate(&self) -> Result<(), String> {
        if self.handlers.len() > MAX_HANDLERS {
            return Err("too many handlers for the scalar prototype".into());
        }
        self.initializer
            .validate(self.state, &[], ValueType::Unit, true)?;
        for handler in &self.handlers {
            if handler.parameters.len() > MAX_PARAMETERS {
                return Err("too many parameters for the scalar prototype".into());
            }
            handler.function.validate(
                self.state,
                &handler.parameters,
                handler.reply.unwrap_or(ValueType::Unit),
                false,
            )?;
        }
        Ok(())
    }
}

impl Function {
    fn validate(
        &self,
        state: ValueType,
        parameters: &[(String, ValueType)],
        reply: ValueType,
        initializer: bool,
    ) -> Result<(), String> {
        if self.instructions.len() > MAX_OPERATIONS {
            return Err("too many operations for the scalar prototype".into());
        }
        let mut types = Vec::new();
        for instruction in &self.instructions {
            let valid = match instruction.operation {
                Operation::Constant(value) => instruction.ty.accepts(value),
                Operation::State => !initializer && instruction.ty == state,
                Operation::Argument(index) => parameters
                    .get(index)
                    .is_some_and(|parameter| parameter.1 == instruction.ty),
                Operation::Negate(value) => {
                    instruction.ty == ValueType::Int && types.get(value) == Some(&ValueType::Int)
                }
                Operation::Binary(_, left, right) => {
                    instruction.ty == ValueType::Int
                        && types.get(left) == Some(&ValueType::Int)
                        && types.get(right) == Some(&ValueType::Int)
                }
            };
            if !valid {
                return Err("invalid scalar IR operand, type or forward reference".into());
            }
            types.push(instruction.ty);
        }
        if types.get(self.state) != Some(&state) || types.get(self.reply) != Some(&reply) {
            return Err("invalid scalar IR state or reply register".into());
        }
        Ok(())
    }
}
