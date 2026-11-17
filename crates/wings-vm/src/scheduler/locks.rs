use std::ops::{Deref, DerefMut};
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{Shared, State};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerLockMetrics {
    pub acquisitions: u64,
    pub wait_ns: u64,
    pub held_ns: u64,
    pub max_wait_ns: u64,
    pub max_held_ns: u64,
    pub saturated: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerLockStatistics {
    pub claim: SchedulerLockMetrics,
    pub start: SchedulerLockMetrics,
    pub snapshot: SchedulerLockMetrics,
    pub finish: SchedulerLockMetrics,
}

#[derive(Clone, Copy)]
pub(super) enum LockPhase {
    Claim,
    Start,
    Snapshot,
    Finish,
}

#[derive(Default)]
struct Counters {
    acquisitions: AtomicU64,
    wait_ns: AtomicU64,
    held_ns: AtomicU64,
    max_wait_ns: AtomicU64,
    max_held_ns: AtomicU64,
    saturated: AtomicBool,
}

#[derive(Default)]
pub(super) struct WorkerLockObservation {
    phases: [Counters; 4],
}

impl Counters {
    fn add(&self, counter: &AtomicU64, value: u64) {
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let updated = current.saturating_add(value);
            match counter.compare_exchange_weak(
                current,
                updated,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if current.checked_add(value).is_none() {
                        self.saturated.store(true, Ordering::Relaxed);
                    }
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    fn nanos(&self, duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or_else(|_| {
            self.saturated.store(true, Ordering::Relaxed);
            u64::MAX
        })
    }

    fn record(&self, wait: Duration, held: Duration) {
        let wait = self.nanos(wait);
        let held = self.nanos(held);
        self.add(&self.acquisitions, 1);
        self.add(&self.wait_ns, wait);
        self.add(&self.held_ns, held);
        self.max_wait_ns.fetch_max(wait, Ordering::Relaxed);
        self.max_held_ns.fetch_max(held, Ordering::Relaxed);
    }

    fn snapshot(&self) -> SchedulerLockMetrics {
        SchedulerLockMetrics {
            acquisitions: self.acquisitions.load(Ordering::Relaxed),
            wait_ns: self.wait_ns.load(Ordering::Relaxed),
            held_ns: self.held_ns.load(Ordering::Relaxed),
            max_wait_ns: self.max_wait_ns.load(Ordering::Relaxed),
            max_held_ns: self.max_held_ns.load(Ordering::Relaxed),
            saturated: self.saturated.load(Ordering::Relaxed),
        }
    }
}

impl WorkerLockObservation {
    pub(super) fn snapshot(&self) -> SchedulerLockStatistics {
        SchedulerLockStatistics {
            claim: self.phases[LockPhase::Claim as usize].snapshot(),
            start: self.phases[LockPhase::Start as usize].snapshot(),
            snapshot: self.phases[LockPhase::Snapshot as usize].snapshot(),
            finish: self.phases[LockPhase::Finish as usize].snapshot(),
        }
    }
}

struct Measurement<'observer> {
    counters: &'observer Counters,
    wait: Duration,
    acquired: Instant,
}

pub(super) struct WorkerStateGuard<'state> {
    state: Option<MutexGuard<'state, State>>,
    measurement: Option<Measurement<'state>>,
}

impl Shared {
    pub(super) fn worker_lock(&self, worker: usize, phase: LockPhase) -> WorkerStateGuard<'_> {
        match self.lock_observations.get(worker) {
            Some(observer) => {
                let requested = Instant::now();
                let state = self.lock();
                let acquired = Instant::now();
                WorkerStateGuard {
                    state: Some(state),
                    measurement: Some(Measurement {
                        counters: &observer.phases[phase as usize],
                        wait: acquired.duration_since(requested),
                        acquired,
                    }),
                }
            }
            None => WorkerStateGuard {
                state: Some(self.lock()),
                measurement: None,
            },
        }
    }
}

impl Deref for WorkerStateGuard<'_> {
    type Target = State;

    fn deref(&self) -> &State {
        self.state.as_deref().expect("held worker state")
    }
}

impl DerefMut for WorkerStateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        self.state.as_deref_mut().expect("held worker state")
    }
}

