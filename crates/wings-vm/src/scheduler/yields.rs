use super::*;
use crate::{PreemptionObservation, PreemptionToken, YieldObservation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerYieldSample {
    pub pid: SchedulerPid,
    pub worker: usize,
    pub sequence: u64,
    pub observation: YieldObservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerYieldStatistics {
    pub preemption: PreemptionObservation,
    pub retained: usize,
    pub taken: u64,
    pub dropped: u64,
    pub published: u64,
    pub overflowed: bool,
}

pub(super) struct WorkerObservation {
    pub token: PreemptionToken,
    queue: Mutex<Queue>,
    _charge: Reservation,
}

struct Queue {
    slots: Box<[Option<SchedulerYieldSample>]>,
    head: usize,
    count: usize,
    taken: u64,
    dropped: u64,
    published: u64,
    overflowed: bool,
}

impl Queue {
    fn record(&mut self, sample: SchedulerYieldSample) {
        self.published = sample.sequence;
        if self.count == self.slots.len() {
            self.overflowed |= self.dropped == u64::MAX;
            self.dropped = self.dropped.saturating_add(1);
        } else {
            let tail = (self.head + self.count) % self.slots.len();
            self.slots[tail] = Some(sample);
            self.count += 1;
        }
    }

    fn take(&mut self) -> Option<SchedulerYieldSample> {
        if self.count == 0 {
            return None;
        }
        let sample = self.slots[self.head].take();
        self.head = (self.head + 1) % self.slots.len();
        self.count -= 1;
        self.overflowed |= self.taken == u64::MAX;
        self.taken = self.taken.saturating_add(1);
        sample
    }
}

impl WorkerObservation {
    pub(super) fn new(tenant: &ResourceAccount, capacity: usize) -> Result<Self, String> {
        let charge = tenant.reserve(Resources {
            bytes: (capacity * std::mem::size_of::<Option<SchedulerYieldSample>>() + 64) as u64,
            ..Resources::default()
        })?;
        Ok(Self {
            token: PreemptionToken::new_observed(tenant)?,
            queue: crate::native_sync::mutex(Queue {
                slots: storage(capacity, || None)?,
                head: 0,
                count: 0,
                taken: 0,
                dropped: 0,
                published: 0,
                overflowed: false,
            }),
            _charge: charge,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn record(&self, pid: SchedulerPid, worker: usize) {
        let state = self.token.observation().expect("enabled observer");
        let mut queue = self.lock();
        if state.counters.confirmed != queue.published {
            queue.record(SchedulerYieldSample {
                pid,
                worker,
                sequence: state.counters.confirmed,
                observation: state.last_yield.expect("confirmed yield"),
            });
        }
        queue.overflowed |= state.counters.overflowed;
    }

    pub(super) fn take(&self) -> Option<SchedulerYieldSample> {
        self.lock().take()
    }

    pub(super) fn statistics(&self) -> SchedulerYieldStatistics {
        let preemption = self.token.observation().expect("enabled observer");
        let queue = self.lock();
        SchedulerYieldStatistics {
            preemption,
            retained: queue.count,
            taken: queue.taken,
            dropped: queue.dropped,
            published: queue.published,
            overflowed: queue.overflowed || preemption.counters.overflowed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(sequence: u64) -> SchedulerYieldSample {
        let requested_at = Instant::now();
        SchedulerYieldSample {
            pid: SchedulerPid {
                scheduler: 7,
                slot: 2,
                generation: 3,
            },
            worker: 1,
            sequence,
            observation: YieldObservation {
                requested_at,
                yielded_at: requested_at,
            },
        }
    }

    #[test]
    fn fixed_fifo_refuses_new_samples_and_reuses_slots_without_overwriting() {
        let mut queue = Queue {
            slots: storage(2, || None).unwrap(),
            head: 0,
            count: 0,
            taken: 0,
            dropped: 0,
            published: 0,
            overflowed: false,
        };
        assert!(queue.take().is_none());
        queue.record(sample(1));
        queue.record(sample(2));
        queue.record(sample(3));
        assert_eq!(queue.dropped, 1);
        assert_eq!(queue.take().unwrap().sequence, 1);
        queue.record(sample(4));
        assert_eq!(queue.take().unwrap().sequence, 2);
        assert_eq!(queue.take().unwrap().sequence, 4);
        assert!(queue.take().is_none());
        assert_eq!(
            queue.published,
            queue.taken + queue.dropped + queue.count as u64
        );
        assert!(!queue.overflowed);
        queue.dropped = u64::MAX;
        queue.record(sample(5));
        queue.record(sample(6));
        queue.record(sample(7));
        assert_eq!(queue.dropped, u64::MAX);
        assert!(queue.overflowed);
        queue.overflowed = false;
        queue.taken = u64::MAX;
        assert!(queue.take().is_some());
        assert_eq!(queue.taken, u64::MAX);
        assert!(queue.overflowed);
    }

    #[test]
    fn repeated_publication_does_not_duplicate_the_last_confirmation() {
        let vm = ResourceAccount::vm(Resources {
            bytes: 32768,
            ..Resources::default()
        });
        let observer = WorkerObservation::new(&vm, 1).unwrap();
        let pid = sample(1).pid;
        observer.record(pid, 0);
        assert!(observer.take().is_none());
        for _ in 0..2 {
            observer.token.request();
            let requested = observer.token.take_request().unwrap();
            observer.token.confirm_yield(requested);
            observer.record(pid, 0);
            observer.record(pid, 0);
        }
        let statistics = observer.statistics();
        assert_eq!(statistics.published, 2);
        assert_eq!(statistics.retained, 1);
        assert_eq!(statistics.dropped, 1);
        assert_eq!(observer.take().unwrap().sequence, 1);
        assert!(observer.take().is_none());
        assert_eq!(observer.statistics().taken, 1);
        drop(observer);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn observer_token_failure_rolls_back_the_queue_reservation() {
        let vm = ResourceAccount::vm(Resources {
            bytes: 4096,
            ..Resources::default()
        });
        assert!(WorkerObservation::new(&vm, 1).is_err());
        assert_eq!(vm.used(), Resources::default());
    }
}
