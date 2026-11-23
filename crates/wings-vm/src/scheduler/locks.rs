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
    pub worker_cpu: super::SchedulerWorkerCpu,
    pub claim: SchedulerLockMetrics,
    pub start: SchedulerLockMetrics,
    pub snapshot: SchedulerLockMetrics,
    pub finish: SchedulerLockMetrics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerContentionMetrics {
    pub contended_acquisitions: u64,
    pub failed_attempts: u64,
    pub yield_calls: u64,
    pub wait_ns: u64,
    pub max_wait_ns: u64,
    pub saturated: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerContentionStatistics {
    pub fast_slot: SchedulerContentionMetrics,
    pub mailbox: SchedulerContentionMetrics,
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
    pub(super) cpu: super::cpu::WorkerCpuObservation,
    phases: [Counters; 4],
}

#[derive(Default)]
pub(super) struct ContentionCounters {
    acquisitions: AtomicU64,
    failures: AtomicU64,
    yields: AtomicU64,
    wait_ns: AtomicU64,
    max_wait_ns: AtomicU64,
    saturated: AtomicBool,
}

#[derive(Default)]
pub(super) struct LockContentionObservation {
    pub(super) fast_slot: ContentionCounters,
    pub(super) mailbox: ContentionCounters,
}

impl ContentionCounters {
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

    pub(super) fn record_contended(&self, failures: u64, yields: u64, wait: Duration) {
        self.record(failures, yields, wait);
    }

    fn record(&self, failures: u64, yields: u64, wait: Duration) {
        let wait = u64::try_from(wait.as_nanos()).unwrap_or_else(|_| {
            self.saturated.store(true, Ordering::Relaxed);
            u64::MAX
        });
        self.add(&self.acquisitions, 1);
        self.add(&self.failures, failures);
        self.add(&self.yields, yields);
        self.add(&self.wait_ns, wait);
        self.max_wait_ns.fetch_max(wait, Ordering::Relaxed);
    }

    fn snapshot(&self) -> SchedulerContentionMetrics {
        SchedulerContentionMetrics {
            contended_acquisitions: self.acquisitions.load(Ordering::Relaxed),
            failed_attempts: self.failures.load(Ordering::Relaxed),
            yield_calls: self.yields.load(Ordering::Relaxed),
            wait_ns: self.wait_ns.load(Ordering::Relaxed),
            max_wait_ns: self.max_wait_ns.load(Ordering::Relaxed),
            saturated: self.saturated.load(Ordering::Relaxed),
        }
    }
}

impl LockContentionObservation {
    pub(super) fn snapshot(&self) -> SchedulerContentionStatistics {
        SchedulerContentionStatistics {
            fast_slot: self.fast_slot.snapshot(),
            mailbox: self.mailbox.snapshot(),
        }
    }
}

pub(super) fn spin_lock(lock: &AtomicBool, observation: Option<&ContentionCounters>) {
    if observation.is_none() {
        let mut spins = 0u32;
        loop {
            if lock
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
            while lock.load(Ordering::Relaxed) {
                if spins < 16 {
                    std::hint::spin_loop();
                    spins += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        }
    }

    let observation = observation.expect("checked contention observation");
    let mut spins = 0u32;
    let mut failures = 0u64;
    let mut yields = 0u64;
    let mut contended_at = None;
    loop {
        match lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => break,
            Err(locked) => {
                failures = failures.saturating_add(1);
                if locked {
                    contended_at.get_or_insert_with(Instant::now);
                }
                while lock.load(Ordering::Relaxed) {
                    if spins < 16 {
                        std::hint::spin_loop();
                        spins += 1;
                    } else {
                        yields = yields.saturating_add(1);
                        std::thread::yield_now();
                    }
                }
            }
        }
    }
    if let Some(contended_at) = contended_at {
        observation.record(failures, yields, contended_at.elapsed());
    }
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
            worker_cpu: self.cpu.snapshot(),
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
    shared: &'state Shared,
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
                    shared: self,
                    state: Some(state),
                    measurement: Some(Measurement {
                        counters: &observer.phases[phase as usize],
                        wait: acquired.duration_since(requested),
                        acquired,
                    }),
                }
            }
            None => WorkerStateGuard {
                shared: self,
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
        if let Some(state) = self.state.as_deref() {
            self.shared.publish_signal(state);
        }
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
    fn contention_counters_track_failures_yields_wait_and_saturation() {
        let counters = ContentionCounters::default();
        counters.record(3, 1, Duration::from_nanos(7));
        counters.record(5, 2, Duration::from_nanos(11));
        assert_eq!(
            counters.snapshot(),
            SchedulerContentionMetrics {
                contended_acquisitions: 2,
                failed_attempts: 8,
                yield_calls: 3,
                wait_ns: 18,
                max_wait_ns: 11,
                saturated: false,
            }
        );
        counters.failures.store(u64::MAX, Ordering::Relaxed);
        counters.record(1, 0, Duration::ZERO);
        assert!(counters.snapshot().saturated);
    }

    #[test]
    fn uncontended_spin_lock_does_not_report_contention() {
        let lock = AtomicBool::new(false);
        let counters = ContentionCounters::default();
        spin_lock(&lock, Some(&counters));
        lock.store(false, Ordering::Release);
        assert_eq!(counters.snapshot(), SchedulerContentionMetrics::default());
    }

    #[test]
    fn spin_lock_records_a_waiter_after_the_lock_is_released() {
        let lock = AtomicBool::new(true);
        let counters = ContentionCounters::default();
        let (started, receiver) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                started.send(()).unwrap();
                spin_lock(&lock, Some(&counters));
                lock.store(false, Ordering::Release);
            });
            receiver.recv_timeout(Duration::from_secs(1)).unwrap();
            std::thread::sleep(Duration::from_millis(5));
            lock.store(false, Ordering::Release);
            worker.join().unwrap();
        });
        let statistics = counters.snapshot();
        assert_eq!(statistics.contended_acquisitions, 1);
        assert!(statistics.failed_attempts > 0);
        assert!(statistics.max_wait_ns <= statistics.wait_ns);
        assert!(!statistics.saturated);
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
            assert_eq!(handle.contention_statistics().is_some(), enabled);
            if let Some(statistics) = handle.lock_statistics(0).unwrap() {
                assert_eq!(
                    statistics.worker_cpu,
                    super::super::SchedulerWorkerCpu::Pending
                );
            }
            drop(scheduler);
            for worker in 0..2 {
                if let Some(statistics) = handle.lock_statistics(worker).unwrap() {
                    assert!(matches!(
                        statistics.worker_cpu,
                        super::super::SchedulerWorkerCpu::Nanoseconds(_)
                    ));
                }
            }
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
            (2 * std::mem::size_of::<WorkerLockObservation>()
                + std::mem::size_of::<LockContentionObservation>()
                + 2 * std::mem::size_of::<usize>()) as u64
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
