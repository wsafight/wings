use std::fs;
use std::path::Path;
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, Block, InstBuilder, MemFlags, SigRef, StackSlotData, StackSlotKind, Value,
    condcodes::IntCC, types,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use wings_abi::{
    HEAP_COLLECT, HEAP_READ_INT, HEAP_RECORD_INT, SCALAR_ABI_VERSION, STATUS_BUDGET_EXHAUSTED,
    STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO, STATUS_INVALID_CONTROL, STATUS_OVERFLOW,
    STATUS_STACK_EXHAUSTED, ScalarContext, ShadowFrame, host_target,
};
use wings_artifact::{FORMAT_VERSION, INITIALIZER_SYMBOL, Manifest, handler_symbol, library_name};
use wings_ir::{Comparison, Function, Operation, Operator, Program};

pub const MAX_SCALAR_FRAME_BYTES: u32 = 64 * 1024;
const ENTRY_STACK_MARGIN: u32 = 4 * 1024;

pub fn build(program: &Program, directory: &Path) -> Result<(), String> {
    build_with_frame_limit(program, directory, MAX_SCALAR_FRAME_BYTES)
}

pub fn build_with_frame_limit(
    program: &Program,
    directory: &Path,
    max_frame_bytes: u32,
) -> Result<(), String> {
    if max_frame_bytes == 0 || max_frame_bytes > MAX_SCALAR_FRAME_BYTES {
        return Err("scalar frame limit must be within 1..=65536 bytes".into());
    }
    program.validate()?;
    let manifest = Manifest {
        format: FORMAT_VERSION,
        abi: SCALAR_ABI_VERSION,
        target: host_target()?.into(),
        process: program.name.clone(),
        state: program.state,
        handlers: program
            .handlers
            .iter()
            .map(|handler| wings_artifact::Handler {
                name: handler.name.clone(),
                parameters: handler.parameters.clone(),
                reply: handler.reply,
            })
            .collect(),
    }
    .encode()?;
    let mut flags = settings::builder();
    flags
        .set("is_pic", "true")
        .map_err(|error| error.to_string())?;
    flags
        .set("opt_level", "speed")
        .map_err(|error| error.to_string())?;
    let isa = cranelift_native::builder_with_options(false)
        .map_err(|error| error.to_string())?
        .finish(settings::Flags::new(flags))
        .map_err(|error| error.to_string())?;
    let object = ObjectBuilder::new(isa, "wings_scalar", default_libcall_names())
        .map_err(|error| error.to_string())?;
    let mut module = ObjectModule::new(object);
    compile_function(
        &mut module,
        INITIALIZER_SYMBOL,
        &program.initializer,
        max_frame_bytes,
    )?;
    for (index, handler) in program.handlers.iter().enumerate() {
        compile_function(
            &mut module,
            &handler_symbol(index),
            &handler.function,
            max_frame_bytes,
        )?;
    }
    let bytes = module.finish().emit().map_err(|error| error.to_string())?;
    fs::create_dir(directory).map_err(|error| format!("create new artifact directory: {error}"))?;
    let directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let object_path = directory.join("program.o");
    fs::write(&object_path, bytes).map_err(|error| error.to_string())?;
    let linker = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let mut command = Command::new(linker);
    if cfg!(target_os = "macos") {
        command.arg("-dynamiclib");
    } else {
        command.args(["-shared", "-Wl,-z,defs", "-Wl,-z,noexecstack"]);
    }
    let output = command
        .arg(&object_path)
        .arg("-o")
        .arg(directory.join(library_name()))
        .output()
        .map_err(|error| format!("start system linker (cc or CC): {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "system linker failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    fs::write(directory.join("manifest.json"), manifest).map_err(|error| error.to_string())?;
    fs::remove_file(object_path).map_err(|error| error.to_string())?;
    Ok(())
}

fn compile_function(
    module: &mut ObjectModule,
    name: &str,
    function: &Function,
    max_frame_bytes: u32,
) -> Result<(), String> {
    let (body, frame_bytes) = compile_body(module, name, function, max_frame_bytes)?;
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
        .declare_function(name, Linkage::Export, &context.func.signature)
        .map_err(|error| error.to_string())?;
    let body = module.declare_func_in_func(body, &mut context.func);
    let mut builder_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let parameters = builder.block_params(entry).to_vec();
    let limit = builder.ins().load(
        pointer_type,
        MemFlags::new(),
        parameters[3],
        std::mem::offset_of!(ScalarContext, stack_limit) as i32,
    );
    let missing = builder.ins().icmp_imm(IntCC::Equal, limit, 0);
    fail_if(&mut builder, missing, STATUS_STACK_EXHAUSTED);
    let stack_pointer = builder.ins().get_stack_pointer(pointer_type);
    let below_limit = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, stack_pointer, limit);
    fail_if(&mut builder, below_limit, STATUS_STACK_EXHAUSTED);
    let available = builder.ins().isub(stack_pointer, limit);
    let insufficient = builder.ins().icmp_imm(
        IntCC::UnsignedLessThan,
        available,
        i64::from(frame_bytes + ENTRY_STACK_MARGIN),
    );
    fail_if(&mut builder, insufficient, STATUS_STACK_EXHAUSTED);
    let call = builder.ins().call(body, &parameters);
    let status = builder.inst_results(call)[0];
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
    builder.finalize();
    module
        .define_function(identifier, &mut context)
        .map_err(|error| error.to_string())?;
    let entry_bytes = context
        .compiled_code()
        .ok_or("backend did not produce scalar entry machine code")?
        .frame_size;
    if entry_bytes > ENTRY_STACK_MARGIN {
        return Err(format!(
            "scalar entry frame for {name} requires {entry_bytes} bytes, limit is {ENTRY_STACK_MARGIN}"
        ));
    }
    module.clear_context(&mut context);
    Ok(())
}

fn compile_body(
    module: &mut ObjectModule,
    name: &str,
    function: &Function,
    max_frame_bytes: u32,
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
    let plan = function.root_plan();
    let uses_heap = function.instructions.iter().any(|instruction| {
        matches!(
            instruction.operation,
            Operation::RecordInt(_) | Operation::ReadInt(_) | Operation::Collect
        )
    });
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let slow_signature = builder.import_signature(slow_signature);
    let heap_signature = builder.import_signature(heap_signature);
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
        let slot =
            builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        builder.ins().stack_addr(pointer_type, slot, 0)
    });
    let mut registers = Vec::new();
    let mut branches = Vec::new();
    let mut loops = Vec::new();
    for (index, instruction) in function.instructions.iter().enumerate() {
        let loop_header = if let Operation::Loop(initial) = instruction.operation {
            let header = builder.create_block();
            let after = builder.create_block();
            builder.append_block_param(header, types::I64);
            builder.append_block_param(after, types::I64);
            builder.ins().jump(header, &[registers[initial]]);
            builder.switch_to_block(header);
            loops.push((header, after, index));
            Some(builder.block_params(header)[0])
        } else {
            None
        };
        let merged = if let Operation::EndIf(value) = instruction.operation {
            let (_, merge_block) = branches.pop().expect("validated branch");
            builder.ins().jump(merge_block, &[registers[value]]);
            builder.switch_to_block(merge_block);
            Some(builder.block_params(merge_block)[0])
        } else {
            None
        };
        let early_value = loop_header.or(merged);
        if let Some(value) = early_value {
            registers.push(value);
        }
        publish_roots(
            &mut builder,
            pointer_type,
            roots,
            &plan.at[index],
            &registers,
        );
        poll_control(
            &mut builder,
            pointer_type,
            parameters[3],
            slow_signature,
            exit,
        );
        let value = match instruction.operation {
            Operation::Loop(_) => loop_header.expect("validated loop header"),
            Operation::While(condition) => {
                let (_, after, header_index) = *loops.last().expect("validated loop");
                let body = builder.create_block();
                builder.ins().brif(
                    registers[condition],
                    body,
                    &[],
                    after,
                    &[registers[header_index]],
                );
                builder.switch_to_block(body);
                builder.ins().iconst(types::I64, 0)
            }
            Operation::EndLoop(value) => {
                let (header, after, _) = loops.pop().expect("validated loop");
                builder.ins().jump(header, &[registers[value]]);
                builder.switch_to_block(after);
                builder.block_params(after)[0]
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
            Operation::Else(value) => {
                let (else_block, merge_block) = *branches.last().expect("validated branch");
                builder.append_block_param(merge_block, types::I64);
                builder.ins().jump(merge_block, &[registers[value]]);
                builder.switch_to_block(else_block);
                builder.ins().iconst(types::I64, 0)
            }
            Operation::EndIf(_) => merged.expect("validated branch merge"),
            Operation::Constant(value) => builder.ins().iconst(types::I64, value),
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
                fail_if_to(&mut builder, overflow, STATUS_OVERFLOW, exit);
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
                    Operator::Divide => {
                        let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                        fail_if_to(&mut builder, zero, STATUS_DIVISION_BY_ZERO, exit);
                        let minimum = builder.ins().icmp_imm(IntCC::Equal, left, i64::MIN);
                        let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                        let overflow = builder.ins().band(minimum, negative_one);
                        fail_if_to(&mut builder, overflow, STATUS_OVERFLOW, exit);
                        builder.ins().sdiv(left, right)
                    }
                    _ => {
                        let (result, overflow) = match operator {
                            Operator::Add => builder.ins().sadd_overflow(left, right),
                            Operator::Subtract => builder.ins().ssub_overflow(left, right),
                            Operator::Multiply => builder.ins().smul_overflow(left, right),
                            Operator::Divide => unreachable!(),
                        };
                        fail_if_to(&mut builder, overflow, STATUS_OVERFLOW, exit);
                        result
                    }
                }
            }
            Operation::RecordInt(operand) | Operation::ReadInt(operand) => {
                let operation = if matches!(instruction.operation, Operation::RecordInt(_)) {
                    HEAP_RECORD_INT
                } else {
                    HEAP_READ_INT
                };
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    operation,
                    registers[operand],
                    heap_output.expect("heap operation scratch"),
                    exit,
                )
            }
            Operation::Collect => {
                let argument = builder.ins().iconst(types::I64, 0);
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    HEAP_COLLECT,
                    argument,
                    heap_output.expect("heap operation scratch"),
                    exit,
                )
            }
        };
        if early_value.is_none() {
            registers.push(value);
        }
    }
    publish_roots(
        &mut builder,
        pointer_type,
        roots,
        &plan.at[function.instructions.len()],
        &registers,
    );
    poll_control(
        &mut builder,
        pointer_type,
        parameters[3],
        slow_signature,
        exit,
    );
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

