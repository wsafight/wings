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

mod body;
mod emit;

use body::compile_body;
use emit::fail_if;

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
