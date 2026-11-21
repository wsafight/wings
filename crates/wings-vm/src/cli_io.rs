use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::os::fd::FromRawFd;

use wings_vm::{Reservation, ResourceAccount, ResourceLevel, Resources};

const INPUT_BYTES: usize = 8 << 10;
const OUTPUT_BYTES: usize = 128;
const CONTROL_BYTES: usize = 4 << 10;
const IO_BYTES: u64 = (INPUT_BYTES + CONTROL_BYTES) as u64;

pub struct Stdio {
    reader: BufReader<File>,
    writer: Option<BufWriter<File>>,
    _charge: Reservation,
}

impl Stdio {
    pub fn new(account: &ResourceAccount) -> Result<Self, String> {
        Self::open(
            account,
            || duplicate(libc::STDIN_FILENO),
            || duplicate(libc::STDOUT_FILENO),
        )
    }

    fn open(
        account: &ResourceAccount,
        input: impl FnOnce() -> io::Result<File>,
        output: impl FnOnce() -> io::Result<File>,
    ) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process {
            return Err("CLI I/O requires a process resource account".into());
        }
        let charge = account.reserve(Resources {
            bytes: IO_BYTES,
            ..Resources::default()
        })?;
        let input = input().map_err(|error| format!("CLI stdin: {error}"))?;
        let output = output().map_err(|error| format!("CLI stdout: {error}"))?;
        Ok(Self {
            reader: BufReader::with_capacity(INPUT_BYTES, input),
            writer: Some(BufWriter::with_capacity(OUTPUT_BYTES, output)),
            _charge: charge,
        })
    }

    pub fn streams(&mut self) -> (&mut BufReader<File>, &mut BufWriter<File>) {
        (
            &mut self.reader,
            self.writer.as_mut().expect("live CLI writer"),
        )
    }
}

impl Drop for Stdio {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            drop(writer.into_parts());
        }
    }
}

