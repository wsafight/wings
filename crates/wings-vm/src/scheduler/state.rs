use super::ready::WakeSignal;
use super::*;
use crate::RequestOutcome;
use std::ops::{Deref, Index, IndexMut};

pub(super) struct RequestTable {
    slots: Box<[RequestSlot]>,
    maybe_active: [u64; 4],
}

impl RequestTable {
    pub(super) fn new(slots: Box<[RequestSlot]>) -> Self {
        Self {
            slots,
            maybe_active: [0; 4],
        }
    }

    pub(super) fn clear_all(&mut self) {
        for slot in &mut self.slots {
            slot.pending.take();
        }
        self.maybe_active = [0; 4];
    }

    pub(super) fn is_empty_fast(&self) -> bool {
        self.maybe_active == [0; 4]
    }

    fn get_active_mut(&mut self, index: usize) -> Option<&mut RequestSlot> {
        let slot = self.slots.get_mut(index)?;
        self.maybe_active[index / 64] |= 1u64 << (index % 64);
        Some(slot)
    }

    fn retire_index(&mut self, index: usize) {
        retire_request(&mut self.slots[index]);
        if self.slots[index].pending.is_none() {
            self.maybe_active[index / 64] &= !(1u64 << (index % 64));
        }
    }
}

impl Deref for RequestTable {
    type Target = [RequestSlot];

    fn deref(&self) -> &Self::Target {
        &self.slots
    }
}

impl Index<usize> for RequestTable {
    type Output = RequestSlot;

    fn index(&self, index: usize) -> &Self::Output {
        &self.slots[index]
    }
}

impl IndexMut<usize> for RequestTable {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.maybe_active[index / 64] |= 1u64 << (index % 64);
        &mut self.slots[index]
    }
}

impl State {
    pub(super) fn open(&self) -> Result<(), String> {
        if self.closed {
            Err("scheduler is closed".into())
        } else {
            Ok(())
        }
    }

    pub(super) fn pid(&self, scheduler: u64, index: usize) -> SchedulerPid {
        SchedulerPid {
            scheduler,
            slot: index as u32,
            generation: self.slots[index].generation,
        }
    }

    pub(super) fn actor(&self, identity: u64, pid: SchedulerPid) -> Result<&Actor, String> {
        self.actor_checked(identity, pid).map_err(String::from)
    }

    pub(super) fn actor_checked(
        &self,
        identity: u64,
        pid: SchedulerPid,
    ) -> Result<&Actor, crate::VmError> {
        if identity != pid.scheduler {
            return Err(crate::VmError::ForeignPid);
        }
        self.slots
            .get(pid.slot as usize)
            .filter(|slot| slot.generation == pid.generation)
            .and_then(|slot| slot.actor.as_ref())
            .ok_or(crate::VmError::StalePid)
    }

    pub(super) fn actor_mut(
        &mut self,
        identity: u64,
        pid: SchedulerPid,
    ) -> Result<&mut Actor, String> {
        self.actor_mut_checked(identity, pid).map_err(String::from)
    }

    pub(super) fn actor_mut_checked(
        &mut self,
        identity: u64,
        pid: SchedulerPid,
    ) -> Result<&mut Actor, crate::VmError> {
        if identity != pid.scheduler {
            return Err(crate::VmError::ForeignPid);
        }
        self.slots
            .get_mut(pid.slot as usize)
            .filter(|slot| slot.generation == pid.generation)
            .and_then(|slot| slot.actor.as_mut())
            .ok_or(crate::VmError::StalePid)
    }

    pub(super) fn record_exit(&mut self, capability: i64, reason: i64) {
        let idx = self.recent_exit_cursor % self.recent_exits.len();
        self.recent_exits[idx] = (capability, reason);
        self.recent_exit_cursor = self.recent_exit_cursor.wrapping_add(1);
    }

    pub(super) fn lookup_recent_exit(&self, capability: i64) -> Option<i64> {
        let len = self.recent_exits.len();
        for offset in 0..len {
            let idx = self.recent_exit_cursor.wrapping_add(len - 1 - offset) % len;
            let (cap, reason) = self.recent_exits[idx];
            if cap == capability && cap > 0 {
                return Some(reason);
            }
        }
        None
    }

