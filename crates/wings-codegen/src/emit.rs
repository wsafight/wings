use super::*;

pub(super) fn is_pure_scalar(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Constant(_)
            | Operation::NullPid
            | Operation::State
            | Operation::Argument(_)
            | Operation::Negate(_)
            | Operation::Compare(_, _, _)
            | Operation::Binary(_, _, _)
    )
}

pub(super) fn needs_inline_roots(operation: &Operation) -> bool {
    !is_pure_scalar(operation)
        && !matches!(
            operation,
            Operation::Loop(_)
                | Operation::While(_)
                | Operation::EndLoop(_)
                | Operation::If(_)
                | Operation::Else(_)
                | Operation::EndIf(_)
                | Operation::PhiValue(_)
        )
}

pub(super) fn emit_pure_scalar(
    builder: &mut FunctionBuilder<'_>,
    operation: &Operation,
    parameters: &[Value],
    registers: &[Value],
    exit: Block,
    fast_step: Option<(Value, Value, Value, i64)>,
) -> Value {
    match *operation {
        Operation::Constant(value) => builder.ins().iconst(types::I64, value),
        Operation::NullPid => builder.ins().iconst(types::I64, 0),
        Operation::State => parameters[0],
        Operation::Argument(index) => builder.ins().load(
            types::I64,
            MemFlags::new(),
            parameters[1],
            (index * 8) as i32,
        ),
        Operation::Negate(value) => {
            let zero = builder.ins().iconst(types::I64, 0);
            let (result, overflow) = builder.ins().ssub_overflow(zero, registers[value]);
            fail_scalar_if(builder, overflow, STATUS_OVERFLOW, exit, fast_step);
            result
        }
        Operation::Compare(comparison, left, right) => {
            let condition = match comparison {
                Comparison::Equal => IntCC::Equal,
                Comparison::NotEqual => IntCC::NotEqual,
                Comparison::Less => IntCC::SignedLessThan,
                Comparison::LessEqual => IntCC::SignedLessThanOrEqual,
                Comparison::Greater => IntCC::SignedGreaterThan,
                Comparison::GreaterEqual => IntCC::SignedGreaterThanOrEqual,
            };
            let result = builder
                .ins()
                .icmp(condition, registers[left], registers[right]);
            builder.ins().uextend(types::I64, result)
        }
        Operation::Binary(operator, left, right) => {
            let left = registers[left];
            let right = registers[right];
            match operator {
                Operator::Divide | Operator::Remainder => {
                    let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                    fail_scalar_if(builder, zero, STATUS_DIVISION_BY_ZERO, exit, fast_step);
                    let minimum = builder.ins().icmp_imm(IntCC::Equal, left, i64::MIN);
                    let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                    let overflow = builder.ins().band(minimum, negative_one);
                    fail_scalar_if(builder, overflow, STATUS_OVERFLOW, exit, fast_step);
                    if matches!(operator, Operator::Divide) {
                        builder.ins().sdiv(left, right)
                    } else {
                        builder.ins().srem(left, right)
                    }
                }
                _ => {
                    let (result, overflow) = match operator {
                        Operator::Add => builder.ins().sadd_overflow(left, right),
                        Operator::Subtract => builder.ins().ssub_overflow(left, right),
                        Operator::Multiply => builder.ins().smul_overflow(left, right),
                        Operator::Divide | Operator::Remainder => unreachable!(),
                    };
                    fail_scalar_if(builder, overflow, STATUS_OVERFLOW, exit, fast_step);
                    result
                }
            }
        }
        _ => unreachable!("expected pure scalar operation"),
    }
}

