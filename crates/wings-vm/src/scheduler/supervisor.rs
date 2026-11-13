use super::*;
use crate::RestartPolicy;
use crate::supervision::RestartHistory;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerChildId(SchedulerPid, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum SchedulerTermination {
    Failed(SchedulerExitSnapshot),
    Stopped,
    Closed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SchedulerRestartStatus {
    Restarted(SchedulerPid),
    Disabled,
    IntensityExceeded,
    Failed(String),
    Stopped,
    Closed,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SchedulerSupervisorEvent {
    pub child: SchedulerChildId,
    pub pid: SchedulerPid,
    pub termination: SchedulerTermination,
    pub restart: SchedulerRestartStatus,
}

struct ExitTicket {
    outcome: Mutex<ExitState>,
    _charge: Reservation,
}

struct ExitState {
    published: bool,
    termination: SchedulerTermination,
}

pub(super) struct ExitPublisher {
    ticket: Arc<ExitTicket>,
}

impl ExitPublisher {
    fn new(tenant: &ResourceAccount) -> Result<Self, String> {
        let charge = tenant.reserve(Resources {
            bytes: (std::mem::size_of::<ExitTicket>() + 64) as u64,
            ..Resources::default()
        })?;
        Ok(Self {
            ticket: Arc::new(ExitTicket {
                outcome: Mutex::new(ExitState {
                    published: false,
                    termination: SchedulerTermination::Stopped,
                }),
                _charge: charge,
            }),
        })
    }

    pub(super) fn set(&mut self, outcome: SchedulerTermination) {
        self.ticket
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .termination = outcome;
    }
}

impl Drop for ExitPublisher {
    fn drop(&mut self) {
        let mut outcome = self
            .ticket
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(!outcome.published, "single termination publisher");
        outcome.published = true;
    }
}

pub struct SchedulerSupervisor {
    scheduler: Scheduler,
    children: Box<[ChildSlot]>,
    free: Option<usize>,
    cursor: usize,
    tenant: ResourceAccount,
    _charge: Reservation,
}

struct ChildSlot {
    child: Option<Child>,
    next: Option<usize>,
}

struct Child {
    identity: SchedulerChildId,
    pid: SchedulerPid,
    module: ScalarModule,
    limits: Resources,
    mailbox: usize,
    heap: Option<usize>,
    ticket: Arc<ExitTicket>,
    history: RestartHistory,
    _charge: Reservation,
}

impl SchedulerSupervisor {
    pub fn new(tenant: &ResourceAccount, config: SchedulerConfig) -> Result<Self, String> {
        if tenant.level() != ResourceLevel::Tenant || !(1..=MAX_ACTORS).contains(&config.capacity) {
            return Err("scheduler supervisor requires tenant and 1..=16384 child slots".into());
        }
        let charge = tenant.reserve(Resources {
            bytes: (std::mem::size_of::<Self>()
                + config.capacity * std::mem::size_of::<ChildSlot>()) as u64,
            ..Resources::default()
        })?;
        let mut next_slot = 0;
        let children = storage(config.capacity, || {
            next_slot += 1;
            ChildSlot {
                child: None,
                next: (next_slot < config.capacity).then_some(next_slot),
            }
        })?;
        let scheduler = Scheduler::new(tenant, config)?;
        Ok(Self {
            scheduler,
            children,
            free: Some(0),
            cursor: 0,
            tenant: tenant.clone(),
            _charge: charge,
        })
    }

    pub fn handle(&self) -> SchedulerHandle {
        self.scheduler.handle()
    }

    pub fn spawn(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        policy: RestartPolicy,
    ) -> Result<SchedulerChildId, String> {
        self.spawn_configured(module, limits, mailbox, None, policy)
    }

    pub fn spawn_with_heap(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        records: usize,
        policy: RestartPolicy,
    ) -> Result<SchedulerChildId, String> {
        self.spawn_configured(module, limits, mailbox, Some(records), policy)
    }

    fn spawn_configured(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        heap: Option<usize>,
        policy: RestartPolicy,
    ) -> Result<SchedulerChildId, String> {
        let index = self.free.ok_or("supervisor child capacity exhausted")?;
        let (capacity, _) = RestartHistory::capacity(policy)?;
        let charge = self.tenant.reserve(Resources {
            bytes: (capacity * std::mem::size_of::<Option<Instant>>()) as u64,
            ..Resources::default()
        })?;
        let history = RestartHistory::new(policy)?;
        let publisher = ExitPublisher::new(&self.tenant)?;
        let ticket = publisher.ticket.clone();
        let pid = self.scheduler.handle.spawn_with_options(
            module,
            limits,
            mailbox,
            heap,
            None,
            Some(publisher),
        )?;
        let identity = SchedulerChildId(pid, index as u32);
        self.free = self.children[index].next.take();
        self.children[index].child = Some(Child {
            identity,
            pid,
            module: module.clone(),
            limits,
            mailbox,
            heap,
            ticket,
            history,
            _charge: charge,
        });
        Ok(identity)
    }

    pub fn current_pid(&self, child: SchedulerChildId) -> Result<SchedulerPid, String> {
        let index = self.child_index(child)?;
        Ok(self.children[index]
            .child
            .as_ref()
            .expect("validated child")
            .pid)
    }

    pub fn stop(&mut self, child: SchedulerChildId) -> Result<(), String> {
        let index = self.child_index(child)?;
        let pid = self.children[index]
            .child
            .as_ref()
            .expect("validated child")
            .pid;
        match self.scheduler.handle.stop(pid) {
            Ok(()) => {}
            Err(error) => {
                let entry = self.children[index]
                    .child
                    .as_ref()
                    .expect("validated child");
                if !entry
                    .ticket
                    .outcome
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .published
                {
                    return Err(error);
                }
            }
        }
        self.children[index].child.take();
        self.release_slot(index);
        Ok(())
    }

    fn child_index(&self, child: SchedulerChildId) -> Result<usize, String> {
        let index = child.1 as usize;
        self.children
            .get(index)
            .and_then(|slot| slot.child.as_ref())
            .filter(|entry| entry.identity == child)
            .map(|_| index)
            .ok_or_else(|| "stale or foreign scheduler child ID".into())
    }

    fn release_slot(&mut self, index: usize) {
        assert!(self.children[index].child.is_none());
        self.children[index].next = self.free;
        self.free = Some(index);
    }

    pub fn poll(&mut self) -> Option<SchedulerSupervisorEvent> {
        for _ in 0..self.children.len() {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.children.len();
            let Some(child) = self.children[index].child.as_ref() else {
                continue;
            };
            let outcome = child
                .ticket
                .outcome
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !outcome.published {
                continue;
            }
            let termination = outcome.termination;
            drop(outcome);
            let mut child = self.children[index].child.take().expect("terminal child");
            let identity = child.identity;
            let pid = child.pid;
            let restart = match termination {
                SchedulerTermination::Stopped => SchedulerRestartStatus::Stopped,
                SchedulerTermination::Closed => SchedulerRestartStatus::Closed,
                SchedulerTermination::Failed(_) if !child.history.enabled() => {
                    SchedulerRestartStatus::Disabled
                }
                SchedulerTermination::Failed(_) if !child.history.admit(Instant::now()) => {
                    SchedulerRestartStatus::IntensityExceeded
                }
                SchedulerTermination::Failed(_) => {
                    let replacement = ExitPublisher::new(&self.tenant).and_then(|publisher| {
                        let ticket = publisher.ticket.clone();
                        self.scheduler
                            .handle
                            .spawn_with_options(
                                &child.module,
                                child.limits,
                                child.mailbox,
                                child.heap,
                                None,
                                Some(publisher),
                            )
                            .map(|pid| (pid, ticket))
                    });
                    match replacement {
                        Ok((pid, ticket)) => {
                            child.pid = pid;
                            child.ticket = ticket;
                            child.history.record(Instant::now());
                            self.children[index].child = Some(child);
                            SchedulerRestartStatus::Restarted(pid)
                        }
                        Err(error) => SchedulerRestartStatus::Failed(error),
                    }
                }
            };
            if self.children[index].child.is_none() {
                self.release_slot(index);
            }
            return Some(SchedulerSupervisorEvent {
                child: identity,
                pid,
                termination,
                restart,
            });
        }
        None
    }
}