impl Drop for WorkerStateGuard<'_> {
    fn drop(&mut self) {
        let measured = self.measurement.as_ref().map(|measurement| {
            (
                measurement.counters,
                measurement.wait,
                measurement.acquired.elapsed(),
            )
        });
        drop(self.state.take());
        if let Some((counters, wait, held)) = measured {
            counters.record(wait, held);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResourceAccount, Resources, Scheduler, SchedulerConfig};

    fn limits() -> Resources {
        Resources {
            bytes: 128 << 20,
            processes: 4,
            messages: 8,
            requests: 8,
        }
    }

    fn fixture(enabled: bool) -> (ResourceAccount, ResourceAccount, Scheduler) {
        let vm = ResourceAccount::vm(limits());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers: 2,
                capacity: 1,
                requests: 1,
                lock_statistics: enabled,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        (vm, tenant, scheduler)
    }

    #[test]
    fn lock_counters_preserve_totals_maxima_and_phase_separation() {
        let observer = WorkerLockObservation::default();
        let counters = &observer.phases[LockPhase::Start as usize];
        counters.record(Duration::from_nanos(7), Duration::from_nanos(11));
        counters.record(Duration::from_nanos(3), Duration::from_nanos(19));
        assert_eq!(
            observer.snapshot(),
            SchedulerLockStatistics {
                start: SchedulerLockMetrics {
                    acquisitions: 2,
                    wait_ns: 10,
                    held_ns: 30,
                    max_wait_ns: 7,
                    max_held_ns: 19,
                    saturated: false,
                },
                ..SchedulerLockStatistics::default()
            }
        );
    }

    #[test]
    fn lock_counters_saturate_without_wrapping_or_hiding_duration_overflow() {
        let counters = Counters::default();
        counters.acquisitions.store(u64::MAX, Ordering::Relaxed);
        counters.wait_ns.store(u64::MAX - 1, Ordering::Relaxed);
        counters.record(Duration::from_nanos(7), Duration::MAX);
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.acquisitions, u64::MAX);
        assert_eq!(snapshot.wait_ns, u64::MAX);
        assert_eq!(snapshot.held_ns, u64::MAX);
        assert_eq!(snapshot.max_held_ns, u64::MAX);
        assert!(snapshot.saturated);
        counters.record(Duration::from_nanos(1), Duration::from_nanos(1));
        assert_eq!(counters.snapshot().held_ns, u64::MAX);
    }

    #[test]
    fn lock_statistics_concurrent_queries_do_not_lose_records() {
        let counters = Counters::default();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let counters = &counters;
                scope.spawn(move || {
                    for _ in 0..1000 {
                        counters.record(Duration::from_nanos(2), Duration::from_nanos(3));
                        let _ = counters.snapshot();
                    }
                });
            }
        });
        assert_eq!(
            counters.snapshot(),
            SchedulerLockMetrics {
                acquisitions: 4000,
                wait_ns: 8000,
                held_ns: 12000,
                max_wait_ns: 2,
                max_held_ns: 3,
                saturated: false,
            }
        );
    }

    #[test]
    fn worker_lock_guard_is_optional_and_records_each_phase_on_release() {
        for enabled in [false, true] {
            let (vm, tenant, scheduler) = fixture(enabled);
            let handle = scheduler.handle();
            assert!(handle.lock_statistics(2).is_err());
            assert_eq!(handle.lock_statistics(0).unwrap().is_some(), enabled);
            drop(scheduler);
            for (worker, phase) in [
                (0, LockPhase::Start),
                (1, LockPhase::Snapshot),
                (0, LockPhase::Finish),
            ] {
                let before = handle.lock_statistics(worker).unwrap();
                let guard = handle.shared.worker_lock(worker, phase);
                assert!(guard.closed);
                assert!(handle.shared.state.try_lock().is_err());
                assert_eq!(handle.lock_statistics(worker).unwrap(), before);
                drop(guard);
                assert!(handle.shared.state.try_lock().is_ok());
                let after = handle.lock_statistics(worker).unwrap();
                if enabled {
                    let metric = match phase {
                        LockPhase::Start => after.unwrap().start,
                        LockPhase::Snapshot => after.unwrap().snapshot,
                        LockPhase::Finish => after.unwrap().finish,
                        LockPhase::Claim => unreachable!(),
                    };
                    assert_eq!(metric.acquisitions, 1);
                    assert!(metric.wait_ns >= metric.max_wait_ns);
                    assert!(metric.held_ns >= metric.max_held_ns);
                    assert!(!metric.saturated);
                } else {
                    assert_eq!(after, None);
                }
            }
            drop((handle, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn worker_lock_contention_preserves_the_original_mutex_and_final_counts() {
        let (vm, tenant, scheduler) = fixture(true);
        let handle = scheduler.handle();
        drop(scheduler);
        let guard = handle.shared.lock();
        let (attempted, receiver) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let shared = &handle.shared;
            let worker = scope.spawn(move || {
                attempted.send(()).unwrap();
                let state = shared.worker_lock(0, LockPhase::Finish);
                assert!(state.closed);
            });
            receiver.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_eq!(
                handle
                    .lock_statistics(0)
                    .unwrap()
                    .unwrap()
                    .finish
                    .acquisitions,
                0
            );
            drop(guard);
            worker.join().unwrap();
        });
        assert_eq!(
            handle
                .lock_statistics(0)
                .unwrap()
                .unwrap()
                .finish
                .acquisitions,
            1
        );
        drop((handle, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn lock_observation_storage_is_admitted_and_retained_until_the_last_handle() {
        let (vm, tenant, scheduler) = fixture(false);
        let unobserved = scheduler.handle.shared._charge.amount().bytes;
        drop((scheduler, tenant));
        assert_eq!(vm.used(), Resources::default());
        let tenant = vm.tenant_accounted(limits()).unwrap();
        let baseline = vm.used();
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers: 2,
                capacity: 1,
                requests: 1,
                lock_statistics: true,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let observed = scheduler.handle.shared._charge.amount().bytes;
        assert_eq!(
            observed - unobserved,
            (2 * std::mem::size_of::<WorkerLockObservation>()) as u64
        );
        let handle = scheduler.handle();
        drop(scheduler);
        assert!(vm.used().bytes > baseline.bytes);
        assert!(handle.lock_statistics(0).unwrap().is_some());
        let last = handle.clone();
        drop(handle);
        assert!(last.lock_statistics(1).unwrap().is_some());
        drop(last);
        assert_eq!(vm.used(), baseline);
        for restrict_vm in [false, true] {
            let account = if restrict_vm { &vm } else { &tenant };
            let fill = account
                .reserve(Resources {
                    bytes: account.limit().bytes - account.used().bytes - observed + 1,
                    ..Resources::default()
                })
                .unwrap();
            let filled = vm.used();
            assert!(
                Scheduler::new(
                    &tenant,
                    SchedulerConfig {
                        workers: 2,
                        capacity: 1,
                        requests: 1,
                        lock_statistics: true,
                        ..SchedulerConfig::default()
                    }
                )
                .is_err()
            );
            assert_eq!(vm.used(), filled);
            drop(fill);
            assert_eq!(vm.used(), baseline);
        }
        drop(tenant);
        assert_eq!(vm.used(), Resources::default());
    }
}