pub(super) fn fail_scalar_if(
    builder: &mut FunctionBuilder<'_>,
    condition: Value,
    status: u32,
    exit: Block,
    fast_step: Option<(Value, Value, Value, i64)>,
) {
    if let Some((context, remaining, slice, consumed)) = fast_step {
        let failure = builder.create_block();
        let continuation = builder.create_block();
        builder
            .ins()
            .brif(condition, failure, &[], continuation, &[]);
        builder.switch_to_block(failure);
        let next_remaining = builder.ins().iadd_imm(remaining, -consumed);
        builder.ins().store(
            MemFlags::new(),
            next_remaining,
            context,
            std::mem::offset_of!(ScalarContext, remaining_steps) as i32,
        );
        let next_slice = builder.ins().iadd_imm(slice, -consumed);
        builder.ins().store(
            MemFlags::new(),
            next_slice,
            context,
            std::mem::offset_of!(ScalarContext, slice_remaining) as i32,
        );
        let status = builder.ins().iconst(types::I32, i64::from(status));
        builder.ins().jump(exit, &[status]);
        builder.switch_to_block(continuation);
    } else {
        fail_if_to(builder, condition, status, exit);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn poll_control(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: types::Type,
    context: Value,
    slow_signature: SigRef,
    exit: Block,
    roots: Option<(Value, Value, Value)>,
    live: &[usize],
    registers: &[Value],
) {
    let readonly = MemFlags::new().with_readonly();
    let poll = builder.create_block();
    builder.ins().jump(poll, &[]);
    builder.switch_to_block(poll);
    let cancellation = builder.ins().load(
        pointer_type,
        readonly,
        context,
        std::mem::offset_of!(ScalarContext, cancellation) as i32,
    );
    let cancel_flag = builder
        .ins()
        .atomic_load(types::I32, MemFlags::new(), cancellation);
    let yield_request = builder.ins().load(
        pointer_type,
        readonly,
        context,
        std::mem::offset_of!(ScalarContext, yield_request) as i32,
    );
    let yield_flag = builder
        .ins()
        .atomic_load(types::I32, MemFlags::new(), yield_request);
    let remaining = builder.ins().load(
        types::I64,
        MemFlags::new(),
        context,
        std::mem::offset_of!(ScalarContext, remaining_steps) as i32,
    );
    let slice = builder.ins().load(
        types::I64,
        MemFlags::new(),
        context,
        std::mem::offset_of!(ScalarContext, slice_remaining) as i32,
    );
    let any_flag = builder.ins().bor(cancel_flag, yield_flag);
    let clean_flags = builder.ins().icmp_imm(IntCC::Equal, any_flag, 0);
    let min_budget = builder.ins().umin(remaining, slice);
    let has_budget = builder.ins().icmp_imm(IntCC::NotEqual, min_budget, 0);
    let can_continue = builder.ins().band(clean_flags, has_budget);
    let continuation = builder.create_block();
    let slow_check = builder.create_block();
    builder
        .ins()
        .brif(can_continue, continuation, &[], slow_check, &[]);

    builder.switch_to_block(slow_check);
    let cancelled = builder.ins().icmp_imm(IntCC::NotEqual, cancel_flag, 0);
    fail_if_to(builder, cancelled, STATUS_CANCELLED, exit);
    let exhausted = builder.ins().icmp_imm(IntCC::Equal, remaining, 0);
    fail_if_to(builder, exhausted, STATUS_BUDGET_EXHAUSTED, exit);
    publish_roots(builder, pointer_type, roots, live, registers);
    let callback = builder.ins().load(
        pointer_type,
        readonly,
        context,
        std::mem::offset_of!(ScalarContext, slow_path) as i32,
    );
    let missing = builder.ins().icmp_imm(IntCC::Equal, callback, 0);
    fail_if_to(builder, missing, STATUS_INVALID_CONTROL, exit);
    let call = builder
        .ins()
        .call_indirect(slow_signature, callback, &[context]);
    let status = builder.inst_results(call)[0];
    let success = builder.ins().icmp_imm(IntCC::Equal, status, 0);
    builder.ins().brif(success, poll, &[], exit, &[status]);

    builder.switch_to_block(continuation);
    let remaining = builder.ins().iadd_imm(remaining, -1);
    builder.ins().store(
        MemFlags::new(),
        remaining,
        context,
        std::mem::offset_of!(ScalarContext, remaining_steps) as i32,
    );
    let slice = builder.ins().iadd_imm(slice, -1);
    builder.ins().store(
        MemFlags::new(),
        slice,
        context,
        std::mem::offset_of!(ScalarContext, slice_remaining) as i32,
    );
}

pub(super) fn publish_roots(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: types::Type,
    roots: Option<(Value, Value, Value)>,
    live: &[usize],
    registers: &[Value],
) {
    if let Some((frame, slots, _)) = roots {
        for (index, register) in live.iter().enumerate() {
            builder.ins().store(
                MemFlags::new(),
                registers[*register],
                slots,
                (index * 8) as i32,
            );
        }
        let count = builder.ins().iconst(pointer_type, live.len() as i64);
        builder.ins().store(
            MemFlags::new(),
            count,
            frame,
            std::mem::offset_of!(ShadowFrame, count) as i32,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn call_heap(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: types::Type,
    context: Value,
    signature: SigRef,
    operation: u32,
    argument: Value,
    output: Value,
    exit: Block,
) -> Value {
    let callback = builder.ins().load(
        pointer_type,
        MemFlags::new().with_readonly(),
        context,
        std::mem::offset_of!(ScalarContext, heap_call) as i32,
    );
    let missing = builder.ins().icmp_imm(IntCC::Equal, callback, 0);
    fail_if_to(builder, missing, STATUS_INVALID_CONTROL, exit);
    let operation = builder.ins().iconst(types::I32, i64::from(operation));
    let call =
        builder
            .ins()
            .call_indirect(signature, callback, &[context, operation, argument, output]);
    let status = builder.inst_results(call)[0];
    let success = builder.ins().icmp_imm(IntCC::Equal, status, 0);
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(success, continuation, &[], exit, &[status]);
    builder.switch_to_block(continuation);
    builder.ins().load(types::I64, MemFlags::new(), output, 0)
}

pub(super) fn fail_if_to(
    builder: &mut FunctionBuilder<'_>,
    condition: Value,
    status: u32,
    exit: Block,
) {
    let continuation = builder.create_block();
    let status = builder.ins().iconst(types::I32, i64::from(status));
    builder
        .ins()
        .brif(condition, exit, &[status], continuation, &[]);
    builder.switch_to_block(continuation);
}

pub(super) fn fail_if(builder: &mut FunctionBuilder<'_>, condition: Value, status: u32) {
    let failure = builder.create_block();
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(condition, failure, &[], continuation, &[]);
    builder.switch_to_block(failure);
    let status = builder.ins().iconst(types::I32, i64::from(status));
    builder.ins().return_(&[status]);
    builder.switch_to_block(continuation);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_heap_op(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: types::Type,
    context: Value,
    heap_signature: SigRef,
    output: Value,
    function: &Function,
    operation: &Operation,
    registers: &[Value],
    exit: Block,
) -> Value {
    match *operation {
        Operation::FillBytes(_, _)
        | Operation::BytesLen(_)
        | Operation::ByteAt(_, _)
        | Operation::ReplaceByte(_)
        | Operation::ReadI64Be(_, _)
        | Operation::ReplaceI64Be(_)
        | Operation::StringFromBytes(_)
        | Operation::StringBytes(_)
        | Operation::StringLen(_)
        | Operation::StringAt(_, _)
        | Operation::StringSlice(_)
        | Operation::ConcatStrings(_, _)
        | Operation::EqualStrings(_, _) => {
            let (operation, arguments) = match *operation {
                Operation::StringFromBytes(value) => {
                    (wings_abi::HEAP_STRING_FROM_BYTES, [Some(value), None, None])
                }
                Operation::StringBytes(value) => {
                    (wings_abi::HEAP_STRING_BYTES, [Some(value), None, None])
                }
                Operation::StringLen(value) => {
                    (wings_abi::HEAP_STRING_LEN, [Some(value), None, None])
                }
                Operation::StringAt(value, index) => {
                    (wings_abi::HEAP_STRING_AT, [Some(value), Some(index), None])
                }
                Operation::StringSlice([value, start, length]) => (
                    wings_abi::HEAP_STRING_SLICE,
                    [Some(value), Some(start), Some(length)],
                ),
                Operation::ConcatStrings(left, right) => (
                    wings_abi::HEAP_CONCAT_STRINGS,
                    [Some(left), Some(right), None],
                ),
                Operation::EqualStrings(left, right) => (
                    wings_abi::HEAP_EQUAL_STRINGS,
                    [Some(left), Some(right), None],
                ),
                Operation::FillBytes(length, value) => (
                    wings_abi::HEAP_FILL_BYTES,
                    [Some(length), Some(value), None],
                ),
                Operation::BytesLen(value) => {
                    (wings_abi::HEAP_BYTES_LEN, [Some(value), None, None])
                }
                Operation::ByteAt(value, index) => {
                    (wings_abi::HEAP_BYTE_AT, [Some(value), Some(index), None])
                }
                Operation::ReplaceByte([value, index, byte]) => (
                    wings_abi::HEAP_REPLACE_BYTE,
                    [Some(value), Some(index), Some(byte)],
                ),
                Operation::ReadI64Be(value, index) => (
                    wings_abi::HEAP_READ_I64_BE,
                    [Some(value), Some(index), None],
                ),
                Operation::ReplaceI64Be([value, index, number]) => (
                    wings_abi::HEAP_REPLACE_I64_BE,
                    [Some(value), Some(index), Some(number)],
                ),
                _ => unreachable!(),
            };
            for (index, argument) in arguments.into_iter().enumerate() {
                let value = argument
                    .map(|register| registers[register])
                    .unwrap_or_else(|| builder.ins().iconst(types::I64, 0));
                builder
                    .ins()
                    .store(MemFlags::new(), value, output, (index * 8) as i32);
            }
            let argument = if pointer_type == types::I64 {
                output
            } else {
                builder.ins().uextend(types::I64, output)
            };
            call_heap(
                builder,
                pointer_type,
                context,
                heap_signature,
                operation,
                argument,
                output,
                exit,
            )
        }
        Operation::RecordNew {
            schema,
            ref arguments,
        } => {
            let identifier = builder.ins().iconst(types::I64, i64::from(schema));
            builder.ins().store(MemFlags::new(), identifier, output, 0);
            for (index, register) in arguments.iter().enumerate() {
                builder.ins().store(
                    MemFlags::new(),
                    registers[*register],
                    output,
                    ((index + 1) * 8) as i32,
                );
            }
            let argument = if pointer_type == types::I64 {
                output
            } else {
                builder.ins().uextend(types::I64, output)
            };
            call_heap(
                builder,
                pointer_type,
                context,
                heap_signature,
                wings_abi::HEAP_RECORD_NEW,
                argument,
                output,
                exit,
            )
        }
        Operation::RecordField { value, field } => {
            let wings_abi::ValueType::RecordRef(schema) = function.instructions[value].ty else {
                unreachable!("validated record")
            };
            let identifier = builder.ins().iconst(types::I64, i64::from(schema));
            let index = builder.ins().iconst(types::I64, field as i64);
            builder.ins().store(MemFlags::new(), identifier, output, 0);
            builder
                .ins()
                .store(MemFlags::new(), registers[value], output, 8);
            builder.ins().store(MemFlags::new(), index, output, 16);
            let argument = if pointer_type == types::I64 {
                output
            } else {
                builder.ins().uextend(types::I64, output)
            };
            call_heap(
                builder,
                pointer_type,
                context,
                heap_signature,
                wings_abi::HEAP_RECORD_FIELD,
                argument,
                output,
                exit,
            )
        }
        Operation::PrependInt(value, tail) => {
            builder
                .ins()
                .store(MemFlags::new(), registers[tail], output, 0);
            call_heap(
                builder,
                pointer_type,
                context,
                heap_signature,
                HEAP_PREPEND_INT,
                registers[value],
                output,
                exit,
            )
        }
        Operation::HeadInt(value) | Operation::TailInts(value) | Operation::IsEmptyInts(value) => {
            let operation = match *operation {
                Operation::HeadInt(_) => HEAP_HEAD_INT,
                Operation::TailInts(_) => HEAP_TAIL_INTS,
                _ => HEAP_IS_EMPTY_INTS,
            };
            call_heap(
                builder,
                pointer_type,
                context,
                heap_signature,
                operation,
                registers[value],
                output,
                exit,
            )
        }
        Operation::Collect | Operation::EmptyInts => {
            let argument = builder.ins().iconst(types::I64, 0);
            call_heap(
                builder,
                pointer_type,
                context,
                heap_signature,
                if matches!(*operation, Operation::Collect) {
                    HEAP_COLLECT
                } else {
                    HEAP_EMPTY_INTS
                },
                argument,
                output,
                exit,
            )
        }
        _ => unreachable!("expected heap operation"),
    }
}
