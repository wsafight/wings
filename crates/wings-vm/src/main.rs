use std::path::Path;
use std::process::ExitCode;

use wings_vm::{ResourceAccount, Resources, ScalarModule, StackPool};

mod cli;
mod cli_args;
mod cli_io;

fn main() -> ExitCode {
    match start() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("wings-vm: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<ExitCode, String> {
    let limits = Resources {
        bytes: 16 << 20,
        processes: 1,
        messages: 0,
        requests: 0,
    };
    let vm = ResourceAccount::vm_accounted(limits)?;
    let tenant = vm.tenant_accounted(limits)?;
    let account = tenant.process_accounted(limits)?;
    let startup = match cli_args::Startup::new(&account) {
        Ok(startup) => startup,
        Err(error) => {
            eprintln!("{error}");
            return Ok(ExitCode::from(2));
        }
    };
    match startup.command() {
        cli_args::Command::Help => {
            println!(
                "{}\n\nRuns only trusted host-native AOT code. Reads bounded JSON lines from stdin; this is not a sandbox or a full microprocess VM. Each handler defaults to 1000000 execution steps. Both execution modes use fixed 16 MiB VM/tenant/process logical byte limits. Startup accepts at most seven arguments and 16 KiB including terminators, excluding the executable name.",
                cli_args::USAGE
            );
        }
        cli_args::Command::Run {
            directory,
            max_steps,
            heap_records,
        } => {
            run(
                &vm,
                &tenant,
                &account,
                Path::new(directory),
                *max_steps,
                *heap_records,
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run(
    vm: &ResourceAccount,
    tenant: &ResourceAccount,
    account: &ResourceAccount,
    directory: &Path,
    max_steps: u64,
    heap_records: Option<usize>,
) -> Result<(), String> {
    let pool = StackPool::new_accounted(tenant, 1, 0)?.for_process(account)?;
    let module = unsafe { ScalarModule::load_accounted(directory, vm) }?;
    let mut adapter = cli::Adapter::new(account)?;
    let mut stdio = cli_io::Stdio::new(account)?;
    let mut process = match heap_records {
        None => module.instantiate_in_pool(&pool)?,
        Some(capacity) => module.instantiate_with_heap(&pool, account, capacity)?,
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
