use std::path::Path;

use wings_vm::{CancellationToken, ScalarModule, ScalarProcess};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let directory = Path::new(&arguments[1]);
    let module = unsafe { ScalarModule::load_trusted(directory) }.expect("trusted Counter module");
    let increment = module.handler("Inc").expect("resolve Inc");
    let get = module.handler("Get").expect("resolve Get");
    assert_eq!(increment.schema().name, "Inc");
    assert!(module.handler("Missing").is_err());
    let mut first = module.instantiate().expect("first");
    let mut second = module.instantiate().expect("second");
    assert!(std::ptr::eq(first.manifest(), second.manifest()));
    let cancellation = CancellationToken::default();
    assert_eq!(
        first.invoke_handler(&increment, &[3], 4, &cancellation),
        Ok(Some(3))
    );
    assert_eq!(
        second.invoke_handler(&get, &[], 2, &cancellation),
        Ok(Some(0))
    );
    assert!(
        first
            .invoke_handler(&increment, &[], 4, &cancellation)
            .is_err()
    );
    let other_module =
        unsafe { ScalarModule::load_trusted(directory) }.expect("other module identity");
    let other_increment = other_module.handler("Inc").expect("other ID");
    assert!(
        first
            .invoke_handler(&other_increment, &[1], 4, &cancellation)
            .unwrap_err()
            .contains("different module")
    );
    assert_eq!(
        first.invoke_handler(&get, &[], 2, &cancellation),
        Ok(Some(3))
    );
    let mut fresh = module.instantiate().expect("independent initializer");
    assert_eq!(
        fresh.invoke_handler(&get, &[], 2, &cancellation),
        Ok(Some(0))
    );
    drop(module);
    assert_eq!(
        first.invoke_handler(&increment, &[4], 4, &cancellation),
        Ok(Some(7))
    );
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert!(
        second
            .invoke_handler(&increment, &[1], 4, &cancelled)
            .unwrap_err()
            .contains("cancelled")
    );
    assert!(
        second
            .invoke_handler(&get, &[], 2, &cancellation)
            .unwrap_err()
            .contains("terminated")
    );
    assert_eq!(
        first.invoke_handler(&get, &[], 2, &cancellation),
        Ok(Some(7))
    );
    assert_eq!(fresh.invoke("Inc", &[9]), Ok(Some(9)));
    println!(
        "Shared module verified: independent state/initialization/failure, module-bound IDs and lifetime; process header {} bytes",
        std::mem::size_of::<ScalarProcess>()
    );
}
