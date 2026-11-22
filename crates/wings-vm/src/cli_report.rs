use std::fmt;
use std::fs::File;
use std::io::{self, Write};

use wings_vm::{Reservation, ResourceAccount, ResourceLevel, Resources};

use crate::cli_args::USAGE;
use crate::cli_io::duplicate;

const REPORT_BYTES: u64 = 8 << 10;
const MAX_DIAGNOSTIC: usize = 4096;
const TRUNCATED: &[u8] = b"... [truncated]";
const HELP: &[u8] = b"\n\nRuns only trusted host-native AOT code. Reads bounded JSON lines from stdin; this is not a sandbox or a full microprocess VM. Each handler defaults to 1000000 execution steps. Both execution modes use fixed 16 MiB VM/tenant/process logical byte limits. Startup accepts at most seven arguments and 16 KiB including terminators, excluding the executable name.\n";

pub struct Reporter {
    buffer: Diagnostic,
    output: File,
    _charge: Reservation,
}

struct Diagnostic {
    bytes: [u8; MAX_DIAGNOSTIC],
    length: usize,
    truncated: bool,
}

impl Reporter {
    pub fn new(vm: &ResourceAccount) -> Result<Self, String> {
        Self::open(vm, || duplicate(libc::STDERR_FILENO))
    }

    fn open(
        vm: &ResourceAccount,
        output: impl FnOnce() -> io::Result<File>,
    ) -> Result<Self, String> {
        if vm.level() != ResourceLevel::Vm {
            return Err("CLI reporting requires a VM resource account".into());
        }
        let charge = vm.reserve(Resources {
            bytes: REPORT_BYTES,
            ..Resources::default()
        })?;
        let output = output().map_err(|error| format!("CLI stderr: {error}"))?;
        Ok(Self {
            buffer: Diagnostic::new(),
            output,
            _charge: charge,
        })
    }

    pub fn report(&mut self, arguments: fmt::Arguments<'_>) {
        let bytes = self.buffer.format(arguments);
        let _ = self.output.write_all(bytes);
    }

    pub fn help(&self) -> io::Result<()> {
        let mut output = duplicate(libc::STDOUT_FILENO)?;
        write_help(&mut output)
    }
}

fn write_help(output: &mut impl Write) -> io::Result<()> {
    output.write_all(USAGE.as_bytes())?;
    output.write_all(HELP)
}

