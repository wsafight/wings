use wings_abi::{MAX_SHADOW_ROOTS, ValueType};

mod root_plan;

pub const MAX_OPERATIONS: usize = 4096;
pub const MAX_HANDLERS: usize = 128;
pub const MAX_PARAMETERS: usize = 64;
pub const MAX_FUNCTIONS: usize = 64;
pub const MAX_TOTAL_OPERATIONS: usize = 65536;
pub const MAX_LITERAL_BYTES: usize = 1 << 20;

pub type Register = usize;

#[derive(Clone, Copy, Debug)]
pub enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
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
    StringLiteral(String),
    BytesLiteral(Vec<u8>),
    IntToString(Register),
    BoolToString(Register),
    RecordNew {
        schema: u16,
        arguments: Vec<Register>,
    },
    RecordField {
        value: Register,
        field: usize,
    },
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
    StringFromBytes(Register),
    StringBytes(Register),
    StringLen(Register),
    StringAt(Register, Register),
    StringSlice([Register; 3]),
    ConcatStrings(Register, Register),
    EqualStrings(Register, Register),
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
    ElseValues(Vec<Register>),
    EndIfValues(Vec<Register>),
    Loop(Register),
    LoopValues(Vec<Register>),
    While(Register),
    EndLoop(Register),
    EndLoopValues(Vec<Register>),
    PhiValue(usize),
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
    pub completion: Option<ValueType>,
    pub function: Function,
}

impl Handler {
    pub fn is_completion(&self) -> bool {
        self.completion_reply().is_some()
    }

    pub fn completion_reply(&self) -> Option<ValueType> {
        self.completion
            .or(match (self.reply, self.parameters.as_slice()) {
                (
                    None,
                    [
                        (_, ValueType::RequestId),
                        (_, ValueType::Int),
                        (
                            _,
                            ty @ (ValueType::Int
                            | ValueType::Bool
                            | ValueType::Unit
                            | ValueType::Ref
                            | ValueType::ListRef
                            | ValueType::BytesRef
                            | ValueType::StringRef),
                        ),
                    ],
                ) => Some(*ty),
                _ => None,
            })
    }
}

#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub records: Vec<wings_abi::RecordSchema>,
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

#[derive(Clone, Debug)]
enum ElseArm {
    Single(ValueType),
    Many(Vec<ValueType>),
}

enum Region {
    Branch(usize, Option<ElseArm>),
    Loop(usize, ValueType, bool),
    LoopValues(usize, Vec<ValueType>, bool),
}

