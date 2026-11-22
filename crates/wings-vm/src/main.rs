use std::path::Path;
use std::process::ExitCode;

use wings_vm::{
    ResourceAccount, Resources, ScalarModule, Scheduler, SchedulerConfig, StackPool, WingsAllocator,
};

#[global_allocator]
static GLOBAL_ALLOCATOR: WingsAllocator = WingsAllocator::new();

mod cli;
mod cli_args;
mod cli_io;
mod cli_record;
mod cli_report;
mod cli_serve;

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
        cli_args::Command::Serve {
            directory,
            workers,
            max_steps,
            heap_records,
            listen,
        } => {
            serve(
                &account,
                Path::new(directory),
                *workers,
                *max_steps,
                *heap_records,
                *listen,
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn serve(
    cli_account: &ResourceAccount,
    directory: &Path,
    workers: usize,
    max_steps: u64,
    heap_records: Option<usize>,
    listen: Option<cli_args::ListenEndpoint>,
) -> Result<(), String> {
    let app_limits = Resources {
        bytes: 256 << 20,
        processes: 1024,
        messages: 4096,
        requests: 256,
    };
    let app_vm = ResourceAccount::vm_accounted(app_limits)?;
    let app_tenant = app_vm.tenant_accounted(app_limits)?;
    let (modules, entry_idx) = if directory.join("bundle.json").exists() {
        unsafe { ScalarModule::load_bundle_accounted(directory, &app_vm) }?
    } else {
        let single = unsafe { ScalarModule::load_accounted(directory, &app_vm) }?;
        (vec![single], 0)
    };
    let scheduler = Scheduler::new(
        &app_tenant,
        SchedulerConfig {
            workers,
            capacity: 256,
            requests: 256,
            ..SchedulerConfig::default()
        },
    )?;
    let handle = scheduler.handle();
    handle.register_modules(&modules)?;
    let entry_module = &modules[entry_idx];
    let actor_limits = Resources {
        bytes: 16 << 20,
        processes: 256,
        messages: 512,
        requests: 128,
    };
    let entry_pid = match heap_records {
        Some(records) => handle.spawn_with_heap(entry_module, actor_limits, 64, records)?,
        None => handle.spawn_with_heap(entry_module, actor_limits, 64, 256)?,
    };
    match listen {
        None => cli_serve::serve_stdio(cli_account, entry_module, entry_pid, &handle, max_steps),
        Some(cli_args::ListenEndpoint::Tcp(addr)) => cli_serve::serve_tcp(
            addr,
            &app_tenant,
            entry_module,
            entry_pid,
            &handle,
            max_steps,
        ),
        Some(cli_args::ListenEndpoint::Http(addr)) => cli_serve::serve_http(
            addr,
            &app_tenant,
            entry_module,
            entry_pid,
            &handle,
            max_steps,
        ),
    }
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
        let arguments = request.arguments.scalars(&schema.parameters)?;
        let reply = process.invoke_value(
            &handler,
            &arguments[..schema.parameters.len()],
            max_steps,
            adapter.cancellation(),
        )?;
        adapter.write_reply_value(writer, reply.as_ref())?;
    }
    Ok(())
}
