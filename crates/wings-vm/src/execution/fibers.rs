use super::*;

pub(super) struct FiberStart {
    entry: ScalarEntry,
    state: i64,
    arguments: *const i64,
    argument_count: usize,
    max_steps: u64,
    slice_remaining: Option<u64>,
    cancellation: *const std::sync::atomic::AtomicU32,
    heap: Option<Box<AotHeap>>,
    effects_enabled: bool,
    inline: Option<crate::scheduler::EffectSendTarget>,
}

pub(super) enum FiberCommand {
    Start(FiberStart),
    Run(u64),
    Effect(u32, i64, u64),
    Cancel,
    Shutdown,
}

pub(super) enum FiberEvent {
    Primed,
    Suspended(Checkpoint),
    Complete {
        status: u32,
        output: ScalarOutput,
        heap: Option<Box<AotHeap>>,
        checkpoint: Checkpoint,
    },
}

pub(super) struct ReusableControl<'scope> {
    pub(super) yielder: &'scope Yielder<FiberCommand, FiberEvent>,
    pub(super) preemption: &'scope PreemptionToken,
    /// Lock-free send context for `EFFECT_SELF_PID` / `EFFECT_SEND_SELF` /
    /// `EFFECT_SEND`; `None` (e.g. outside the scheduler) keeps the
    /// suspend-to-scheduler path. The borrow lives in the coroutine frame, so
    /// it stays valid across suspensions of this invocation.
    pub(super) inline: Option<&'scope crate::scheduler::EffectSendTarget>,
    /// `max_steps` of the current invocation; inline sends charge it to the
    /// delivered envelope exactly like the suspended effect path.
    pub(super) budget: u64,
}

pub(super) struct ParkedFiber {
    coroutine: Option<ReusableCoroutine>,
    stack_charge: Option<Reservation>,
    pool: StackPool,
}

impl Drop for ParkedFiber {
    fn drop(&mut self) {
        if let Some(mut coroutine) = self.coroutine.take() {
            if coroutine.started()
                && !coroutine.done()
                && !matches!(
                    coroutine.resume(FiberCommand::Shutdown),
                    CoroutineResult::Return(())
                )
            {
                std::process::abort();
            }
            self.pool.drop_parked_fiber();
        }
    }
}

pub(super) struct ActiveReusableFiber {
    pub(super) coroutine: Option<ReusableCoroutine>,
    pub(super) stack_charge: Option<Reservation>,
    pub(super) pool: StackPool,
    pub(super) _cancellation: Option<CancellationToken>,
}

impl Drop for ActiveReusableFiber {
    fn drop(&mut self) {
        if let Some(mut coroutine) = self.coroutine.take() {
            if self._cancellation.take().is_some()
                && coroutine.started()
                && !coroutine.done()
                && !matches!(
                    coroutine.resume(FiberCommand::Cancel),
                    CoroutineResult::Yield(FiberEvent::Complete { .. })
                )
            {
                std::process::abort();
            }
            if coroutine.started()
                && !coroutine.done()
                && !matches!(
                    coroutine.resume(FiberCommand::Shutdown),
                    CoroutineResult::Return(())
                )
            {
                std::process::abort();
            }
            self.pool.drop_active_fiber();
        }
    }
}

pub(crate) struct WorkerFibers {
    pool: StackPool,
    preemption: PreemptionToken,
    slots: [Option<ParkedFiber>; 2],
    limit: usize,
    native_stack_limit: usize,
}

enum DirectFiberLease {
    Parked(ParkedFiber),
    Demand {
        pool: StackPool,
        lease: crate::stack_pool::DirectStackLease,
    },
}

pub(crate) struct PreparedDirect {
    stack_lease: Option<DirectFiberLease>,
    _control_charge: Option<Reservation>,
    native_stack_limit: usize,
}

pub(crate) struct PreparedFiber {
    active: ActiveReusableFiber,
    control_charge: Reservation,
}

impl WorkerFibers {
    pub(crate) fn new(
        pool: &StackPool,
        preemption: &PreemptionToken,
        native_stack_limit: usize,
    ) -> Self {
        Self {
            pool: pool.clone(),
            preemption: preemption.clone(),
            slots: [None, None],
            limit: pool.max_cached_slots().min(2),
            native_stack_limit,
        }
    }

