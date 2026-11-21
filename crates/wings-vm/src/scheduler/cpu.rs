use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchedulerWorkerCpu {
    #[default]
    Pending,
    Unavailable,
    Nanoseconds(u64),
}

#[derive(Default)]
pub(super) struct WorkerCpuObservation {
    state: AtomicU64,
}

pub(super) struct WorkerCpuGuard<'observer> {
    observation: &'observer WorkerCpuObservation,
    started: Option<u64>,
    thread: PhantomData<Rc<()>>,
}

impl WorkerCpuObservation {
    pub(super) fn begin(&self) -> WorkerCpuGuard<'_> {
        WorkerCpuGuard {
            observation: self,
            started: thread_clock(),
            thread: PhantomData,
        }
    }

    fn finish(&self, started: Option<u64>, finished: Option<u64>) {
        let value = started
            .zip(finished)
            .and_then(|(started, finished)| finished.checked_sub(started))
            .and_then(|elapsed| elapsed.checked_add(2))
            .unwrap_or(1);
        self.state.store(value, Ordering::Release);
    }

    pub(super) fn snapshot(&self) -> SchedulerWorkerCpu {
        match self.state.load(Ordering::Acquire) {
            0 => SchedulerWorkerCpu::Pending,
            1 => SchedulerWorkerCpu::Unavailable,
            value => SchedulerWorkerCpu::Nanoseconds(value - 2),
        }
    }
}

impl Drop for WorkerCpuGuard<'_> {
    fn drop(&mut self) {
        self.observation.finish(self.started, thread_clock());
    }
}

fn timestamp(value: libc::timespec) -> Option<u64> {
    if !(0..1_000_000_000).contains(&value.tv_nsec) {
        return None;
    }
    u64::try_from(value.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(value.tv_nsec).ok()?)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn thread_clock() -> Option<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut value) } != 0 {
        return None;
    }
    timestamp(value)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn thread_clock() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_native_times_and_overflow_are_unavailable() {
        for value in [
            libc::timespec {
                tv_sec: -1,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: 0,
                tv_nsec: -1,
            },
            libc::timespec {
                tv_sec: 0,
                tv_nsec: 1_000_000_000,
            },
            libc::timespec {
                tv_sec: i64::MAX,
                tv_nsec: 0,
            },
        ] {
            assert_eq!(timestamp(value), None);
        }
        assert_eq!(
            timestamp(libc::timespec {
                tv_sec: 1,
                tv_nsec: 23
            }),
            Some(1_000_000_023)
        );
    }

    #[test]
    fn pending_failed_backwards_and_zero_intervals_are_distinct() {
        for (started, finished, expected) in [
            (None, Some(1), SchedulerWorkerCpu::Unavailable),
            (Some(1), None, SchedulerWorkerCpu::Unavailable),
            (Some(2), Some(1), SchedulerWorkerCpu::Unavailable),
            (Some(0), Some(u64::MAX), SchedulerWorkerCpu::Unavailable),
            (Some(0), Some(u64::MAX - 1), SchedulerWorkerCpu::Unavailable),
            (Some(3), Some(3), SchedulerWorkerCpu::Nanoseconds(0)),
            (Some(3), Some(9), SchedulerWorkerCpu::Nanoseconds(6)),
            (
                Some(0),
                Some(u64::MAX - 2),
                SchedulerWorkerCpu::Nanoseconds(u64::MAX - 2),
            ),
        ] {
            let observation = WorkerCpuObservation::default();
            assert_eq!(observation.snapshot(), SchedulerWorkerCpu::Pending);
            observation.finish(started, finished);
            assert_eq!(observation.snapshot(), expected);
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn native_thread_span_is_published_after_the_guard_finishes() {
        let observation = WorkerCpuObservation::default();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let guard = observation.begin();
                    assert_eq!(observation.snapshot(), SchedulerWorkerCpu::Pending);
                    std::hint::black_box((0..1000u64).sum::<u64>());
                    drop(guard);
                })
                .join()
                .unwrap();
        });
        assert!(matches!(
            observation.snapshot(),
            SchedulerWorkerCpu::Nanoseconds(_)
        ));
    }
}
