use std::fs;
use std::path::Path;
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlags, SigRef, Value, condcodes::IntCC, types,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use wings_abi::{
    SCALAR_ABI_VERSION, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED, STATUS_DIVISION_BY_ZERO,
    STATUS_INVALID_CONTROL, STATUS_OVERFLOW, ScalarContext, host_target,
};
use wings_artifact::{FORMAT_VERSION, INITIALIZER_SYMBOL, Manifest, handler_symbol, library_name};
use wings_ir::{Function, Operation, Operator, Program};

pub fn build(program: &Program, directory: &Path) -> Result<(), String> {
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
    compile_function(&mut module, INITIALIZER_SYMBOL, &program.initializer)?;
    for (index, handler) in program.handlers.iter().enumerate() {
        compile_function(&mut module, &handler_symbol(index), &handler.function)?;
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
) -> Result<(), String> {
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
    let mut builder_context = FunctionBuilderContext::new();
    let mut slow_signature = module.make_signature();
    slow_signature.params.push(AbiParam::new(pointer_type));
    slow_signature.returns.push(AbiParam::new(types::I32));
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let slow_signature = builder.import_signature(slow_signature);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let parameters = builder.block_params(entry).to_vec();
    let mut registers = Vec::new();
    for instruction in &function.instructions {
        poll_control(&mut builder, pointer_type, parameters[3], slow_signature);
        let value = match instruction.operation {
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
                fail_if(&mut builder, overflow, STATUS_OVERFLOW);
                result
            }
            Operation::Binary(operator, left, right) => {
                let left = registers[left];
                let right = registers[right];
                match operator {
                    Operator::Divide => {
                        let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                        fail_if(&mut builder, zero, STATUS_DIVISION_BY_ZERO);
                        let minimum = builder.ins().icmp_imm(IntCC::Equal, left, i64::MIN);
                        let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                        let overflow = builder.ins().band(minimum, negative_one);
                        fail_if(&mut builder, overflow, STATUS_OVERFLOW);
                        builder.ins().sdiv(left, right)
                    }
                    _ => {
                        let (result, overflow) = match operator {
                            Operator::Add => builder.ins().sadd_overflow(left, right),
                            Operator::Subtract => builder.ins().ssub_overflow(left, right),
                            Operator::Multiply => builder.ins().smul_overflow(left, right),
                            Operator::Divide => unreachable!(),
                        };
                        fail_if(&mut builder, overflow, STATUS_OVERFLOW);
                        result
                    }
                }
            }
        };
        registers.push(value);
    }
    poll_control(&mut builder, pointer_type, parameters[3], slow_signature);
    builder
        .ins()
        .store(MemFlags::new(), registers[function.state], parameters[2], 0);
    builder
        .ins()
        .store(MemFlags::new(), registers[function.reply], parameters[2], 8);
    let status = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
    builder.finalize();
    module
        .define_function(identifier, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(())
}

fn poll_control(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: types::Type,
    context: Value,
    slow_signature: SigRef,
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
    fail_if(builder, cancelled, STATUS_CANCELLED);
    let remaining = builder.ins().load(
        types::I64,
        MemFlags::new(),
        context,
        std::mem::offset_of!(ScalarContext, remaining_steps) as i32,
    );
    let exhausted = builder.ins().icmp_imm(IntCC::Equal, remaining, 0);
    fail_if(builder, exhausted, STATUS_BUDGET_EXHAUSTED);
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
    fail_if(builder, missing, STATUS_INVALID_CONTROL);
    let call = builder
        .ins()
        .call_indirect(slow_signature, callback, &[context]);
    let status = builder.inst_results(call)[0];
    let success = builder.ins().icmp_imm(IntCC::Equal, status, 0);
    let failure = builder.create_block();
    builder.ins().brif(success, poll, &[], failure, &[]);
    builder.switch_to_block(failure);
    builder.ins().return_(&[status]);
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
