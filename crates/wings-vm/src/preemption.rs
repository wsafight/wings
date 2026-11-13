use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{Reservation, ResourceAccount, Resources};

#[derive(Clone, Default)]
pub struct PreemptionToken {
    pub(crate) flag: Arc<AtomicU32>,
    observer: Option<Arc<Observer>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YieldObservation {
    pub requested_at: Instant,
    pub yielded_at: Instant,
}

impl YieldObservation {
    pub fn latency(&self) -> Duration {
        self.yielded_at.duration_since(self.requested_at)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreemptionObservation {
    pub pending_since: Option<Instant>,
    pub last_yield: Option<YieldObservation>,
}

struct Observer {
    state: Mutex<PreemptionObservation>,
    _charge: Reservation,
}

impl PreemptionToken {
    pub fn new_observed(account: &ResourceAccount) -> Result<Self, String> {
        let charge = account.reserve(Resources {
            bytes: 4096,
            ..Resources::default()
        })?;
        Ok(Self {
            flag: Arc::new(AtomicU32::new(0)),
            observer: Some(Arc::new(Observer {
                state: Mutex::new(PreemptionObservation::default()),
                _charge: charge,
            })),
        })
    }

    pub fn request(&self) {
        if let Some(observer) = &self.observer {
            let mut state = observer
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pending_since.get_or_insert_with(Instant::now);
            self.flag.store(1, Ordering::SeqCst);
        } else {
            self.flag.store(1, Ordering::SeqCst);
        }
    }

    pub fn observation(&self) -> Option<PreemptionObservation> {
        self.observer.as_ref().map(|observer| {
            *observer
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        })
    }

    pub(crate) fn clear(&self) {
        if self.observer.is_some() {
            self.take_request();
        } else {
            self.flag.store(0, Ordering::SeqCst);
        }
    }

    pub(crate) fn take_request(&self) -> Option<Instant> {
        if let Some(observer) = &self.observer {
            let mut state = observer
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.flag.swap(0, Ordering::SeqCst);
            state.pending_since.take()
        } else {
            self.flag.swap(0, Ordering::SeqCst);
            None
        }
    }

    pub(crate) fn confirm_yield(&self, requested_at: Instant) {
        if let Some(observer) = &self.observer {
            let yielded_at = Instant::now();
            observer
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last_yield = Some(YieldObservation {
                requested_at,
                yielded_at,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_observer_charge_covers_control_and_arc_headers() {
        assert!(std::mem::size_of::<Observer>() + std::mem::size_of::<AtomicU32>() + 64 <= 4096);
    }

    #[test]
    fn later_request_survives_confirmation_and_poisoned_lock_recovers() {
        let vm = ResourceAccount::vm(Resources {
            bytes: 8192,
            ..Resources::default()
        });
        let token = PreemptionToken::new_observed(&vm).unwrap();
        token.request();
        let first = token.take_request().unwrap();
        token.request();
        let second = token.observation().unwrap().pending_since;
        token.confirm_yield(first);
        assert_eq!(token.observation().unwrap().pending_since, second);
        assert_eq!(
            token
                .observation()
                .unwrap()
                .last_yield
                .unwrap()
                .requested_at,
            first
        );
        let observer = token.observer.as_ref().unwrap().clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = observer.state.lock().unwrap();
                panic!("observer lock poison fixture");
            })
            .join()
            .is_err()
        );
        assert_eq!(token.take_request(), second);
        token.request();
        token.clear();
        assert_eq!(token.observation().unwrap().pending_since, None);
        token.confirm_yield(first);
        assert!(token.observation().unwrap().last_yield.is_some());
        drop(token);
        assert_eq!(vm.used(), Resources::default());
    }
}
