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
    #[cfg(test)]
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            bytes: self.bytes.checked_add(other.bytes)?,
            processes: self.processes.checked_add(other.processes)?,
            messages: self.messages.checked_add(other.messages)?,
            requests: self.requests.checked_add(other.requests)?,
        })
    }

    fn is_zero(self) -> bool {
        self.bytes == 0 && self.processes == 0 && self.messages == 0 && self.requests == 0
    }

    // Bytes and messages share one packed counter word (see `packed` below):
    // an envelope reservation adds and refunds both dimensions with a single
    // CAS/fetch per account level instead of two independent counter chains.
    fn envelope_dimensions(self) -> Option<(u64, u64)> {
        if self.processes == 0 && self.requests == 0 && (self.bytes != 0 || self.messages != 0) {
            Some((self.bytes, self.messages))
        } else {
            None
        }
    }

    fn single_dimension(self) -> Option<(usize, u64)> {
        let mask = u8::from(self.bytes != 0)
            | (u8::from(self.processes != 0) << 1)
            | (u8::from(self.messages != 0) << 2)
            | (u8::from(self.requests != 0) << 3);
        match mask {
            1 => Some((0, self.bytes)),
            2 => Some((1, self.processes)),
            4 => Some((2, self.messages)),
            8 => Some((3, self.requests)),
            _ => None,
        }
    }

    fn is_multi_dimensional(self) -> bool {
        if self.envelope_dimensions().is_some() {
            return false;
        }
        let count = u8::from(self.bytes != 0)
            + u8::from(self.processes != 0)
            + u8::from(self.messages != 0)
            + u8::from(self.requests != 0);
        count > 1
    }

    fn add_unchecked(self, other: Self) -> Self {
        Self {
            bytes: self.bytes + other.bytes,
            processes: self.processes + other.processes,
            messages: self.messages + other.messages,
            requests: self.requests + other.requests,
        }
    }

    fn as_array(self) -> [u64; 4] {
        [self.bytes, self.processes, self.messages, self.requests]
    }

    #[cfg(test)]
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

#[repr(align(64))]
struct AlignedCounter(AtomicU64);

impl std::ops::Deref for AlignedCounter {
    type Target = AtomicU64;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

struct AccountNode {
    level: ResourceLevel,
    accounted_root: bool,
    limit: Resources,
    // `[0]` packs bytes + messages, `[1]` holds processes, `[2]` holds requests.
    used: [AlignedCounter; 3],
    gate: Arc<Mutex<()>>,
    parent: Option<Reservation>,
}

// The bytes dimension shares counter word `used[0]` with messages: bytes take
// the low `BYTES_BITS` and messages the high `MESSAGES_BITS`, so an envelope
// reservation touches one atomic per account level. The split still leaves a
// 1 TiB byte ceiling and a 16.7M in-flight message ceiling, both far above
// the scheduler's `MAX_ACTORS * mailbox capacity` bounds.
const BYTES_BITS: u32 = 40;
const MESSAGES_BITS: u32 = 24;
const BYTES_FIELD_MAX: u64 = (1 << BYTES_BITS) - 1;
const MESSAGES_FIELD_MAX: u64 = (1 << MESSAGES_BITS) - 1;

fn packed(bytes: u64, messages: u64) -> u64 {
    (messages << BYTES_BITS) | bytes
}

fn packed_bytes(word: u64) -> u64 {
    word & BYTES_FIELD_MAX
}

fn packed_messages(word: u64) -> u64 {
    word >> BYTES_BITS
}

fn packed_delta_fits(bytes: u64, messages: u64) -> bool {
    bytes <= BYTES_FIELD_MAX && messages <= MESSAGES_FIELD_MAX
}

pub struct Reservation {
    account: ResourceAccount,
    amount: Resources,
}

pub(crate) struct EnvelopeCharge {
    account: ResourceAccount,
    bytes: u64,
    messages: u8,
    gated: bool,
}

impl ResourceAccount {
    pub fn vm(limit: Resources) -> Self {
        Self::create(ResourceLevel::Vm, limit, None, 0)
    }

