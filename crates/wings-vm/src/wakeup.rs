use std::cell::Cell;
use std::io;
use std::marker::PhantomData;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("worker wakeup is only supported on Linux and macOS");

pub struct WorkerParker {
    notifier: WorkerNotifier,
    #[cfg(target_os = "linux")]
    poller: OwnedFd,
    _single_waiter: PhantomData<Cell<()>>,
}

#[derive(Clone)]
pub struct WorkerNotifier {
    descriptor: Arc<OwnedFd>,
}

impl WorkerParker {
    pub fn notifier(&self) -> WorkerNotifier {
        self.notifier.clone()
    }
}

fn wait_with_timeout(
    timeout: Duration,
    mut poll: impl FnMut(Duration) -> io::Result<bool>,
) -> io::Result<bool> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker timeout overflows Instant",
        )
    })?;
    loop {
        match poll(deadline.saturating_duration_since(Instant::now())) {
            Ok(true) => return Ok(true),
            Ok(false) if Instant::now() >= deadline => return Ok(false),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn retry_interrupted(mut operation: impl FnMut() -> libc::c_int) -> io::Result<libc::c_int> {
    loop {
        let result = operation();
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