impl Program {
    pub fn validate(&self) -> Result<(), String> {
        wings_abi::validate_records(&self.records)?;
        if self.functions.iter().any(|procedure| std::iter::once(procedure.result)
            .chain(procedure.parameters.iter().map(|(_, ty)| *ty))
            .any(|ty| matches!(ty, ValueType::RecordRef(index) if usize::from(index) >= self.records.len()))) {
            return Err("invalid named record function signature".into());
        }
        let functions = self
            .functions
            .iter()
            .map(|procedure| &procedure.function)
            .chain(self.handlers.iter().map(|handler| &handler.function))
            .chain(std::iter::once(&self.initializer));
        for function in functions {
            for instruction in &function.instructions {
                if matches!(instruction.ty, ValueType::RecordRef(index) if usize::from(index) >= self.records.len())
                {
                    return Err("invalid named record type index".into());
                }
                match &instruction.operation {
                    Operation::RecordNew { schema, arguments } => {
                        let record = self
                            .records
                            .get(usize::from(*schema))
                            .ok_or("invalid record schema index")?;
                        if instruction.ty != ValueType::RecordRef(*schema)
                            || record.fields.len() != arguments.len()
                            || record
                                .fields
                                .iter()
                                .zip(arguments)
                                .any(|((_, ty), register)| {
                                    function.instructions.get(*register).map(|value| value.ty)
                                        != Some(*ty)
                                })
                        {
                            return Err("record constructor field type mismatch".into());
                        }
                    }
                    Operation::RecordField { value, field } => {
                        let Some(ValueType::RecordRef(schema)) =
                            function.instructions.get(*value).map(|value| value.ty)
                        else {
                            return Err("record field requires named record".into());
                        };
                        if self
                            .records
                            .get(usize::from(schema))
                            .and_then(|record| record.fields.get(*field))
                            .map(|(_, ty)| *ty)
                            != Some(instruction.ty)
                        {
                            return Err("invalid record field index or result type".into());
                        }
                    }
                    _ => {}
                }
            }
        }
        if matches!(self.state, ValueType::RecordRef(index) if usize::from(index) >= self.records.len())
            || self.handlers.iter().any(|handler| {
                handler
                    .parameters
                    .iter()
                    .any(|(_, ty)| matches!(ty, ValueType::RecordRef(_)))
            })
        {
            return Err("named records cannot be message parameters".into());
        }
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
        self.functions
            .iter()
            .map(|function| &function.function)
            .chain(self.handlers.iter().map(|handler| &handler.function))
            .chain(std::iter::once(&self.initializer))
            .flat_map(|function| &function.instructions)
            .try_fold(0usize, |total, instruction| match &instruction.operation {
                Operation::StringLiteral(value) => total.checked_add(value.len()),
                Operation::BytesLiteral(value) => total.checked_add(value.len()),
                _ => Some(total),
            })
            .filter(|total| *total <= MAX_LITERAL_BYTES)
            .ok_or("program exceeds 1 MiB total string literal bytes")?;
        let mut paths = Vec::new();
        for (index, procedure) in self.functions.iter().enumerate() {
            if procedure.parameters.len() > MAX_PARAMETERS {
                return Err("too many managed function parameters".into());
            }
            procedure.function.validate(
                ValueType::Unit,
                &procedure.parameters,
                procedure.result,
                false,
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
        if self.handlers.len() > MAX_HANDLERS {
            return Err("too many handlers for the scalar prototype".into());
        }
        self.initializer
            .validate(self.state, &[], ValueType::Unit, false, false)?;
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
            if let Some(ValueType::RecordRef(schema)) = handler.completion {
                let expansion = wings_abi::expand_record_schema(&self.records, schema)?;
                if handler.reply.is_some()
                    || handler.parameters.len() != expansion.count + 2
                    || handler.parameters[0].1 != ValueType::RequestId
                    || handler.parameters[1].1 != ValueType::Int
                    || handler.parameters[2..]
                        .iter()
                        .zip(&expansion.fields[..expansion.count])
                        .any(|((_, actual), expected)| actual != expected)
                {
                    return Err("invalid named record completion signature".into());
                }
            } else if handler.completion.is_some() {
                return Err("explicit completion metadata is reserved for named records".into());
            }
            if let Some(ValueType::RecordRef(schema)) = handler.reply {
                wings_abi::expand_record_schema(&self.records, schema)?;
            }
            handler.function.validate(
                self.state,
                &handler.parameters,
                handler.reply.unwrap_or(ValueType::Unit),
                true,
                true,
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
                        Some(
                            ValueType::Int
                                | ValueType::Bool
                                | ValueType::Unit
                                | ValueType::Ref
                                | ValueType::ListRef
                                | ValueType::BytesRef
                                | ValueType::StringRef
                                | ValueType::RecordRef(_)
                        )
                    ) || callback.completion_reply() != target.reply
                        || matches!(target.reply, Some(ValueType::RecordRef(index))
                            if wings_abi::expand_record_schema(&self.records, index).is_err())
                        || target.parameters.len() != arguments.len()
                        || arguments
                            .iter()
                            .zip(&target.parameters)
                            .any(|(register, (_, ty))| {
                                handler.function.instructions[*register].ty != *ty
                            })
                    {
                        return Err(
                            "request requires a supported reply, matching completion event and matching parameters"
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
        allow_state: bool,
        allow_loops: bool,
    ) -> Result<(), String> {
        if self.instructions.len() > MAX_OPERATIONS {
            return Err("too many operations for the scalar prototype".into());
        }
        let mut types = Vec::new();
        let mut available = Vec::new();
        let mut branches = Vec::new();
        let mut expected_phis: std::collections::VecDeque<(usize, ValueType)> =
            std::collections::VecDeque::new();
        for instruction in &self.instructions {
            if let Some((expected_index, expected_ty)) = expected_phis.pop_front() {
                if !matches!(instruction.operation, Operation::PhiValue(actual) if actual == expected_index)
                    || instruction.ty != expected_ty
                {
                    return Err("invalid or missing phi result instruction".into());
                }
                types.push(instruction.ty);
                available.push(true);
                continue;
            }
            if matches!(instruction.operation, Operation::PhiValue(_)) {
                return Err("unexpected phi result instruction".into());
            }
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
                Operation::StringLiteral(ref value) => {
                    instruction.ty == ValueType::StringRef && value.len() <= 65536
                }
                Operation::BytesLiteral(ref value) => {
                    instruction.ty == ValueType::BytesRef && value.len() <= 65536
                }
                Operation::IntToString(value) => {
                    instruction.ty == ValueType::StringRef
                        && types.get(value) == Some(&ValueType::Int)
                }
                Operation::BoolToString(value) => {
                    instruction.ty == ValueType::StringRef
                        && types.get(value) == Some(&ValueType::Bool)
                }
                Operation::RecordNew { .. } | Operation::RecordField { .. } => true,
                Operation::State => allow_state && instruction.ty == state,
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
                Operation::StringFromBytes(value) => {
                    instruction.ty == ValueType::StringRef
                        && types.get(value) == Some(&ValueType::BytesRef)
                }
                Operation::StringBytes(value) => {
                    instruction.ty == ValueType::BytesRef
                        && types.get(value) == Some(&ValueType::StringRef)
                }
                Operation::StringLen(value) => {
                    instruction.ty == ValueType::Int
                        && types.get(value) == Some(&ValueType::StringRef)
                }
                Operation::StringAt(value, index) => {
                    instruction.ty == ValueType::Int
                        && types.get(value) == Some(&ValueType::StringRef)
                        && types.get(index) == Some(&ValueType::Int)
                }
                Operation::StringSlice([value, start, length]) => {
                    instruction.ty == ValueType::StringRef
                        && types.get(value) == Some(&ValueType::StringRef)
                        && types.get(start) == Some(&ValueType::Int)
                        && types.get(length) == Some(&ValueType::Int)
                }
                Operation::ConcatStrings(left, right) => {
                    instruction.ty == ValueType::StringRef
                        && types.get(left) == Some(&ValueType::StringRef)
                        && types.get(right) == Some(&ValueType::StringRef)
                }
                Operation::EqualStrings(left, right) => {
                    instruction.ty == ValueType::Bool
                        && types.get(left) == Some(&ValueType::StringRef)
                        && types.get(right) == Some(&ValueType::StringRef)
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
                                        | ValueType::StringRef
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
                                        | ValueType::StringRef
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
                                        | ValueType::StringRef
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
                    let Some(Region::Branch(start, arm)) = branches.last_mut() else {
                        return Err("else without matching if".into());
                    };
                    if arm.is_some() {
                        return Err("duplicate else in conditional".into());
                    }
                    let Some(&ty) = types.get(value) else {
                        return Err("invalid scalar IR operand, type or forward reference".into());
                    };
                    *arm = Some(ElseArm::Single(ty));
                    available[start.saturating_add(1)..].fill(false);
                    instruction.ty == ValueType::Unit
                }
                Operation::ElseValues(ref values) => {
                    let Some(Region::Branch(start, arm)) = branches.last_mut() else {
                        return Err("else without matching if".into());
                    };
                    if arm.is_some() {
                        return Err("duplicate else in conditional".into());
                    }
                    if values.is_empty() || values.len() > MAX_PARAMETERS {
                        return Err("invalid multi-value branch arity".into());
                    }
                    let then_types: Vec<ValueType> = values.iter().map(|&reg| types[reg]).collect();
                    *arm = Some(ElseArm::Many(then_types));
                    available[start.saturating_add(1)..].fill(false);
                    instruction.ty == ValueType::Unit
                }
                Operation::EndIf(value) => {
                    let Some(Region::Branch(start, Some(ElseArm::Single(then_type)))) =
                        branches.pop()
                    else {
                        return Err("end-if without matching if/else".into());
                    };
                    available[start.saturating_add(1)..].fill(false);
                    instruction.ty == then_type && types.get(value) == Some(&then_type)
                }
                Operation::EndIfValues(ref values) => {
                    let Some(Region::Branch(start, Some(ElseArm::Many(then_types)))) =
                        branches.pop()
                    else {
                        return Err("end-if without matching multi-value if/else".into());
                    };
                    available[start.saturating_add(1)..].fill(false);
                    let valid = values.len() == then_types.len()
                        && instruction.ty == then_types[0]
                        && values
                            .iter()
                            .zip(&then_types)
                            .all(|(&reg, &ty)| types.get(reg) == Some(&ty));
                    for (idx, &ty) in then_types.iter().enumerate().skip(1) {
                        expected_phis.push_back((idx, ty));
                    }
                    valid
                }
                Operation::Loop(initial) => {
                    if branches.len() == 64 {
                        return Err("control nesting exceeds 64 regions".into());
                    }
                    branches.push(Region::Loop(types.len(), instruction.ty, false));
                    allow_loops
                        && allow_state
                        && instruction.ty == state
                        && types.get(initial) == Some(&state)
                }
                Operation::LoopValues(ref initials) => {
                    if branches.len() == 64 {
                        return Err("control nesting exceeds 64 regions".into());
                    }
                    if !allow_loops || initials.is_empty() || initials.len() > MAX_PARAMETERS {
                        false
                    } else {
                        let phi_types: Vec<ValueType> =
                            initials.iter().map(|&reg| types[reg]).collect();
                        let valid = instruction.ty == phi_types[0];
                        for (idx, &ty) in phi_types.iter().enumerate().skip(1) {
                            expected_phis.push_back((idx, ty));
                        }
                        branches.push(Region::LoopValues(types.len(), phi_types, false));
                        valid
                    }
                }
                Operation::While(condition) => {
                    let tested = match branches.last_mut() {
                        Some(Region::Loop(_, _, tested) | Region::LoopValues(_, _, tested)) => {
                            tested
                        }
                        _ => return Err("while without matching loop".into()),
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
                Operation::EndLoopValues(ref values) => {
                    let Some(Region::LoopValues(start, phi_types, true)) = branches.pop() else {
                        return Err("end-loop without matching loop/while".into());
                    };
                    available[start..].fill(false);
                    let valid = values.len() == phi_types.len()
                        && instruction.ty == phi_types[0]
                        && values
                            .iter()
                            .zip(&phi_types)
                            .all(|(&reg, &ty)| types.get(reg) == Some(&ty));
                    for (idx, &ty) in phi_types.iter().enumerate().skip(1) {
                        expected_phis.push_back((idx, ty));
                    }
                    valid
                }
                Operation::PhiValue(_) => false,
            };
            if !valid {
                return Err("invalid scalar IR operand, type or forward reference".into());
            }
            types.push(instruction.ty);
            available.push(!matches!(
                instruction.operation,
                Operation::If(_)
                    | Operation::Else(_)
                    | Operation::ElseValues(_)
                    | Operation::While(_)
            ));
        }
        if !expected_phis.is_empty() {
            return Err("missing phi result instruction".into());
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
        root_plan::build(self)
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
        | Operation::BytesLen(value)
        | Operation::StringFromBytes(value)
        | Operation::StringBytes(value)
        | Operation::StringLen(value)
        | Operation::IntToString(value)
        | Operation::BoolToString(value) => ([Some(value), None], &[]),
        Operation::StringAt(value, index) => ([Some(value), Some(index)], &[]),
        Operation::Loop(value) | Operation::While(value) | Operation::EndLoop(value) => {
            ([Some(value), None], &[])
        }
        Operation::ElseValues(ref values)
        | Operation::EndIfValues(ref values)
        | Operation::LoopValues(ref values)
        | Operation::EndLoopValues(ref values) => ([None, None], values.as_slice()),
        Operation::Binary(_, left, right)
        | Operation::Compare(_, left, right)
        | Operation::FillBytes(left, right)
        | Operation::ConcatStrings(left, right)
        | Operation::EqualStrings(left, right)
        | Operation::ByteAt(left, right)
        | Operation::ReadI64Be(left, right)
        | Operation::PrependInt(left, right) => ([Some(left), Some(right)], &[]),
        Operation::ReplaceByte(ref arguments) | Operation::ReplaceI64Be(ref arguments) => {
            ([None, None], arguments.as_slice())
        }
        Operation::StringSlice(ref arguments) => ([None, None], arguments.as_slice()),
        Operation::SendSelf { ref arguments, .. } => ([None, None], arguments),
        Operation::Call { ref arguments, .. } => ([None, None], arguments),
        Operation::RecordNew { ref arguments, .. } => ([None, None], arguments),
        Operation::RecordField { value, .. } => ([Some(value), None], &[]),
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