fn poll_control(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: types::Type,
    context: Value,
    slow_signature: SigRef,
    exit: Block,
) {
    let poll = builder.create_block();
    builder.ins().jump(poll, &[]);
    builder.switch_to_block(poll);
    let cancellation = builder.ins().load(
        pointer_type,
        MemFlags::new(),
        context,
        std::mem::offset_of!(ScalarContext, cancellation) as i32,
    );
    let requested = builder
        .ins()
        .atomic_load(types::I32, MemFlags::new(), cancellation);
    let cancelled = builder.ins().icmp_imm(IntCC::NotEqual, requested, 0);
    fail_if_to(builder, cancelled, STATUS_CANCELLED, exit);
    let remaining = builder.ins().load(
        types::I64,
        MemFlags::new(),
        context,
        std::mem::offset_of!(ScalarContext, remaining_steps) as i32,
    );
    let exhausted = builder.ins().icmp_imm(IntCC::Equal, remaining, 0);
    fail_if_to(builder, exhausted, STATUS_BUDGET_EXHAUSTED, exit);
    let yield_request = builder.ins().load(
        pointer_type,
        MemFlags::new(),
        context,
        std::mem::offset_of!(ScalarContext, yield_request) as i32,
    );
    let requested = builder
        .ins()
        .atomic_load(types::I32, MemFlags::new(), yield_request);
    let requested = builder.ins().icmp_imm(IntCC::NotEqual, requested, 0);
    let slice = builder.ins().load(
        types::I64,
        MemFlags::new(),
        context,
        std::mem::offset_of!(ScalarContext, slice_remaining) as i32,
    );
    let expired = builder.ins().icmp_imm(IntCC::Equal, slice, 0);
    let should_yield = builder.ins().bor(requested, expired);
    let slow_path = builder.create_block();
    let continuation = builder.create_block();
    builder
        .ins()
        .brif(should_yield, slow_path, &[], continuation, &[]);
    builder.switch_to_block(slow_path);
    let callback = builder.ins().load(
        pointer_type,
        MemFlags::new(),
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
    let failure = builder.create_block();
    builder.ins().brif(success, poll, &[], failure, &[]);
    builder.switch_to_block(failure);
    builder.ins().jump(exit, &[status]);
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

fn publish_roots(
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
fn call_heap(
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
        MemFlags::new(),
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

fn fail_if_to(builder: &mut FunctionBuilder<'_>, condition: Value, status: u32, exit: Block) {
    let continuation = builder.create_block();
    let status = builder.ins().iconst(types::I32, i64::from(status));
    builder
        .ins()
        .brif(condition, exit, &[status], continuation, &[]);
    builder.switch_to_block(continuation);
}

fn fail_if(builder: &mut FunctionBuilder<'_>, condition: Value, status: u32) {
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
