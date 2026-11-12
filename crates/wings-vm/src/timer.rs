use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::{PreemptionToken, Reservation, ResourceAccount, Resources};

const TIMER_STACK_BYTES: usize = 256 * 1024;

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
    deadline: Option<Instant>,
    stopped: bool,
}

impl DeadlineTimer {
    pub(crate) fn new(
        tenant: &ResourceAccount,
        preemption: &PreemptionToken,
    ) -> Result<Self, String> {
        let page_bytes = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(|_| "failed to query timer OS page size")?;
        if !page_bytes.is_power_of_two() {
            return Err("unsupported timer OS page size".into());
        }
        let charge = tenant.reserve(Resources {
            bytes: (TIMER_STACK_BYTES + 2 * page_bytes + 4096) as u64,
            ..Resources::default()
        })?;
        let control = Arc::new(Control {
            state: Mutex::new(State {
                deadline: None,
                stopped: false,
            }),
            changed: Condvar::new(),
        });
        let shared = control.clone();
        let token = preemption.clone();
        let thread = thread::Builder::new()
            .name("wings-deadline".into())
            .stack_size(TIMER_STACK_BYTES)
            .spawn(move || run(shared, token))
            .map_err(|error| format!("failed to create worker timer: {error}"))?;
        Ok(Self {
            thread: Some(thread),
            control,
            preemption: preemption.clone(),
            _charge: charge,
        })
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
