use super::*;

pub(super) struct DemandState {
    pub(super) cached: Vec<CachedStack>,
    active: usize,
}

pub(super) struct CachedStack {
    mapping: DefaultStack,
    charge: Reservation,
}

impl StackPool {
    pub(crate) fn new_on_demand(
        tenant: &ResourceAccount,
        capacity: usize,
        max_cached: usize,
    ) -> Result<Self, String> {
        wings_abi::host_target()?;
        if tenant.level() != ResourceLevel::Tenant
            || !(1..=16384).contains(&capacity)
            || max_cached > capacity
        {
            return Err(
                "on-demand stacks require a tenant, 1..=16384 slots and bounded cache".into(),
            );
        }
        let page_bytes = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(|_| "failed to query OS page size")?;
        if !page_bytes.is_power_of_two() || page_bytes < STACK_ALIGNMENT {
            return Err("unsupported OS stack page size".into());
        }
        let slot_bytes = EXECUTION_STACK_BYTES
            .checked_add(page_bytes - 1)
            .map(|bytes| bytes & !(page_bytes - 1))
            .and_then(|bytes| bytes.checked_add(page_bytes))
            .ok_or("stack size overflow")?;
        let metadata_bytes = std::mem::size_of::<PoolInner>()
            + std::mem::size_of::<StackPool>()
            + 2 * std::mem::size_of::<usize>()
            + max_cached * std::mem::size_of::<CachedStack>();
        let metadata = tenant.reserve(Resources {
            bytes: metadata_bytes as u64,
            ..Resources::default()
        })?;
        let mut cached = Vec::new();
        cached
            .try_reserve_exact(max_cached)
            .map_err(|_| "on-demand stack cache allocation failed")?;
        Ok(Self {
            inner: Rc::new(PoolInner {
                mapping: None,
                mapping_bytes: 0,
                slot_bytes,
                page_bytes,
                capacity,
                max_cached,
                state: RefCell::new(PoolState {
                    free: Vec::new(),
                    cached: Vec::new(),
                    active: 0,
                    reset_failures: 0,
                    charges: Vec::new(),
                }),
                accounting: Some(PoolAccounting {
                    tenant: tenant.clone(),
                    _metadata: metadata,
                    charged_bytes: metadata_bytes as u64,
                }),
                demand: Some(RefCell::new(DemandState { cached, active: 0 })),
            }),
            process: None,
        })
    }

    pub(super) fn demand_stats(&self, state: &DemandState) -> StackPoolStats {
        let mapped_bytes = (state.active + state.cached.len()) * self.inner.slot_bytes;
        StackPoolStats {
            capacity: self.inner.capacity,
            active: state.active,
            cached: state.cached.len(),
            available: self.inner.capacity - state.active,
            reserved_bytes: mapped_bytes,
            usable_bytes_per_stack: self.inner.slot_bytes - self.inner.page_bytes,
            guard_bytes_per_stack: self.inner.page_bytes,
            reset_failures: 0,
            charged_bytes: self
                .inner
                .accounting
                .as_ref()
                .expect("demand accounting")
                .charged_bytes
                + mapped_bytes as u64,
        }
    }

    pub(super) fn acquire_demand(&self) -> Result<PooledStack, String> {
        self.acquire_demand_with(|| {
            DefaultStack::new(EXECUTION_STACK_BYTES)
                .map_err(|error| format!("allocate on-demand stack: {error}"))
        })
    }

    fn acquire_demand_with(
        &self,
        allocate: impl FnOnce() -> Result<DefaultStack, String>,
    ) -> Result<PooledStack, String> {
        let process = self
            .process
            .as_ref()
            .ok_or("on-demand stack pool must be bound to a process")?;
        let mut state = self
            .inner
            .demand
            .as_ref()
            .expect("demand pool")
            .borrow_mut();
        if state.active == self.inner.capacity {
            return Err("execution stack pool exhausted".into());
        }
        let CachedStack { mapping, charge } = if let Some(cached) = state.cached.last_mut() {
            cached.charge.transfer(process)?;
            state.cached.pop().expect("cached stack")
        } else {
            let charge = process.reserve(Resources {
                bytes: self.inner.slot_bytes as u64,
                ..Resources::default()
            })?;
            let mapping = allocate()?;
            if mapping.base().get() - mapping.limit().get() != self.inner.slot_bytes {
                return Err("unexpected on-demand stack mapping size".into());
            }
            CachedStack { mapping, charge }
        };
        state.active += 1;
        Ok(PooledStack {
            pool: self.inner.clone(),
            index: 0,
            charge: Some(charge),
            mapping: Some(mapping),
        })
    }
}

impl PooledStack {
    pub(super) fn release_demand(&mut self) {
        let mut state = self.pool.demand.as_ref().expect("demand pool").borrow_mut();
        let mapping = self.mapping.take().expect("active demand stack");
        let mut charge = self.charge.take().expect("active demand charge");
        if state.cached.len() < self.pool.max_cached {
            charge
                .transfer(
                    &self
                        .pool
                        .accounting
                        .as_ref()
                        .expect("demand accounting")
                        .tenant,
                )
                .expect("return cached stack charge");
            state.cached.push(CachedStack { mapping, charge });
        } else {
            drop(mapping);
            drop(charge);
        }
        state.active -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_failure_after_admission_restores_quota_and_retryable_capacity() {
        let limits = Resources {
            bytes: 4 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant_accounted(limits).unwrap();
        let process = tenant.process_accounted(limits).unwrap();
        let pool = StackPool::new_on_demand(&tenant, 1, 0).unwrap();
        let bound = pool.for_process(&process).unwrap();
        let baseline = vm.used();
        let stats = pool.stats();
        assert!(
            bound
                .acquire_demand_with(|| {
                    assert_eq!(process.used().bytes, pool.inner.slot_bytes as u64);
                    Err("injected mapping failure".into())
                })
                .is_err()
        );
        assert_eq!(vm.used(), baseline);
        assert_eq!(pool.stats(), stats);
        let stack = bound.acquire().unwrap();
        assert_eq!(pool.stats().active, 1);
        drop((pool, bound, stack, process, tenant));
        assert_eq!(vm.used(), Resources::default());
    }
}
