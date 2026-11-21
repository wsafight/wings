use std::fs;
use std::path::Path;
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, Block, InstBuilder, MemFlags, SigRef, StackSlotData, StackSlotKind, Value,
    condcodes::IntCC, types,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use wings_abi::{
    HEAP_COLLECT, HEAP_EMPTY_INTS, HEAP_HEAD_INT, HEAP_IS_EMPTY_INTS, HEAP_PREPEND_INT,
    HEAP_TAIL_INTS, SCALAR_ABI_VERSION, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED,
    STATUS_DIVISION_BY_ZERO, STATUS_INVALID_CONTROL, STATUS_OVERFLOW, STATUS_STACK_EXHAUSTED,
    ScalarContext, ShadowFrame, host_target,
};
use wings_artifact::{
    BUNDLE_FORMAT_VERSION, BundleManifest, FORMAT_VERSION, INITIALIZER_SYMBOL, Manifest,
    handler_symbol, library_name,
};
use wings_ir::{Comparison, Function, Operation, Operator, Program};

pub const MAX_SCALAR_FRAME_BYTES: u32 = 64 * 1024;
const ENTRY_STACK_MARGIN: u32 = 4 * 1024;
const EFFECT_RESULT_OFFSET: i32 = std::mem::size_of::<wings_abi::EffectRequest>() as i32;
const EFFECT_ARGUMENT_OFFSET: i32 = EFFECT_RESULT_OFFSET + 8;

pub fn build(program: &Program, directory: &Path) -> Result<(), String> {
    build_with_frame_limit(program, directory, MAX_SCALAR_FRAME_BYTES)
}

