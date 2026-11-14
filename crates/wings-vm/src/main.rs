use std::path::Path;
use std::process::ExitCode;

use wings_abi::DEFAULT_MAX_STEPS;
use wings_vm::{ResourceAccount, Resources, ScalarModule, StackPool};

mod cli;
mod cli_io;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!(
            "Usage: wings-vm run --trusted <artifact-directory> [--max-steps <u64>] [--heap-records <1..=1024>]\n\nRuns only trusted host-native AOT code. Reads bounded JSON lines from stdin; this is not a sandbox or a full microprocess VM. Each handler defaults to 1000000 execution steps. Both execution modes use fixed 16 MiB VM/tenant/process logical byte limits."
        );
        return ExitCode::SUCCESS;
    }
    if !matches!(arguments.len(), 3 | 5 | 7) || arguments[0] != "run" || arguments[1] != "--trusted"
    {
        eprintln!(
            "Usage: wings-vm run --trusted <artifact-directory> [--max-steps <u64>] [--heap-records <1..=1024>]"
        );
        return ExitCode::from(2);
    }
    let mut steps = None;
    let mut heap_records = None;
    for option in arguments[3..].chunks_exact(2) {
        match option[0].as_str() {
            "--max-steps" if steps.is_none() => {
                let Ok(value) = option[1].parse::<u64>() else {
                    eprintln!("--max-steps must be an unsigned 64-bit integer");
                    return ExitCode::from(2);
                };
                steps = Some(value);
            }
            "--heap-records" if heap_records.is_none() => {
                let Ok(value @ 1..=1024) = option[1].parse::<usize>() else {
                    eprintln!("--heap-records must be within 1..=1024");
                    return ExitCode::from(2);
                };
                heap_records = Some(value);
            }
            _ => {
                eprintln!("unknown or duplicate execution option");
                return ExitCode::from(2);
            }
        }
    }
    match run(
        Path::new(&arguments[2]),
        steps.unwrap_or(DEFAULT_MAX_STEPS),
        heap_records,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wings-vm: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(directory: &Path, max_steps: u64, heap_records: Option<usize>) -> Result<(), String> {
    let limits = Resources {
        bytes: 16 << 20,
        processes: 1,
        messages: 0,
        requests: 0,
    };
    let vm = ResourceAccount::vm_accounted(limits)?;
    let tenant = vm.tenant_accounted(limits)?;
    let account = tenant.process_accounted(limits)?;
    let pool = StackPool::new_accounted(&tenant, 1, 0)?.for_process(&account)?;
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let mut adapter = cli::Adapter::new(&account)?;
    let mut stdio = cli_io::Stdio::new(&account)?;
    let mut process = match heap_records {
        None => module.instantiate_in_pool(&pool)?,
        Some(capacity) => module.instantiate_with_heap(&pool, &account, capacity)?,
    };
    let (reader, writer) = stdio.streams();
    while let Some(request) = adapter.read_request(reader)? {
        let handler = process.module().handler(request.message.as_str())?;
        let schema = handler.schema();
        let reply_type = schema.reply;
        let arguments = request.arguments.scalars(&schema.parameters)?;
        let reply = process.invoke_handler(
            &handler,
            &arguments[..schema.parameters.len()],
            max_steps,
            adapter.cancellation(),
        )?;
        adapter.write_reply(writer, reply_type, reply)?;
    }
    Ok(())
}
