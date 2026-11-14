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
        self.spawn_configured(module, limits, mailbox, None, policy, None)
    }

    pub fn spawn_with_heap(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        records: usize,
        policy: RestartPolicy,
    ) -> Result<SchedulerChildId, String> {
        self.spawn_configured(module, limits, mailbox, Some(records), policy, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_heap_in_pool(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        records: usize,
        policy: RestartPolicy,
        pool: &StackPool,
    ) -> Result<SchedulerChildId, String> {
        self.spawn_configured(module, limits, mailbox, Some(records), policy, Some(pool))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_configured(
        &mut self,
        module: &ScalarModule,
        limits: Resources,
        mailbox: usize,
        heap: Option<usize>,
        policy: RestartPolicy,
        pool: Option<&StackPool>,
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
            pool,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RequestOutcome;
    use wings_abi::ValueType;

    #[test]
    fn termination_publication_follows_heap_mailbox_and_actor_charge_cleanup() {
        for workers in [1, 2] {
            for terminal in 0..3 {
                for caller in 0..4 {
                    let limits = Resources {
                        bytes: 32 << 20,
                        processes: 2,
                        messages: 4,
                        requests: 1,
                    };
                    let vm = ResourceAccount::vm(limits);
                    let tenant = vm.tenant_accounted(limits).unwrap();
                    let mut supervisor = SchedulerSupervisor::new(
                        &tenant,
                        SchedulerConfig {
                            workers,
                            capacity: 1,
                            requests: 1,
                            ..SchedulerConfig::default()
                        },
                    )
                    .unwrap();
                    let mut module = super::super::tests::module();
                    Arc::get_mut(&mut module.inner).unwrap().manifest.handlers[1].parameters =
                        vec![("payload".into(), ValueType::BytesRef)];
                    let child = supervisor
                        .spawn_with_heap(&module, limits, 2, 16, RestartPolicy::Never)
                        .unwrap();
                    let pid = supervisor.current_pid(child).unwrap();
                    let ticket = supervisor.children[0]
                        .child
                        .as_ref()
                        .unwrap()
                        .ticket
                        .clone();
                    let handle = supervisor.handle();
                    let mut state = handle.shared.lock();
                    let actor = state.actor_mut(handle.shared.identity, pid).unwrap();
                    let account = actor.account.clone();
                    let mut bytes = BytePayload::new(&account, 1, 4096).unwrap();
                    bytes.data.resize(4096, 17);
                    let store = module.handler("Store").unwrap();
                    let arguments = [ListPayload::encode(0, 4096)];
                    validate(
                        actor,
                        &store,
                        &arguments,
                        false,
                        0,
                        &ListPayload::default(),
                        &bytes,
                    )
                    .unwrap();
                    let payload = envelope(
                        actor,
                        &store,
                        &arguments,
                        32,
                        0,
                        ListPayload::default(),
                        bytes,
                        false,
                        Reply::None,
                    )
                    .unwrap();
                    actor.mailbox.push(payload).unwrap();
                    let deadline = if caller == 2 {
                        Instant::now()
                    } else {
                        Instant::now() + Duration::from_secs(60)
                    };
                    let (request, publisher) = handle
                        .shared
                        .replies
                        .start(deadline, &account, ValueType::Int)
                        .unwrap();
                    let mut request = Some(request);
                    if caller == 1 {
                        request.as_ref().unwrap().cancel().unwrap();
                    }
                    if caller == 1 || caller == 2 {
                        assert_eq!(
                            request.as_mut().unwrap().try_take().unwrap(),
                            Some(if caller == 1 {
                                RequestOutcome::Cancelled
                            } else {
                                RequestOutcome::TimedOut
                            })
                        );
                    }
                    if caller == 3 {
                        drop(request.take());
                    }
                    let message = envelope(
                        actor,
                        &module.handler("Read").unwrap(),
                        &[],
                        32,
                        0,
                        ListPayload::default(),
                        BytePayload::default(),
                        false,
                        Reply::External(publisher),
                    )
                    .unwrap();
                    actor.mailbox.push(message).unwrap();
                    let termination = match terminal {
                        0 => SchedulerTermination::Stopped,
                        1 => SchedulerTermination::Closed,
                        _ => SchedulerTermination::Failed(SchedulerExitSnapshot {
                            pid,
                            worker: 0,
                            reason: SchedulerExitReason::ExecutionControl,
                            execution: None,
                            state_type: ValueType::Int,
                            committed_state: Some(0),
                            byte_state: None,
                            queued: 2,
                            resources_before_cleanup: account.used(),
                            handler: [0; 256],
                            handler_len: 0,
                        }),
                    };
                    actor.termination.as_mut().unwrap().set(termination);
                    assert_eq!(account.used().processes, 1);
                    assert_eq!(account.used().messages, 2);
                    assert_eq!(account.used().requests, 1);
                    state.enqueue(pid);
                    let retired = state.retire(handle.shared.identity, pid).unwrap();
                    drop(state);
                    std::thread::scope(|scope| {
                        let outcome = ticket.outcome.lock().unwrap();
                        assert!(!outcome.published);
                        let cleanup = scope.spawn(move || drop(retired));
                        let end = Instant::now() + Duration::from_secs(3);
                        while account.used().processes != 0 {
                            assert!(Instant::now() < end, "termination blocked actor cleanup");
                            std::thread::yield_now();
                        }
                        assert_eq!(
                            account.used(),
                            Resources {
                                requests: u64::from(caller == 0),
                                ..Resources::default()
                            }
                        );
                        if let Some(request) = request.as_mut() {
                            if caller == 0 {
                                assert_eq!(
                                    request.try_take().unwrap(),
                                    Some(RequestOutcome::Failed)
                                );
                            }
                            assert!(request.try_take().is_err());
                        }
                        assert_eq!(account.used(), Resources::default());
                        assert!(!outcome.published);
                        drop(outcome);
                        cleanup.join().unwrap();
                    });
                    let outcome = ticket.outcome.lock().unwrap();
                    assert!(outcome.published);
                    assert_eq!(outcome.termination, termination);
                    drop(outcome);
                    let event = supervisor.poll().unwrap();
                    assert_eq!(event.child, child);
                    assert_eq!(event.pid, pid);
                    assert_eq!(event.termination, termination);
                    assert_eq!(
                        event.restart,
                        match termination {
                            SchedulerTermination::Stopped => SchedulerRestartStatus::Stopped,
                            SchedulerTermination::Closed => SchedulerRestartStatus::Closed,
                            SchedulerTermination::Failed(_) => SchedulerRestartStatus::Disabled,
                        }
                    );
                    assert!(supervisor.poll().is_none());
                    drop((
                        request, account, ticket, handle, supervisor, store, module, tenant,
                    ));
                    assert_eq!(vm.used(), Resources::default());
                }
            }
        }
    }
}
