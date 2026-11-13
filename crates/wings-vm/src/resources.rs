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
    parent: Option<ResourceAccount>,
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
            .is_some_and(|owner| owner.same(parent))
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

    fn create(level: ResourceLevel, limit: Resources, parent: Option<Self>) -> Self {
        let gate = parent.as_ref().map_or_else(
            || Arc::new(Mutex::new(())),
            |parent| parent.node.gate.clone(),
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
        Ok(Self::create(level, limit, Some(self.clone())))
    }

    fn path(&self) -> [Option<&Self>; 3] {
        let parent = self.node.parent.as_ref();
        let root = parent.and_then(|parent| parent.node.parent.as_ref());
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
}