pub(super) fn duplicate(descriptor: libc::c_int) -> io::Result<File> {
    loop {
        let copied = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 3) };
        if copied >= 0 {
            return Ok(unsafe { File::from_raw_fd(copied) });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::{BufRead, Read, Write};
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;

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

    fn null() -> io::Result<File> {
        File::options().read(true).write(true).open("/dev/null")
    }

    fn file(socket: UnixStream) -> File {
        File::from(OwnedFd::from(socket))
    }

    #[test]
    fn fixed_stdio_storage_fits_control_admission() {
        assert!(std::mem::size_of::<Stdio>() + OUTPUT_BYTES + 256 <= CONTROL_BYTES);
        assert_eq!(IO_BYTES, 12 << 10);
    }

    #[test]
    fn admission_precedes_open_and_checks_each_level_without_leaks() {
        let (vm, tenant, account) = accounts();
        let baseline = [account.used(), tenant.used(), vm.used()];
        for invalid in [&vm, &tenant] {
            assert!(
                Stdio::open(
                    invalid,
                    || panic!("opened input"),
                    || panic!("opened output")
                )
                .is_err()
            );
        }
        for owner in [&account, &tenant, &vm] {
            for remaining in [0, IO_BYTES - 1, IO_BYTES] {
                let fill = owner
                    .reserve(bytes(owner.limit().bytes - owner.used().bytes - remaining))
                    .unwrap();
                let before = [account.used(), tenant.used(), vm.used()];
                let opened = Cell::new(0);
                let open = || {
                    opened.set(opened.get() + 1);
                    null()
                };
                if remaining < IO_BYTES {
                    let error = Stdio::open(&account, open, open).err().unwrap();
                    assert!(error.contains(&format!(
                        "resource_exhausted at {:?} account",
                        owner.level()
                    )));
                    assert_eq!(opened.get(), 0);
                } else {
                    let stdio = Stdio::open(&account, open, open).unwrap();
                    assert_eq!(opened.get(), 2);
                    assert_eq!(stdio.reader.capacity(), INPUT_BYTES);
                    assert_eq!(stdio.writer.as_ref().unwrap().capacity(), OUTPUT_BYTES);
                    assert_eq!(stdio._charge.amount(), bytes(IO_BYTES));
                    for (used, previous) in [account.used(), tenant.used(), vm.used()]
                        .into_iter()
                        .zip(before)
                    {
                        assert_eq!(used, bytes(previous.bytes + IO_BYTES));
                    }
                    assert!(
                        Stdio::open(
                            &account,
                            || panic!("opened input"),
                            || panic!("opened output")
                        )
                        .is_err()
                    );
                    drop(stdio);
                }
                assert_eq!([account.used(), tenant.used(), vm.used()], before);
                drop(fill);
                assert_eq!([account.used(), tenant.used(), vm.used()], baseline);
            }
        }
    }

    #[test]
    fn failed_open_rolls_back_charge_and_closes_prior_descriptor() {
        let (vm, tenant, account) = accounts();
        let baseline = [account.used(), tenant.used(), vm.used()];
        let error = Stdio::open(
            &account,
            || Err(io::Error::from_raw_os_error(libc::EBADF)),
            || panic!("opened output after input failed"),
        )
        .err()
        .unwrap();
        assert!(error.starts_with("CLI stdin:"));
        assert_eq!([account.used(), tenant.used(), vm.used()], baseline);
        let (mut peer, input) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let error = Stdio::open(
            &account,
            || Ok(file(input)),
            || Err(io::Error::from_raw_os_error(libc::EMFILE)),
        )
        .err()
        .unwrap();
        assert!(error.starts_with("CLI stdout:"));
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        assert_eq!([account.used(), tenant.used(), vm.used()], baseline);
    }

    #[test]
    fn duplicated_descriptor_closes_on_exec_and_preserves_original_owner() {
        let original = null().unwrap();
        let copied = duplicate(original.as_raw_fd()).unwrap();
        assert_ne!(original.as_raw_fd(), copied.as_raw_fd());
        let flags = unsafe { libc::fcntl(copied.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        drop(copied);
        assert!(unsafe { libc::fcntl(original.as_raw_fd(), libc::F_GETFD) } >= 0);
        assert_eq!(duplicate(-1).unwrap_err().raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn failed_flush_retains_charge_until_buffer_and_descriptor_are_discarded() {
        let (vm, tenant, account) = accounts();
        let baseline = [account.used(), tenant.used(), vm.used()];
        let (destination, output) = UnixStream::pair().unwrap();
        let mut stdio = Stdio::open(&account, null, || Ok(file(output))).unwrap();
        drop(destination);
        let (_, writer) = stdio.streams();
        writer.write_all(b"pending").unwrap();
        assert_eq!(
            writer.flush().unwrap_err().raw_os_error(),
            Some(libc::EPIPE)
        );
        assert_eq!(writer.buffer(), b"pending");
        for (used, previous) in [account.used(), tenant.used(), vm.used()]
            .into_iter()
            .zip(baseline)
        {
            assert_eq!(used, bytes(previous.bytes + IO_BYTES));
        }
        drop(stdio);
        assert_eq!([account.used(), tenant.used(), vm.used()], baseline);
    }

    #[test]
    fn buffers_reuse_storage_flush_completed_output_and_discard_unflushed_tail() {
        let (vm, tenant, account) = accounts();
        let baseline = [account.used(), tenant.used(), vm.used()];
        let (mut source, input) = UnixStream::pair().unwrap();
        let (mut destination, output) = UnixStream::pair().unwrap();
        destination.set_nonblocking(true).unwrap();
        source.write_all(b"one\ntwo\n").unwrap();
        source.shutdown(std::net::Shutdown::Write).unwrap();
        let mut stdio = Stdio::open(&account, || Ok(file(input)), || Ok(file(output))).unwrap();
        let (reader, writer) = stdio.streams();
        let storage = reader.fill_buf().unwrap().as_ptr();
        assert_eq!(reader.fill_buf().unwrap(), b"one\ntwo\n");
        reader.consume(4);
        assert_eq!(reader.fill_buf().unwrap().as_ptr(), storage.wrapping_add(4));
        assert_eq!(reader.fill_buf().unwrap(), b"two\n");
        writer.write_all(b"flushed\n").unwrap();
        writer.flush().unwrap();
        let mut received = [0; 8];
        destination.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"flushed\n");
        writer.write_all(b"discard this tail").unwrap();
        assert_eq!(
            destination.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(stdio);
        assert_eq!(destination.read(&mut [0]).unwrap(), 0);
        assert_eq!([account.used(), tenant.used(), vm.used()], baseline);
    }
}
