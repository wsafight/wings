use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};
use wings_abi::{DEFAULT_MAX_STEPS, ValueType};
use wings_vm::{
    CancellationToken, ResourceAccount, Resources, ScalarModule, ScalarProcess, StackPool,
};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!(
            "Usage: wings-vm run --trusted <artifact-directory> [--max-steps <u64>] [--heap-records <1..=1024>]\n\nRuns only trusted host-native AOT code. Reads bounded JSON lines from stdin; this is not a sandbox or a full microprocess VM. Each handler defaults to 1000000 execution steps. Heap mode uses fixed 16 MiB VM/tenant/process byte limits."
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
    let mut process = match heap_records {
        None => unsafe { ScalarProcess::load_trusted(directory) }?,
        Some(capacity) => {
            let limits = Resources {
                bytes: 16 << 20,
                processes: 1,
                messages: 0,
                requests: 0,
            };
            let vm = ResourceAccount::vm(limits);
            let tenant = vm.tenant_accounted(limits)?;
            let account = tenant.process_accounted(limits)?;
            let pool = StackPool::new_accounted(&tenant, 1, 0)?.for_process(&account)?;
            unsafe { ScalarModule::load_accounted(directory, &vm) }?
                .instantiate_with_heap(&pool, &account, capacity)?
        }
    };
    let cancellation = CancellationToken::default();
    let mut reader = io::stdin().lock();
    let mut writer = io::stdout().lock();
    while let Some(line) = read_line(&mut reader)? {
        let input: Value = serde_json::from_slice(&line).map_err(|error| error.to_string())?;
        let object = input.as_object().ok_or("expected a JSON object")?;
        if object.len() != 2 || !object.contains_key("message") || !object.contains_key("args") {
            return Err("expected exactly message and args fields".into());
        }
        let name = input["message"].as_str().ok_or("expected a message name")?;
        let values = input["args"].as_array().ok_or("expected an args array")?;
        let handler = process.module().handler(name)?;
        let schema = handler.schema();
        if values.len() != schema.parameters.len() {
            return Err("handler argument count mismatch".into());
        }
        let reply_type = schema.reply;
        let mut arguments = [0i64; 64];
        for ((slot, value), (_, ty)) in arguments.iter_mut().zip(values).zip(&schema.parameters) {
            *slot = match ty {
                ValueType::Int => value
                    .as_i64()
                    .ok_or_else(|| "expected an i64 argument".to_owned()),
                ValueType::Bool => value
                    .as_bool()
                    .map(i64::from)
                    .ok_or_else(|| "expected a Bool argument".to_owned()),
                ValueType::Unit if value.is_null() => Ok(0),
                ValueType::Unit => Err("expected null for Unit".into()),
                ValueType::Ref | ValueType::ListRef | ValueType::Pid | ValueType::RequestId => {
                    Err("record references are not JSON arguments".into())
                }
            }?;
        }
        let reply = process.invoke_handler(
            &handler,
            &arguments[..values.len()],
            max_steps,
            &cancellation,
        )?;
        let output = match reply {
            Some(value) => json!({ "reply": match reply_type {
                Some(ValueType::Int) => json!(value),
                Some(ValueType::Bool) => json!(value != 0),
                _ => Value::Null,
            } }),
            None => json!({ "accepted": true }),
        };
        serde_json::to_writer(&mut writer, &output).map_err(|error| error.to_string())?;
        writeln!(writer)
            .and_then(|_| writer.flush())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn read_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().map_err(|error| error.to_string())?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let length = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if line.len() + length > 64 * 1024 {
            return Err("input line exceeds 64 KiB".into());
        }
        let finished = buffer[length - 1] == b'\n';
        line.extend_from_slice(&buffer[..length]);
        reader.consume(length);
        if finished {
            return Ok(Some(line));
        }
    }
}
