use std::path::Path;
use std::sync::atomic::AtomicU32;

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use libloading::Library;
use wings_abi::{STATUS_OK, STATUS_STACK_EXHAUSTED, ScalarContext, ScalarEntry, ScalarOutput};
use wings_artifact::{Manifest, handler_symbol, library_name};

fn stack_pointer() -> usize {
    let address: usize;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("mov {}, sp", out(reg) address, options(nomem, nostack, preserves_flags));
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::asm!("mov {}, rsp", out(reg) address, options(nomem, nostack, preserves_flags));
    }
    address
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let directory = Path::new(&arguments[1]);
    let manifest = Manifest::read(directory).unwrap();
    let index = manifest
        .handlers
        .iter()
        .position(|handler| handler.name == "work")
        .unwrap();
    let library = unsafe { Library::new(directory.join(library_name())) }.unwrap();
    let handler = unsafe {
        *library
            .get::<ScalarEntry>(handler_symbol(index).as_bytes())
            .unwrap()
    };
    for available in [8 * 1024, 128 * 1024] {
        let mut coroutine = Coroutine::with_stack(
            DefaultStack::new(256 * 1024).unwrap(),
            move |_: &Yielder<(), ()>, ()| {
                let flag = AtomicU32::new(0);
                let mut context = ScalarContext {
                    cancellation: &flag,
                    remaining_steps: 100_000,
                    yield_request: &flag,
                    slice_remaining: u64::MAX,
                    slow_path: None,
                    data: std::ptr::null_mut(),
                    stack_limit: stack_pointer() - available,
                    roots: std::ptr::null(),
                    heap_call: None,
                    heap_data: std::ptr::null_mut(),
                    effect_call: None,
                };
                let mut output = ScalarOutput {
                    state: 71,
                    reply: 91,
                };
                let status = unsafe { handler(0, [2].as_ptr(), &mut output, &mut context) };
                (
                    status,
                    output,
                    context.remaining_steps,
                    context.roots.is_null(),
                )
            },
        );
        let CoroutineResult::Return((status, output, remaining, unlinked)) = coroutine.resume(())
        else {
            panic!("managed stack must return normally");
        };
        assert!(unlinked && remaining < 100_000);
        if available == 128 * 1024 {
            assert_eq!(status, STATUS_OK);
            assert_eq!((output.state, output.reply), (99, 17_363));
        } else {
            assert_eq!(status, STATUS_STACK_EXHAUSTED);
            assert_eq!((output.state, output.reply), (71, 91));
        }
    }
    println!(
        "Managed stack verified: nested frame admission, normal failure propagation and output isolation"
    );
}
