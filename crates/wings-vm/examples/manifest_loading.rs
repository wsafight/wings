use std::path::Path;

use wings_vm::{ResourceAccount, Resources, ScalarModule, StackPool};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() >= 3 && arguments[0] == "--trusted");
    assert_eq!((arguments.len() - 1) % 2, 0);
    let limits = Resources {
        bytes: 32 << 20,
        processes: 4,
        messages: 4,
        requests: 4,
    };
    let vm = ResourceAccount::vm_accounted(limits).unwrap();
    let baseline = vm.used();
    let mut accepted = 0;
    let mut rejected = 0;
    for pair in arguments[1..].chunks_exact(2) {
        let path = Path::new(&pair[0]);
        let expected = pair[1].as_str();
        let fill = vm
            .reserve(Resources {
                bytes: vm.limit().bytes - baseline.bytes,
                ..Resources::default()
            })
            .unwrap();
        let full = vm.used();
        let error = unsafe { ScalarModule::load_accounted(path, &vm) }
            .err()
            .unwrap();
        assert!(error.contains("resource_exhausted at Vm account"));
        assert_eq!(vm.used(), full);
        drop(fill);
        for _ in 0..2 {
            let result = unsafe { ScalarModule::load_accounted(path, &vm) };
            if expected == "ok" {
                let module = result.unwrap();
                let tenant = vm.tenant_accounted(limits).unwrap();
                let account = tenant.process_accounted(limits).unwrap();
                let pool = StackPool::new_accounted(&tenant, 1, 0)
                    .unwrap()
                    .for_process(&account)
                    .unwrap();
                let mut process = module.instantiate_in_pool(&pool).unwrap();
                assert_eq!(process.invoke("Inc", &[7]).unwrap(), Some(7));
                assert_eq!(process.invoke("Get", &[]).unwrap(), Some(7));
                drop((process, pool, module, account, tenant));
                accepted += 1;
            } else {
                let error = result.err().unwrap();
                assert!(error.contains(expected), "{error}");
                rejected += 1;
            }
            assert_eq!(vm.used(), baseline);
            let tools = unsafe { ScalarModule::load_trusted(path) };
            if expected == "ok" {
                drop(tools.unwrap());
            } else {
                let error = tools.err().unwrap();
                assert!(error.contains(expected), "{error}");
            }
            assert_eq!(vm.used(), baseline);
        }
    }
    println!(
        "{{\"status\":\"ok\",\"accepted\":{accepted},\"rejected\":{rejected},\"bootstrap_bytes\":{},\"remaining_bytes\":{},\"resources_released\":true}}",
        baseline.bytes,
        vm.used().bytes
    );
}
