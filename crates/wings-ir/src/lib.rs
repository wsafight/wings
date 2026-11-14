use wings_abi::{MAX_SHADOW_ROOTS, ValueType};

pub const MAX_OPERATIONS: usize = 4096;
pub const MAX_HANDLERS: usize = 128;
pub const MAX_PARAMETERS: usize = 64;
pub const MAX_FUNCTIONS: usize = 64;
pub const MAX_TOTAL_OPERATIONS: usize = 65536;

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
    EmptyInts,
    PrependInt(Register, Register),
    HeadInt(Register),
    TailInts(Register),
    IsEmptyInts(Register),
    FillBytes(Register, Register),
    BytesLen(Register),
    ByteAt(Register, Register),
    ReplaceByte([Register; 3]),
    ReadI64Be(Register, Register),
    ReplaceI64Be([Register; 3]),
    SendSelf {
        handler: usize,
        arguments: Vec<Register>,
    },
    SelfPid,
    SpawnSelf,
    Send {
        target: Register,
        handler: usize,
        arguments: Vec<Register>,
    },
    Request {
        target: Register,
        handler: usize,
        callback: usize,
        timeout: Register,
        arguments: Vec<Register>,
    },
    CancelRequest(Register),
    Call {
        function: usize,
        arguments: Vec<Register>,
    },
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

impl Handler {
    pub fn is_completion(&self) -> bool {
        self.completion_reply().is_some()
    }