    #[cfg(test)]
    pub(super) fn retire(&mut self, identity: u64, pid: SchedulerPid) -> Option<Actor> {
        self.retire_with_reason(identity, pid, 0)
            .map(RetiredActor::into_actor)
    }

    pub(super) fn retire_with_reason(
        &mut self,
        identity: u64,
        pid: SchedulerPid,
        exit_reason: i64,
    ) -> Option<RetiredActor> {
        let dead_capability = self.actor(identity, pid).ok()?.capability;
        self.record_exit(dead_capability, exit_reason);
        self.remove_ready(pid.slot as usize);
        let slot = &mut self.slots[pid.slot as usize];
        let actor = slot.actor.take()?;
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
        self.release_slot(pid.slot as usize);
        if !self.requests.is_empty_fast() {
            for word_index in 0..4 {
                let mut bits = self.requests.maybe_active[word_index];
                while bits != 0 {
                    let bit = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let index = word_index * 64 + bit;
                    if index >= self.requests.slots.len() {
                        self.requests.maybe_active[word_index] &= !(1u64 << bit);
                        continue;
                    }
                    if let Some(pending) = &mut self.requests.slots[index].pending {
                        if pending.owner == pid {
                            pending.completion.take();
                        }
                        if pending.target == pid {
                            pending.publisher = false;
                            if pending.outcome.is_none() {
                                pending.outcome = Some(if Instant::now() >= pending.deadline {
                                    RequestOutcome::TimedOut
                                } else {
                                    RequestOutcome::Failed
                                });
                            }
                        }
                    }
                    self.requests.retire_index(index);
                }
            }
        }
        let mut retired_timers: Option<Box<ScheduledTimer>> = None;
        let mut timer_cursor = &mut self.timers;
        while timer_cursor.is_some() {
            if timer_cursor.as_ref().expect("timer node").target == pid {
                let mut removed = timer_cursor.take().expect("removed timer");
                *timer_cursor = removed.next.take();
                self.timer_count -= 1;
                removed.next = retired_timers;
                retired_timers = Some(removed);
            } else {
                timer_cursor = &mut timer_cursor.as_mut().expect("timer node").next;
            }
        }
        let mut retired_monitors: Option<Box<MonitorEntry>> = None;
        let mut pending_monitors = self.pending_monitors.take();
        let mut mon_cursor = &mut self.monitors;
        while mon_cursor.is_some() {
            let entry = mon_cursor.as_ref().expect("monitor node");
            if entry.owner == pid {
                let mut removed = mon_cursor.take().expect("removed monitor");
                *mon_cursor = removed.next.take();
                self.monitor_count -= 1;
                removed.next = retired_monitors;
                retired_monitors = Some(removed);
            } else if entry.target_capability == dead_capability && entry.reason.is_none() {
                let mut moved = mon_cursor.take().expect("pending monitor");
                *mon_cursor = moved.next.take();
                moved.reason = Some(exit_reason);
                moved.next = pending_monitors;
                pending_monitors = Some(moved);
            } else {
                mon_cursor = &mut mon_cursor.as_mut().expect("monitor node").next;
            }
        }
        self.pending_monitors = pending_monitors;
        let mut pending_cursor = &mut self.pending_monitors;
        while pending_cursor.is_some() {
            if pending_cursor.as_ref().expect("pending monitor").owner == pid {
                let mut removed = pending_cursor.take().expect("removed pending monitor");
                *pending_cursor = removed.next.take();
                self.monitor_count -= 1;
                removed.next = retired_monitors;
                retired_monitors = Some(removed);
            } else {
                pending_cursor = &mut pending_cursor.as_mut().expect("pending monitor").next;
            }
        }
        Some(RetiredActor {
            actor,
            timers: retired_timers,
            monitors: retired_monitors,
        })
    }

