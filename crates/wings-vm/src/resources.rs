use std::cell::Cell;
use std::rc::Rc;

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
    node: Rc<AccountNode>,
}

struct AccountNode {
    level: ResourceLevel,
    limit: Resources,
    used: Cell<Resources>,
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
        self.node.used.get()
    }

    pub(crate) fn is_child_of(&self, parent: &Self) -> bool {
        self.node
            .parent
            .as_ref()
            .is_some_and(|owner| owner.same(parent))
    }

    pub fn reserve(&self, amount: Resources) -> Result<Reservation, String> {
        for node in self.path().into_iter().flatten() {
            node.admit(amount)?;
        }
        for node in self.path().into_iter().flatten() {
            node.node
                .used
                .set(node.used().checked_add(amount).expect("admitted resources"));
        }
        Ok(Reservation {
            account: self.clone(),
            amount,
        })
    }

    fn create(level: ResourceLevel, limit: Resources, parent: Option<Self>) -> Self {
        Self {
            node: Rc::new(AccountNode {
                level,
                limit,
                parent,
                used: Cell::new(Resources::default()),
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

    fn admit(&self, amount: Resources) -> Result<(), String> {
        if self
            .used()
            .checked_add(amount)
            .is_some_and(|used| used.fits(self.limit()))
        {
            Ok(())
        } else {
            Err(format!("resource_exhausted at {:?} account", self.level()))
        }
    }

    fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.node, &other.node)
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
        let source_path = self.account.path();
        let target_path = target.path();
        for node in target_path.into_iter().flatten() {
            if !source_path
                .into_iter()
                .flatten()
                .any(|source| node.same(source))
            {
                node.admit(self.amount)?;
            }
        }
        for node in target_path.into_iter().flatten() {
            if !source_path
                .into_iter()
                .flatten()
                .any(|source| node.same(source))
            {
                node.node.used.set(
                    node.used()
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
                node.node.used.set(node.used().subtract(self.amount));
            }
        }
        self.account = target.clone();
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        for node in self.account.path().into_iter().flatten() {
            node.node.used.set(node.used().subtract(self.amount));
        }
    }
}