pub fn build_app(programs: &[Program], directory: &Path) -> Result<(), String> {
    if programs.is_empty() || programs.len() > wings_artifact::MAX_BUNDLE_ACTORS {
        return Err("application bundle requires 1..=32 actors".into());
    }
    let actors: Vec<String> = programs.iter().map(|p| p.name.clone()).collect();
    let entry = actors
        .iter()
        .find(|name| name.as_str() == "Main")
        .cloned()
        .unwrap_or_else(|| actors.last().expect("non-empty actors").clone());
    let bundle_bytes = BundleManifest {
        format: BUNDLE_FORMAT_VERSION,
        abi: SCALAR_ABI_VERSION,
        target: host_target()?.into(),
        entry,
        actors,
    }
    .encode()?;

    fs::create_dir(directory).map_err(|error| format!("create new bundle directory: {error}"))?;
    let build_inner = (|| -> Result<(), String> {
        let actors_dir = directory.join("actors");
        fs::create_dir(&actors_dir).map_err(|error| error.to_string())?;
        for program in programs {
            build(program, &actors_dir.join(&program.name))?;
        }
        fs::write(directory.join("bundle.json"), bundle_bytes)
            .map_err(|error| error.to_string())?;
        Ok(())
    })();
    if build_inner.is_err() {
        let _ = fs::remove_dir_all(directory);
    }
    build_inner
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
        records: program.records.clone(),
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
                completion: handler.completion,
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
    let pointer_type = module.target_config().pointer_type();
    let mut entry_sig = module.make_signature();
    entry_sig.params = vec![
        AbiParam::new(types::I64),
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
    ];
    entry_sig.returns = vec![AbiParam::new(types::I32)];
    let mut procedures = Vec::with_capacity(program.functions.len());
    for index in 0..program.functions.len() {
        let func_id = module
            .declare_function(
                &format!("wings_managed_function_{index}"),
                Linkage::Local,
                &entry_sig,
            )
            .map_err(|error| error.to_string())?;
        procedures.push(func_id);
    }
    let mut procedure_frames = Vec::with_capacity(program.functions.len());
    for (index, procedure) in program.functions.iter().enumerate() {
        let (_, entry_frame) = compile_function(
            &mut module,
            &format!("wings_managed_function_{index}"),
            &procedure.function,
            max_frame_bytes,
            Linkage::Local,
            Some(procedures[index]),
            &procedures,
        )?;
        procedure_frames.push(entry_frame);
    }
    let mut procedure_summaries = vec![DirectSummary::default(); program.functions.len()];
    for _ in 0..program.functions.len() {
        let mut next = Vec::with_capacity(program.functions.len());
        let mut changed = false;
        for (index, procedure) in program.functions.iter().enumerate() {
            let summary = summarize_direct_function(
                &procedure.function,
                procedure_frames[index],
                &procedure_summaries,
            );
            if summary.eligible != procedure_summaries[index].eligible
                || summary.base_steps != procedure_summaries[index].base_steps
                || summary.max_stack_bytes != procedure_summaries[index].max_stack_bytes
            {
                changed = true;
            }
            next.push(summary);
        }
        procedure_summaries = next;
        if !changed {
            break;
        }
    }
    compile_function(
        &mut module,
        INITIALIZER_SYMBOL,
        &program.initializer,
        max_frame_bytes,
        Linkage::Export,
        None,
        &procedures,
    )?;
    let mut direct_meta_bytes = Vec::with_capacity(program.handlers.len() * 16);
    for (index, handler) in program.handlers.iter().enumerate() {
        let (_, entry_frame) = compile_function(
            &mut module,
            &handler_symbol(index),
            &handler.function,
            max_frame_bytes,
            Linkage::Export,
            None,
            &procedures,
        )?;
        let summary =
            summarize_direct_function(&handler.function, entry_frame, &procedure_summaries);
        let (word0, word1) = summary.encode();
        direct_meta_bytes.extend_from_slice(&word0.to_ne_bytes());
        direct_meta_bytes.extend_from_slice(&word1.to_ne_bytes());
    }
    let meta_id = module
        .declare_data("wings_scalar_direct_meta", Linkage::Export, false, false)
        .map_err(|error| error.to_string())?;
    let mut meta_data = DataDescription::new();
    meta_data.define(if direct_meta_bytes.is_empty() {
        vec![0u8; 16].into_boxed_slice()
    } else {
        direct_meta_bytes.into_boxed_slice()
    });
    meta_data.set_align(8);
    module
        .define_data(meta_id, &meta_data)
        .map_err(|error| error.to_string())?;
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

#[derive(Clone, Copy, Default)]
struct DirectSummary {
    eligible: bool,
    uses_heap: bool,
    base_steps: u64,
    state_byte_copies: u64,
    record_allocs: u64,
    max_stack_bytes: u32,
}

impl DirectSummary {
    fn encode(self) -> (u64, u64) {
        if !self.eligible
            || self.base_steps == 0
            || self.max_stack_bytes > 32 * 1024
            || self.state_byte_copies > u64::from(u16::MAX)
            || self.record_allocs > u64::from(u16::MAX)
        {
            return (0, 0);
        }
        let flags =
            u64::from(self.uses_heap) | (self.state_byte_copies << 1) | (self.record_allocs << 17);
        (self.base_steps, flags)
    }
}

fn summarize_direct_function(
    function: &Function,
    own_frame_bytes: u32,
    callees: &[DirectSummary],
) -> DirectSummary {
    let mut base_steps = 1u64;
    let mut uses_heap = false;
    let mut state_byte_copies = 0u64;
    let mut record_allocs = 0u64;
    let mut max_callee_stack = 0u32;
    for instruction in &function.instructions {
        match instruction.operation {
            Operation::Constant(_)
            | Operation::NullPid
            | Operation::State
            | Operation::Argument(_)
            | Operation::Negate(_)
            | Operation::Compare(_, _, _)
            | Operation::Binary(_, _, _)
            | Operation::If(_)
            | Operation::Else(_)
            | Operation::EndIf(_)
            | Operation::PhiValue(_) => {
                let Some(next) = base_steps.checked_add(1) else {
                    return DirectSummary::default();
                };
                base_steps = next;
            }
            Operation::Call {
                function: callee_idx,
                ..
            } => {
                let Some(callee) = callees.get(callee_idx).copied() else {
                    return DirectSummary::default();
                };
                if !callee.eligible {
                    return DirectSummary::default();
                }
                let Some(next) = base_steps
                    .checked_add(1)
                    .and_then(|steps| steps.checked_add(callee.base_steps))
                else {
                    return DirectSummary::default();
                };
                base_steps = next;
                uses_heap |= callee.uses_heap;
                state_byte_copies = state_byte_copies.saturating_add(callee.state_byte_copies);
                record_allocs = record_allocs.saturating_add(callee.record_allocs);
                max_callee_stack = max_callee_stack.max(callee.max_stack_bytes);
            }
            Operation::BytesLen(_)
            | Operation::ByteAt(_, _)
            | Operation::ReadI64Be(_, _)
            | Operation::IsEmptyInts(_)
            | Operation::HeadInt(_)
            | Operation::TailInts(_)
            | Operation::RecordField { .. } => {
                uses_heap = true;
                let Some(next) = base_steps.checked_add(2) else {
                    return DirectSummary::default();
                };
                base_steps = next;
            }
            Operation::EmptyInts | Operation::PrependInt(_, _) | Operation::RecordNew { .. } => {
                uses_heap = true;
                let Some(next) = base_steps.checked_add(2) else {
                    return DirectSummary::default();
                };
                base_steps = next;
                record_allocs = record_allocs.saturating_add(1);
            }
            Operation::ReplaceByte([source, _, _]) | Operation::ReplaceI64Be([source, _, _]) => {
                if !function
                    .instructions
                    .get(source)
                    .is_some_and(|origin| matches!(origin.operation, Operation::State))
                {
                    return DirectSummary::default();
                }
                uses_heap = true;
                let Some(next) = base_steps.checked_add(2) else {
                    return DirectSummary::default();
                };
                base_steps = next;
                state_byte_copies = state_byte_copies.saturating_add(1);
            }
            _ => return DirectSummary::default(),
        }
    }
    let Some(max_stack_bytes) = own_frame_bytes.checked_add(max_callee_stack) else {
        return DirectSummary::default();
    };
    DirectSummary {
        eligible: true,
        uses_heap,
        base_steps,
        state_byte_copies,
        record_allocs,
        max_stack_bytes,
    }
}

fn compile_function(
    module: &mut ObjectModule,
    name: &str,
    function: &Function,
    max_frame_bytes: u32,
    linkage: Linkage,
    declared_id: Option<FuncId>,
    procedures: &[FuncId],
) -> Result<(FuncId, u32), String> {
    let (body, frame_bytes) = compile_body(module, name, function, max_frame_bytes, procedures)?;
    let mut context = module.make_context();
    let pointer_type = module.target_config().pointer_type();
    context.func.signature.params = vec![
        AbiParam::new(types::I64),
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
        AbiParam::new(pointer_type),
    ];
    context.func.signature.returns = vec![AbiParam::new(types::I32)];
    let identifier = match declared_id {
        Some(id) => id,
        None => module
            .declare_function(name, linkage, &context.func.signature)
            .map_err(|error| error.to_string())?,
    };
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
    Ok((identifier, frame_bytes + ENTRY_STACK_MARGIN))
}

fn compile_body(
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
                let cancellation = builder.ins().load(
                    pointer_type,
                    MemFlags::new(),
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, cancellation) as i32,
                );
                let cancelled =
                    builder
                        .ins()
                        .atomic_load(types::I32, MemFlags::new(), cancellation);
                let not_cancelled = builder.ins().icmp_imm(IntCC::Equal, cancelled, 0);
                let yield_request = builder.ins().load(
                    pointer_type,
                    MemFlags::new(),
                    parameters[3],
                    std::mem::offset_of!(ScalarContext, yield_request) as i32,
                );
                let yielded = builder
                    .ins()
                    .atomic_load(types::I32, MemFlags::new(), yield_request);
                let not_yielded = builder.ins().icmp_imm(IntCC::Equal, yielded, 0);
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
                let clean_flags = builder.ins().band(not_cancelled, not_yielded);
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
                    MemFlags::new(),
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
            Operation::Constant(value) => builder.ins().iconst(types::I64, value),
            Operation::NullPid => builder.ins().iconst(types::I64, 0),
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
                    Operator::Divide | Operator::Remainder => {
                        let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                        fail_if_to(&mut builder, zero, STATUS_DIVISION_BY_ZERO, exit);
                        let minimum = builder.ins().icmp_imm(IntCC::Equal, left, i64::MIN);
                        let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                        let overflow = builder.ins().band(minimum, negative_one);
                        fail_if_to(&mut builder, overflow, STATUS_OVERFLOW, exit);
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
                        fail_if_to(&mut builder, overflow, STATUS_OVERFLOW, exit);
                        result
                    }
                }
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
            | Operation::EqualStrings(_, _) => {
                let output = heap_output.expect("heap operation scratch");
                let (operation, arguments) = match instruction.operation {
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
                    &mut builder,
                    pointer_type,
                    parameters[3],
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
                let output = heap_output.expect("record scratch");
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
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    wings_abi::HEAP_RECORD_NEW,
                    argument,
                    output,
                    exit,
                )
            }
            Operation::RecordField { value, field } => {
                let output = heap_output.expect("record scratch");
                let wings_abi::ValueType::RecordRef(schema) = function.instructions[value].ty
                else {
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
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    wings_abi::HEAP_RECORD_FIELD,
                    argument,
                    output,
                    exit,
                )
            }
            Operation::PrependInt(value, tail) => {
                let output = heap_output.expect("heap operation scratch");
                builder
                    .ins()
                    .store(MemFlags::new(), registers[tail], output, 0);
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    HEAP_PREPEND_INT,
                    registers[value],
                    output,
                    exit,
                )
            }
            Operation::HeadInt(value)
            | Operation::TailInts(value)
            | Operation::IsEmptyInts(value) => {
                let operation = match instruction.operation {
                    Operation::HeadInt(_) => HEAP_HEAD_INT,
                    Operation::TailInts(_) => HEAP_TAIL_INTS,
                    _ => HEAP_IS_EMPTY_INTS,
                };
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    operation,
                    registers[value],
                    heap_output.expect("heap operation scratch"),
                    exit,
                )
            }
            Operation::Collect | Operation::EmptyInts => {
                let argument = builder.ins().iconst(types::I64, 0);
                call_heap(
                    &mut builder,
                    pointer_type,
                    parameters[3],
                    heap_signature,
                    if matches!(instruction.operation, Operation::Collect) {
                        HEAP_COLLECT
                    } else {
                        HEAP_EMPTY_INTS
                    },
                    argument,
                    heap_output.expect("heap operation scratch"),
                    exit,
                )
            }
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

fn is_pure_scalar(operation: &Operation) -> bool {
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

fn needs_inline_roots(operation: &Operation) -> bool {
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

fn emit_pure_scalar(
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

fn fail_scalar_if(
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
fn poll_control(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: types::Type,
    context: Value,
    slow_signature: SigRef,
    exit: Block,
    roots: Option<(Value, Value, Value)>,
    live: &[usize],
    registers: &[Value],
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
    publish_roots(builder, pointer_type, roots, live, registers);
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