    pub(super) fn finish(&mut self, key: RequestKey, outcome: RequestOutcome) {
        let Some(slot) = self
            .requests
            .get_active_mut(key.slot)
            .filter(|slot| slot.generation == key.generation)
        else {
            return;
        };
        if let Some(pending) = &mut slot.pending {
            if pending.outcome.is_none() {
                pending.outcome = Some(if Instant::now() >= pending.deadline {
                    RequestOutcome::TimedOut
                } else {
                    outcome
                });
            }
            pending.publisher = false;
        }
        self.requests.retire_index(key.slot);
    }

    pub(super) fn cancel(
        &mut self,
        owner: SchedulerPid,
        capability: i64,
    ) -> Result<(), crate::VmError> {
        let (index, _) = wings_abi::decode_request_capability(owner.scheduler, capability)
            .ok_or(crate::VmError::InvalidCapability)?;
        let pending = self
            .requests
            .get_active_mut(index)
            .filter(|slot| slot.generation != 0)
            .and_then(|slot| slot.pending.as_mut())
            .filter(|pending| {
                pending.owner == owner
                    && pending.capability == capability
                    && pending.completion.is_some()
            })
            .ok_or(crate::VmError::InvalidCapability)?;
        if pending.outcome.is_none() {
            pending.outcome = Some(if Instant::now() >= pending.deadline {
                RequestOutcome::TimedOut
            } else {
                RequestOutcome::Cancelled
            });
        }
        Ok(())
    }