pub fn startup_failed() {
    let bytes = b"wings-vm: startup resource or diagnostic initialization failed\n";
    let mut written = 0;
    while written < bytes.len() {
        let count = unsafe {
            libc::write(
                libc::STDERR_FILENO,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };
        if count > 0 {
            written += count as usize;
        } else if count == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
}

impl Diagnostic {
    fn new() -> Self {
        Self {
            bytes: [0; MAX_DIAGNOSTIC],
            length: 0,
            truncated: false,
        }
    }

    fn format(&mut self, arguments: fmt::Arguments<'_>) -> &[u8] {
        self.length = 0;
        self.truncated = false;
        let _ = fmt::write(self, arguments);
        if self.truncated {
            self.bytes[self.length..self.length + TRUNCATED.len()].copy_from_slice(TRUNCATED);
            self.length += TRUNCATED.len();
        }
        self.bytes[self.length] = b'\n';
        self.length += 1;
        &self.bytes[..self.length]
    }
}

impl fmt::Write for Diagnostic {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let available = MAX_DIAGNOSTIC - TRUNCATED.len() - 1 - self.length;
        let mut length = value.len().min(available);
        while !value.is_char_boundary(length) {
            length -= 1;
        }
        self.bytes[self.length..self.length + length].copy_from_slice(&value.as_bytes()[..length]);
        self.length += length;
        if length != value.len() {
            self.truncated = true;
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Read;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;

    fn bytes(amount: u64) -> Resources {
        Resources {
            bytes: amount,
            ..Resources::default()
        }
    }

    fn null() -> io::Result<File> {
        File::options().read(true).write(true).open("/dev/null")
    }

    #[test]
    fn fixed_report_storage_fits_admission() {
        assert!(std::mem::size_of::<Reporter>() + 512 <= REPORT_BYTES as usize);
    }

    #[test]
    fn root_admission_precedes_open_and_exact_budget_cleans_up() {
        let vm = ResourceAccount::vm_accounted(bytes(1 << 20)).unwrap();
        let baseline = vm.used();
        let tenant = vm.tenant_accounted(bytes(1 << 19)).unwrap();
        let account = tenant.process_accounted(bytes(1 << 18)).unwrap();
        for invalid in [&tenant, &account] {
            assert!(Reporter::open(invalid, || panic!("opened stderr")).is_err());
        }
        drop((account, tenant));
        assert_eq!(vm.used(), baseline);
        for remaining in [0, REPORT_BYTES - 1, REPORT_BYTES] {
            let fill = vm
                .reserve(bytes(vm.limit().bytes - vm.used().bytes - remaining))
                .unwrap();
            let before = vm.used();
            let opened = Cell::new(false);
            let result = Reporter::open(&vm, || {
                opened.set(true);
                null()
            });
            if remaining < REPORT_BYTES {
                assert!(
                    result
                        .err()
                        .unwrap()
                        .contains("resource_exhausted at Vm account")
                );
                assert!(!opened.get());
            } else {
                let reporter = result.unwrap();
                assert!(opened.get());
                assert_eq!(reporter._charge.amount(), bytes(REPORT_BYTES));
                assert_eq!(vm.used().bytes, vm.limit().bytes);
                drop(reporter);
            }
            assert_eq!(vm.used(), before);
            drop(fill);
            assert_eq!(vm.used(), baseline);
        }
        assert!(Reporter::open(&vm, || Err(io::Error::from_raw_os_error(libc::EMFILE))).is_err());
        assert_eq!(vm.used(), baseline);
    }

    #[test]
    fn diagnostics_reuse_fixed_storage_and_truncate_at_utf8_boundaries() {
        let mut buffer = Diagnostic::new();
        let storage = buffer.bytes.as_ptr();
        let body_limit = MAX_DIAGNOSTIC - TRUNCATED.len() - 1;
        for value in [
            "plain".to_owned(),
            "x".repeat(body_limit),
            "é🦀".repeat(MAX_DIAGNOSTIC),
        ] {
            let result = buffer.format(format_args!("{value}"));
            assert!(result.len() <= MAX_DIAGNOSTIC && result.ends_with(b"\n"));
            assert!(std::str::from_utf8(result).is_ok());
            if value.len() <= body_limit {
                assert_eq!(result, format!("{value}\n").as_bytes());
            } else {
                assert!(result.ends_with(b"... [truncated]\n"));
            }
            assert_eq!(buffer.bytes.as_ptr(), storage);
        }
        let prefix = "x".repeat(body_limit - 1);
        assert!(
            buffer
                .format(format_args!("{prefix}é"))
                .ends_with(b"... [truncated]\n")
        );
        assert_eq!(buffer.format(format_args!("next {}", 7)), b"next 7\n");
    }

    #[test]
    fn reporter_writes_without_flush_closes_copy_and_ignores_broken_diagnostic_output() {
        let vm = ResourceAccount::vm_accounted(bytes(1 << 20)).unwrap();
        let baseline = vm.used();
        let (mut peer, original) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let output = duplicate(original.as_raw_fd()).unwrap();
        let mut reporter = Reporter::open(&vm, || Ok(output)).unwrap();
        reporter.report(format_args!("wings-vm: error {}", 7));
        let mut received = [0; 18];
        peer.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"wings-vm: error 7\n");
        drop(reporter);
        assert_eq!(vm.used(), baseline);
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(original);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        let (peer, output) = UnixStream::pair().unwrap();
        let mut reporter = Reporter::open(&vm, || Ok(File::from(OwnedFd::from(output)))).unwrap();
        drop(peer);
        reporter.report(format_args!("broken"));
        assert_eq!(vm.used().bytes, baseline.bytes + REPORT_BYTES);
        drop(reporter);
        assert_eq!(vm.used(), baseline);
    }

    #[test]
    fn help_write_failure_is_returned_without_panicking() {
        let (peer, output) = UnixStream::pair().unwrap();
        drop(peer);
        let mut output = File::from(OwnedFd::from(output));
        assert_eq!(
            write_help(&mut output).unwrap_err().raw_os_error(),
            Some(libc::EPIPE)
        );
        let mut output = Vec::new();
        write_help(&mut output).unwrap();
        assert!(output.starts_with(USAGE.as_bytes()) && output.ends_with(b"\n"));
        assert!(output.len() < MAX_DIAGNOSTIC);
    }
}