    pub(crate) fn can_run_direct(
        &self,
        process: &ScalarProcess,
        handler_index: usize,
        max_steps: u64,
        slice_steps: u64,
        yield_observed: bool,
    ) -> bool {
        !yield_observed
            && self.native_stack_limit != 0
            && self.preemption.flag.load(Ordering::Acquire) == 0
            && process
                .module
                .direct_max_steps(handler_index, process.heap.as_deref())
                .is_some_and(|bound| bound <= max_steps && bound <= slice_steps)
    }

    pub(crate) fn can_run_direct_image_index(
        &self,
        module: &ScalarModule,
        image: &crate::DetachedProcess,
        handler_index: usize,
        max_steps: u64,
        slice_steps: u64,
        yield_observed: bool,
    ) -> bool {
        !yield_observed
            && !image.is_failed()
            && self.native_stack_limit != 0
            && self.preemption.flag.load(Ordering::Acquire) == 0
            && module
                .direct_max_steps(handler_index, image.heap_ref())
                .is_some_and(|bound| bound <= max_steps && bound <= slice_steps)
    }

    pub(crate) fn prepare_direct_account(
        &mut self,
        failed: bool,
        process_account: &ResourceAccount,
    ) -> Result<PreparedDirect, String> {
        if failed {
            return Err("scalar process has failed and must be restarted".into());
        }
        if process_account.level() != ResourceLevel::Process {
            return Err("accounted stack pool must be bound to a process".into());
        }
        for slot in &mut self.slots[..self.limit] {
            if slot.is_some() {
                let control_charge = process_account.reserve_bytes(INVOCATION_OVERHEAD)?;
                let mut parked = slot.take().expect("checked parked fiber");
                let charge_ref = parked
                    .stack_charge
                    .as_mut()
                    .expect("parked fiber stack charge");
                match self.pool.activate_parked_fiber(charge_ref, process_account) {
                    Ok(()) => {
                        return Ok(PreparedDirect {
                            stack_lease: Some(DirectFiberLease::Parked(parked)),
                            _control_charge: Some(control_charge),
                            native_stack_limit: self.native_stack_limit,
                        });
                    }
                    Err(error) => {
                        *slot = Some(parked);
                        return Err(error);
                    }
                }
            }
        }
        let lease = self
            .pool
            .acquire_direct_with_overhead(process_account, INVOCATION_OVERHEAD)?;
        Ok(PreparedDirect {
            stack_lease: Some(DirectFiberLease::Demand {
                pool: self.pool.clone(),
                lease,
            }),
            _control_charge: None,
            native_stack_limit: self.native_stack_limit,
        })
    }

    pub(crate) fn prepare_direct(
        &mut self,
        process: &ScalarProcess,
        process_account: &ResourceAccount,
    ) -> Result<PreparedDirect, String> {
        self.prepare_direct_account(process.failed, process_account)
    }

    #[inline]
    pub(crate) fn can_run_fast(
        handler: &HandlerId,
        lists: &ListPayload,
        bytes: &BytePayload,
    ) -> bool {
        lists.mask == 0
            && lists.is_empty()
            && bytes.is_trivially_empty()
            && handler.fast_meta().fast_scalar()
    }

    pub(crate) fn prepare(
        &mut self,
        process: &ScalarProcess,
        process_account: &ResourceAccount,
    ) -> Result<PreparedFiber, String> {
        if process.failed {
            return Err("scalar process has failed and must be restarted".into());
        }
        if process_account.level() != ResourceLevel::Process {
            return Err("accounted stack pool must be bound to a process".into());
        }
        let control_charge = process_account.reserve(Resources {
            bytes: INVOCATION_OVERHEAD,
            ..Resources::default()
        })?;
        for slot in &mut self.slots[..self.limit] {
            if let Some(mut parked) = slot.take() {
                let charge_ref = parked
                    .stack_charge
                    .as_mut()
                    .expect("parked fiber stack charge");
                match self.pool.activate_parked_fiber(charge_ref, process_account) {
                    Ok(()) => {
                        let coroutine = parked.coroutine.take().expect("parked fiber coroutine");
                        let stack_charge =
                            parked.stack_charge.take().expect("activated stack charge");
                        return Ok(PreparedFiber {
                            active: ActiveReusableFiber {
                                coroutine: Some(coroutine),
                                stack_charge: Some(stack_charge),
                                pool: self.pool.clone(),
                                _cancellation: None,
                            },
                            control_charge,
                        });
                    }
                    Err(error) => {
                        *slot = Some(parked);
                        return Err(error);
                    }
                }
            }
        }
        let (stack, stack_charge) = self.pool.allocate_fiber_stack(process_account)?;
        let coroutine = new_reusable_coroutine(stack, self.preemption.clone());
        Ok(PreparedFiber {
            active: ActiveReusableFiber {
                coroutine: Some(coroutine),
                stack_charge: Some(stack_charge),
                pool: self.pool.clone(),
                _cancellation: None,
            },
            control_charge,
        })
    }

