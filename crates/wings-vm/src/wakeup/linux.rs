use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use super::{WorkerNotifier, WorkerParker, retry_interrupted, wait_with_timeout};

impl WorkerParker {
    pub fn new() -> io::Result<Self> {
        let descriptor = retry_interrupted(|| unsafe {
            libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC)
        })?;
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let poller = retry_interrupted(|| unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) })?;
        let poller = unsafe { OwnedFd::from_raw_fd(poller) };
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0,
        };
        retry_interrupted(|| unsafe {
            libc::epoll_ctl(
                poller.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                descriptor.as_raw_fd(),
                &mut event,
            )
        })?;
        Ok(Self {
            notifier: WorkerNotifier {
                descriptor: Arc::new(descriptor),
            },
            poller,
            _single_waiter: PhantomData,
        })
    }

    pub fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
        wait_with_timeout(timeout, |remaining| {
            let mut event = libc::epoll_event { events: 0, u64: 0 };
            let ready = unsafe {
                libc::epoll_wait(
                    self.poller.as_raw_fd(),
                    &mut event,
                    1,
                    timeout_millis(remaining),
                )
            };
            if ready < 0 {
                return Err(io::Error::last_os_error());
            }
            if ready == 0 {
                return Ok(false);
            }
            if event.events & libc::EPOLLIN as u32 == 0
                || event.events & (libc::EPOLLERR | libc::EPOLLHUP) as u32 != 0
            {
                return Err(io::Error::other("unexpected worker epoll event"));
            }
            let mut counter = 0_u64;
            eventfd_io(|| unsafe {
                libc::read(
                    self.notifier.descriptor.as_raw_fd(),
                    (&mut counter as *mut u64).cast(),
                    size_of::<u64>(),
                )
            })
            .map(|()| true)
        })
    }
}

impl WorkerNotifier {
    pub fn notify(&self) -> io::Result<()> {
        let counter = 1_u64;
        eventfd_io(|| unsafe {
            libc::write(
                self.descriptor.as_raw_fd(),
                (&counter as *const u64).cast(),
                size_of::<u64>(),
            )
        })
    }
}

fn timeout_millis(timeout: Duration) -> libc::c_int {
    libc::c_int::try_from(timeout.as_nanos().div_ceil(1_000_000)).unwrap_or(libc::c_int::MAX)
}

fn eventfd_io(mut operation: impl FnMut() -> libc::ssize_t) -> io::Result<()> {
    loop {
        let transferred = operation();
        if transferred == size_of::<u64>() as libc::ssize_t {
            return Ok(());
        }
        if transferred >= 0 {
            return Err(io::Error::other(
                "worker eventfd transferred an incomplete counter",
            ));
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::WouldBlock => return Ok(()),
            _ => return Err(error),
        }
    }
}