    pub(super) fn maintain(&mut self, identity: u64) -> WakeSignal {
        if self.requests.is_empty_fast() && self.timers.is_none() && self.pending_monitors.is_none()
        {
            return WakeSignal::NONE;
        }
        let mut wake = WakeSignal::NONE;
        let now = Instant::now();
        {
            let mut pending = self.timers.take();
            let mut kept_head: Option<Box<ScheduledTimer>> = None;
            let mut kept_tail = &mut kept_head;
            while let Some(mut node) = pending {
                if node.deadline > now {
                    *kept_tail = Some(node);
                    break;
                }
                pending = node.next.take();
                let target = node.target;
                let can_deliver = match self.actor(identity, target) {
                    Ok(actor) => {
                        !actor.stopping
                            && actor.mailbox.count() + actor.mailbox.in_flight()
                                < actor.mailbox.capacity()
                    }
                    Err(_) => {
                        self.timer_count -= 1;
                        drop(node);
                        continue;
                    }
                };
                if !can_deliver {
                    let slot = kept_tail.insert(node);
                    kept_tail = &mut slot.next;
                    continue;
                }
                let envelope = node.envelope;
                match self
                    .actor_mut(identity, target)
                    .expect("timer target")
                    .mailbox
                    .push_admitted(envelope)
                {
                    None => {
                        self.timer_count -= 1;
                        wake = wake.combine(self.enqueue(target));
                    }
                    Some(envelope) => {
                        node.envelope = envelope;
                        let slot = kept_tail.insert(node);
                        kept_tail = &mut slot.next;
                    }
                }
            }
            self.timers = kept_head;
        }
        {
            let mut pending = self.pending_monitors.take();
            let mut kept_head: Option<Box<MonitorEntry>> = None;
            let mut kept_tail = &mut kept_head;
            while let Some(mut entry) = pending {
                pending = entry.next.take();
                let Some(reason) = entry.reason else {
                    let slot = kept_tail.insert(entry);
                    kept_tail = &mut slot.next;
                    continue;
                };
                let owner = entry.owner;
                let can_deliver = match self.actor(identity, owner) {
                    Ok(actor) => {
                        !actor.stopping
                            && actor.mailbox.count() + actor.mailbox.in_flight()
                                < actor.mailbox.capacity()
                    }
                    Err(_) => {
                        self.monitor_count -= 1;
                        drop(entry);
                        continue;
                    }
                };
                if !can_deliver {
                    let slot = kept_tail.insert(entry);
                    kept_tail = &mut slot.next;
                    continue;
                }
                let message = Envelope {
                    handler: entry.handler_index,
                    arguments: MessageArguments::from_inline(&[entry.target_capability, reason]),
                    max_steps: entry.max_steps,
                    lists: None,
                    bytes: None,
                    reply: Reply::None,
                    fast_eligible: entry.fast_eligible,
                    _charge: entry.envelope_charge,
                };
                match self
                    .actor_mut(identity, owner)
                    .expect("monitor owner")
                    .mailbox
                    .push_admitted(message)
                {
                    None => {
                        self.monitor_count -= 1;
                        wake = wake.combine(self.enqueue(owner));
                    }
                    Some(message) => {
                        entry.envelope_charge = message._charge;
                        let slot = kept_tail.insert(entry);
                        kept_tail = &mut slot.next;
                    }
                }
            }
            self.pending_monitors = kept_head;
        }
        if self.requests.is_empty_fast() {
            return wake;
        }
        let mut ready_completions = [0usize; MAX_REQUESTS];
        let mut ready_completion_count = 0;
        for word_index in 0..4 {
            let mut bits = self.requests.maybe_active[word_index];
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let index = word_index * 64 + bit;
                if index >= self.requests.slots.len() {
                    self.requests.maybe_active[word_index] &= !(1u64 << bit);
                    continue;
                }
                let Some(pending) = self.requests.slots[index].pending.as_mut() else {
                    self.requests.maybe_active[word_index] &= !(1u64 << bit);
                    continue;
                };
                if pending.outcome.is_none() && now >= pending.deadline {
                    pending.outcome = Some(RequestOutcome::TimedOut);
                }
                if pending.outcome.is_some() && pending.completion.is_some() {
                    ready_completions[ready_completion_count] = index;
                    ready_completion_count += 1;
                }
            }
        }
        for &index in &ready_completions[..ready_completion_count] {
            let Some(pending) = self.requests.slots[index].pending.as_ref() else {
                self.requests.maybe_active[index / 64] &= !(1u64 << (index % 64));
                continue;
            };
            let Ok(actor) = self.actor(identity, pending.owner) else {
                self.requests.slots[index]
                    .pending
                    .as_mut()
                    .expect("pending completion")
                    .completion
                    .take();
                self.requests.retire_index(index);
                continue;
            };
            if pending.outcome.is_none()
                || actor.stopping
                || actor.mailbox.count() + actor.mailbox.in_flight() >= actor.mailbox.capacity()
            {
                continue;
            }
            let pending = self.requests.slots[index]
                .pending
                .as_mut()
                .expect("ready completion");
            let completion = pending.completion.take().expect("completion route");
            let outcome = pending.outcome.clone().expect("terminal completion");
            let mut lists = ListPayload::default();
            let mut bytes = BytePayload::default();
            let mut arguments = [0; 64];
            let mut count = 3;
            if let Some(wings_abi::ValueType::RecordRef(schema)) =
                completion.handler.schema().completion_reply()
            {
                let (reply, status) = match &outcome {
                    RequestOutcome::ReplyRecord(reply) => (Some(reply), 0),
                    RequestOutcome::TimedOut => (None, 1),
                    RequestOutcome::Cancelled => (None, 2),
                    _ => (None, 3),
                };
                let records = &completion.handler.module.manifest().records;
                let response = crate::RecordReply::completion(
                    reply,
                    schema,
                    records,
                    pending.capability,
                    status,
                )
                .or_else(|_| {
                    crate::RecordReply::completion(None, schema, records, pending.capability, 3)
                })
                .expect("validated named record completion");
                arguments = response.arguments;
                count = response.count;
                lists = response.lists;
                bytes = response.bytes;
            } else {
                let (status, value) = match outcome {
                    RequestOutcome::Reply(value) => (0, value),
                    RequestOutcome::ReplyList(value) => {
                        let length = value.as_slice().len();
                        if lists.values.try_extend(value.as_slice()).is_err() {
                            pending.completion = Some(completion);
                            continue;
                        }
                        (0, ListPayload::encode(0, length))
                    }
                    RequestOutcome::ReplyBytes(value) => {
                        let length = value.as_slice().len();
                        bytes = BytePayload::completion(Some(value));
                        (0, ListPayload::encode(0, length))
                    }
                    RequestOutcome::ReplyString(value) => {
                        let length = value.as_str().len();
                        bytes = BytePayload::string_completion(Some(value));
                        (0, ListPayload::encode(0, length))
                    }
                    RequestOutcome::ReplyRecord(_) => (3, 0),
                    RequestOutcome::TimedOut => (1, 0),
                    RequestOutcome::Cancelled => (2, 0),
                    RequestOutcome::Failed => (3, 0),
                };
                arguments[..3].copy_from_slice(&[pending.capability, status, value]);
            }
            let owner = pending.owner;
            let message_arguments = match MessageArguments::try_from_slice(&arguments[..count]) {
                Ok(arguments) => arguments,
                Err(_) => {
                    pending.completion = Some(completion);
                    continue;
                }
            };
            if completion.handler.schema().completion_reply() == Some(wings_abi::ValueType::ListRef)
            {
                lists.mask = 4;
            }
            if completion.handler.schema().completion_reply()
                == Some(wings_abi::ValueType::BytesRef)
                && bytes.mask == 0
            {
                bytes = BytePayload::completion(None);
            }
            if completion.handler.schema().completion_reply()
                == Some(wings_abi::ValueType::StringRef)
                && bytes.mask == 0
            {
                bytes = BytePayload::string_completion(None);
            }
            let fast_eligible = envelope_fast_eligible(&completion.handler, &lists, &bytes);
            let message = Envelope {
                handler: completion.handler.index,
                arguments: message_arguments,
                max_steps: completion.max_steps,
                lists: box_list_payload(lists),
                bytes: box_byte_payload(bytes),
                reply: Reply::None,
                fast_eligible,
                _charge: crate::resources::EnvelopeCharge::from_reservation(completion.charge),
            };
            match self
                .actor_mut(identity, owner)
                .expect("completion owner")
                .mailbox
                .push_admitted(message)
            {
                None => {
                    self.requests.retire_index(index);
                    wake = wake.combine(self.enqueue(owner));
                }
                Some(returned) => {
                    self.requests.slots[index]
                        .pending
                        .as_mut()
                        .expect("ready completion")
                        .completion = Some(Completion {
                        handler: completion.handler,
                        max_steps: completion.max_steps,
                        charge: returned._charge.into_reservation(),
                    });
                }
            }
        }
        wake
    }

    pub(super) fn target(
        &self,
        identity: u64,
        capability: i64,
    ) -> Result<SchedulerPid, crate::VmError> {
        let (index, generation) = wings_abi::decode_capability(identity, capability)
            .ok_or(crate::VmError::InvalidCapability)?;
        let slot = self
            .slots
            .get(index)
            .filter(|slot| slot.generation == generation)
            .ok_or(crate::VmError::InvalidCapability)?;
        if slot.actor.as_ref().is_some_and(|actor| !actor.stopping) {
            Ok(self.pid(identity, index))
        } else {
            Err(crate::VmError::InvalidCapability)
        }
    }
}

