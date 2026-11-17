use std::sync::{Condvar, Mutex};

#[cfg(target_os = "macos")]
pub(crate) const MUTEX_BYTES: usize = std::mem::size_of::<libc::pthread_mutex_t>();
#[cfg(not(target_os = "macos"))]
pub(crate) const MUTEX_BYTES: usize = 0;

#[cfg(target_os = "macos")]
pub(crate) const CONDVAR_BYTES: usize = std::mem::size_of::<libc::pthread_cond_t>();
#[cfg(not(target_os = "macos"))]
pub(crate) const CONDVAR_BYTES: usize = 0;

pub(crate) const fn shared_bytes(mutexes: usize, condvars: usize) -> usize {
    2 * std::mem::size_of::<usize>() + mutexes * MUTEX_BYTES + condvars * CONDVAR_BYTES
}

pub(crate) fn mutex<Value>(value: Value) -> Mutex<Value> {
    let mutex = Mutex::new(value);
    #[cfg(target_os = "macos")]
    drop(mutex.lock().unwrap_or_else(|error| error.into_inner()));
    mutex
}

pub(crate) fn condvar() -> Condvar {
    let condvar = Condvar::new();
    #[cfg(target_os = "macos")]
    condvar.notify_all();
    condvar
}
