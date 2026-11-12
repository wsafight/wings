use wings_abi::{MAX_SHADOW_ROOTS, ValueType};

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

#[derive(Clone, Copy, Debug)]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Debug)]
pub enum Operation {
    Constant(i64),
    State,
    Argument(usize),
    Negate(Register),
    Binary(Operator, Register, Register),
    Compare(Comparison, Register, Register),
    RecordInt(Register),
    ReadInt(Register),
    Collect,
    If(Register),
    Else(Register),
    EndIf(Register),
    Loop(Register),
    While(Register),
    EndLoop(Register),
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

enum Region {
    Branch(usize, Option<ValueType>),
    Loop(usize, ValueType, bool),
}

impl Program {
    pub fn validate(&self) -> Result<(), String> {
        if self.handlers.iter().any(|handler| {
            handler.reply == Some(ValueType::Ref)
                || handler
                    .parameters
                    .iter()
                    .any(|(_, ty)| *ty == ValueType::Ref)
        }) {
            return Err("record references cannot cross scalar IR boundaries".into());
        }
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
        let mut available = Vec::new();
        let mut branches = Vec::new();
        for instruction in &self.instructions {
            let operands = operands(&instruction.operation);
            if operands
                .into_iter()
                .flatten()
                .any(|register| available.get(register) != Some(&true))
            {
                return Err(
                    "invalid scalar IR operand, type or forward reference, or branch scope".into(),
                );
            }
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
                Operation::Compare(comparison, left, right) => {
                    instruction.ty == ValueType::Bool
                        && types.get(left) == types.get(right)
                        && match comparison {
                            Comparison::Equal | Comparison::NotEqual => matches!(
                                types.get(left),
                                Some(ValueType::Int | ValueType::Bool | ValueType::Unit)
                            ),
                            _ => types.get(left) == Some(&ValueType::Int),
                        }
                }
                Operation::RecordInt(value) => {
                    instruction.ty == ValueType::Ref && types.get(value) == Some(&ValueType::Int)
                }
                Operation::ReadInt(value) => {
                    instruction.ty == ValueType::Int && types.get(value) == Some(&ValueType::Ref)
                }
                Operation::Collect => instruction.ty == ValueType::Unit,
                Operation::If(condition) => {
                    if branches.len() == 64 {
                        return Err("conditional nesting exceeds 64 branches".into());
                    }
                    branches.push(Region::Branch(types.len(), None));
                    instruction.ty == ValueType::Unit
                        && types.get(condition) == Some(&ValueType::Bool)
                }
                Operation::Else(value) => {
                    let Some(Region::Branch(start, then_type)) = branches.last_mut() else {
                        return Err("else without matching if".into());
                    };
                    if then_type.is_some() {
                        return Err("duplicate else in conditional".into());
                    }
                    *then_type = types.get(value).copied();
                    available[start.saturating_add(1)..].fill(false);
                    instruction.ty == ValueType::Unit && *then_type != Some(ValueType::Ref)
                }
                Operation::EndIf(value) => {
                    let Some(Region::Branch(start, Some(then_type))) = branches.pop() else {
                        return Err("end-if without matching if/else".into());
                    };
                    available[start.saturating_add(1)..].fill(false);
                    instruction.ty != ValueType::Ref
                        && instruction.ty == then_type
                        && types.get(value) == Some(&then_type)
                }
                Operation::Loop(initial) => {
                    if branches.len() == 64 {
                        return Err("control nesting exceeds 64 regions".into());
                    }
                    branches.push(Region::Loop(types.len(), instruction.ty, false));
                    !initializer
                        && state != ValueType::Ref
                        && instruction.ty == state
                        && types.get(initial) == Some(&state)
                }
                Operation::While(condition) => {
                    let Some(Region::Loop(_, _, tested)) = branches.last_mut() else {
                        return Err("while without matching loop".into());
                    };
                    if *tested {
                        return Err("duplicate while in loop".into());
                    }
                    *tested = true;
                    instruction.ty == ValueType::Unit
                        && types.get(condition) == Some(&ValueType::Bool)
                }
                Operation::EndLoop(value) => {
                    let Some(Region::Loop(start, state_type, true)) = branches.pop() else {
                        return Err("end-loop without matching loop/while".into());
                    };
                    available[start..].fill(false);
                    instruction.ty == state_type && types.get(value) == Some(&state_type)
                }
            };
            if !valid {
                return Err("invalid scalar IR operand, type or forward reference".into());
            }
            types.push(instruction.ty);
            available.push(!matches!(
                instruction.operation,
                Operation::If(_) | Operation::Else(_) | Operation::While(_)
            ));
        }
        if !branches.is_empty() {
            return Err("unterminated conditional in scalar IR".into());
        }
        if types.get(self.state) != Some(&state)
            || types.get(self.reply) != Some(&reply)
            || available.get(self.state) != Some(&true)
            || available.get(self.reply) != Some(&true)
        {
            return Err("invalid scalar IR state or reply register".into());
        }
        if self.root_plan().capacity > MAX_SHADOW_ROOTS {
            return Err("AOT function exceeds 64 simultaneously live reference roots".into());
        }
        Ok(())
    }

    pub fn root_plan(&self) -> RootPlan {
        let mut last_uses: Vec<usize> = (0..self.instructions.len()).collect();
        for (index, instruction) in self.instructions.iter().enumerate() {
            let operands = operands(&instruction.operation);
            for operand in operands.into_iter().flatten() {
                if let Some(last) = last_uses.get_mut(operand) {
                    *last = index;
                }
            }
        }
        for output in [self.state, self.reply] {
            if let Some(last) = last_uses.get_mut(output) {
                *last = self.instructions.len();
            }
        }
        let mut loops = Vec::new();
        for (index, instruction) in self.instructions.iter().enumerate() {
            match instruction.operation {
                Operation::Loop(_) => loops.push(index),
                Operation::EndLoop(_) => {
                    if let Some(start) = loops.pop() {
                        for last in &mut last_uses[..start] {
                            if *last > start && *last < index {
                                *last = index;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut active = Vec::new();
        let mut capacity = 0;
        for (index, instruction) in self.instructions.iter().enumerate() {
            active.retain(|register: &usize| last_uses[*register] >= index);
            capacity = capacity.max(active.len());
            if instruction.ty == ValueType::Ref && last_uses[index] > index {
                active.push(index);
            }
        }
        active.retain(|register: &usize| last_uses[*register] == self.instructions.len());
        capacity = capacity.max(active.len());
        RootPlan {
            last_uses,
            capacity,
        }
    }
}

fn operands(operation: &Operation) -> [Option<Register>; 2] {
    match *operation {
        Operation::Negate(value)
        | Operation::RecordInt(value)
        | Operation::ReadInt(value)
        | Operation::If(value)
        | Operation::Else(value)
        | Operation::EndIf(value) => [Some(value), None],
        Operation::Loop(value) | Operation::While(value) | Operation::EndLoop(value) => {
            [Some(value), None]
        }
        Operation::Binary(_, left, right) | Operation::Compare(_, left, right) => {
            [Some(left), Some(right)]
        }
        _ => [None, None],
    }
}

pub struct RootPlan {
    pub last_uses: Vec<usize>,
    pub capacity: usize,
}