    pub(super) fn recycle(
        &mut self,
        mut coroutine: ReusableCoroutine,
        mut stack_charge: Reservation,
        pool: StackPool,
    ) {
        if self.limit > 0 && pool.park_active_fiber(&mut stack_charge) {
            for slot in &mut self.slots[..self.limit] {
                if slot.is_none() {
                    *slot = Some(ParkedFiber {
                        coroutine: Some(coroutine),
                        stack_charge: Some(stack_charge),
                        pool,
                    });
                    return;
                }
            }
            drop(ParkedFiber {
                coroutine: Some(coroutine),
                stack_charge: Some(stack_charge),
                pool,
            });
            return;
        }
        if coroutine.started()
            && !coroutine.done()
            && !matches!(
                coroutine.resume(FiberCommand::Shutdown),
                CoroutineResult::Return(())
            )
        {
            std::process::abort();
        }
    }
}

impl Drop for PreparedDirect {
    fn drop(&mut self) {
        if let Some(lease) = self.stack_lease.take() {
            match lease {
                DirectFiberLease::Parked(mut parked) => {
                    if let Some(mut coroutine) = parked.coroutine.take() {
                        if coroutine.started()
                            && !coroutine.done()
                            && !matches!(
                                coroutine.resume(FiberCommand::Shutdown),
                                CoroutineResult::Return(())
                            )
                        {
                            std::process::abort();
                        }
                    }
                    parked.pool.drop_active_fiber();
                }
                DirectFiberLease::Demand { pool, lease } => {
                    pool.release_direct_stack_charge(lease);
                }
            }
        }
    }
}

static DIRECT_NEVER_YIELD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

unsafe extern "C" fn direct_unexpected_slow(context: *mut ScalarContext) -> u32 {
    let context = unsafe { &mut *context };
    if unsafe { &*context.cancellation }.load(Ordering::Acquire) != 0 {
        return STATUS_CANCELLED;
    }
    if context.remaining_steps == 0 {
        return wings_abi::STATUS_BUDGET_EXHAUSTED;
    }
    wings_abi::STATUS_INVALID_CONTROL
}

