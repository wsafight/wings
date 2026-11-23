use super::*;
use crate::emit::{
    call_heap, emit_heap_op, emit_pure_scalar, fail_if_to, is_pure_scalar, needs_inline_roots,
    poll_control, publish_roots,
};

pub(super) fn compile_body(
    module: &mut ObjectModule,
    name: &str,
    function: &Function,
    max_frame_bytes: u32,
    procedures: &[FuncId],
) -> Result<(FuncId, u32), String> {
    let mut context = module.make_context();
    let pointer_type = module.target_config().pointer_type();
    context.func.signature.params = vec![
        AbiParam::new(types::I64),
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
    ];
    context.func.signature.returns = vec![AbiParam::new(types::I32)];
    let identifier = module
        .declare_function(
            &format!("{name}_body"),
            Linkage::Local,
            &context.func.signature,
        )
        .map_err(|error| error.to_string())?;
    let callees: Vec<_> = procedures
        .iter()
        .map(|identifier| module.declare_func_in_func(*identifier, &mut context.func))
        .collect();
    let mut literals = Vec::with_capacity(function.instructions.len());
    for instruction in &function.instructions {
        let raw_bytes: Option<&[u8]> = match &instruction.operation {
            Operation::StringLiteral(value) => Some(value.as_bytes()),
            Operation::BytesLiteral(value) => Some(value.as_slice()),
            _ => None,
        };
        let literal = if let Some(bytes) = raw_bytes {
            let identifier = module
                .declare_anonymous_data(false, false)
                .map_err(|error| error.to_string())?;
            let mut data = DataDescription::new();
            data.define(if bytes.is_empty() {
                vec![0].into_boxed_slice()
            } else {
                bytes.into()
            });
            module
                .define_data(identifier, &data)
                .map_err(|error| error.to_string())?;
            Some(module.declare_data_in_func(identifier, &mut context.func))
        } else {
            None
        };
        literals.push(literal);
    }
    let mut builder_context = FunctionBuilderContext::new();
    let mut slow_signature = module.make_signature();
    slow_signature.params.push(AbiParam::new(pointer_type));
    slow_signature.returns.push(AbiParam::new(types::I32));
    let mut heap_signature = module.make_signature();
    heap_signature.params = vec![
        AbiParam::new(pointer_type),
        AbiParam::new(types::I32),
        AbiParam::new(types::I64),
        AbiParam::new(pointer_type),
    ];
    heap_signature.returns.push(AbiParam::new(types::I32));
    let mut effect_signature = module.make_signature();
    effect_signature.params = vec![
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
    ];
    effect_signature.returns.push(AbiParam::new(types::I32));
    let plan = function.root_plan();
    let uses_heap = function.instructions.iter().any(|instruction| {
        matches!(
            instruction.operation,
            Operation::RecordNew { .. }
                | Operation::RecordField { .. }
                | Operation::Collect
                | Operation::EmptyInts
                | Operation::PrependInt(_, _)
                | Operation::HeadInt(_)
                | Operation::TailInts(_)
                | Operation::IsEmptyInts(_)
                | Operation::FillBytes(_, _)
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
                | Operation::EqualStrings(_, _)
                | Operation::StringLiteral(_)
                | Operation::BytesLiteral(_)
                | Operation::IntToString(_)
                | Operation::BoolToString(_)
        )
    });
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let slow_signature = builder.import_signature(slow_signature);
    let heap_signature = builder.import_signature(heap_signature);
    let effect_signature = builder.import_signature(effect_signature);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let parameters = builder.block_params(entry).to_vec();
    let exit = builder.create_block();
    builder.append_block_param(exit, types::I32);
    let roots = if plan.capacity != 0 {
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (std::mem::size_of::<ShadowFrame>() + plan.capacity * 8) as u32,
            3,
        ));
        let frame = builder.ins().stack_addr(pointer_type, slot, 0);
        let slots = builder.ins().stack_addr(
            pointer_type,
            slot,
            std::mem::size_of::<ShadowFrame>() as i32,
        );
        let previous = builder.ins().load(
            pointer_type,
            MemFlags::new(),
            parameters[3],
            std::mem::offset_of!(ScalarContext, roots) as i32,
        );
        builder.ins().store(
            MemFlags::new(),
            previous,
            frame,
            std::mem::offset_of!(ShadowFrame, previous) as i32,
        );
        builder.ins().store(
            MemFlags::new(),
            slots,
            frame,
            std::mem::offset_of!(ShadowFrame, slots) as i32,
        );
        let zero = builder.ins().iconst(pointer_type, 0);
        builder.ins().store(
            MemFlags::new(),
            zero,
            frame,
            std::mem::offset_of!(ShadowFrame, count) as i32,
        );
        builder.ins().store(
            MemFlags::new(),
            frame,
            parameters[3],
            std::mem::offset_of!(ScalarContext, roots) as i32,
        );
        Some((frame, slots, previous))
    } else {
        None
    };
    let heap_output = uses_heap.then(|| {
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            if function
                .instructions
                .iter()
                .any(|instruction| matches!(instruction.operation, Operation::RecordNew { .. }))
            {
                80
            } else {
                24
            },
            3,
        ));
        builder.ins().stack_addr(pointer_type, slot, 0)
    });
    let effect_arguments = function
        .instructions
        .iter()
        .any(|instruction| {
            matches!(
                instruction.operation,
                Operation::SendSelf { .. }
                    | Operation::SendAfterSelf { .. }
                    | Operation::Send { .. }
                    | Operation::SendActor { .. }
                    | Operation::SendAfter { .. }
                    | Operation::SendAfterActor { .. }
                    | Operation::Monitor { .. }
                    | Operation::SelfPid
                    | Operation::SpawnSelf
                    | Operation::SpawnActor(_)
                    | Operation::Request { .. }
                    | Operation::RequestActor { .. }
                    | Operation::CancelRequest(_)
            )
        })
        .then(|| {
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                64 * 8 + EFFECT_ARGUMENT_OFFSET as u32,
                3,
            ));
            builder.ins().stack_addr(pointer_type, slot, 0)
        });
    let mut registers = Vec::new();
    let call_storage = function
        .instructions
        .iter()
        .filter_map(|instruction| {
            if let Operation::Call { arguments, .. } = &instruction.operation {
                Some(arguments.len())
            } else {
                None
            }
        })
        .max()
        .map(|arguments| {
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                (16 + arguments * 8) as u32,
                3,
            ));
            builder.ins().stack_addr(pointer_type, slot, 0)
        });
    let mut branches = Vec::new();
    let mut loops = Vec::new();
    let mut index = 0;
    let mut tail_polled = false;
    while index < function.instructions.len() {
        if is_pure_scalar(&function.instructions[index].operation) {
            let max_end = (index + 32).min(function.instructions.len());
            let mut end = index + 1;
            while end < max_end && is_pure_scalar(&function.instructions[end].operation) {
                end += 1;
            }
            let is_tail = end == function.instructions.len() && (end - index) < 32;
            let total_steps = (end - index) + usize::from(is_tail);
            if total_steps >= 2 {
                let readonly = MemFlags::new().with_readonly();
                let cancellation = builder.ins().load(
                    pointer_type,
                    readonly,
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, cancellation) as i32,
                );
                let cancelled =
                    builder
                        .ins()
                        .atomic_load(types::I32, MemFlags::new(), cancellation);
                let yield_request = builder.ins().load(
                    pointer_type,
                    readonly,
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, yield_request) as i32,
                );
                let yielded = builder
                    .ins()
                    .atomic_load(types::I32, MemFlags::new(), yield_request);
                let any_flag = builder.ins().bor(cancelled, yielded);
                let clean_flags = builder.ins().icmp_imm(IntCC::Equal, any_flag, 0);
                let remaining = builder.ins().load(
                    types::I64,
                    MemFlags::new(),
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, remaining_steps) as i32,
                );
                let enough_remaining = builder.ins().icmp_imm(
                    IntCC::UnsignedGreaterThanOrEqual,
                    remaining,
                    total_steps as i64,
                );
                let slice = builder.ins().load(
                    types::I64,
                    MemFlags::new(),
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, slice_remaining) as i32,
                );
                let enough_slice = builder.ins().icmp_imm(
                    IntCC::UnsignedGreaterThanOrEqual,
                    slice,
                    total_steps as i64,
                );
                let enough_budget = builder.ins().band(enough_remaining, enough_slice);
                let can_fast = builder.ins().band(clean_flags, enough_budget);
                let fast_block = builder.create_block();
                let slow_block = builder.create_block();
                let join_block = builder.create_block();
                for _ in index..end {
                    builder.append_block_param(join_block, types::I64);
                }
                builder
                    .ins()
                    .brif(can_fast, fast_block, &[], slow_block, &[]);

                builder.switch_to_block(fast_block);
                registers.truncate(index);
                for offset in 0..(end - index) {
                    let value = emit_pure_scalar(
                        &mut builder,
                        &function.instructions[index + offset].operation,
                        &parameters,
                        &registers,
                        exit,
                        Some((parameters[3], remaining, slice, (offset + 1) as i64)),
                    );
                    registers.push(value);
                }
                let next_remaining = builder.ins().iadd_imm(remaining, -(total_steps as i64));
                builder.ins().store(
                    MemFlags::new(),
                    next_remaining,
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, remaining_steps) as i32,
                );
                let next_slice = builder.ins().iadd_imm(slice, -(total_steps as i64));
                builder.ins().store(
                    MemFlags::new(),
                    next_slice,
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, slice_remaining) as i32,
                );
                builder.ins().jump(join_block, &registers[index..end]);

                builder.switch_to_block(slow_block);
                registers.truncate(index);
                for current in index..end {
                    poll_control(
                        &mut builder,
                        pointer_type,
                        parameters[3],
                        slow_signature,
                        exit,
                        roots,
                        &plan.at[current],
                        &registers,
                    );
                    let value = emit_pure_scalar(
                        &mut builder,
                        &function.instructions[current].operation,
                        &parameters,
                        &registers,
                        exit,
                        None,
                    );
                    registers.push(value);
                }
                if is_tail {
                    poll_control(
                        &mut builder,
                        pointer_type,
                        parameters[3],
                        slow_signature,
                        exit,
                        roots,
                        &plan.at[function.instructions.len()],
                        &registers,
                    );
                }
                builder.ins().jump(join_block, &registers[index..end]);

                builder.switch_to_block(join_block);
                registers.truncate(index);
                registers.extend_from_slice(builder.block_params(join_block));
                index = end;
                if is_tail {
                    tail_polled = true;
                }
                continue;
            }
        }
        let instruction = &function.instructions[index];
        let early_count = match instruction.operation {
            Operation::Loop(ref initials) => {
                let header = builder.create_block();
                let after = builder.create_block();
                let arity = initials.len();
                for _ in 0..arity {
                    builder.append_block_param(header, types::I64);
                    builder.append_block_param(after, types::I64);
                }
                let args: Vec<Value> = initials.iter().map(|&reg| registers[reg]).collect();
                builder.ins().jump(header, &args);
                builder.switch_to_block(header);
                loops.push((header, after, index, arity));
                registers.extend_from_slice(builder.block_params(header));
                arity
            }
            Operation::EndIf(ref values) => {
                let (_, merge_block) = branches.pop().expect("validated branch");
                let args: Vec<Value> = values.iter().map(|&reg| registers[reg]).collect();
                builder.ins().jump(merge_block, &args);
                builder.switch_to_block(merge_block);
                registers.extend_from_slice(builder.block_params(merge_block));
                values.len()
            }
            _ => 0usize,
        };
        poll_control(
            &mut builder,
            pointer_type,
            parameters[3],
            slow_signature,
            exit,
            roots,
            &plan.at[index],
            &registers,
        );
        if needs_inline_roots(&instruction.operation) {
            publish_roots(
                &mut builder,
                pointer_type,
                roots,
                &plan.at[index],
                &registers,
            );
        }
        let value = match instruction.operation {
            Operation::Call {
                function,
                ref arguments,
            } => {
                let output = call_storage.expect("managed call storage");
                let buffer = builder.ins().iadd_imm(output, 16);
                for (index, register) in arguments.iter().enumerate() {
                    builder.ins().store(
                        MemFlags::new(),
                        registers[*register],
                        buffer,
                        (index * 8) as i32,
                    );
                }
                let state = builder.ins().iconst(types::I64, 0);
                let call = builder
                    .ins()
                    .call(callees[function], &[state, buffer, output, parameters[3]]);
                let status = builder.inst_results(call)[0];
                let success = builder.ins().icmp_imm(IntCC::Equal, status, 0);
                let continuation = builder.create_block();
                let failure = builder.create_block();
                builder.ins().brif(success, continuation, &[], failure, &[]);
                builder.switch_to_block(failure);
                builder.ins().jump(exit, &[status]);
                builder.switch_to_block(continuation);
                builder.ins().load(types::I64, MemFlags::new(), output, 8)
            }
            Operation::SendSelf { .. }
            | Operation::SendAfterSelf { .. }
            | Operation::Send { .. }
            | Operation::SendActor { .. }
            | Operation::SendAfter { .. }
            | Operation::SendAfterActor { .. }
            | Operation::Monitor { .. }
            | Operation::SelfPid
            | Operation::SpawnSelf
            | Operation::SpawnActor(_)
            | Operation::Request { .. }
            | Operation::RequestActor { .. }
            | Operation::CancelRequest(_) => {
                let (operation, handler, arguments): (_, _, &[usize]) = match &instruction.operation
                {
                    Operation::SendSelf { handler, arguments } => {
                        (wings_abi::EFFECT_SEND_SELF, *handler, arguments)
                    }
                    Operation::SendAfterSelf {
                        handler, arguments, ..
                    } => (wings_abi::EFFECT_SEND_AFTER_SELF, *handler, arguments),
                    Operation::Send {
                        handler, arguments, ..
                    } => (wings_abi::EFFECT_SEND, *handler, arguments),
                    Operation::SendActor {
                        actor,
                        handler,
                        arguments,
                        ..
                    } => (
                        wings_abi::EFFECT_SEND_ACTOR,
                        wings_abi::encode_actor_handler(*actor, *handler)? as usize,
                        arguments,
                    ),
                    Operation::SendAfter {
                        handler, arguments, ..
                    } => (wings_abi::EFFECT_SEND_AFTER, *handler, arguments),
                    Operation::SendAfterActor {
                        actor,
                        handler,
                        arguments,
                        ..
                    } => (
                        wings_abi::EFFECT_SEND_AFTER_ACTOR,
                        wings_abi::encode_actor_handler(*actor, *handler)? as usize,
                        arguments,
                    ),
                    Operation::Monitor { handler, .. } => {
                        (wings_abi::EFFECT_MONITOR, *handler, &[])
                    }
                    Operation::SelfPid => (wings_abi::EFFECT_SELF_PID, 0, &[]),
                    Operation::SpawnSelf => (wings_abi::EFFECT_SPAWN_SELF, 0, &[]),
                    Operation::SpawnActor(actor) => (wings_abi::EFFECT_SPAWN_ACTOR, *actor, &[]),
                    Operation::Request {
                        handler, arguments, ..
                    } => (wings_abi::EFFECT_REQUEST, *handler, arguments),
                    Operation::RequestActor {
                        actor,
                        handler,
                        arguments,
                        ..
                    } => (
                        wings_abi::EFFECT_REQUEST_ACTOR,
                        wings_abi::encode_actor_handler(*actor, *handler)? as usize,
                        arguments,
                    ),
                    Operation::CancelRequest(_) => (wings_abi::EFFECT_CANCEL_REQUEST, 0, &[]),
                    _ => unreachable!(),
                };
                let request = effect_arguments.expect("effect argument storage");
                let list_mask =
                    arguments
                        .iter()
                        .enumerate()
                        .fold(0u64, |mask, (index, register)| {
                            mask | (u64::from(
                                function.instructions[*register].ty
                                    == wings_abi::ValueType::ListRef,
                            ) << index)
                        });
                let list_mask = builder.ins().iconst(types::I64, list_mask as i64);
                builder.ins().store(
                    MemFlags::new(),
                    list_mask,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, list_mask) as i32,
                );
                let bytes_mask =
                    arguments
                        .iter()
                        .enumerate()
                        .fold(0u64, |mask, (index, register)| {
                            mask | (u64::from(
                                function.instructions[*register].ty
                                    == wings_abi::ValueType::BytesRef,
                            ) << index)
                        });
                let bytes_mask = builder.ins().iconst(types::I64, bytes_mask as i64);
                builder.ins().store(
                    MemFlags::new(),
                    bytes_mask,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, bytes_mask) as i32,
                );
                let string_mask =
                    arguments
                        .iter()
                        .enumerate()
                        .fold(0u64, |mask, (index, register)| {
                            mask | (u64::from(
                                function.instructions[*register].ty
                                    == wings_abi::ValueType::StringRef,
                            ) << index)
                        });
                let string_mask = builder.ins().iconst(types::I64, string_mask as i64);
                builder.ins().store(
                    MemFlags::new(),
                    string_mask,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, string_mask) as i32,
                );
                let buffer = builder
                    .ins()
                    .iadd_imm(request, i64::from(EFFECT_ARGUMENT_OFFSET));
                let output = builder
                    .ins()
                    .iadd_imm(request, i64::from(EFFECT_RESULT_OFFSET));
                for (index, register) in arguments.iter().enumerate() {
                    builder.ins().store(
                        MemFlags::new(),
                        registers[*register],
                        buffer,
                        (index * 8) as i32,
                    );
                }
                let callback = builder.ins().load(
                    pointer_type,
                    MemFlags::new().with_readonly(),
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, effect_call) as i32,
                );
                let missing = builder.ins().icmp_imm(IntCC::Equal, callback, 0);
                fail_if_to(&mut builder, missing, STATUS_INVALID_CONTROL, exit);
                let target = builder.ins().iconst(types::I32, handler as i64);
                let operation = builder.ins().iconst(types::I32, i64::from(operation));
                let pid = match &instruction.operation {
                    Operation::Send { target, .. }
                    | Operation::SendActor { target, .. }
                    | Operation::SendAfter { target, .. }
                    | Operation::SendAfterActor { target, .. }
                    | Operation::Monitor { target, .. }
                    | Operation::Request { target, .. }
                    | Operation::RequestActor { target, .. } => registers[*target],
                    Operation::CancelRequest(target) => registers[*target],
                    _ => builder.ins().iconst(types::I64, 0),
                };
                let count = builder.ins().iconst(pointer_type, arguments.len() as i64);
                let (completion, timeout) = match &instruction.operation {
                    Operation::Request {
                        callback, timeout, ..
                    }
                    | Operation::RequestActor {
                        callback, timeout, ..
                    } => (*callback, registers[*timeout]),
                    Operation::SendAfterSelf { delay, .. }
                    | Operation::SendAfter { delay, .. }
                    | Operation::SendAfterActor { delay, .. } => (0, registers[*delay]),
                    _ => (0, builder.ins().iconst(types::I64, 0)),
                };
                let completion = builder.ins().iconst(types::I32, completion as i64);
                builder.ins().store(
                    MemFlags::new(),
                    completion,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, callback) as i32,
                );
                builder.ins().store(
                    MemFlags::new(),
                    timeout,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, timeout_ms) as i32,
                );
                builder.ins().store(
                    MemFlags::new(),
                    operation,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, operation) as i32,
                );
                builder.ins().store(
                    MemFlags::new(),
                    target,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, handler) as i32,
                );
                builder.ins().store(
                    MemFlags::new(),
                    pid,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, target) as i32,
                );
                builder.ins().store(
                    MemFlags::new(),
                    buffer,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, arguments) as i32,
                );
                builder.ins().store(
                    MemFlags::new(),
                    count,
                    request,
                    std::mem::offset_of!(wings_abi::EffectRequest, count) as i32,
                );
                let call = builder.ins().call_indirect(
                    effect_signature,
                    callback,
                    &[parameters[3], request, output],
                );
                let status = builder.inst_results(call)[0];
                let failed = builder.ins().icmp_imm(IntCC::NotEqual, status, 0);
                let continued = builder.create_block();
                builder.ins().brif(failed, exit, &[status], continued, &[]);
                builder.switch_to_block(continued);
                builder.ins().load(types::I64, MemFlags::new(), output, 0)
            }
            Operation::Loop(_) => builder.ins().iconst(types::I64, 0),
            Operation::While(condition) => {
                let (_, after, header_index, arity) = *loops.last().expect("validated loop");
                let body = builder.create_block();
                builder.ins().brif(
                    registers[condition],
                    body,
                    &[],
                    after,
                    &registers[header_index..header_index + arity],
                );
                builder.switch_to_block(body);
                builder.ins().iconst(types::I64, 0)
            }
            Operation::EndLoop(ref values) => {
                let (header, after, _, _) = loops.pop().expect("validated loop");
                let args: Vec<Value> = values.iter().map(|&reg| registers[reg]).collect();
                builder.ins().jump(header, &args);
                builder.switch_to_block(after);
                registers.extend_from_slice(builder.block_params(after));
                index += values.len();
                continue;
            }
            Operation::If(condition) => {
                let then_block = builder.create_block();
                let else_block = builder.create_block();
                let merge_block = builder.create_block();
                builder
                    .ins()
                    .brif(registers[condition], then_block, &[], else_block, &[]);
                builder.switch_to_block(then_block);
                branches.push((else_block, merge_block));
                builder.ins().iconst(types::I64, 0)
            }
            Operation::Else(ref values) => {
                let (else_block, merge_block) = *branches.last().expect("validated branch");
                for _ in 0..values.len() {
                    builder.append_block_param(merge_block, types::I64);
                }
                let args: Vec<Value> = values.iter().map(|&reg| registers[reg]).collect();
                builder.ins().jump(merge_block, &args);
                builder.switch_to_block(else_block);
                builder.ins().iconst(types::I64, 0)
            }
            Operation::EndIf(_) => builder.ins().iconst(types::I64, 0),
            Operation::PhiValue(_) => unreachable!("phi values are consumed with owner op"),
            Operation::Constant(_)
            | Operation::NullPid
            | Operation::State
            | Operation::Argument(_)
            | Operation::Negate(_)
            | Operation::Compare(_, _, _)
            | Operation::Binary(_, _, _) => emit_pure_scalar(
                &mut builder,
                &instruction.operation,
                &parameters,
                &registers,
                exit,
                None,
            ),
            Operation::StringLiteral(ref value) => {
                let output = heap_output.expect("literal heap scratch");
                let address = builder
                    .ins()
                    .global_value(pointer_type, literals[index].expect("literal data"));
                builder.ins().store(MemFlags::new(), address, output, 0);
                let length = builder.ins().iconst(types::I64, value.len() as i64);
                builder.ins().store(MemFlags::new(), length, output, 8);
                let unused = builder.ins().iconst(types::I64, 0);
                builder.ins().store(MemFlags::new(), unused, output, 16);
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    wings_abi::HEAP_STRING_LITERAL,
                    output,
                    output,
                    exit,
                )
            }
            Operation::BytesLiteral(ref value) => {
                let output = heap_output.expect("literal heap scratch");
                let address = builder
                    .ins()
                    .global_value(pointer_type, literals[index].expect("literal data"));
                builder.ins().store(MemFlags::new(), address, output, 0);
                let length = builder.ins().iconst(types::I64, value.len() as i64);
                builder.ins().store(MemFlags::new(), length, output, 8);
                let unused = builder.ins().iconst(types::I64, 0);
                builder.ins().store(MemFlags::new(), unused, output, 16);
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    wings_abi::HEAP_BYTES_LITERAL,
                    output,
                    output,
                    exit,
                )
            }
            Operation::IntToString(operand) | Operation::BoolToString(operand) => {
                let op = if matches!(instruction.operation, Operation::IntToString(_)) {
                    wings_abi::HEAP_INT_TO_STRING
                } else {
                    wings_abi::HEAP_BOOL_TO_STRING
                };
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    op,
                    registers[operand],
                    heap_output.expect("heap operation scratch"),
                    exit,
                )
            }
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
            | Operation::EqualStrings(_, _)
            | Operation::RecordNew { .. }
            | Operation::RecordField { .. }
            | Operation::PrependInt(_, _)
            | Operation::HeadInt(_)
            | Operation::TailInts(_)
            | Operation::IsEmptyInts(_)
            | Operation::Collect
            | Operation::EmptyInts => emit_heap_op(
                &mut builder,
                pointer_type,
                parameters[3],
                heap_signature,
                heap_output.expect("heap operation scratch"),
                function,
                &instruction.operation,
                &registers,
                exit,
            ),
        };
        if early_count == 0 {
            registers.push(value);
            index += 1;
        } else {
            index += early_count;
        }
    }
    if !tail_polled {
        poll_control(
            &mut builder,
            pointer_type,
            parameters[3],
            slow_signature,
            exit,
            roots,
            &plan.at[function.instructions.len()],
            &registers,
        );
    }
    builder
        .ins()
        .store(MemFlags::new(), registers[function.state], parameters[2], 0);
    builder
        .ins()
        .store(MemFlags::new(), registers[function.reply], parameters[2], 8);
    let status = builder.ins().iconst(types::I32, 0);
    builder.ins().jump(exit, &[status]);
    builder.switch_to_block(exit);
    let status = builder.block_params(exit)[0];
    if let Some((_, _, previous)) = roots {
        builder.ins().store(
            MemFlags::new(),
            previous,
            parameters[3],
            std::mem::offset_of!(ScalarContext, roots) as i32,
        );
    }
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
    builder.finalize();
    module
        .define_function(identifier, &mut context)
        .map_err(|error| error.to_string())?;
    let frame_bytes = context
        .compiled_code()
        .ok_or("backend did not produce scalar machine code")?
        .frame_size;
    if frame_bytes > max_frame_bytes {
        return Err(format!(
            "scalar stack frame for {name} requires {frame_bytes} bytes, limit is {max_frame_bytes}"
        ));
    }
    module.clear_context(&mut context);
    Ok((identifier, frame_bytes))
}
