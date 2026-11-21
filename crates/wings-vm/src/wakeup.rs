use std::cell::Cell;
use std::io;
use std::marker::PhantomData;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Reservation, ResourceAccount, Resources};

const WAIT_CONTROL_BYTES: u64 = 4096;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("worker wakeup is only supported on Linux and macOS");

pub struct WorkerParker {
    #[cfg(target_os = "linux")]
    poller: OwnedFd,
    notifier: WorkerNotifier,
    _single_waiter: PhantomData<Cell<()>>,
}

#[derive(Clone)]
pub struct WorkerNotifier {
    endpoint: Arc<WakeEndpoint>,
}

struct WakeEndpoint {
    descriptor: OwnedFd,
    _charge: Option<Reservation>,
}

impl WorkerParker {
    pub fn new() -> io::Result<Self> {
        Self::create(None)
    }

    pub fn new_accounted(account: &ResourceAccount) -> Result<Self, String> {
        Self::new_accounted_with(account, Self::create)
    }

    fn new_accounted_with(
        account: &ResourceAccount,
        create: impl FnOnce(Option<Reservation>) -> io::Result<Self>,
    ) -> Result<Self, String> {
        let charge = account.reserve(Resources {
            bytes: WAIT_CONTROL_BYTES,
            ..Resources::default()
        })?;
        create(Some(charge)).map_err(|error| format!("create worker wait: {error}"))
    }

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