impl PreparedDirect {
    pub(crate) fn release(mut self, fibers: &mut WorkerFibers) {
        if let Some(lease) = self.stack_lease.take() {
            match lease {
                DirectFiberLease::Parked(mut parked) => {
                    let coroutine = parked.coroutine.take().expect("parked fiber coroutine");
                    let stack_charge = parked
                        .stack_charge
                        .take()
                        .expect("activated parked stack charge");
                    let pool = parked.pool.clone();
                    fibers.recycle(coroutine, stack_charge, pool);
                }
                DirectFiberLease::Demand { pool, lease } => {
                    pool.release_direct_stack_charge(lease);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run(
        &self,
        mut process: ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        slice_steps: u64,
        cancellation: &CancellationToken,
    ) -> (
        ScalarProcess,
        ExecutionSnapshot,
        Result<ExecutionStatus, String>,
    ) {
        let mut output = ScalarOutput::default();
        let mut heap = process.heap.take();
        let mut context = ScalarContext {
            cancellation: &*cancellation.flag,
            remaining_steps: max_steps,
            yield_request: &DIRECT_NEVER_YIELD,
            slice_remaining: slice_steps,
            slow_path: Some(direct_unexpected_slow),
            data: std::ptr::null_mut(),
            stack_limit: self.native_stack_limit,
            roots: std::ptr::null(),
            heap_call: heap
                .as_ref()
                .map(|_| crate::aot_heap::heap_call as wings_abi::HeapCall),
            heap_data: heap.as_mut().map_or(std::ptr::null_mut(), |heap| {
                std::ptr::from_mut(&mut **heap).cast()
            }),
            effect_call: None,
        };
        let entry = process.module.inner.entries[handler];
        let status = unsafe { entry(process.state, arguments.as_ptr(), &mut output, &mut context) };
        let status = if context.roots.is_null() {
            status
        } else {
            wings_abi::STATUS_INVALID_ROOT
        };
        if let Some(heap_ref) = &mut heap {
            heap_ref.clear_incoming();
        }
        let remaining_steps = context.remaining_steps;
        let heap_stats = heap.as_deref().map(AotHeap::stats);
        process.heap = heap;
        let result = process.finish_invocation(handler, status, output, None, None, None);
        let snapshot = ExecutionSnapshot {
            phase: if result.is_ok() {
                ExecutionPhase::Complete
            } else {
                ExecutionPhase::Failed
            },
            fault: if result.is_err() {
                Some(ExecutionFault::from_status(status).unwrap_or(ExecutionFault::InvalidOutput))
            } else {
                None
            },
            max_steps,
            remaining_steps,
            heap: heap_stats,
        };
        (process, snapshot, result)
    }
}

impl PreparedFiber {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn into_ready_task(
        mut self,
        fibers: &WorkerFibers,
        mut process: ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        cancellation: &CancellationToken,
        inline: Option<crate::scheduler::EffectSendTarget>,
    ) -> Result<ScalarTask, (ScalarProcess, String)> {
        let snapshot = ExecutionSnapshot {
            phase: ExecutionPhase::Ready,
            fault: None,
            max_steps,
            remaining_steps: max_steps,
            heap: process.heap_stats(),
        };
        let entry = process.module.inner.entries[handler];
        let start = FiberStart {
            entry,
            state: process.state,
            arguments: arguments.as_ptr(),
            argument_count: arguments.len(),
            max_steps,
            slice_remaining: None,
            cancellation: &*cancellation.flag,
            heap: process.heap.take(),
            effects_enabled: true,
            inline,
        };
        self.active._cancellation = Some(cancellation.clone());
        let primed = self
            .active
            .coroutine
            .as_mut()
            .expect("prepared reusable coroutine")
            .resume(FiberCommand::Start(start));
        if !matches!(primed, CoroutineResult::Yield(FiberEvent::Primed)) {
            std::process::abort();
        }
        let invocation = Invocation {
            handler,
            backend: Some(InvocationBackend::Reusable(self.active)),
            snapshot,
            effect: None,
            acknowledgement: None,
            preemption: fibers.preemption.clone(),
            _charge: Some(self.control_charge),
        };
        Ok(ScalarTask {
            invocation: Some(invocation),
            process,
            last: None,
            effects_enabled: true,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_first_slice(
        mut self,
        fibers: &mut WorkerFibers,
        mut process: ScalarProcess,
        handler: usize,
        arguments: &[i64],
        max_steps: u64,
        slice_steps: u64,
        cancellation: &CancellationToken,
        inline: Option<crate::scheduler::EffectSendTarget>,
    ) -> Result<
        (
            ScalarProcess,
            ExecutionSnapshot,
            Result<ExecutionStatus, String>,
        ),
        Box<ScalarTask>,
    > {
        let initial_heap = process.heap_stats();
        let entry = process.module.inner.entries[handler];
        let start = FiberStart {
            entry,
            state: process.state,
            arguments: arguments.as_ptr(),
            argument_count: arguments.len(),
            max_steps,
            slice_remaining: Some(slice_steps),
            cancellation: &*cancellation.flag,
            heap: process.heap.take(),
            effects_enabled: true,
            inline,
        };
        let step = self
            .active
            .coroutine
            .as_mut()
            .expect("prepared reusable coroutine")
            .resume(FiberCommand::Start(start));
        match step {
            CoroutineResult::Yield(FiberEvent::Complete {
                status,
                output,
                heap,
                checkpoint,
            }) => {
                let coroutine = self.active.coroutine.take().expect("completed coroutine");
                let stack_charge = self
                    .active
                    .stack_charge
                    .take()
                    .expect("completed stack charge");
                let pool = self.active.pool.clone();
                fibers.recycle(coroutine, stack_charge, pool);
                drop(self.control_charge);
                process.heap = heap;
                let result = process.finish_invocation(handler, status, output, None, None, None);
                let snapshot = ExecutionSnapshot {
                    phase: if result.is_ok() {
                        ExecutionPhase::Complete
                    } else {
                        ExecutionPhase::Failed
                    },
                    fault: if result.is_err() {
                        Some(
                            ExecutionFault::from_status(status)
                                .unwrap_or(ExecutionFault::InvalidOutput),
                        )
                    } else {
                        None
                    },
                    max_steps,
                    remaining_steps: checkpoint.remaining_steps,
                    heap: checkpoint.heap,
                };
                Ok((process, snapshot, result))
            }
            CoroutineResult::Yield(FiberEvent::Suspended(checkpoint)) => {
                if let Some(requested_at) = checkpoint.yield_requested {
                    fibers.preemption.confirm_yield(requested_at);
                }
                self.active._cancellation = Some(cancellation.clone());
                let snapshot = ExecutionSnapshot {
                    phase: ExecutionPhase::Suspended,
                    fault: None,
                    max_steps,
                    remaining_steps: checkpoint.remaining_steps,
                    heap: checkpoint.heap.or(initial_heap),
                };
                let invocation = Invocation {
                    handler,
                    backend: Some(InvocationBackend::Reusable(self.active)),
                    snapshot,
                    effect: checkpoint.effect,
                    acknowledgement: None,
                    preemption: fibers.preemption.clone(),
                    _charge: Some(self.control_charge),
                };
                Err(Box::new(ScalarTask {
                    invocation: Some(invocation),
                    process,
                    last: None,
                    effects_enabled: true,
                }))
            }
            CoroutineResult::Yield(FiberEvent::Primed) | CoroutineResult::Return(()) => {
                std::process::abort()
            }
        }
    }
}

fn new_reusable_coroutine(stack: FiberStack, preemption: PreemptionToken) -> ReusableCoroutine {
    let stack_limit = stack.managed_limit();
    Coroutine::with_stack(stack, move |yielder, mut command| {
        let mut arguments = [0i64; 64];
        loop {
            let FiberCommand::Start(mut start) = command else {
                return;
            };
            if start.argument_count != 0 {
                arguments[..start.argument_count].copy_from_slice(unsafe {
                    std::slice::from_raw_parts(start.arguments, start.argument_count)
                });
            }
            let mut output = ScalarOutput::default();
            let slice_remaining = match start.slice_remaining {
                Some(steps) => steps,
                None => match yielder.suspend(FiberEvent::Primed) {
                    FiberCommand::Run(steps) => steps,
                    _ => {
                        let checkpoint = Checkpoint {
                            remaining_steps: start.max_steps,
                            heap: start.heap.as_deref().map(AotHeap::stats),
                            effect: None,
                            yield_requested: None,
                        };
                        command = yielder.suspend(FiberEvent::Complete {
                            status: STATUS_CANCELLED,
                            output,
                            heap: start.heap,
                            checkpoint,
                        });
                        continue;
                    }
                },
            };
            let mut control = ReusableControl {
                yielder,
                preemption: &preemption,
                inline: start.inline.as_ref(),
                budget: start.max_steps,
            };
            let mut context = ScalarContext {
                cancellation: start.cancellation,
                remaining_steps: start.max_steps,
                yield_request: &*preemption.flag,
                slice_remaining,
                slow_path: Some(suspend_reusable),
                data: std::ptr::from_mut(&mut control).cast(),
                stack_limit,
                roots: std::ptr::null(),
                heap_call: start
                    .heap
                    .as_ref()
                    .map(|_| crate::aot_heap::heap_call as wings_abi::HeapCall),
                heap_data: start.heap.as_mut().map_or(std::ptr::null_mut(), |heap| {
                    std::ptr::from_mut(&mut **heap).cast()
                }),
                effect_call: start
                    .effects_enabled
                    .then_some(send_self_reusable as wings_abi::EffectCall),
            };
            let raw_status = unsafe {
                (start.entry)(start.state, arguments.as_ptr(), &mut output, &mut context)
            };
            let status = if context.roots.is_null() {
                raw_status
            } else {
                wings_abi::STATUS_INVALID_ROOT
            };
            let checkpoint = Checkpoint {
                remaining_steps: context.remaining_steps,
                heap: start.heap.as_deref().map(AotHeap::stats),
                effect: None,
                yield_requested: None,
            };
            if let Some(heap) = &mut start.heap {
                heap.clear_incoming();
            }
            command = yielder.suspend(FiberEvent::Complete {
                status,
                output,
                heap: start.heap,
                checkpoint,
            });
        }
    })
}
