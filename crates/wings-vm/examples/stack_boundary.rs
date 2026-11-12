use std::path::Path;
use std::sync::atomic::AtomicU32;

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use libloading::Library;
use wings_abi::{STATUS_OK, STATUS_STACK_EXHAUSTED, ScalarContext, ScalarEntry, ScalarOutput};
use wings_artifact::{INITIALIZER_SYMBOL, Manifest, handler_symbol, library_name};

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

unsafe extern "C" fn unexpected_slow_path(context: *mut ScalarContext) -> u32 {
    unsafe { *(*context).data.cast::<bool>() = true };
    STATUS_STACK_EXHAUSTED
}

fn verify(directory: &Path, name: &str, argument: i64, expected: i64) {
    let manifest = Manifest::read(directory).expect("matching ABI");
    let index = manifest
        .handlers
        .iter()
        .position(|handler| handler.name == name)
        .expect("handler");
    let library = unsafe { Library::new(directory.join(library_name())) }.expect("trusted library");
    let initializer = unsafe {
        *library
            .get::<ScalarEntry>(INITIALIZER_SYMBOL.as_bytes())
            .expect("initializer")
    };
    let handler = unsafe {
        *library
            .get::<ScalarEntry>(handler_symbol(index).as_bytes())
            .expect("handler")
    };
    for (entry, reply) in [(initializer, 0), (handler, expected)] {
        for mode in 0..4 {
            let mut coroutine = Coroutine::with_stack(
                DefaultStack::new(256 * 1024).expect("guarded test stack"),
                move |_: &Yielder<(), ()>, ()| {
                    let cancellation = AtomicU32::new(0);
                    let request = AtomicU32::new(0);
                    let mut called = false;
                    let mut context = ScalarContext {
                        effect_call: None,
                        cancellation: &cancellation,
                        remaining_steps: 100_000,
                        yield_request: &request,
                        slice_remaining: if mode == 3 { u64::MAX } else { 0 },
                        slow_path: Some(unexpected_slow_path),
                        data: std::ptr::addr_of_mut!(called).cast(),
                        stack_limit: match mode {
                            0 => 0,
                            1 => usize::MAX,
                            2 => stack_pointer() - 1024,
                            _ => stack_pointer() - 128 * 1024,
                        },
                        roots: std::ptr::null(),
                        heap_call: None,
                        heap_data: std::ptr::null_mut(),
                    };
                    let mut output = ScalarOutput {
                        state: 71,
                        reply: 91,
                    };
                    let status =
                        unsafe { entry(0, [argument].as_ptr(), &mut output, &mut context) };
                    (status, output, context.remaining_steps, called)
                },
            );
            let CoroutineResult::Return((status, output, remaining, called)) = coroutine.resume(())
            else {
                panic!("raw ABI entry must return");
            };
            assert!(!called);
            if mode == 3 {
                assert_eq!(status, STATUS_OK);
                assert_eq!(output.reply, reply);
                assert!(remaining < 100_000);
            } else {
                assert_eq!(status, STATUS_STACK_EXHAUSTED);
                assert_eq!((output.state, output.reply, remaining), (71, 91, 100_000));
            }
        }
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    verify(Path::new(&arguments[1]), "Inc", 3, 3);
    verify(Path::new(&arguments[2]), "Work", 2, 8384);
    println!(
        "Stack boundary verified: zero, wrapped/insufficient space and valid frames, no output or budget changes on rejection"
    );
}
