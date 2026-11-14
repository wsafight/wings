use std::path::Path;
use std::process::ExitCode;

use wings_vm::{ResourceAccount, Resources, ScalarModule, StackPool};

mod cli;
mod cli_args;
mod cli_io;
mod cli_report;

fn main() -> ExitCode {
    let limits = Resources {
        bytes: 16 << 20,
        processes: 1,
        messages: 0,
        requests: 0,
    };
    let vm = match ResourceAccount::vm_accounted(limits) {
        Ok(vm) => vm,
        Err(_) => {
            cli_report::startup_failed();
            return ExitCode::FAILURE;
        }
    };
    let mut reporter = match cli_report::Reporter::new(&vm) {
        Ok(reporter) => reporter,
        Err(_) => {
            cli_report::startup_failed();
            return ExitCode::FAILURE;
        }
    };
    match start(&vm, &mut reporter) {
        Ok(code) => code,
        Err(error) => {
            reporter.report(format_args!("wings-vm: {error}"));
            ExitCode::FAILURE
        }
    }
}

fn start(vm: &ResourceAccount, reporter: &mut cli_report::Reporter) -> Result<ExitCode, String> {
    let limits = vm.limit();
    let tenant = vm.tenant_accounted(limits)?;
    let account = tenant.process_accounted(limits)?;
    let startup = match cli_args::Startup::new(&account) {
        Ok(startup) => startup,
        Err(error) => {
            reporter.report(format_args!("{error}"));
            return Ok(ExitCode::from(2));
        }
    };
    match startup.command() {
        cli_args::Command::Help => {
            reporter.help().map_err(|error| error.to_string())?;
        }
        cli_args::Command::Run {
            directory,
            max_steps,
            heap_records,
        } => {
            run(
                vm,
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
