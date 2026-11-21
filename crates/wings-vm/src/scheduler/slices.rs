use super::*;
use crate::ExecutionStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerSliceOutcome {
    Suspended,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerSliceSample {
    pub pid: SchedulerPid,
    pub worker: usize,
    pub sequence: u64,
    pub started_at: Instant,
    pub returned_at: Instant,
    pub outcome: SchedulerSliceOutcome,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerSliceStatistics {
    pub retained: usize,
    pub taken: u64,
    pub dropped: u64,
    pub published: u64,
    pub suspended: u64,
    pub completed: u64,
    pub failed: u64,
    pub overflowed: bool,
}

pub(super) struct WorkerSliceObservation {
    queue: Mutex<Queue>,
    _charge: Reservation,
}

struct Queue {
    slots: Box<[Option<SchedulerSliceSample>]>,
    head: usize,
    statistics: SchedulerSliceStatistics,
}

fn increment(counter: &mut u64, overflowed: &mut bool) {
    if let Some(updated) = counter.checked_add(1) {
        *counter = updated;
    } else {
        *overflowed = true;
    }
}

impl Queue {
    fn record(&mut self, mut sample: SchedulerSliceSample) {
        let statistics = &mut self.statistics;
        increment(&mut statistics.published, &mut statistics.overflowed);
        increment(
            match sample.outcome {
                SchedulerSliceOutcome::Suspended => &mut statistics.suspended,
                SchedulerSliceOutcome::Complete => &mut statistics.completed,
                SchedulerSliceOutcome::Failed => &mut statistics.failed,
            },
            &mut statistics.overflowed,
        );
        sample.sequence = statistics.published;
        if statistics.retained == self.slots.len() {
            increment(&mut statistics.dropped, &mut statistics.overflowed);
        } else {
            let tail = (self.head + statistics.retained) % self.slots.len();
            self.slots[tail] = Some(sample);
            statistics.retained += 1;
        }
    }

    fn take(&mut self) -> Option<SchedulerSliceSample> {
        if self.statistics.retained == 0 {
            return None;
        }
        let sample = self.slots[self.head].take();
        self.head = (self.head + 1) % self.slots.len();
        self.statistics.retained -= 1;
        increment(&mut self.statistics.taken, &mut self.statistics.overflowed);
        sample
    }
}

impl WorkerSliceObservation {
    pub(super) fn new(tenant: &ResourceAccount, capacity: usize) -> Result<Self, String> {
        let charge = tenant.reserve(Resources {
            bytes: (capacity * std::mem::size_of::<Option<SchedulerSliceSample>>()
                + crate::native_sync::MUTEX_BYTES) as u64,
            ..Resources::default()
        })?;
        Ok(Self {
            queue: crate::native_sync::mutex(Queue {
                slots: storage(capacity, || None)?,
                head: 0,
                statistics: SchedulerSliceStatistics::default(),
            }),
            _charge: charge,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn record(
        &self,
        pid: SchedulerPid,
        worker: usize,
        started_at: Instant,
        returned_at: Instant,
        result: &Result<ExecutionStatus, String>,
    ) {
        let outcome = match result {
            Ok(ExecutionStatus::Suspended) => SchedulerSliceOutcome::Suspended,
            Ok(_) => SchedulerSliceOutcome::Complete,
            Err(_) => SchedulerSliceOutcome::Failed,
        };
        self.lock().record(SchedulerSliceSample {
            pid,
            worker,
            sequence: 0,
            started_at,
            returned_at,
            outcome,
        });
    }

    pub(super) fn take(&self) -> Option<SchedulerSliceSample> {
        self.lock().take()
    }

    pub(super) fn statistics(&self) -> SchedulerSliceStatistics {
        self.lock().statistics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(outcome: SchedulerSliceOutcome) -> SchedulerSliceSample {
        let started_at = Instant::now();
        SchedulerSliceSample {
            pid: SchedulerPid {
                scheduler: 7,
                slot: 2,
                generation: 3,
            },
            worker: 1,
            sequence: 0,
            started_at,
            returned_at: started_at,
            outcome,
        }
    }

    #[test]
    fn fixed_fifo_preserves_identity_order_and_result_totals_across_loss_and_reuse() {
        let mut queue = Queue {
            slots: storage(2, || None).unwrap(),
            head: 0,
            statistics: SchedulerSliceStatistics::default(),
        };
        let first = sample(SchedulerSliceOutcome::Suspended);
        queue.record(first);
        queue.record(sample(SchedulerSliceOutcome::Complete));
        queue.record(sample(SchedulerSliceOutcome::Failed));
        let retained = queue.take().unwrap();
        assert_eq!(
            retained,
            SchedulerSliceSample {
                sequence: 1,
                ..first
            }
        );
        queue.record(sample(SchedulerSliceOutcome::Failed));
        assert_eq!(queue.take().unwrap().sequence, 2);
        assert_eq!(queue.take().unwrap().sequence, 4);
        assert!(queue.take().is_none());
        assert_eq!(
            queue.statistics,
            SchedulerSliceStatistics {
                retained: 0,
                taken: 3,
                dropped: 1,
                published: 4,
                suspended: 1,
                completed: 1,
                failed: 2,
                overflowed: false,
            }
        );
    }

    #[test]
    fn every_counter_saturates_and_marks_incomplete_observation() {
        for counter in 0..6 {
            let mut queue = Queue {
                slots: storage(1, || None).unwrap(),
                head: 0,
                statistics: SchedulerSliceStatistics::default(),
            };
            let maximum = match counter {
                0 => &mut queue.statistics.taken,
                1 => &mut queue.statistics.dropped,
                2 => &mut queue.statistics.published,
                3 => &mut queue.statistics.suspended,
                4 => &mut queue.statistics.completed,
                5 => &mut queue.statistics.failed,
                _ => unreachable!(),
            };
            *maximum = u64::MAX;
            queue.record(sample(SchedulerSliceOutcome::Suspended));
            queue.record(sample(SchedulerSliceOutcome::Complete));
            queue.record(sample(SchedulerSliceOutcome::Failed));
            queue.take().unwrap();
            assert!(queue.statistics.overflowed);
            assert_eq!(
                match counter {
                    0 => queue.statistics.taken,
                    1 => queue.statistics.dropped,
                    2 => queue.statistics.published,
                    3 => queue.statistics.suspended,
                    4 => queue.statistics.completed,
                    5 => queue.statistics.failed,
                    _ => unreachable!(),
                },
                u64::MAX
            );
            assert_eq!(
                queue.statistics.published,
                if counter == 2 { u64::MAX } else { 3 }
            );
        }
    }

    fn limits() -> Resources {
        Resources {
            bytes: 64 << 20,
            processes: 4,
            messages: 4,
            requests: 4,
        }
    }

    #[test]
    fn configured_capacity_worker_bounds_and_last_handle_preserve_accounting() {
        for capacity in [0, 1, 65536] {
            let vm = ResourceAccount::vm(limits());
            let tenant = vm.tenant_accounted(limits()).unwrap();
            let baseline = vm.used();
            let scheduler = Scheduler::new(
                &tenant,
                SchedulerConfig {
                    workers: 2,
                    capacity: 1,
                    requests: 1,
                    slice_samples: capacity,
                    ..SchedulerConfig::default()
                },
            )
            .unwrap();
            let handle = scheduler.handle();
            assert_eq!(
                handle.shared.slice_observations.len(),
                if capacity == 0 { 0 } else { 2 }
            );
            for worker in 0..2 {
                assert!(handle.take_slice_sample(worker).unwrap().is_none());
                assert_eq!(
                    handle.slice_statistics(worker).unwrap(),
                    (capacity != 0).then(SchedulerSliceStatistics::default)
                );
            }
            assert!(handle.take_slice_sample(2).is_err());
            assert!(handle.slice_statistics(usize::MAX).is_err());
            drop(scheduler);
            assert!(vm.used().bytes > baseline.bytes);
            let last = handle.clone();
            drop(handle);
            assert!(last.take_slice_sample(0).unwrap().is_none());
            drop(last);
            assert_eq!(vm.used(), baseline);
            for invalid in [65537, usize::MAX] {
                assert!(
                    Scheduler::new(
                        &tenant,
                        SchedulerConfig {
                            slice_samples: invalid,
                            ..SchedulerConfig::default()
                        }
                    )
                    .is_err()
                );
                assert_eq!(vm.used(), baseline);
            }
            drop(tenant);
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn each_parent_rejects_slice_storage_before_allocation_and_rolls_back_startup() {
        for restrict_vm in [false, true] {
            let vm = ResourceAccount::vm(Resources {
                bytes: 128 << 20,
                ..limits()
            });
            let tenant = vm.tenant_accounted(limits()).unwrap();
            let baseline = vm.used();
            let account = if restrict_vm { &vm } else { &tenant };
            let held = account
                .reserve(Resources {
                    bytes: account.limit().bytes - account.used().bytes - 1,
                    ..Resources::default()
                })
                .unwrap();
            let before = vm.used();
            assert!(WorkerSliceObservation::new(&tenant, 1).is_err());
            assert!(
                Scheduler::new(
                    &tenant,
                    SchedulerConfig {
                        slice_samples: 65536,
                        ..SchedulerConfig::default()
                    }
                )
                .is_err()
            );
            assert_eq!(vm.used(), before);
            drop(held);
            assert_eq!(vm.used(), baseline);
            let config = SchedulerConfig {
                workers: 2,
                capacity: 1,
                requests: 1,
                slice_samples: 1,
                ..SchedulerConfig::default()
            };
            let scheduler = Scheduler::new(&tenant, config).unwrap();
            let shared = &scheduler.handle.shared;
            let first_worker_bytes = shared._charge.amount().bytes
                + scheduler._threads_charge.amount().bytes
                + shared.slice_observations[0]._charge.amount().bytes;
            drop(scheduler);
            assert_eq!(vm.used(), baseline);
            let held = account
                .reserve(Resources {
                    bytes: account.limit().bytes - account.used().bytes - first_worker_bytes,
                    ..Resources::default()
                })
                .unwrap();
            let before = vm.used();
            assert!(Scheduler::new(&tenant, config).is_err());
            assert_eq!(vm.used(), before);
            drop(held);
            assert_eq!(vm.used(), baseline);
            drop(tenant);
            assert_eq!(vm.used(), Resources::default());
        }
    }
}
