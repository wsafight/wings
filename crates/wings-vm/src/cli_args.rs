use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

use wings_abi::DEFAULT_MAX_STEPS;
use wings_vm::{Reservation, ResourceAccount, ResourceLevel, Resources};

pub const USAGE: &str = "Usage: wings-vm run --trusted <artifact-directory> [--max-steps <u64>] [--heap-records <1..=1024>]\n       wings-vm serve --trusted <bundle-or-artifact> [--workers <1..=16>] [--max-steps <u64>] [--heap-records <1..=1024>]";
const MAX_ARGUMENTS: usize = 7;
const MAX_BYTES: usize = 16 << 10;
const CONTROL_BYTES: usize = 4 << 10;
const STARTUP_BYTES: u64 = (MAX_BYTES + CONTROL_BYTES) as u64;

pub struct Startup {
    command: Command,
    _charge: Reservation,
}

pub enum Command {
    Help,
    Run {
        directory: OsString,
        max_steps: u64,
        heap_records: Option<usize>,
    },
    Serve {
        directory: OsString,
        workers: usize,
        max_steps: u64,
        heap_records: Option<usize>,
    },
}

struct Arguments<'argument> {
    values: [&'argument [u8]; MAX_ARGUMENTS],
    length: usize,
}

impl Startup {
    pub fn new(account: &ResourceAccount) -> Result<Self, String> {
        Self::read(account, native)
    }

    fn read<'argument>(
        account: &ResourceAccount,
        source: impl FnOnce() -> Result<Arguments<'argument>, String>,
    ) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process {
            return Err("CLI startup requires a process resource account".into());
        }
        let charge = account.reserve(Resources {
            bytes: STARTUP_BYTES,
            ..Resources::default()
        })?;
        let arguments = source()?;
        let values = &arguments.values[..arguments.length];
        let command = if matches!(values, [b"--help"] | [b"-h"]) {
            Command::Help
        } else if matches!(values.len(), 3 | 5 | 7)
            && matches!(values[0], b"run" | b"serve")
            && values[1] == b"--trusted"
        {
            let is_serve = values[0] == b"serve";
            let mut steps = None;
            let mut heap_records = None;
            let mut workers = None;
            for option in values[3..].chunks_exact(2) {
                match option[0] {
                    b"--max-steps" if steps.is_none() => {
                        steps = Some(
                            std::str::from_utf8(option[1])
                                .ok()
                                .and_then(|value| value.parse::<u64>().ok())
                                .ok_or("--max-steps must be an unsigned 64-bit integer")?,
                        );
                    }
                    b"--heap-records" if heap_records.is_none() => {
                        heap_records = Some(
                            std::str::from_utf8(option[1])
                                .ok()
                                .and_then(|value| value.parse::<usize>().ok())
                                .filter(|value| (1..=1024).contains(value))
                                .ok_or("--heap-records must be within 1..=1024")?,
                        );
                    }
                    b"--workers" if is_serve && workers.is_none() => {
                        workers = Some(
                            std::str::from_utf8(option[1])
                                .ok()
                                .and_then(|value| value.parse::<usize>().ok())
                                .filter(|value| (1..=16).contains(value))
                                .ok_or("--workers must be within 1..=16")?,
                        );
                    }
                    _ => return Err("unknown or duplicate execution option".into()),
                }
            }
            if is_serve {
                Command::Serve {
                    directory: std::ffi::OsStr::from_bytes(values[2]).to_os_string(),
                    workers: workers.unwrap_or(2),
                    max_steps: steps.unwrap_or(DEFAULT_MAX_STEPS),
                    heap_records,
                }
            } else {
                Command::Run {
                    directory: std::ffi::OsStr::from_bytes(values[2]).to_os_string(),
                    max_steps: steps.unwrap_or(DEFAULT_MAX_STEPS),
                    heap_records,
                }
            }
        } else {
            return Err(USAGE.into());
        };
        Ok(Self {
            command,
            _charge: charge,
        })
    }

    pub fn command(&self) -> &Command {
        &self.command
    }
}

fn native() -> Result<Arguments<'static>, String> {
    let (count, pointers) = platform::raw();
    unsafe { Arguments::from_raw(count, pointers) }
}