    pub fn completion_reply(&self) -> Option<ValueType> {
        match (self.reply, self.parameters.as_slice()) {
            (
                None,
                [
                    (_, ValueType::RequestId),
                    (_, ValueType::Int),
                    (_, ty @ (ValueType::Int | ValueType::Bool | ValueType::Unit | ValueType::Ref)),
                ],
            ) => Some(*ty),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub state: ValueType,
    pub initializer: Function,
    pub functions: Vec<Procedure>,
    pub handlers: Vec<Handler>,
}

#[derive(Clone, Debug)]
pub struct Procedure {
    pub name: String,
    pub parameters: Vec<(String, ValueType)>,
    pub result: ValueType,
    pub function: Function,
}

enum Region {
    Branch(usize, Option<ValueType>),
    Loop(usize, ValueType, bool),
}

impl Program {
    pub fn validate(&self) -> Result<(), String> {
        if self.functions.len() > MAX_FUNCTIONS {
            return Err("too many managed functions".into());
        }
        let total = self
            .functions
            .iter()
            .map(|function| &function.function)
            .chain(self.handlers.iter().map(|handler| &handler.function))
            .chain(std::iter::once(&self.initializer))
            .try_fold(0usize, |total, function| {
                total.checked_add(function.instructions.len())
            })
            .ok_or("total IR operation count overflow")?;
        if total > MAX_TOTAL_OPERATIONS {
            return Err("program exceeds 65536 total IR operations".into());
        }
        let mut paths = Vec::new();
        for (index, procedure) in self.functions.iter().enumerate() {
            if procedure.parameters.len() > MAX_PARAMETERS {
                return Err("too many managed function parameters".into());
            }
            procedure.function.validate(
                ValueType::Unit,
                &procedure.parameters,
                procedure.result,
                true,
            )?;
            if procedure
                .function
                .instructions
                .iter()
                .any(|instruction| is_effect(&instruction.operation))
            {
                return Err("effects are not supported in managed functions".into());
            }
            let path = self.validate_calls(&procedure.function, index, &paths)?;
            if path.0 > 32 {
                return Err("function call chain exceeds 32".into());
            }
            paths.push(path);
        }
        if self.state.is_capability()
            || self.handlers.iter().any(|handler| {
                handler.reply.is_some_and(ValueType::is_capability)
                    || handler.parameters.iter().any(|(_, ty)| {
                        *ty == ValueType::Pid
                            || (*ty == ValueType::RequestId && !handler.is_completion())
                    })
            })
        {
            return Err("Pid is only supported as a local value".into());
        }
        if self.handlers.iter().any(|handler| {
            matches!(
                handler.reply,
                Some(ValueType::ListRef | ValueType::BytesRef)
            )
        }) {
            return Err("list and byte references cannot cross scalar IR boundaries".into());
        }
        if self.handlers.len() > MAX_HANDLERS {
            return Err("too many handlers for the scalar prototype".into());
        }
        self.initializer
            .validate(self.state, &[], ValueType::Unit, true)?;
        self.validate_calls(&self.initializer, self.functions.len(), &paths)?;
        if self.initializer.instructions.iter().any(|instruction| {
            matches!(
                instruction.operation,
                Operation::SendSelf { .. }
                    | Operation::Send { .. }
                    | Operation::SelfPid
                    | Operation::SpawnSelf
                    | Operation::Request { .. }
                    | Operation::CancelRequest(_)
            )
        }) {
            return Err("self-send is not supported in initializers".into());
        }
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
            self.validate_calls(&handler.function, self.functions.len(), &paths)?;
            for instruction in &handler.function.instructions {
                if let Operation::Request {
                    handler: target,
                    callback,
                    arguments,
                    ..
                } = &instruction.operation
                {
                    let target = self
                        .handlers
                        .get(*target)
                        .ok_or("invalid request handler")?;
                    let callback = self
                        .handlers
                        .get(*callback)
                        .ok_or("invalid request completion")?;
                    if !matches!(
                        target.reply,
                        Some(ValueType::Int | ValueType::Bool | ValueType::Unit | ValueType::Ref)
                    ) || callback.completion_reply() != target.reply
                        || target.parameters.len() != arguments.len()
                        || arguments
                            .iter()
                            .zip(&target.parameters)
                            .any(|(register, (_, ty))| {
                                handler.function.instructions[*register].ty != *ty
                            })
                    {
                        return Err(
                            "request requires scalar or RecordInt reply, matching completion event and matching parameters"
                                .into(),
                        );
                    }
                }
                if let Operation::SendSelf {
                    handler: target,
                    arguments,
                }
                | Operation::Send {
                    handler: target,
                    arguments,
                    ..
                } = &instruction.operation
                {
                    let target = self
                        .handlers
                        .get(*target)
                        .ok_or("invalid self-send handler")?;
                    if target.reply.is_some()
                        || target.is_completion()
                        || target.parameters.len() != arguments.len()
                        || arguments
                            .iter()
                            .zip(&target.parameters)
                            .any(|(register, (_, ty))| {
                                handler.function.instructions[*register].ty != *ty
                            })
                    {
                        return Err("self-send requires matching one-way handler parameters".into());
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_calls(
        &self,
        function: &Function,
        allowed: usize,
        paths: &[(usize, usize)],
    ) -> Result<(usize, usize), String> {
        let mut depth = 1;
        let mut child_roots = 0;
        for instruction in &function.instructions {
            if let Operation::Call {
                function: target,
                arguments,
            } = &instruction.operation
            {
                let procedure = self
                    .functions
                    .get(*target)
                    .filter(|_| *target < allowed)
                    .ok_or("unknown or forward managed function call")?;
                if instruction.ty != procedure.result
                    || arguments.len() != procedure.parameters.len()
                    || arguments
                        .iter()
                        .zip(&procedure.parameters)
                        .any(|(register, (_, ty))| function.instructions[*register].ty != *ty)
                {
                    return Err("managed function call signature mismatch".into());
                }
                let path = paths[*target];
                depth = depth.max(1 + path.0);
                child_roots = child_roots.max(path.1);
            }
        }
        let roots = function.root_plan().capacity + child_roots;
        if roots > MAX_SHADOW_ROOTS {
            return Err("managed call path exceeds 64 shadow roots".into());
        }
        Ok((depth, roots))
    }
}

fn is_effect(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::SendSelf { .. }
            | Operation::Send { .. }
            | Operation::SelfPid
            | Operation::SpawnSelf
            | Operation::Request { .. }
            | Operation::CancelRequest(_)
    )
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
                Operation::EmptyInts => instruction.ty == ValueType::ListRef,
                Operation::PrependInt(value, tail) => {
                    instruction.ty == ValueType::ListRef
                        && types.get(value) == Some(&ValueType::Int)
                        && types.get(tail) == Some(&ValueType::ListRef)
                }
                Operation::HeadInt(value) => {
                    instruction.ty == ValueType::Int
                        && types.get(value) == Some(&ValueType::ListRef)
                }
                Operation::TailInts(value) => {
                    instruction.ty == ValueType::ListRef
                        && types.get(value) == Some(&ValueType::ListRef)
                }
                Operation::IsEmptyInts(value) => {
                    instruction.ty == ValueType::Bool
                        && types.get(value) == Some(&ValueType::ListRef)
                }
                Operation::FillBytes(length, value) => {
                    instruction.ty == ValueType::BytesRef
                        && types.get(length) == Some(&ValueType::Int)
                        && types.get(value) == Some(&ValueType::Int)
                }
                Operation::BytesLen(value) => {
                    instruction.ty == ValueType::Int
                        && types.get(value) == Some(&ValueType::BytesRef)
                }
                Operation::ByteAt(value, index) | Operation::ReadI64Be(value, index) => {
                    instruction.ty == ValueType::Int
                        && types.get(value) == Some(&ValueType::BytesRef)
                        && types.get(index) == Some(&ValueType::Int)
                }
                Operation::ReplaceByte([value, index, byte])
                | Operation::ReplaceI64Be([value, index, byte]) => {
                    instruction.ty == ValueType::BytesRef
                        && types.get(value) == Some(&ValueType::BytesRef)
                        && types.get(index) == Some(&ValueType::Int)
                        && types.get(byte) == Some(&ValueType::Int)
                }
                Operation::SendSelf { ref arguments, .. } => {
                    instruction.ty == ValueType::Unit
                        && arguments.len() <= MAX_PARAMETERS
                        && arguments.iter().all(|register| {
                            types.get(*register).is_some_and(|ty| {
                                matches!(
                                    ty,
                                    ValueType::Int
                                        | ValueType::Bool
                                        | ValueType::Unit
                                        | ValueType::Ref
                                        | ValueType::ListRef
                                        | ValueType::BytesRef
                                )
                            })
                        })
                }
                Operation::SelfPid | Operation::SpawnSelf => instruction.ty == ValueType::Pid,
                Operation::Request {
                    target,
                    timeout,
                    ref arguments,
                    ..
                } => {
                    instruction.ty == ValueType::RequestId
                        && types.get(target) == Some(&ValueType::Pid)
                        && types.get(timeout) == Some(&ValueType::Int)
                        && arguments.len() <= MAX_PARAMETERS
                        && arguments.iter().all(|register| {
                            types.get(*register).is_some_and(|ty| {
                                matches!(
                                    ty,
                                    ValueType::Int
                                        | ValueType::Bool
                                        | ValueType::Unit
                                        | ValueType::Ref
                                        | ValueType::ListRef
                                        | ValueType::BytesRef
                                )
                            })
                        })
                }
                Operation::CancelRequest(value) => {
                    instruction.ty == ValueType::Unit
                        && types.get(value) == Some(&ValueType::RequestId)
                }
                Operation::Call { ref arguments, .. } => arguments.len() <= MAX_PARAMETERS,
                Operation::Send {
                    target,
                    ref arguments,
                    ..
                } => {
                    instruction.ty == ValueType::Unit
                        && types.get(target) == Some(&ValueType::Pid)
                        && arguments.len() <= MAX_PARAMETERS
                        && arguments.iter().all(|register| {
                            types.get(*register).is_some_and(|ty| {
                                matches!(
                                    ty,
                                    ValueType::Int
                                        | ValueType::Bool
                                        | ValueType::Unit
                                        | ValueType::Ref
                                        | ValueType::ListRef
                                        | ValueType::BytesRef
                                )
                            })
                        })
                }
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
                    instruction.ty == ValueType::Unit
                }
                Operation::EndIf(value) => {
                    let Some(Region::Branch(start, Some(then_type))) = branches.pop() else {
                        return Err("end-if without matching if/else".into());
                    };
                    available[start.saturating_add(1)..].fill(false);
                    instruction.ty == then_type && types.get(value) == Some(&then_type)
                }
                Operation::Loop(initial) => {
                    if branches.len() == 64 {
                        return Err("control nesting exceeds 64 regions".into());
                    }
                    branches.push(Region::Loop(types.len(), instruction.ty, false));
                    !initializer && instruction.ty == state && types.get(initial) == Some(&state)
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
            for operand in operands {
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
                Operation::While(_) => {
                    if let Some(start) = loops.last() {
                        last_uses[*start] = last_uses[*start].max(index);
                    }
                }
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
        let mut branches = Vec::new();
        let mut loops = Vec::new();
        let mut at = Vec::with_capacity(self.instructions.len() + 1);
        let mut capacity = 0;
        for (index, instruction) in self.instructions.iter().enumerate() {
            if matches!(instruction.operation, Operation::Loop(_)) {
                active.retain(|register: &usize| last_uses[*register] > index);
                loops.push(active.clone());
                if instruction.ty.is_reference() && last_uses[index] > index {
                    active.push(index);
                }
            }
            if matches!(instruction.operation, Operation::EndIf(_)) {
                active = branches.pop().unwrap_or_default();
                if instruction.ty.is_reference() && last_uses[index] > index {
                    active.push(index);
                }
            }
            active.retain(|register: &usize| last_uses[*register] >= index);
            capacity = capacity.max(active.len());
            at.push(active.clone());
            match instruction.operation {
                Operation::If(_) => branches.push(active.clone()),
                Operation::Else(_) => active = branches.last().cloned().unwrap_or_default(),
                Operation::EndLoop(_) => active = loops.pop().unwrap_or_default(),
                _ => {}
            }
            if instruction.ty.is_reference()
                && last_uses[index] > index
                && !matches!(
                    instruction.operation,
                    Operation::EndIf(_) | Operation::Loop(_)
                )
            {
                active.push(index);
            }
        }
        active.retain(|register: &usize| last_uses[*register] == self.instructions.len());
        capacity = capacity.max(active.len());
        at.push(active);
        RootPlan {
            last_uses,
            capacity,
            at,
        }
    }
}

fn operands(operation: &Operation) -> impl Iterator<Item = Register> + '_ {
    let (fixed, extra): (_, &[Register]) = match *operation {
        Operation::Negate(value)
        | Operation::RecordInt(value)
        | Operation::ReadInt(value)
        | Operation::If(value)
        | Operation::Else(value)
        | Operation::EndIf(value) => ([Some(value), None], &[]),
        Operation::HeadInt(value)
        | Operation::TailInts(value)
        | Operation::IsEmptyInts(value)
        | Operation::BytesLen(value) => ([Some(value), None], &[]),
        Operation::Loop(value) | Operation::While(value) | Operation::EndLoop(value) => {
            ([Some(value), None], &[])
        }
        Operation::Binary(_, left, right)
        | Operation::Compare(_, left, right)
        | Operation::FillBytes(left, right)
        | Operation::ByteAt(left, right)
        | Operation::ReadI64Be(left, right)
        | Operation::PrependInt(left, right) => ([Some(left), Some(right)], &[]),
        Operation::ReplaceByte(ref arguments) | Operation::ReplaceI64Be(ref arguments) => {
            ([None, None], arguments.as_slice())
        }
        Operation::SendSelf { ref arguments, .. } => ([None, None], arguments),
        Operation::Call { ref arguments, .. } => ([None, None], arguments),
        Operation::Send {
            target,
            ref arguments,
            ..
        } => ([Some(target), None], arguments),
        Operation::Request {
            target,
            timeout,
            ref arguments,
            ..
        } => ([Some(target), Some(timeout)], arguments),
        Operation::CancelRequest(value) => ([Some(value), None], &[]),
        _ => ([None, None], &[]),
    };
    fixed.into_iter().flatten().chain(extra.iter().copied())
}

pub struct RootPlan {
    pub last_uses: Vec<usize>,
    pub capacity: usize,
    pub at: Vec<Vec<Register>>,
}