fn retire_request(slot: &mut RequestSlot) {
    if slot
        .pending
        .as_ref()
        .is_some_and(|pending| !pending.publisher && pending.completion.is_none())
    {
        slot.pending.take();
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
    }
}

impl Shared {
    pub(super) fn effect(
        self: &Arc<Self>,
        pid: SchedulerPid,
        effect: crate::execution::ProcessEffect,
        budget: u64,
        pool: &StackPool,
    ) -> (u32, i64) {
        if matches!(
            effect.operation,
            wings_abi::EFFECT_SPAWN_SELF | wings_abi::EFFECT_SPAWN_ACTOR
        ) && effect.arguments.is_empty()
        {
            let options = {
                let state = self.lock();
                let spawn_module = if effect.operation == wings_abi::EFFECT_SPAWN_SELF {
                    state
                        .actor(self.identity, pid)
                        .ok()
                        .map(|actor| actor.module.clone())
                } else {
                    state
                        .modules
                        .as_ref()
                        .and_then(|(mods, _)| mods.get(effect.handler).cloned())
                };
                state
                    .actor(self.identity, pid)
                    .ok()
                    .filter(|actor| !actor.stopping && !state.closed)
                    .zip(spawn_module)
                    .map(|(actor, target_mod)| {
                        (
                            target_mod,
                            actor.account.limit(),
                            actor.mailbox.capacity(),
                            actor.heap_records,
                        )
                    })
            };
            let Some((module, limits, capacity, heap)) = options else {
                return (wings_abi::STATUS_SPAWN_REJECTED, 0);
            };
            let handle = SchedulerHandle {
                shared: self.clone(),
            };
            return match handle.spawn_with_options(
                &module,
                limits,
                capacity,
                heap,
                Some(pool),
                None,
            ) {
                Ok(child) => match self.lock().actor(self.identity, child) {
                    Ok(actor) => (wings_abi::STATUS_OK, actor.capability),
                    Err(_) => (wings_abi::STATUS_SPAWN_REJECTED, 0),
                },
                Err(_) => (wings_abi::STATUS_SPAWN_REJECTED, 0),
            };
        }
        let mut state = self.lock();
        if state.closed || state.actor_checked(self.identity, pid).is_err() {
            return (wings_abi::STATUS_CANCELLED, 0);
        }
        let mut wake = WakeSignal::NONE;
        let result: Result<i64, crate::VmError> = (|| {
            let actor = state.actor_checked(self.identity, pid)?;
            if actor.stopping {
                return Err(crate::VmError::StalePid);
            }
            let module = actor.module.clone();
            match effect.operation {
                wings_abi::EFFECT_SELF_PID if effect.arguments.is_empty() => Ok(actor.capability),
                wings_abi::EFFECT_CANCEL_REQUEST => {
                    state.cancel(pid, effect.target)?;
                    wake = state.wake_all_parked();
                    Ok(0)
                }
                wings_abi::EFFECT_MONITOR => {
                    if !effect.arguments.is_empty()
                        || effect.target <= 0
                        || state.monitor_count >= MAX_MONITORS
                    {
                        return Err(crate::VmError::InvalidArguments(
                            "invalid monitor target or capacity",
                        ));
                    }
                    let handler = module.handler_by_index(effect.handler)?;
                    let schema = handler.schema();
                    if schema.is_completion()
                        || schema.reply.is_some()
                        || schema.parameters.len() != 2
                        || schema.parameters[0].1 != wings_abi::ValueType::Pid
                        || schema.parameters[1].1 != wings_abi::ValueType::Int
                    {
                        return Err(crate::VmError::InvalidHandler);
                    }
                    let owner = state.actor_checked(self.identity, pid)?;
                    let envelope_charge = owner
                        .account
                        .reserve_envelope(std::mem::size_of::<Envelope>() as u64)?;
                    let node_charge = owner.account.reserve_checked(Resources {
                        bytes: std::mem::size_of::<MonitorEntry>() as u64,
                        ..Resources::default()
                    })?;
                    let reason = if state.target(self.identity, effect.target).is_ok() {
                        None
                    } else {
                        Some(state.lookup_recent_exit(effect.target).unwrap_or(-1))
                    };
                    let fast_eligible = handler.fast_meta().fast_scalar();
                    let entry = Box::new(MonitorEntry {
                        owner: pid,
                        target_capability: effect.target,
                        handler_index: handler.index,
                        fast_eligible,
                        max_steps: budget,
                        reason,
                        envelope_charge,
                        _node_charge: node_charge,
                        next: None,
                    });
                    if reason.is_some() {
                        let head = &mut state.pending_monitors;
                        let mut entry = entry;
                        entry.next = head.take();
                        *head = Some(entry);
                    } else {
                        let head = &mut state.monitors;
                        let mut entry = entry;
                        entry.next = head.take();
                        *head = Some(entry);
                    }
                    state.monitor_count += 1;
                    if reason.is_some() {
                        wake = state.maintain(self.identity);
                    }
                    Ok(0)
                }
                wings_abi::EFFECT_SEND
                | wings_abi::EFFECT_SEND_SELF
                | wings_abi::EFFECT_SEND_ACTOR
                | wings_abi::EFFECT_SEND_AFTER_SELF
                | wings_abi::EFFECT_SEND_AFTER
                | wings_abi::EFFECT_SEND_AFTER_ACTOR
                | wings_abi::EFFECT_REQUEST
                | wings_abi::EFFECT_REQUEST_ACTOR => {
                    let is_request = matches!(
                        effect.operation,
                        wings_abi::EFFECT_REQUEST | wings_abi::EFFECT_REQUEST_ACTOR
                    );
                    let is_after = matches!(
                        effect.operation,
                        wings_abi::EFFECT_SEND_AFTER_SELF
                            | wings_abi::EFFECT_SEND_AFTER
                            | wings_abi::EFFECT_SEND_AFTER_ACTOR
                    );
                    let is_cross_actor = matches!(
                        effect.operation,
                        wings_abi::EFFECT_SEND_ACTOR
                            | wings_abi::EFFECT_SEND_AFTER_ACTOR
                            | wings_abi::EFFECT_REQUEST_ACTOR
                    );
                    let target = if matches!(
                        effect.operation,
                        wings_abi::EFFECT_SEND_SELF | wings_abi::EFFECT_SEND_AFTER_SELF
                    ) {
                        pid
                    } else {
                        state.target(self.identity, effect.target)?
                    };
                    let (target_module, handler_idx) = if is_cross_actor {
                        let (actor_idx, h_idx) = wings_abi::decode_actor_handler(effect.handler);
                        let reg_mod = state
                            .modules
                            .as_ref()
                            .and_then(|(mods, _)| mods.get(actor_idx).cloned())
                            .ok_or(crate::VmError::InvalidHandler)?;
                        (reg_mod, h_idx)
                    } else {
                        (module.clone(), effect.handler)
                    };
                    let target_actor = state.actor_checked(self.identity, target)?;
                    let handler = target_module.handler_by_index(handler_idx)?;
                    let schema = handler.schema();
                    let arguments = effect.arguments.as_slice();
                    validate_checked(
                        target_actor,
                        &handler,
                        arguments,
                        false,
                        &effect.lists,
                        &effect.bytes,
                    )?;
                    if is_after {
                        if schema.reply.is_some()
                            || !(0..=86_400_000).contains(&effect.timeout_ms)
                            || state.timer_count >= MAX_TIMERS
                        {
                            return Err(crate::VmError::InvalidArguments(
                                "invalid send_after parameters or timer capacity",
                            ));
                        }
                        let deadline = Instant::now()
                            .checked_add(Duration::from_millis(effect.timeout_ms as u64))
                            .ok_or(crate::VmError::InvalidArguments("timer deadline overflow"))?;
                        let node_bytes = std::mem::size_of::<ScheduledTimer>()
                            .saturating_sub(std::mem::size_of::<Envelope>());
                        let node_charge = target_actor.account.reserve_checked(Resources {
                            bytes: node_bytes as u64,
                            ..Resources::default()
                        })?;
                        let message = delayed_envelope_checked(
                            target_actor,
                            &handler,
                            arguments,
                            budget,
                            effect.lists,
                            effect.bytes,
                            Reply::None,
                        )?;
                        let mut cursor = &mut state.timers;
                        let mut inserted_at_head = true;
                        while cursor
                            .as_ref()
                            .is_some_and(|existing| existing.deadline <= deadline)
                        {
                            inserted_at_head = false;
                            cursor = &mut cursor.as_mut().expect("timer node").next;
                        }
                        let next = cursor.take();
                        *cursor = Some(Box::new(ScheduledTimer {
                            target,
                            deadline,
                            envelope: message,
                            _node_charge: node_charge,
                            next,
                        }));
                        state.timer_count += 1;
                        if effect.timeout_ms == 0 {
                            wake = state.maintain(self.identity);
                        } else if inserted_at_head && effect.timeout_ms <= 1 {
                            wake = state.wake_all_parked();
                        }
                        return Ok(0);
                    }
                    if !is_request {
                        if schema.reply.is_some() {
                            return Err(crate::VmError::InvalidHandler);
                        }
                        let message = envelope_checked(
                            target_actor,
                            &handler,
                            arguments,
                            budget,
                            effect.lists,
                            effect.bytes,
                            false,
                            Reply::None,
                        )?;
                        state
                            .actor_mut_checked(self.identity, target)?
                            .mailbox
                            .push_checked(message)?;
                        wake = state.enqueue(target);
                        return Ok(0);
                    }
                    if !schema.reply.is_some_and(|ty| ty.is_supported_reply())
                        || !(0..=86_400_000).contains(&effect.timeout_ms)
                    {
                        return Err(crate::VmError::InvalidHandler);
                    }
                    let callback = module.handler_by_index(effect.callback)?;
                    let valid_callback = match (callback.schema().completion_reply(), schema.reply)
                    {
                        (Some(cb_ty), Some(rep_ty)) => wings_abi::equivalent_types(
                            &module.manifest().records,
                            cb_ty,
                            &target_module.manifest().records,
                            rep_ty,
                        ),
                        _ => false,
                    };
                    if !valid_callback {
                        return Err(crate::VmError::InvalidHandler);
                    }
                    let index = state
                        .requests
                        .iter()
                        .position(|slot| slot.generation != 0 && slot.pending.is_none())
                        .ok_or(crate::VmError::RequestTableFull)?;
                    let owner = state.actor_checked(self.identity, pid)?;
                    let request_charge = owner.account.reserve_checked(Resources {
                        requests: 1,
                        ..Resources::default()
                    })?;
                    let list_overflow = match callback.schema().completion_reply() {
                        Some(wings_abi::ValueType::ListRef)
                        | Some(wings_abi::ValueType::RecordRef(_)) => {
                            ListPayload::max_overflow_bytes()
                        }
                        _ => 0,
                    };
                    let completion_charge = owner.account.reserve_checked(Resources {
                        messages: 1,
                        bytes: std::mem::size_of::<Envelope>() as u64
                            + MessageArguments::overflow_bytes(callback.schema().parameters.len())
                            + list_overflow,
                        ..Resources::default()
                    })?;
                    let generation = state.requests[index].generation;
                    let capability = wings_abi::encode_request_capability(
                        self.identity,
                        index as u32,
                        generation,
                    )
                    .ok_or(crate::VmError::InvalidCapability)?;
                    let key = RequestKey {
                        slot: index,
                        generation,
                    };
                    let message = envelope_checked(
                        target_actor,
                        &handler,
                        arguments,
                        budget,
                        effect.lists,
                        effect.bytes,
                        false,
                        Reply::Language(key),
                    )?;
                    let pending = Pending {
                        owner: pid,
                        target,
                        capability,
                        deadline: Instant::now()
                            .checked_add(Duration::from_millis(effect.timeout_ms as u64))
                            .ok_or(crate::VmError::InvalidArguments(
                                "request deadline overflow",
                            ))?,
                        outcome: None,
                        publisher: true,
                        completion: Some(Completion {
                            handler: callback,
                            max_steps: budget,
                            charge: completion_charge,
                        }),
                        _charge: request_charge,
                    };
                    state
                        .actor_mut_checked(self.identity, target)?
                        .mailbox
                        .push_checked(message)?;
                    state.requests[index].pending = Some(pending);
                    wake = state.enqueue(target);
                    Ok(capability)
                }
                _ => Err(crate::VmError::InvalidHandler),
            }
        })();
        self.publish_signal(&mut state);
        drop(state);
        wake.fire(&self.changed);
        match result {
            Ok(value) => (wings_abi::STATUS_OK, value),
            Err(_) => (
                match effect.operation {
                    wings_abi::EFFECT_REQUEST
                    | wings_abi::EFFECT_REQUEST_ACTOR
                    | wings_abi::EFFECT_CANCEL_REQUEST
                    | wings_abi::EFFECT_MONITOR => wings_abi::STATUS_REQUEST_REJECTED,
                    wings_abi::EFFECT_SEND
                    | wings_abi::EFFECT_SEND_ACTOR
                    | wings_abi::EFFECT_SEND_SELF
                    | wings_abi::EFFECT_SEND_AFTER_SELF
                    | wings_abi::EFFECT_SEND_AFTER
                    | wings_abi::EFFECT_SEND_AFTER_ACTOR => wings_abi::STATUS_SEND_REJECTED,
                    _ => wings_abi::STATUS_INVALID_CONTROL,
                },
                0,
            ),
        }
    }
}
