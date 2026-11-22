use std::io::{Read, Write};
use std::path::Path;

use serde_json::{Value, json};
use wings_vm::{ResourceAccount, ScalarModule};

#[path = "support/failure_counter.rs"]
mod failure_counter;
use failure_counter::{MAX_INPUT, Workload, decode};

fn execute(directory: &Path, workload: &Workload) -> Result<(i32, Value), String> {
    let vm = ResourceAccount::vm_accounted(failure_counter::limits())?;
    let bootstrap = vm.used();
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let tenant = vm.tenant_accounted(failure_counter::limits())?;
    let mut supervisor = failure_counter::supervisor(&tenant, workload.actors)?;
    let completed =
        failure_counter::epoch(&module, &tenant, &mut supervisor, workload, None, None)?;
    completed.reject_stale(&module, &supervisor)?;
    let mut output = completed.result;
    drop((supervisor, module, tenant));
    failure_counter::remaining(completed.deadline)?;
    if vm.used() != bootstrap {
        return Err("concurrent failure resources retained".into());
    }
    output["resources_released"] = json!(true);
    output["bootstrap_bytes"] = json!(bootstrap.bytes);
    output["remaining_bytes"] = json!(vm.used().bytes);
    Ok((i32::from(output["status"] != "ok"), output))
}

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "--trusted" {
        std::process::exit(2);
    }
    let mut input = [0; MAX_INPUT + 1];
    let mut count = 0;
    let mut source = std::io::stdin().lock();
    while count < input.len() {
        match source.read(&mut input[count..]) {
            Ok(0) => break,
            Ok(size) => count += size,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => std::process::exit(2),
        }
    }
    let (code, output) = match decode(&input[..count]) {
        Ok(workload) => match execute(Path::new(&arguments[1]), &workload) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("{error}");
                (3, json!({"version": 9, "status": "oracle_error"}))
            }
        },
        Err(_) => (2, json!({"version": 9, "status": "invalid"})),
    };
    let _ = writeln!(std::io::stdout().lock(), "{output}");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_rejects_routes_sequences_operations_limits_and_unsafe_interleavings() {
        let words = [9u64, 256, 0, 1, 1, 0, 1, 1, 7, 2];
        let input: Vec<_> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(decode(&input).unwrap().streams[0][0].operation, 2);
        for (index, value) in [
            (0, 3),
            (1, 257),
            (3, 0),
            (3, 101),
            (4, 129),
            (6, 2),
            (7, 2),
            (9, 0),
            (9, 3),
            (2, i64::MAX as u64),
        ] {
            let mut changed = input.clone();
            changed[index * 8..index * 8 + 8].copy_from_slice(&value.to_be_bytes());
            assert!(decode(&changed).is_err());
        }
        for length in [0, 1, 47, 48, 79] {
            assert!(decode(&input[..length]).is_err());
        }
    }
}
