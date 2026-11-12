use std::time::{Duration, Instant};

use crate::{
    HandlerId, Pid, RequestId, RequestOutcome, Reservation, ResourceAccount, ResourceLevel,
    Resources, ScalarModule, ScalarWorker, WorkerEvent,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildId(Pid);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    OnFailure {
        max_restarts: usize,
        within: Duration,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum RestartStatus {
    Restarted(Pid),
    Disabled,
    IntensityExceeded,
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SupervisorEvent {
    Worker(WorkerEvent),
    Exited {
        child: ChildId,
        pid: Pid,
        reason: String,
        restart: RestartStatus,
    },
}

pub struct ScalarSupervisor {
    worker: ScalarWorker,
    children: Box<[Option<Child>]>,
    tenant: ResourceAccount,
    _charge: Reservation,
}

struct Child {
    identity: ChildId,
    pid: Pid,
    module: ScalarModule,
    limits: Resources,
    mailbox_capacity: usize,
    history: RestartHistory,
    _charge: Reservation,
}

struct RestartHistory {
    times: Box<[Option<Instant>]>,
    within: Duration,
    head: usize,
    count: usize,
}

impl RestartHistory {
    fn capacity(policy: RestartPolicy) -> Result<(usize, Duration), String> {
        match policy {
            RestartPolicy::Never => Ok((0, Duration::ZERO)),
            RestartPolicy::OnFailure {
                max_restarts,
                within,
            } if (1..=64).contains(&max_restarts)
                && !within.is_zero()
                && Instant::now().checked_add(within).is_some() =>
            {
                Ok((max_restarts, within))
            }
            _ => Err("restart policy requires 1..=64 restarts and a finite nonzero window".into()),
        }
    }

    fn admit(&mut self, now: Instant) -> bool {
        while self.count > 0 {
            let oldest = self.times[self.head].expect("occupied restart history");
            if now.saturating_duration_since(oldest) < self.within {
                break;
            }
            self.times[self.head] = None;
            self.head = (self.head + 1) % self.times.len();
            self.count -= 1;
        }
        self.count < self.times.len()
    }

    fn record(&mut self, now: Instant) {
        let tail = (self.head + self.count) % self.times.len();
        self.times[tail] = Some(now);
        self.count += 1;
    }
}

impl ScalarSupervisor {
    pub fn new(
        tenant: &ResourceAccount,
        capacity: usize,
        request_capacity: usize,
    ) -> Result<Self, String> {
        if tenant.level() != ResourceLevel::Tenant || !(1..=256).contains(&capacity) {
            return Err("supervisor requires a tenant and 1..=256 child slots".into());
        }
        let bytes = std::mem::size_of::<Self>() + capacity * std::mem::size_of::<Option<Child>>();
        let charge = tenant.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let children = storage(capacity)?;
        let worker = ScalarWorker::with_request_capacity(tenant, capacity, request_capacity)?;
        Ok(Self {
            worker,
            children,
            tenant: tenant.clone(),
            _charge: charge,
        })
    }

    pub fn spawn(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox_capacity: usize,
        policy: RestartPolicy,
    ) -> Result<ChildId, String> {
        let (capacity, within) = RestartHistory::capacity(policy)?;
        let index = self
            .children
            .iter()
            .position(Option::is_none)
            .ok_or("supervisor child capacity exhausted")?;
        let charge = self.tenant.reserve(Resources {
            bytes: (capacity * std::mem::size_of::<Option<Instant>>()) as u64,
            ..Resources::default()
        })?;
        let history = RestartHistory {
            times: storage(capacity)?,
            within,
            head: 0,
            count: 0,
        };
        let pid = self.worker.spawn(module, limits, mailbox_capacity)?;
        let identity = ChildId(pid);
        self.children[index] = Some(Child {
            identity,
            pid,
            module: module.clone(),
            limits,
            mailbox_capacity,
            history,
            _charge: charge,
        });
        Ok(identity)
    }

    pub fn current_pid(&self, child: ChildId) -> Result<Pid, String> {
        self.children
            .iter()
            .flatten()
            .find(|entry| entry.identity == child)
            .map(|entry| entry.pid)
            .ok_or_else(|| "stale or foreign child ID".into())
    }

    pub fn send(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
    ) -> Result<(), String> {
        self.worker.send(pid, handler, arguments, max_steps)
    }

    pub fn request(
        &mut self,
        pid: Pid,
        handler: &HandlerId,
        arguments: &[i64],
        max_steps: u64,
        timeout: Duration,
    ) -> Result<RequestId, String> {
        self.worker
            .request(pid, handler, arguments, max_steps, timeout)
    }

    pub fn take_reply(&mut self, request: RequestId) -> Result<Option<RequestOutcome>, String> {
        self.worker.take_reply(request)
    }

    pub fn cancel_request(&mut self, request: RequestId) -> Result<(), String> {
        self.worker.cancel_request(request)
    }

    pub fn enable_timer(&mut self) -> Result<(), String> {
        self.worker.enable_timer()
    }

    pub fn tick(&mut self, slice_steps: u64) -> Result<SupervisorEvent, String> {
        let event = self.worker.tick(slice_steps)?;
        Ok(self.handle(event))
    }

    pub fn tick_timed(
        &mut self,
        slice_steps: u64,
        duration: Duration,
    ) -> Result<SupervisorEvent, String> {
        let event = self.worker.tick_timed(slice_steps, duration)?;
        Ok(self.handle(event))
    }

    pub fn stop(&mut self, child: ChildId) -> Result<(), String> {
        let pid = self.current_pid(child)?;
        self.worker.stop(pid)?;
        let index = self
            .children
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.identity == child))
            .expect("validated supervised child");
        self.children[index].take();
        Ok(())
    }

    pub fn queued(&self, pid: Pid) -> Result<usize, String> {
        self.worker.queued(pid)
    }

    pub fn resources(&self, pid: Pid) -> Result<Resources, String> {
        self.worker.resources(pid)
    }

    pub fn committed_state(&self, pid: Pid) -> Result<i64, String> {
        self.worker.committed_state(pid)
    }

    pub fn process_count(&self) -> usize {
        self.worker.process_count()
    }

    fn handle(&mut self, event: WorkerEvent) -> SupervisorEvent {
        let WorkerEvent::Exited { pid, reason } = event else {
            return SupervisorEvent::Worker(event);
        };
        let index = self
            .children
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.pid == pid))
            .expect("worker only owns supervised children");
        let mut child = self.children[index].take().expect("exited child");
        let identity = child.identity;
        let now = Instant::now();
        let restart = if child.history.times.is_empty() {
            RestartStatus::Disabled
        } else if !child.history.admit(now) {
            RestartStatus::IntensityExceeded
        } else {
            match self
                .worker
                .spawn(&child.module, child.limits, child.mailbox_capacity)
            {
                Ok(replacement) => {
                    child.pid = replacement;
                    child.history.record(Instant::now());
                    self.children[index] = Some(child);
                    RestartStatus::Restarted(replacement)
                }
                Err(error) => RestartStatus::Failed(error),
            }
        };
        SupervisorEvent::Exited {
            child: identity,
            pid,
            reason,
            restart,
        }
    }
}

fn storage<Value>(capacity: usize) -> Result<Box<[Option<Value>]>, String> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|error| error.to_string())?;
    if values.capacity() != capacity {
        return Err("allocator changed requested supervision capacity".into());
    }
    values.resize_with(capacity, || None);
    Ok(values.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_window_expires_at_exact_boundary_without_growing() {
        let mut history = RestartHistory {
            times: storage(2).unwrap(),
            within: Duration::from_secs(10),
            head: 0,
            count: 0,
        };
        let start = Instant::now();
        assert!(history.admit(start));
        history.record(start);
        assert!(history.admit(start));
        history.record(start + Duration::from_secs(1));
        assert!(!history.admit(start + Duration::from_secs(9)));
        assert!(history.admit(start + Duration::from_secs(10)));
        history.record(start + Duration::from_secs(10));
        assert!(!history.admit(start + Duration::from_secs(10)));
        assert!(history.admit(start + Duration::from_secs(11)));
        history.record(start + Duration::from_secs(11));
        assert_eq!(history.count, 2);
        assert_eq!(history.times.len(), 2);
        assert!(history.admit(start + Duration::from_secs(30)));
        assert_eq!(history.count, 0);
    }
}
