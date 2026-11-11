use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use super::{WorkerNotifier, WorkerParker, retry_interrupted, wait_with_timeout};

const USER_EVENT: libc::uintptr_t = 1;

impl WorkerParker {
    pub fn new() -> io::Result<Self> {
        let descriptor = retry_interrupted(|| unsafe { libc::kqueue() })?;
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let flags =
            retry_interrupted(|| unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) })?;
        retry_interrupted(|| unsafe {
            libc::fcntl(
                descriptor.as_raw_fd(),
                libc::F_SETFD,
                flags | libc::FD_CLOEXEC,
            )
        })?;
        submit(&descriptor, &user_event(libc::EV_ADD | libc::EV_CLEAR, 0))?;
        Ok(Self {
            notifier: WorkerNotifier {
                descriptor: Arc::new(descriptor),
            },
            _single_waiter: PhantomData,
        })
    }

    pub fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
        timeout_spec(timeout)?;
        wait_with_timeout(timeout, |remaining| {
            let timeout = timeout_spec(remaining)?;
            let mut event = user_event(0, 0);
            let ready = unsafe {
                libc::kevent(
                    self.notifier.descriptor.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &timeout,
                )
            };
            if ready < 0 {
                return Err(io::Error::last_os_error());
            }
            if ready == 0 {
                return Ok(false);
            }
            if event.flags & libc::EV_ERROR != 0 {
                let error = libc::c_int::try_from(event.data)
                    .ok()
                    .filter(|code| *code > 0)
                    .ok_or_else(|| io::Error::other("invalid worker kqueue error"))?;
                return Err(io::Error::from_raw_os_error(error));
            }
            if event.ident != USER_EVENT
                || event.filter != libc::EVFILT_USER
                || event.flags & libc::EV_EOF != 0
            {
                return Err(io::Error::other("unexpected worker kqueue event"));
            }
            Ok(true)
        })
    }
}

impl WorkerNotifier {
    pub fn notify(&self) -> io::Result<()> {
        submit(&self.descriptor, &user_event(0, libc::NOTE_TRIGGER))
    }
}

fn user_event(flags: u16, filter_flags: u32) -> libc::kevent {
    libc::kevent {
        ident: USER_EVENT,
        filter: libc::EVFILT_USER,
        flags,
        fflags: filter_flags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

fn submit(descriptor: &OwnedFd, event: &libc::kevent) -> io::Result<()> {
    retry_interrupted(|| unsafe {
        libc::kevent(
            descriptor.as_raw_fd(),
            event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    })
    .map(|_| ())
}

fn timeout_spec(timeout: Duration) -> io::Result<libc::timespec> {
    let seconds = libc::time_t::try_from(timeout.as_secs()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker timeout exceeds kqueue second range",
        )
    })?;
    Ok(libc::timespec {
        tv_sec: seconds,
        tv_nsec: timeout.subsec_nanos().into(),
    })
}
