use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{PreemptionToken, Reservation, ResourceAccount, Resources};

const TIMER_STACK_BYTES: usize = 256 * 1024;
const TIMER_CONTROL_BYTES: usize = 4096;
const DISARMED_NS: u64 = u64::MAX;
const ARMED_BIT: u64 = 1;
const FIRING_BIT: u64 = 2;
// Packed control word: the deadline occupies bits [2..64) while the two low
// bits carry ARMED/FIRING, so a single CAS or store updates the deadline and
// the arming state atomically and no sequence counter is needed.
const DEADLINE_SHIFT: u32 = 2;
const PACKED_DEADLINE_MAX: u64 = u64::MAX >> DEADLINE_SHIFT;

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
    base: Instant,
    packed: AtomicU64,
    sleeping_until_ns: AtomicU64,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    startup: Option<Result<(), &'static str>>,
    stopped: bool,
}

fn offset_ns(base: Instant, target: Instant) -> u64 {
    u64::try_from(target.saturating_duration_since(base).as_nanos())
        .unwrap_or(DISARMED_NS - 1)
        .min(DISARMED_NS - 1)
}

fn pack_deadline(deadline_ns: u64) -> u64 {
    deadline_ns.min(PACKED_DEADLINE_MAX) << DEADLINE_SHIFT
}

fn packed_ns(word: u64) -> u64 {
    word >> DEADLINE_SHIFT
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
            base: Instant::now(),
            packed: AtomicU64::new(0),
            sleeping_until_ns: AtomicU64::new(0),
            state: crate::native_sync::mutex(State {
                startup: None,
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

    fn disarm_bits(&self) {
        loop {
            let word = self.control.packed.load(Ordering::Acquire);
            if word & FIRING_BIT != 0 {
                std::hint::spin_loop();
                continue;
            }
            if word & ARMED_BIT == 0 {
                break;
            }
            if self
                .control
                .packed
                .compare_exchange_weak(word, word & !ARMED_BIT, Ordering::SeqCst, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    pub(crate) fn arm_after(&self, quantum: Duration) -> Result<(), String> {
        let now = Instant::now();
        let deadline = now
            .checked_add(quantum)
            .ok_or("scheduler timer deadline overflow")?;
        self.arm_with_now(now, deadline)
    }

    pub(crate) fn arm(&self, deadline: Instant) -> Result<(), String> {
        self.arm_with_now(Instant::now(), deadline)
    }

    fn arm_with_now(&self, now: Instant, deadline: Instant) -> Result<(), String> {
        self.disarm_bits();
        self.preemption.clear();
        if deadline <= now {
            self.preemption.request();
            return Ok(());
        }
        let target_ns = offset_ns(self.control.base, deadline);
        // One store publishes the deadline and the armed state together; a
        // concurrently firing timer either completes the flag transition
        // before this store (its request is consumed by the clear above) or
        // sees the new armed word afterwards.
        self.control
            .packed
            .store(pack_deadline(target_ns) | ARMED_BIT, Ordering::SeqCst);
        if target_ns < self.control.sleeping_until_ns.load(Ordering::SeqCst) {
            let _state = self
                .control
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.control.changed.notify_one();
        }
        Ok(())
    }

    pub(crate) fn disarm(&self) {
        self.disarm_bits();
        self.preemption.clear();
    }
}

impl Drop for DeadlineTimer {
    fn drop(&mut self) {
        self.disarm_bits();
        self.preemption.clear();
        {
            let mut state = self
                .control
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.stopped = true;
            self.control.changed.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(control: Arc<Control>, preemption: PreemptionToken) {
    let mut coaster_until_ns = 0u64;
    loop {
        let word = control.packed.load(Ordering::SeqCst);
        if word & ARMED_BIT != 0 {
            let target_ns = packed_ns(word);
            let now_ns = offset_ns(control.base, Instant::now());
            if target_ns <= now_ns {
                if control
                    .packed
                    .compare_exchange(word, word | FIRING_BIT, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    preemption.request();
                    control
                        .packed
                        .fetch_and(!(ARMED_BIT | FIRING_BIT), Ordering::SeqCst);
                }
                continue;
            }
            coaster_until_ns = coaster_until_ns.max(target_ns);
            let state = control
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.stopped {
                break;
            }
            control.sleeping_until_ns.store(target_ns, Ordering::SeqCst);
            let recheck = control.packed.load(Ordering::SeqCst);
            if recheck & ARMED_BIT != 0 && packed_ns(recheck) < target_ns {
                control.sleeping_until_ns.store(0, Ordering::SeqCst);
                continue;
            }
            let now_ns = offset_ns(control.base, Instant::now());
            if target_ns > now_ns {
                drop(
                    control
                        .changed
                        .wait_timeout(state, Duration::from_nanos(target_ns - now_ns))
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
            }
            control.sleeping_until_ns.store(0, Ordering::SeqCst);
        } else {
            let state = control
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.stopped {
                break;
            }
            let now_ns = offset_ns(control.base, Instant::now());
            if now_ns < coaster_until_ns {
                control
                    .sleeping_until_ns
                    .store(coaster_until_ns, Ordering::SeqCst);
                let recheck = control.packed.load(Ordering::SeqCst);
                if recheck & ARMED_BIT != 0 && packed_ns(recheck) < coaster_until_ns {
                    control.sleeping_until_ns.store(0, Ordering::SeqCst);
                    continue;
                }
                drop(
                    control
                        .changed
                        .wait_timeout(state, Duration::from_nanos(coaster_until_ns - now_ns))
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
                control.sleeping_until_ns.store(0, Ordering::SeqCst);
                continue;
            }
            coaster_until_ns = 0;
            control
                .sleeping_until_ns
                .store(DISARMED_NS, Ordering::SeqCst);
            if control.packed.load(Ordering::SeqCst) & ARMED_BIT != 0 {
                control.sleeping_until_ns.store(0, Ordering::SeqCst);
                continue;
            }
            drop(
                control
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
            control.sleeping_until_ns.store(0, Ordering::SeqCst);
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