    pub fn vm_accounted(limit: Resources) -> Result<Self, String> {
        let bootstrap = (std::mem::size_of::<AccountNode>()
            + std::mem::size_of::<Mutex<()>>()
            + 4 * std::mem::size_of::<usize>()
            + crate::native_sync::MUTEX_BYTES) as u64;
        if limit.bytes < bootstrap {
            return Err("resource_exhausted at Vm account during bootstrap".into());
        }
        Ok(Self::create(ResourceLevel::Vm, limit, None, bootstrap))
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
        let packed_word = self.node.used[0].load(Ordering::Acquire);
        Resources {
            bytes: packed_bytes(packed_word),
            processes: self.node.used[1].load(Ordering::Acquire),
            messages: packed_messages(packed_word),
            requests: self.node.used[2].load(Ordering::Acquire),
        }
    }

    #[inline]
    fn try_add_dim(&self, index: usize, delta: u64) -> bool {
        // Bytes and messages share the packed word; deltas that exceed the
        // field width are simply rejected as exhaustion.
        if index == 0 {
            return self.try_add_packed(delta, 0);
        }
        if index == 2 {
            return self.try_add_packed(0, delta);
        }
        let slot = if index == 3 { 2 } else { index };
        let counter = &self.node.used[slot];
        let limit = self.node.limit.as_array()[index];
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(delta).filter(|&next| next <= limit) else {
                return false;
            };
            match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// One CAS attempt on the shared bytes+messages word: both fields must
    /// satisfy their dimension limits for the whole add to land. Field sums
    /// are computed *before* packing so a bytes overflow cannot carry into
    /// the messages field.
    #[inline]
    fn try_add_packed(&self, bytes: u64, messages: u64) -> bool {
        if !packed_delta_fits(bytes, messages) {
            return false;
        }
        let counter = &self.node.used[0];
        let limit = self.node.limit;
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let Some(next_bytes) = packed_bytes(current)
                .checked_add(bytes)
                .filter(|&next| next <= limit.bytes)
            else {
                return false;
            };
            let Some(next_messages) = packed_messages(current)
                .checked_add(messages)
                .filter(|&next| next <= limit.messages)
            else {
                return false;
            };
            let next = packed(next_bytes, next_messages);
            match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    #[inline]
    fn sub_dim(&self, index: usize, delta: u64) {
        if index == 0 {
            self.node.used[0].fetch_sub(delta, Ordering::Relaxed);
        } else if index == 2 {
            self.node.used[0].fetch_sub(delta << BYTES_BITS, Ordering::Relaxed);
        } else {
            let slot = if index == 3 { 2 } else { index };
            self.node.used[slot].fetch_sub(delta, Ordering::Relaxed);
        }
    }

    #[inline]
    fn sub_packed(&self, bytes: u64, messages: u64) {
        self.node.used[0].fetch_sub(packed(bytes, messages), Ordering::Relaxed);
    }

    fn reserve_dim_status(&self, index: usize, delta: u64) -> Result<(), ResourceLevel> {
        if delta == 0 {
            return Ok(());
        }
        if !self.try_add_dim(index, delta) {
            return Err(self.level());
        }
        if let Some(parent) = self.node.parent.as_ref().map(|parent| &parent.account) {
            if !parent.try_add_dim(index, delta) {
                self.sub_dim(index, delta);
                return Err(parent.level());
            }
            if let Some(root) = parent.node.parent.as_ref().map(|parent| &parent.account) {
                if !root.try_add_dim(index, delta) {
                    parent.sub_dim(index, delta);
                    self.sub_dim(index, delta);
                    return Err(root.level());
                }
            }
        }
        Ok(())
    }

    fn refund_dim_all(&self, index: usize, delta: u64) {
        if delta == 0 {
            return;
        }
        self.sub_dim(index, delta);
        if let Some(parent) = self.node.parent.as_ref().map(|parent| &parent.account) {
            parent.sub_dim(index, delta);
            if let Some(root) = parent.node.parent.as_ref().map(|parent| &parent.account) {
                root.sub_dim(index, delta);
            }
        }
    }

    /// Envelope admission for the whole `Process -> Tenant -> VM` chain in one
    /// packed CAS per level: bytes and messages move together.
    fn reserve_packed_status(&self, bytes: u64, messages: u64) -> Result<(), ResourceLevel> {
        if !self.try_add_packed(bytes, messages) {
            return Err(self.level());
        }
        if let Some(parent) = self.node.parent.as_ref().map(|parent| &parent.account) {
            if !parent.try_add_packed(bytes, messages) {
                self.sub_packed(bytes, messages);
                return Err(parent.level());
            }
            if let Some(root) = parent.node.parent.as_ref().map(|parent| &parent.account) {
                if !root.try_add_packed(bytes, messages) {
                    parent.sub_packed(bytes, messages);
                    self.sub_packed(bytes, messages);
                    return Err(root.level());
                }
            }
        }
        Ok(())
    }

    fn refund_packed_all(&self, bytes: u64, messages: u64) {
        self.sub_packed(bytes, messages);
        if let Some(parent) = self.node.parent.as_ref().map(|parent| &parent.account) {
            parent.sub_packed(bytes, messages);
            if let Some(root) = parent.node.parent.as_ref().map(|parent| &parent.account) {
                root.sub_packed(bytes, messages);
            }
        }
    }

    fn try_add_node(&self, amount: Resources) -> bool {
        if amount.is_zero() {
            return true;
        }
        if let Some((index, delta)) = amount.single_dimension() {
            return self.try_add_dim(index, delta);
        }
        if let Some((bytes, messages)) = amount.envelope_dimensions() {
            return self.try_add_packed(bytes, messages);
        }
        let deltas = amount.as_array();
        for index in 0..4 {
            let delta = deltas[index];
            if delta == 0 {
                continue;
            }
            if !self.try_add_dim(index, delta) {
                for (rollback, &added) in deltas[..index].iter().enumerate() {
                    if added != 0 {
                        self.sub_dim(rollback, added);
                    }
                }
                return false;
            }
        }
        true
    }

    fn sub_node(&self, amount: Resources) {
        if let Some((index, delta)) = amount.single_dimension() {
            self.sub_dim(index, delta);
            return;
        }
        if let Some((bytes, messages)) = amount.envelope_dimensions() {
            self.sub_packed(bytes, messages);
            return;
        }
        let deltas = amount.as_array();
        for (index, &delta) in deltas.iter().enumerate() {
            if delta != 0 {
                self.sub_dim(index, delta);
            }
        }
    }

    pub(crate) fn is_child_of(&self, parent: &Self) -> bool {
        self.node
            .parent
            .as_ref()
            .is_some_and(|owner| owner.account.same(parent))
    }

    #[inline]
    pub(crate) fn reserve_bytes(&self, bytes: u64) -> Result<Reservation, String> {
        self.reserve_dim_status(0, bytes)
            .map(|()| Reservation {
                account: self.clone(),
                amount: Resources {
                    bytes,
                    ..Resources::default()
                },
            })
            .map_err(|level| String::from(crate::VmError::ResourceExhausted(level)))
    }

    pub fn reserve(&self, amount: Resources) -> Result<Reservation, String> {
        self.reserve_checked(amount).map_err(String::from)
    }

    pub(crate) fn reserve_checked(&self, amount: Resources) -> Result<Reservation, crate::VmError> {
        self.reserve_status(amount)
            .map_err(crate::VmError::ResourceExhausted)
    }

    pub(crate) fn reserve_envelope(&self, bytes: u64) -> Result<EnvelopeCharge, crate::VmError> {
        self.reserve_packed_status(bytes, 1)
            .map_err(crate::VmError::ResourceExhausted)?;
        Ok(EnvelopeCharge {
            account: self.clone(),
            bytes,
            messages: 1,
            gated: false,
        })
    }

    pub(crate) fn reserve_status(&self, amount: Resources) -> Result<Reservation, ResourceLevel> {
        if amount.is_zero() {
            return Ok(Reservation {
                account: self.clone(),
                amount,
            });
        }
        if let Some((index, delta)) = amount.single_dimension() {
            self.reserve_dim_status(index, delta)?;
            return Ok(Reservation {
                account: self.clone(),
                amount,
            });
        }
        let _guard = amount.is_multi_dimensional().then(|| {
            self.node
                .gate
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        if !self.try_add_node(amount) {
            return Err(self.level());
        }
        if let Some(parent) = self.node.parent.as_ref().map(|parent| &parent.account) {
            if !parent.try_add_node(amount) {
                self.sub_node(amount);
                return Err(parent.level());
            }
            if let Some(root) = parent.node.parent.as_ref().map(|parent| &parent.account) {
                if !root.try_add_node(amount) {
                    parent.sub_node(amount);
                    self.sub_node(amount);
                    return Err(root.level());
                }
            }
        }
        Ok(Reservation {
            account: self.clone(),
            amount,
        })
    }

    fn create(
        level: ResourceLevel,
        limit: Resources,
        parent: Option<Reservation>,
        bootstrap: u64,
    ) -> Self {
        let gate = parent.as_ref().map_or_else(
            || Arc::new(crate::native_sync::mutex(())),
            |parent| parent.account.node.gate.clone(),
        );
        Self {
            node: Arc::new(AccountNode {
                level,
                accounted_root: bootstrap != 0,
                // Packed-counter ceilings: limits above the field widths are
                // saturated since `used` could never report past them anyway.
                limit: Resources {
                    bytes: limit.bytes.min(BYTES_FIELD_MAX),
                    processes: limit.processes,
                    messages: limit.messages.min(MESSAGES_FIELD_MAX),
                    requests: limit.requests,
                },
                parent,
                used: [
                    AlignedCounter(AtomicU64::new(bootstrap)),
                    AlignedCounter(AtomicU64::new(0)),
                    AlignedCounter(AtomicU64::new(0)),
                ],
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
        if self.has_accounted_root() {
            return self.child_accounted(required, level, limit);
        }
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
            0,
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
        Ok(Self::create(level, limit, Some(charge), 0))
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

    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }

    pub(crate) fn same_vm(&self, other: &Self) -> bool {
        self.root().same(other.root())
    }

    pub(crate) fn has_accounted_root(&self) -> bool {
        self.root().node.accounted_root
    }
}

impl Reservation {
    pub fn amount(&self) -> Resources {
        self.amount
    }

    pub(crate) fn merge(&mut self, mut other: Self) {
        debug_assert!(self.account.same(&other.account));
        self.amount = self.amount.add_unchecked(other.amount);
        other.amount = Resources::default();
    }

    pub fn transfer(&mut self, target: &ResourceAccount) -> Result<(), String> {
        self.transfer_checked(target).map_err(String::from)
    }

    pub(crate) fn transfer_checked(
        &mut self,
        target: &ResourceAccount,
    ) -> Result<(), crate::VmError> {
        if self.account.same(target) {
            return Ok(());
        }
        if target.is_child_of(&self.account) {
            if self.amount.is_zero() {
                self.account = target.clone();
                return Ok(());
            }
            let guard = self.amount.is_multi_dimensional().then(|| {
                target
                    .node
                    .gate
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
            });
            if !target.try_add_node(self.amount) {
                return Err(crate::VmError::ResourceExhausted(target.level()));
            }
            drop(guard);
            self.account = target.clone();
            return Ok(());
        }
        if self.account.is_child_of(target) {
            if !self.amount.is_zero() {
                let guard = self.amount.is_multi_dimensional().then(|| {
                    target
                        .node
                        .gate
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                });
                self.account.sub_node(self.amount);
                drop(guard);
            }
            self.account = target.clone();
            return Ok(());
        }
        if !self.account.root().same(target.root()) {
            return Err(crate::VmError::Other(
                "resource transfer requires the same VM account",
            ));
        }
        if self.amount.is_zero() {
            self.account = target.clone();
            return Ok(());
        }
        let guard = self.amount.is_multi_dimensional().then(|| {
            target
                .node
                .gate
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        let source_path = self.account.path();
        let target_path = target.path();
        for (depth, node) in target_path.iter().enumerate() {
            if let Some(node) = node {
                let shared = source_path.iter().flatten().any(|source| node.same(source));
                if !shared && !node.try_add_node(self.amount) {
                    let level = node.level();
                    for previous in target_path[..depth].iter().flatten() {
                        if !source_path
                            .iter()
                            .flatten()
                            .any(|source| previous.same(source))
                        {
                            previous.sub_node(self.amount);
                        }
                    }
                    return Err(crate::VmError::ResourceExhausted(level));
                }
            }
        }
        for node in source_path.into_iter().flatten() {
            if !target_path
                .iter()
                .flatten()
                .any(|target_node| node.same(target_node))
            {
                node.sub_node(self.amount);
            }
        }
        drop(guard);
        self.account = target.clone();
        Ok(())
    }
}

impl EnvelopeCharge {
    pub(crate) fn from_reservation(mut reservation: Reservation) -> Self {
        let amount = reservation.amount;
        debug_assert!(amount.processes == 0 && amount.requests == 0 && amount.messages <= 1);
        let gated = amount.is_multi_dimensional();
        reservation.amount = Resources::default();
        Self {
            account: reservation.account.clone(),
            bytes: amount.bytes,
            messages: amount.messages as u8,
            gated,
        }
    }

    pub(crate) fn into_reservation(mut self) -> Reservation {
        let reservation = Reservation {
            account: self.account.clone(),
            amount: Resources {
                bytes: self.bytes,
                messages: u64::from(self.messages),
                ..Resources::default()
            },
        };
        // Zero the moved amounts so the Drop below is a no-op refund while
        // still releasing the account reference itself.
        self.bytes = 0;
        self.messages = 0;
        reservation
    }
}

impl Drop for EnvelopeCharge {
    fn drop(&mut self) {
        if self.bytes == 0 && self.messages == 0 {
            return;
        }
        let _guard = self.gated.then(|| {
            self.account
                .node
                .gate
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        if self.messages != 0 || self.bytes != 0 {
            self.account
                .refund_packed_all(self.bytes, u64::from(self.messages));
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some((index, delta)) = self.amount.single_dimension() {
            self.account.refund_dim_all(index, delta);
            return;
        }
        if !self.amount.is_zero() {
            let _guard = self.amount.is_multi_dimensional().then(|| {
                self.account
                    .node
                    .gate
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
            });
            self.account.sub_node(self.amount);
            if let Some(parent) = self
                .account
                .node
                .parent
                .as_ref()
                .map(|parent| &parent.account)
            {
                parent.sub_node(self.amount);
                if let Some(root) = parent.node.parent.as_ref().map(|parent| &parent.account) {
                    root.sub_node(self.amount);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_admission_and_public_diagnostic_share_all_levels_and_dimensions() {
        let limits = |scale| Resources {
            bytes: scale << 20,
            processes: scale,
            messages: scale,
            requests: scale,
        };
        for restricted in 0..3 {
            for dimension in 0..4 {
                let vm = ResourceAccount::vm_accounted(limits(4)).unwrap();
                let tenant = vm.tenant_accounted(limits(3)).unwrap();
                let account = tenant.process_accounted(limits(2)).unwrap();
                let levels = [&account, &tenant, &vm];
                let baseline = levels.map(ResourceAccount::used);
                let target = levels[restricted];
                let available = target.limit().subtract(target.used());
                let mut fill = Resources::default();
                let mut unit = Resources::default();
                match dimension {
                    0 => {
                        fill.bytes = available.bytes;
                        unit.bytes = 1;
                    }
                    1 => {
                        fill.processes = available.processes;
                        unit.processes = 1;
                    }
                    2 => {
                        fill.messages = available.messages;
                        unit.messages = 1;
                    }
                    3 => {
                        fill.requests = available.requests;
                        unit.requests = 1;
                    }
                    _ => unreachable!(),
                }
                let held = target.reserve_status(fill).unwrap();
                let before = levels.map(ResourceAccount::used);
                assert_eq!(account.reserve_status(unit).err(), Some(target.level()));
                assert_eq!(
                    account.reserve(unit).err().unwrap(),
                    format!("resource_exhausted at {:?} account", target.level())
                );
                assert_eq!(levels.map(ResourceAccount::used), before);
                drop(held);
                assert_eq!(levels.map(ResourceAccount::used), baseline);
                let ticket = account.reserve_status(unit).unwrap();
                assert_eq!(ticket.amount(), unit);
                assert_eq!(
                    levels.map(ResourceAccount::used),
                    baseline.map(|used| used.checked_add(unit).unwrap())
                );
                drop(ticket);
                assert_eq!(levels.map(ResourceAccount::used), baseline);
            }
        }
        // The packed word caps the bytes field at `BYTES_FIELD_MAX`; a
        // reservation that fills the field exactly must still succeed, and
        // anything beyond it is rejected as exhaustion.
        let vm = ResourceAccount::vm(Resources {
            bytes: u64::MAX,
            ..Resources::default()
        });
        let ticket = vm
            .reserve_status(Resources {
                bytes: BYTES_FIELD_MAX,
                ..Resources::default()
            })
            .unwrap();
        let before = vm.used();
        assert_eq!(
            vm.reserve_status(Resources {
                bytes: 1,
                ..Resources::default()
            })
            .err(),
            Some(ResourceLevel::Vm)
        );
        assert_eq!(vm.used(), before);
        drop(ticket);
        assert_eq!(vm.used(), Resources::default());
    }

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
    fn accounted_root_requires_its_bootstrap_budget_and_keeps_it_without_self_reservation() {
        let probe = ResourceAccount::vm_accounted(bytes(4096)).unwrap();
        let bootstrap = probe.used();
        assert_eq!(
            bootstrap.bytes,
            (std::mem::size_of::<AccountNode>()
                + std::mem::size_of::<Mutex<()>>()
                + 4 * std::mem::size_of::<usize>()
                + crate::native_sync::MUTEX_BYTES) as u64
        );
        assert!(bootstrap.bytes > node_bytes());
        assert_eq!(bootstrap, bytes(bootstrap.bytes));
        drop(probe);
        for limit in [0, bootstrap.bytes - 1] {
            assert!(ResourceAccount::vm_accounted(bytes(limit)).is_err());
        }
        let vm = ResourceAccount::vm_accounted(bootstrap).unwrap();
        let root = Arc::downgrade(&vm.node);
        let gate = Arc::downgrade(&vm.node.gate);
        let clone = vm.clone();
        assert_eq!(clone.used(), bootstrap);
        assert!(vm.reserve(bytes(1)).is_err());
        assert!(vm.tenant(bytes(0)).is_err());
        assert!(vm.tenant_accounted(bytes(0)).is_err());
        let ticket = vm.reserve(Resources::default()).unwrap();
        drop((vm, clone));
        assert_eq!(
            ResourceAccount {
                node: root.upgrade().unwrap()
            }
            .used(),
            bootstrap
        );
        drop(ticket);
        assert!(root.upgrade().is_none());
        assert!(gate.upgrade().is_none());
    }

    #[test]
    fn accounted_root_forces_legacy_child_paths_to_reserve_parent_metadata() {
        let bootstrap = ResourceAccount::vm_accounted(bytes(4096))
            .unwrap()
            .used()
            .bytes;
        let cost = node_bytes();
        for explicit in [false, true] {
            for (root_limit, tenant_limit, expected) in [
                (bootstrap + 2 * cost - 1, cost, "Vm"),
                (bootstrap + 2 * cost, cost - 1, "Tenant"),
            ] {
                let vm = ResourceAccount::vm_accounted(bytes(root_limit)).unwrap();
                let tenant = if explicit {
                    vm.tenant_accounted(bytes(tenant_limit))
                } else {
                    vm.tenant(bytes(tenant_limit))
                }
                .unwrap();
                let result = if explicit {
                    tenant.process_accounted(bytes(0))
                } else {
                    tenant.process(bytes(0))
                };
                assert!(result.err().unwrap().contains(expected));
                assert_eq!(vm.used(), bytes(bootstrap + cost));
                assert_eq!(tenant.used(), Resources::default());
                assert!(vm.process(bytes(0)).is_err());
                assert!(tenant.tenant(bytes(0)).is_err());
                drop(tenant);
                assert_eq!(vm.used(), bytes(bootstrap));
            }
        }
    }

    #[test]
    fn accounted_root_and_gate_survive_only_until_the_last_cross_thread_child_ticket() {
        let vm = ResourceAccount::vm_accounted(bytes(4096)).unwrap();
        let bootstrap = vm.used().bytes;
        let root = Arc::downgrade(&vm.node);
        let gate = Arc::downgrade(&vm.node.gate);
        let tenant = vm.tenant(bytes(1024)).unwrap();
        let process = tenant.process(bytes(7)).unwrap();
        let ticket = process.reserve(bytes(7)).unwrap();
        let before = bytes(bootstrap + 2 * node_bytes() + 7);
        assert_eq!(vm.used(), before);
        drop((vm, tenant, process));
        assert_eq!(
            ResourceAccount {
                node: root.upgrade().unwrap()
            }
            .used(),
            before
        );
        std::thread::spawn(move || drop(ticket)).join().unwrap();
        assert!(root.upgrade().is_none());
        assert!(gate.upgrade().is_none());
    }

    #[test]
    fn accounted_root_bootstrap_cannot_be_transferred_or_readmitted_with_business_tickets() {
        let bootstrap = ResourceAccount::vm_accounted(bytes(4096))
            .unwrap()
            .used()
            .bytes;
        let cost = node_bytes();
        let vm = ResourceAccount::vm_accounted(bytes(bootstrap + 2 * cost + 7)).unwrap();
        let tenant = vm.tenant(bytes(cost + 7)).unwrap();
        let process = tenant.process(bytes(7)).unwrap();
        let mut ticket = process.reserve(bytes(7)).unwrap();
        assert_eq!(vm.used(), vm.limit());
        ticket.transfer(&vm).unwrap();
        assert_eq!(vm.used(), vm.limit());
        assert_eq!(tenant.used(), bytes(cost));
        assert_eq!(process.used(), Resources::default());
        ticket.transfer(&process).unwrap();
        assert_eq!(vm.used(), vm.limit());
        let other = ResourceAccount::vm_accounted(bytes(4096)).unwrap();
        assert!(ticket.transfer(&other).is_err());
        assert_eq!(other.used(), bytes(bootstrap));
        assert_eq!(vm.used(), vm.limit());
        drop((ticket, process, tenant));
        assert_eq!(vm.used(), bytes(bootstrap));
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
        // Limits saturate at the packed byte field width, so the "unbounded"
        // scenario exercises the widest representable accounting instead.
        let vm = ResourceAccount::vm(bytes(u64::MAX));
        let tenant = vm.tenant_accounted(bytes(u64::MAX)).unwrap();
        let process = tenant.process_accounted(bytes(u64::MAX)).unwrap();
        let baseline = vm.used();
        assert!(vm.process_accounted(bytes(0)).is_err());
        assert!(tenant.tenant_accounted(bytes(0)).is_err());
        assert!(process.tenant_accounted(bytes(0)).is_err());
        assert!(process.process_accounted(bytes(0)).is_err());
        assert_eq!(vm.used(), baseline);
        let held = vm.reserve(bytes(BYTES_FIELD_MAX - baseline.bytes)).unwrap();
        assert!(vm.tenant_accounted(bytes(0)).is_err());
        assert!(tenant.process_accounted(bytes(0)).is_err());
        assert_eq!(vm.used(), bytes(BYTES_FIELD_MAX));
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
        for accounted in [false, true] {
            let bootstrap = if accounted {
                ResourceAccount::vm_accounted(bytes(4096))
                    .unwrap()
                    .used()
                    .bytes
            } else {
                0
            };
            let vm = if accounted {
                ResourceAccount::vm_accounted(bytes(bootstrap + 4 * cost)).unwrap()
            } else {
                ResourceAccount::vm(bytes(4 * cost))
            };
            let barrier = Arc::new(std::sync::Barrier::new(9));
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        let vm = &vm;
                        let barrier = &barrier;
                        scope.spawn(move || {
                            barrier.wait();
                            let account = if accounted {
                                vm.tenant(bytes(0))
                            } else {
                                vm.tenant_accounted(bytes(0))
                            };
                            barrier.wait();
                            barrier.wait();
                            account
                        })
                    })
                    .collect();
                barrier.wait();
                barrier.wait();
                assert_eq!(vm.used(), bytes(bootstrap + 4 * cost));
                barrier.wait();
                let accounts: Vec<_> = handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .collect();
                assert_eq!(accounts.iter().filter(|account| account.is_ok()).count(), 4);
                drop(accounts);
            });
            assert_eq!(vm.used(), bytes(bootstrap));
        }
    }
}