impl<'argument> Arguments<'argument> {
    unsafe fn from_raw(
        count: libc::c_int,
        pointers: *const *const libc::c_char,
    ) -> Result<Self, String> {
        if count < 1 || count as usize > MAX_ARGUMENTS + 1 {
            return Err(USAGE.into());
        }
        if pointers.is_null() {
            return Err("CLI argument pointers unavailable".into());
        }
        let mut arguments = Self {
            values: [&[]; MAX_ARGUMENTS],
            length: count as usize - 1,
        };
        let mut remaining = MAX_BYTES;
        for index in 0..arguments.length {
            let pointer = unsafe { pointers.add(index + 1).read() };
            if pointer.is_null() {
                return Err("CLI argument pointer unavailable".into());
            }
            let mut length = 0;
            loop {
                if length == remaining {
                    return Err("CLI arguments exceed 16 KiB".into());
                }
                if unsafe { pointer.add(length).read() } == 0 {
                    break;
                }
                length += 1;
            }
            arguments.values[index] = unsafe { std::slice::from_raw_parts(pointer.cast(), length) };
            remaining -= length + 1;
        }
        Ok(arguments)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    pub fn raw() -> (libc::c_int, *const *const libc::c_char) {
        unsafe { (libc::_NSGetArgc().read(), libc::_NSGetArgv().read().cast()) }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod platform {
    use std::ptr;
    use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

    static COUNT: AtomicI32 = AtomicI32::new(0);
    static POINTERS: AtomicPtr<*const libc::c_char> = AtomicPtr::new(ptr::null_mut());

    #[used]
    #[unsafe(link_section = ".init_array.00100")]
    static ARGUMENTS: extern "C" fn(
        libc::c_int,
        *const *const libc::c_char,
        *const *const libc::c_char,
    ) = capture;

    extern "C" fn capture(
        count: libc::c_int,
        pointers: *const *const libc::c_char,
        _environment: *const *const libc::c_char,
    ) {
        POINTERS.store(pointers.cast_mut(), Ordering::Relaxed);
        COUNT.store(count, Ordering::Relaxed);
    }

    pub fn raw() -> (libc::c_int, *const *const libc::c_char) {
        (
            COUNT.load(Ordering::Relaxed),
            POINTERS.load(Ordering::Relaxed).cast_const(),
        )
    }
}

#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
compile_error!("bounded CLI startup arguments require macOS or Linux glibc");

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::ffi::CString;

    fn bytes(amount: u64) -> Resources {
        Resources {
            bytes: amount,
            ..Resources::default()
        }
    }

    fn accounts() -> (ResourceAccount, ResourceAccount, ResourceAccount) {
        let vm = ResourceAccount::vm_accounted(bytes(8 << 20)).unwrap();
        let tenant = vm.tenant_accounted(bytes(4 << 20)).unwrap();
        let account = tenant.process_accounted(bytes(2 << 20)).unwrap();
        (vm, tenant, account)
    }

    fn parse(account: &ResourceAccount, values: &[&[u8]]) -> Result<Startup, String> {
        let strings = std::iter::once(CString::new("program").unwrap())
            .chain(values.iter().map(|value| CString::new(*value).unwrap()))
            .collect::<Vec<_>>();
        let pointers = strings
            .iter()
            .map(|value| value.as_ptr())
            .collect::<Vec<_>>();
        Startup::read(account, || unsafe {
            Arguments::from_raw(pointers.len() as libc::c_int, pointers.as_ptr())
        })
    }

    #[test]
    fn fixed_argument_views_and_owned_directory_fit_admission() {
        assert!(
            std::mem::size_of::<Startup>() + std::mem::size_of::<Arguments<'_>>() + 512
                <= CONTROL_BYTES
        );
        assert_eq!(STARTUP_BYTES, 20 << 10);
    }

    #[test]
    fn admission_checks_each_level_before_reading_arguments_and_cleans_up() {
        let (vm, tenant, account) = accounts();
        let baseline = [account.used(), tenant.used(), vm.used()];
        for invalid in [&vm, &tenant] {
            assert!(Startup::read(invalid, || panic!("read arguments")).is_err());
        }
        for owner in [&account, &tenant, &vm] {
            for remaining in [0, STARTUP_BYTES - 1, STARTUP_BYTES] {
                let fill = owner
                    .reserve(bytes(owner.limit().bytes - owner.used().bytes - remaining))
                    .unwrap();
                let before = [account.used(), tenant.used(), vm.used()];
                let read = Cell::new(false);
                let result = Startup::read(&account, || {
                    read.set(true);
                    Ok(Arguments {
                        values: [b"--help", &[], &[], &[], &[], &[], &[]],
                        length: 1,
                    })
                });
                if remaining < STARTUP_BYTES {
                    let error = result.err().unwrap();
                    assert!(error.contains(&format!(
                        "resource_exhausted at {:?} account",
                        owner.level()
                    )));
                    assert!(!read.get());
                } else {
                    let startup = result.unwrap();
                    assert!(read.get());
                    assert!(matches!(startup.command(), Command::Help));
                    assert_eq!(startup._charge.amount(), bytes(STARTUP_BYTES));
                    for (used, previous) in [account.used(), tenant.used(), vm.used()]
                        .into_iter()
                        .zip(before)
                    {
                        assert_eq!(used, bytes(previous.bytes + STARTUP_BYTES));
                    }
                    drop(startup);
                }
                assert_eq!([account.used(), tenant.used(), vm.used()], before);
                drop(fill);
                assert_eq!([account.used(), tenant.used(), vm.used()], baseline);
            }
        }
    }

    #[test]
    fn original_commands_options_and_numeric_boundaries_are_preserved() {
        let (_, _, account) = accounts();
        for help in [b"--help".as_slice(), b"-h"] {
            assert!(matches!(
                parse(&account, &[help]).unwrap().command(),
                Command::Help
            ));
        }
        for values in [
            vec![b"run".as_slice(), b"--trusted", b"artifact"],
            vec![
                b"run",
                b"--trusted",
                b"artifact",
                b"--max-steps",
                b"18446744073709551615",
                b"--heap-records",
                b"1024",
            ],
            vec![
                b"run",
                b"--trusted",
                b"artifact",
                b"--heap-records",
                b"1",
                b"--max-steps",
                b"0",
            ],
        ] {
            let startup = parse(&account, &values).unwrap();
            let Command::Run {
                directory,
                max_steps,
                heap_records,
            } = startup.command()
            else {
                panic!("expected run")
            };
            assert_eq!(directory, "artifact");
            match values.len() {
                3 => {
                    assert_eq!(*max_steps, DEFAULT_MAX_STEPS);
                    assert_eq!(*heap_records, None);
                }
                _ if values[3] == b"--max-steps" => {
                    assert_eq!(*max_steps, u64::MAX);
                    assert_eq!(*heap_records, Some(1024));
                }
                _ => {
                    assert_eq!(*max_steps, 0);
                    assert_eq!(*heap_records, Some(1));
                }
            }
        }
        for values in [
            vec![],
            vec![b"run".as_slice()],
            vec![b"--help", b"extra"],
            vec![b"run", b"--untrusted", b"artifact"],
            vec![b"run", b"--trusted", b"artifact", b"--max-steps"],
            vec![b"run", b"--trusted", b"artifact", b"--unknown", b"1"],
            vec![
                b"run",
                b"--trusted",
                b"artifact",
                b"--max-steps",
                b"1",
                b"--max-steps",
                b"2",
            ],
            vec![
                b"run",
                b"--trusted",
                b"artifact",
                b"--heap-records",
                b"1",
                b"--heap-records",
                b"2",
            ],
        ] {
            let before = account.used();
            assert!(parse(&account, &values).is_err());
            assert_eq!(account.used(), before);
        }
        for (option, values) in [
            (
                b"--max-steps".as_slice(),
                vec![
                    b"-1".as_slice(),
                    b"18446744073709551616",
                    b"1.0",
                    b"1e0",
                    b"",
                    b"\xff",
                ],
            ),
            (
                b"--heap-records",
                vec![b"0".as_slice(), b"1025", b"-1", b"", b"\xff"],
            ),
        ] {
            for value in values {
                let before = account.used();
                assert!(
                    parse(
                        &account,
                        &[b"run", b"--trusted", b"artifact", option, value]
                    )
                    .is_err()
                );
                assert_eq!(account.used(), before);
            }
        }
    }

    #[test]
    fn raw_count_null_pointer_and_exact_nul_inclusive_byte_limits_are_checked() {
        for count in [-1, 0, 9, libc::c_int::MAX] {
            assert!(unsafe { Arguments::from_raw(count, std::ptr::null()) }.is_err());
        }
        assert!(unsafe { Arguments::from_raw(1, std::ptr::null()) }.is_err());
        assert!(
            unsafe { Arguments::from_raw(2, [std::ptr::null(), std::ptr::null()].as_ptr()) }
                .is_err()
        );
        let (_, _, account) = accounts();
        let exact = vec![b'x'; MAX_BYTES - b"run\0--trusted\0".len() - 1];
        let startup = parse(&account, &[b"run", b"--trusted", &exact]).unwrap();
        let Command::Run { directory, .. } = startup.command() else {
            panic!("expected run")
        };
        assert_eq!(directory.as_bytes(), exact);
        drop(startup);
        let before = account.used();
        let too_large = vec![b'x'; exact.len() + 1];
        assert_eq!(
            parse(&account, &[b"run", b"--trusted", &too_large])
                .err()
                .unwrap(),
            "CLI arguments exceed 16 KiB"
        );
        assert_eq!(account.used(), before);
        let first = CString::new("--help").unwrap();
        let pointers = [std::ptr::null(), first.as_ptr()];
        let view = unsafe { Arguments::from_raw(2, pointers.as_ptr()) }.unwrap();
        assert_eq!(view.values[0], b"--help");
    }

    #[test]
    fn non_utf8_directory_is_owned_and_errors_do_not_echo_arguments() {
        let (_, _, account) = accounts();
        let baseline = account.used();
        let startup = parse(&account, &[b"run", b"--trusted", b"artifact-\xff"]).unwrap();
        let Command::Run { directory, .. } = startup.command() else {
            panic!("expected run")
        };
        assert_eq!(directory.as_bytes(), b"artifact-\xff");
        assert_eq!(account.used(), bytes(baseline.bytes + STARTUP_BYTES));
        drop(startup);
        assert_eq!(account.used(), baseline);
        assert_eq!(
            parse(&account, &[b"run", b"--trusted", b"path", b"\xff", b"1"])
                .err()
                .unwrap(),
            "unknown or duplicate execution option"
        );
        assert!(Startup::read(&account, || Err("source unavailable".into())).is_err());
        assert_eq!(account.used(), baseline);
    }

    #[test]
    fn native_snapshot_is_available_after_standard_rust_startup() {
        let arguments = native().unwrap();
        assert!(arguments.length <= MAX_ARGUMENTS);
    }
}
