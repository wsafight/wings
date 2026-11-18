use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::{PreemptionToken, Reservation, ResourceAccount, Resources};

const TIMER_STACK_BYTES: usize = 256 * 1024;
const TIMER_CONTROL_BYTES: usize = 4096;
const _: () = assert!(
    std::mem::size_of::<Control>() + crate::native_sync::shared_bytes(1, 1) <= TIMER_CONTROL_BYTES
);

pub(crate) struct DeadlineTimer {
    thread: Option<JoinHandle<()>>,
    control: Arc<Control>,
    preemption: PreemptionToken,
    _charge: Reservation,
}

struct Control {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    startup: Option<Result<(), &'static str>>,
    deadline: Option<Instant>,
    stopped: bool,
}

impl DeadlineTimer {
    pub(crate) fn new(
        tenant: &ResourceAccount,
        preemption: &PreemptionToken,
    ) -> Result<Self, String> {
        Self::new_with_stack_check(tenant, preemption, crate::native_stack::check_current)
    }

    fn new_with_stack_check(
        tenant: &ResourceAccount,
        preemption: &PreemptionToken,
        stack_check: impl FnOnce(usize) -> Result<(), &'static str> + Send + 'static,
    ) -> Result<Self, String> {
        let page_bytes = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(|_| "failed to query timer OS page size")?;
        if !page_bytes.is_power_of_two() {
            return Err("unsupported timer OS page size".into());
        }
        let charge = tenant.reserve(Resources {
            bytes: (TIMER_STACK_BYTES + 2 * page_bytes + TIMER_CONTROL_BYTES) as u64,
            ..Resources::default()
        })?;
        let control = Arc::new(Control {
            state: crate::native_sync::mutex(State {
                startup: None,
                deadline: None,
                stopped: false,
            }),
            changed: crate::native_sync::condvar(),
        });
        let shared = control.clone();
        let token = preemption.clone();
        let stack_bytes = TIMER_STACK_BYTES + 2 * page_bytes;
        let thread = thread::Builder::new()
            .name("wings-deadline".into())
            .stack_size(TIMER_STACK_BYTES)
            .spawn(move || {
                let status = stack_check(stack_bytes);
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                state.startup = Some(status);
                shared.changed.notify_one();
                drop(state);
                if status.is_ok() {
                    run(shared, token);
                }
            })
            .map_err(|error| format!("failed to create worker timer: {error}"))?;
        let timer = Self {
            thread: Some(thread),
            control,
            preemption: preemption.clone(),
            _charge: charge,
        };
        let status = {
            let mut state = timer
                .control
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            while state.startup.is_none() {
                state = timer
                    .control
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            state.startup.expect("timer startup result")
        };
        status?;
        Ok(timer)
    }

    pub(crate) fn arm(&self, deadline: Instant) -> Result<(), String> {
        let mut state = self
            .control
            .state
            .lock()
            .map_err(|_| "timer lock poisoned")?;
        self.preemption.clear();
        if deadline <= Instant::now() {
            self.preemption.request();
            state.deadline = None;
        } else {
            state.deadline = Some(deadline);
        }
        self.control.changed.notify_one();
        Ok(())
    }

    pub(crate) fn disarm(&self) {
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.deadline = None;
        self.preemption.clear();
        self.control.changed.notify_one();
    }
}

impl Drop for DeadlineTimer {
    fn drop(&mut self) {
        {
            let mut state = self
                .control
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.stopped = true;
            state.deadline = None;
            self.preemption.clear();
            self.control.changed.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(control: Arc<Control>, preemption: PreemptionToken) {
    let mut state = control
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while !state.stopped {
        if let Some(deadline) = state.deadline {
            let now = Instant::now();
            if deadline <= now {
                preemption.request();
                state.deadline = None;
            } else {
                state = control
                    .changed
                    .wait_timeout(state, deadline.duration_since(now))
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            }
        } else {
            state = control
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_native_stack_joins_before_returning_its_original_charge() {
        let limits = Resources {
            bytes: 4 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm_accounted(limits).unwrap();
        let bootstrap = vm.used();
        let tenant = vm.tenant_accounted(limits).unwrap();
        let baseline = vm.used();
        let account = tenant.clone();
        let result = DeadlineTimer::new_with_stack_check(
            &tenant,
            &PreemptionToken::default(),
            move |maximum| {
                let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
                assert_eq!(maximum, TIMER_STACK_BYTES + 2 * page);
                assert_eq!(std::thread::current().name(), Some("wings-deadline"));
                assert!(account.used().bytes >= TIMER_STACK_BYTES as u64);
                Err("injected native timer stack rejection")
            },
        );
        assert_eq!(
            result.err().as_deref(),
            Some("injected native timer stack rejection")
        );
        assert_eq!(vm.used(), baseline);
        drop(tenant);
        assert_eq!(vm.used(), bootstrap);
    }
}
