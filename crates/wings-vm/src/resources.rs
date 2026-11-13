use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resources {
    pub bytes: u64,
    pub processes: u64,
    pub messages: u64,
    pub requests: u64,
}

impl Resources {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            bytes: self.bytes.checked_add(other.bytes)?,
            processes: self.processes.checked_add(other.processes)?,
            messages: self.messages.checked_add(other.messages)?,
            requests: self.requests.checked_add(other.requests)?,
        })
    }

    fn fits(self, limit: Self) -> bool {
        self.bytes <= limit.bytes
            && self.processes <= limit.processes
            && self.messages <= limit.messages
            && self.requests <= limit.requests
    }

    fn subtract(self, other: Self) -> Self {
        Self {
            bytes: self.bytes - other.bytes,
            processes: self.processes - other.processes,
            messages: self.messages - other.messages,
            requests: self.requests - other.requests,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceLevel {
    Vm,
    Tenant,
    Process,
}

#[derive(Clone)]
pub struct ResourceAccount {
    node: Arc<AccountNode>,
}

struct AccountNode {
    level: ResourceLevel,
    limit: Resources,
    used: [AtomicU64; 4],
    gate: Arc<Mutex<()>>,
    parent: Option<Reservation>,
}

pub struct Reservation {
    account: ResourceAccount,
    amount: Resources,
}

impl ResourceAccount {
    pub fn vm(limit: Resources) -> Self {
        Self::create(ResourceLevel::Vm, limit, None)
    }

    pub fn tenant(&self, limit: Resources) -> Result<Self, String> {
        self.child(ResourceLevel::Vm, ResourceLevel::Tenant, limit)
    }

    pub fn process(&self, limit: Resources) -> Result<Self, String> {
        self.child(ResourceLevel::Tenant, ResourceLevel::Process, limit)
    }

    pub fn tenant_accounted(&self, limit: Resources) -> Result<Self, String> {
        self.child_accounted(ResourceLevel::Vm, ResourceLevel::Tenant, limit)
    }

    pub fn process_accounted(&self, limit: Resources) -> Result<Self, String> {
        self.child_accounted(ResourceLevel::Tenant, ResourceLevel::Process, limit)
    }

    pub fn level(&self) -> ResourceLevel {
        self.node.level
    }

    pub fn limit(&self) -> Resources {
        self.node.limit
    }

    pub fn used(&self) -> Resources {
        let _guard = self
            .node
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.read_used()
    }

    fn read_used(&self) -> Resources {
        let [bytes, processes, messages, requests] = self
            .node
            .used
            .each_ref()
            .map(|value| value.load(Ordering::Relaxed));
        Resources {
            bytes,
            processes,
            messages,
            requests,
        }
    }

    fn write_used(&self, value: Resources) {
        let values = [value.bytes, value.processes, value.messages, value.requests];
        for (counter, value) in self.node.used.iter().zip(values) {
            counter.store(value, Ordering::Relaxed);
        }
    }

    pub(crate) fn is_child_of(&self, parent: &Self) -> bool {
        self.node
            .parent
            .as_ref()
            .is_some_and(|owner| owner.account.same(parent))
    }

    pub fn reserve(&self, amount: Resources) -> Result<Reservation, String> {
        let guard = self
            .node
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for node in self.path().into_iter().flatten() {
            if !node.admits(amount) {
                let level = node.level();
                drop(guard);
                return Err(format!("resource_exhausted at {level:?} account"));
            }
        }
        for node in self.path().into_iter().flatten() {
            node.write_used(
                node.read_used()
                    .checked_add(amount)
                    .expect("admitted resources"),
            );
        }
        Ok(Reservation {
            account: self.clone(),
            amount,
        })
    }

    fn create(level: ResourceLevel, limit: Resources, parent: Option<Reservation>) -> Self {
        let gate = parent.as_ref().map_or_else(
            || Arc::new(Mutex::new(())),
            |parent| parent.account.node.gate.clone(),
        );
        Self {
            node: Arc::new(AccountNode {
                level,
                limit,
                parent,
                used: std::array::from_fn(|_| AtomicU64::new(0)),
                gate,
            }),
        }
    }

    fn child(
        &self,
        required: ResourceLevel,
        level: ResourceLevel,
        limit: Resources,
    ) -> Result<Self, String> {
        if self.level() != required {
            return Err("resource account hierarchy must be VM -> tenant -> process".into());
        }
        Ok(Self::create(
            level,
            limit,
            Some(Reservation {
                account: self.clone(),
                amount: Resources::default(),
            }),
        ))
    }

    fn child_accounted(
        &self,
        required: ResourceLevel,
        level: ResourceLevel,
        limit: Resources,
    ) -> Result<Self, String> {
        if self.level() != required {
            return Err("resource account hierarchy must be VM -> tenant -> process".into());
        }
        let charge = self.reserve(Resources {
            bytes: (std::mem::size_of::<AccountNode>() + 2 * std::mem::size_of::<usize>()) as u64,
            ..Resources::default()
        })?;
        Ok(Self::create(level, limit, Some(charge)))
    }

    fn path(&self) -> [Option<&Self>; 3] {
        let parent = self.node.parent.as_ref().map(|parent| &parent.account);
        let root =
            parent.and_then(|parent| parent.node.parent.as_ref().map(|parent| &parent.account));
        [Some(self), parent, root]
    }

    fn root(&self) -> &Self {
        self.path()
            .into_iter()
            .flatten()
            .last()
            .expect("account root")
    }

    fn admits(&self, amount: Resources) -> bool {
        self.read_used()
            .checked_add(amount)
            .is_some_and(|used| used.fits(self.limit()))
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }

    pub(crate) fn same_vm(&self, other: &Self) -> bool {
        self.root().same(other.root())
    }
}

impl Reservation {
    pub fn amount(&self) -> Resources {
        self.amount
    }

    pub fn transfer(&mut self, target: &ResourceAccount) -> Result<(), String> {
        if !self.account.root().same(target.root()) {
            return Err("resource transfer requires the same VM account".into());
        }
        let guard = self
            .account
            .node
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let source_path = self.account.path();
        let target_path = target.path();
        for node in target_path.into_iter().flatten() {
            if !source_path
                .into_iter()
                .flatten()
                .any(|source| node.same(source))
                && !node.admits(self.amount)
            {
                let level = node.level();
                drop(guard);
                return Err(format!("resource_exhausted at {level:?} account"));
            }
        }
        for node in target_path.into_iter().flatten() {
            if !source_path
                .into_iter()
                .flatten()
                .any(|source| node.same(source))
            {
                node.write_used(
                    node.read_used()
                        .checked_add(self.amount)
                        .expect("admitted transfer"),
                );
            }
        }
        for node in source_path.into_iter().flatten() {
            if !target_path
                .into_iter()
                .flatten()
                .any(|target| node.same(target))
            {
                node.write_used(node.read_used().subtract(self.amount));
            }
        }
        drop(guard);
        self.account = target.clone();
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let _guard = self
            .account
            .node
            .gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for node in self.account.path().into_iter().flatten() {
            node.write_used(node.read_used().subtract(self.amount));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_gate_recovers_without_losing_admission_or_ticket_cleanup() {
        let limit = Resources {
            bytes: 8,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limit);
        let tenant = vm.tenant(limit).unwrap();
        let process = tenant.process(limit).unwrap();
        let mut ticket = process.reserve(limit).unwrap();
        let gate = vm.node.gate.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = gate.lock().unwrap();
                panic!("poison internal gate without modifying counters");
            })
            .join()
            .is_err()
        );
        assert_eq!(vm.used(), limit);
        assert!(process.reserve(limit).is_err());
        ticket.transfer(&tenant).unwrap();
        assert_eq!(process.used(), Resources::default());
        assert_eq!(tenant.used(), limit);
        drop(ticket);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn independent_vm_admission_does_not_share_a_gate() {
        let limit = Resources {
            bytes: 8,
            ..Resources::default()
        };
        let first = ResourceAccount::vm(limit);
        let second = ResourceAccount::vm(limit);
        let _guard = first.node.gate.lock().unwrap();
        let ticket = std::thread::spawn(move || second.reserve(limit).unwrap())
            .join()
            .unwrap();
        assert_eq!(ticket.amount(), limit);
        drop(ticket);
    }

    fn node_bytes() -> u64 {
        (std::mem::size_of::<AccountNode>() + 2 * std::mem::size_of::<usize>()) as u64
    }

    fn bytes(amount: u64) -> Resources {
        Resources {
            bytes: amount,
            ..Resources::default()
        }
    }

    #[test]
    fn accounted_nodes_charge_only_the_parent_path_and_last_owner_releases_it() {
        let cost = node_bytes();
        let vm = ResourceAccount::vm(bytes(3 * cost));
        let tenant = vm.tenant_accounted(bytes(2 * cost)).unwrap();
        assert_eq!(vm.used(), bytes(cost));
        assert_eq!(tenant.used(), Resources::default());
        let process = tenant.process_accounted(bytes(0)).unwrap();
        assert_eq!(vm.used(), bytes(2 * cost));
        assert_eq!(tenant.used(), bytes(cost));
        assert_eq!(process.used(), Resources::default());
        let clone = process.clone();
        let ticket = process.reserve(Resources::default()).unwrap();
        drop((process, clone, tenant));
        assert_eq!(vm.used(), bytes(2 * cost));
        std::thread::spawn(move || drop(ticket)).join().unwrap();
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn accounted_nodes_reject_each_parent_or_ancestor_limit_before_allocation() {
        let cost = node_bytes();
        let vm = ResourceAccount::vm(bytes(cost - 1));
        assert!(vm.tenant_accounted(bytes(u64::MAX)).is_err());
        assert_eq!(vm.used(), Resources::default());
        for (root_limit, tenant_limit, error) in
            [(2 * cost - 1, cost, "Vm"), (2 * cost, cost - 1, "Tenant")]
        {
            let vm = ResourceAccount::vm(bytes(root_limit));
            let tenant = vm.tenant_accounted(bytes(tenant_limit)).unwrap();
            assert!(
                tenant
                    .process_accounted(bytes(0))
                    .err()
                    .unwrap()
                    .contains(error)
            );
            assert_eq!(vm.used(), bytes(cost));
            assert_eq!(tenant.used(), Resources::default());
            drop(tenant);
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn accounted_node_hierarchy_rejection_and_overflow_do_not_change_usage() {
        let vm = ResourceAccount::vm(bytes(u64::MAX));
        let tenant = vm.tenant_accounted(bytes(u64::MAX)).unwrap();
        let process = tenant.process_accounted(bytes(u64::MAX)).unwrap();
        let baseline = vm.used();
        assert!(vm.process_accounted(bytes(0)).is_err());
        assert!(tenant.tenant_accounted(bytes(0)).is_err());
        assert!(process.tenant_accounted(bytes(0)).is_err());
        assert!(process.process_accounted(bytes(0)).is_err());
        assert_eq!(vm.used(), baseline);
        let held = vm.reserve(bytes(u64::MAX - baseline.bytes)).unwrap();
        assert!(vm.tenant_accounted(bytes(0)).is_err());
        assert!(tenant.process_accounted(bytes(0)).is_err());
        assert_eq!(vm.used(), bytes(u64::MAX));
        drop((held, process, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn transferring_last_ticket_to_parent_releases_child_metadata_after_unlocking() {
        let cost = node_bytes();
        let vm = ResourceAccount::vm(bytes(3 * cost));
        let tenant = vm.tenant_accounted(bytes(2 * cost)).unwrap();
        let process = tenant.process_accounted(bytes(cost)).unwrap();
        let mut ticket = process.reserve(bytes(cost)).unwrap();
        drop(process);
        assert_eq!(vm.used(), bytes(3 * cost));
        ticket.transfer(&tenant).unwrap();
        assert_eq!(vm.used(), bytes(2 * cost));
        assert_eq!(tenant.used(), bytes(cost));
        drop((ticket, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn concurrent_accounted_node_creation_cannot_overrun_shared_metadata_budget() {
        let cost = node_bytes();
        let vm = ResourceAccount::vm(bytes(4 * cost));
        let barrier = Arc::new(std::sync::Barrier::new(9));
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let vm = &vm;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        let account = vm.tenant_accounted(bytes(0));
                        barrier.wait();
                        barrier.wait();
                        account
                    })
                })
                .collect();
            barrier.wait();
            barrier.wait();
            assert_eq!(vm.used(), bytes(4 * cost));
            barrier.wait();
            let accounts: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            assert_eq!(accounts.iter().filter(|account| account.is_ok()).count(), 4);
            drop(accounts);
        });
        assert_eq!(vm.used(), Resources::default());
    }
}
