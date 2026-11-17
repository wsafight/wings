use super::*;
use crate::RequestOutcome;

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
        if identity != pid.scheduler {
            return Err("Pid belongs to another scheduler".into());
        }
        self.slots
            .get(pid.slot as usize)
            .filter(|slot| slot.generation == pid.generation)
            .and_then(|slot| slot.actor.as_ref())
            .ok_or_else(|| "stale scheduler Pid".into())
    }

    pub(super) fn actor_mut(
        &mut self,
        identity: u64,
        pid: SchedulerPid,
    ) -> Result<&mut Actor, String> {
        if identity != pid.scheduler {
            return Err("Pid belongs to another scheduler".into());
        }
        self.slots
            .get_mut(pid.slot as usize)
            .filter(|slot| slot.generation == pid.generation)
            .and_then(|slot| slot.actor.as_mut())
            .ok_or_else(|| "stale scheduler Pid".into())
    }

    pub(super) fn retire(&mut self, identity: u64, pid: SchedulerPid) -> Option<Actor> {
        if self.actor(identity, pid).is_err() {
            return None;
        }
        self.remove_ready(pid.slot as usize);
        let slot = &mut self.slots[pid.slot as usize];
        let actor = slot.actor.take();
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
        self.release_slot(pid.slot as usize);
        for request in &mut self.requests {
            if let Some(pending) = &mut request.pending {
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
            retire_request(request);
        }
        actor
    }

    pub(super) fn finish(&mut self, key: RequestKey, outcome: RequestOutcome) {
        let Some(slot) = self
            .requests
            .get_mut(key.slot)
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
        retire_request(slot);
    }

    pub(super) fn cancel(&mut self, owner: SchedulerPid, capability: i64) -> Result<(), String> {
        let pending = self
            .requests
            .iter_mut()
            .filter_map(|slot| slot.pending.as_mut())
            .find(|pending| {
                pending.owner == owner
                    && pending.capability == capability
                    && pending.completion.is_some()
            })
            .ok_or("invalid request capability")?;
        if pending.outcome.is_none() {
            pending.outcome = Some(if Instant::now() >= pending.deadline {
                RequestOutcome::TimedOut
            } else {
                RequestOutcome::Cancelled
            });
        }
        Ok(())
    }

    pub(super) fn maintain(&mut self, identity: u64) {
        let now = Instant::now();
        for request in &mut self.requests {
            if let Some(pending) = &mut request.pending {
                if pending.outcome.is_none() && now >= pending.deadline {
                    pending.outcome = Some(RequestOutcome::TimedOut);
                }
            }
        }
        for index in 0..self.requests.len() {
            let Some(pending) = self.requests[index].pending.as_ref() else {
                continue;
            };
            if pending.completion.is_none() {
                continue;
            }
            let Ok(actor) = self.actor(identity, pending.owner) else {
                self.requests[index]
                    .pending
                    .as_mut()
                    .expect("pending completion")
                    .completion
                    .take();
                retire_request(&mut self.requests[index]);
                continue;
            };
            if pending.outcome.is_none()
                || actor.stopping
                || actor.mailbox.count + usize::from(actor.starting) == actor.mailbox.slots.len()
            {
                continue;
            }
            let pending = self.requests[index]
                .pending
                .as_mut()
                .expect("ready completion");
            let completion = pending.completion.take().expect("completion route");
            let mut lists = ListPayload::default();
            let mut bytes = BytePayload::default();
            let (status, value) = match pending.outcome.clone().expect("terminal completion") {
                RequestOutcome::Reply(value) => (0, value),
                RequestOutcome::ReplyList(value) => {
                    lists.count = value.as_slice().len();
                    lists.values[..lists.count].copy_from_slice(value.as_slice());
                    (0, ListPayload::encode(0, lists.count))
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
                RequestOutcome::TimedOut => (1, 0),
                RequestOutcome::Cancelled => (2, 0),
                RequestOutcome::Failed => (3, 0),
            };
            let mut arguments = [0; 64];
            arguments[..3].copy_from_slice(&[pending.capability, status, value]);
            let owner = pending.owner;
            retire_request(&mut self.requests[index]);
            let record_mask = u64::from(
                completion.handler.schema().completion_reply() == Some(wings_abi::ValueType::Ref),
            ) << 2;
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
            let message = Envelope {
                handler: completion.handler,
                arguments,
                count: 3,
                max_steps: completion.max_steps,
                record_mask,
                lists,
                bytes,
                system: true,
                reply: Reply::None,
                _charge: completion.charge,
            };
            self.actor_mut(identity, owner)
                .expect("completion owner")
                .mailbox
                .push(message)
                .expect("reserved completion slot");
            self.enqueue(owner);
        }
    }

    pub(super) fn target(&self, identity: u64, capability: i64) -> Result<SchedulerPid, String> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, slot)| {
                slot.actor
                    .as_ref()
                    .is_some_and(|actor| actor.capability == capability && !actor.stopping)
            })
            .map(|(index, _)| self.pid(identity, index))
            .ok_or_else(|| "invalid scheduler capability".into())
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
        if effect.operation == wings_abi::EFFECT_SPAWN_SELF && effect.count == 0 {
            let options = {
                let state = self.lock();
                state
                    .actor(self.identity, pid)
                    .ok()
                    .filter(|actor| !actor.stopping && !state.closed)
                    .map(|actor| {
                        (
                            actor.module.clone(),
                            actor.account.limit(),
                            actor.mailbox.slots.len(),
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
        if state.closed || state.actor(self.identity, pid).is_err() {
            return (wings_abi::STATUS_CANCELLED, 0);
        }
        let result: Result<i64, String> = (|| {
            let actor = state.actor(self.identity, pid)?;
            if actor.stopping {
                return Err("actor stopping".into());
            }
            let module = actor.module.clone();
            match effect.operation {
                wings_abi::EFFECT_SELF_PID if effect.count == 0 => Ok(actor.capability),
                wings_abi::EFFECT_CANCEL_REQUEST => {
                    state.cancel(pid, effect.target)?;
                    Ok(0)
                }
                wings_abi::EFFECT_SEND
                | wings_abi::EFFECT_SEND_SELF
                | wings_abi::EFFECT_REQUEST => {
                    let target = if effect.operation == wings_abi::EFFECT_SEND_SELF {
                        pid
                    } else {
                        state.target(self.identity, effect.target)?
                    };
                    let schema = module
                        .manifest()
                        .handlers
                        .get(effect.handler)
                        .ok_or("invalid effect handler")?;
                    let handler = module.handler(&schema.name)?;
                    let arguments = &effect.arguments[..effect.count];
                    let target_actor = state.actor(self.identity, target)?;
                    validate(
                        target_actor,
                        &handler,
                        arguments,
                        false,
                        effect.record_mask,
                        &effect.lists,
                        &effect.bytes,
                    )?;
                    if effect.operation != wings_abi::EFFECT_REQUEST {
                        if schema.reply.is_some() {
                            return Err("send requires one-way entry".into());
                        }
                        let message = envelope(
                            target_actor,
                            &handler,
                            arguments,
                            budget,
                            effect.record_mask,
                            effect.lists,
                            effect.bytes,
                            false,
                            Reply::None,
                        )?;
                        state
                            .actor_mut(self.identity, target)?
                            .mailbox
                            .push(message)?;
                        state.enqueue(target);
                        return Ok(0);
                    }
                    if !matches!(
                        schema.reply,
                        Some(
                            wings_abi::ValueType::Int
                                | wings_abi::ValueType::Bool
                                | wings_abi::ValueType::Unit
                                | wings_abi::ValueType::Ref
                                | wings_abi::ValueType::ListRef
                                | wings_abi::ValueType::BytesRef
                                | wings_abi::ValueType::StringRef
                        )
                    ) || !(0..=86_400_000).contains(&effect.timeout_ms)
                    {
                        return Err("invalid language request".into());
                    }
                    let callback = module
                        .manifest()
                        .handlers
                        .get(effect.callback)
                        .filter(|callback| callback.completion_reply() == schema.reply)
                        .ok_or("invalid completion handler")?;
                    let callback = module.handler(&callback.name)?;
                    let index = state
                        .requests
                        .iter()
                        .position(|slot| slot.generation != 0 && slot.pending.is_none())
                        .ok_or("scheduler request table full")?;
                    let owner = state.actor(self.identity, pid)?;
                    let request_charge = owner.account.reserve(Resources {
                        requests: 1,
                        ..Resources::default()
                    })?;
                    let completion_charge = owner.account.reserve(Resources {
                        messages: 1,
                        bytes: std::mem::size_of::<Envelope>() as u64,
                        ..Resources::default()
                    })?;
                    let capability = next_identity(&NEXT_CAPABILITY)? as i64;
                    let key = RequestKey {
                        slot: index,
                        generation: state.requests[index].generation,
                    };
                    let message = envelope(
                        target_actor,
                        &handler,
                        arguments,
                        budget,
                        effect.record_mask,
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
                            .ok_or("request deadline overflow")?,
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
                        .actor_mut(self.identity, target)?
                        .mailbox
                        .push(message)?;
                    state.requests[index].pending = Some(pending);
                    state.enqueue(target);
                    Ok(capability)
                }
                _ => Err("unsupported scheduler effect".into()),
            }
        })();
        drop(state);
        self.changed.notify_all();
        match result {
            Ok(value) => (wings_abi::STATUS_OK, value),
            Err(_) => (
                match effect.operation {
                    wings_abi::EFFECT_REQUEST | wings_abi::EFFECT_CANCEL_REQUEST => {
                        wings_abi::STATUS_REQUEST_REJECTED
                    }
                    wings_abi::EFFECT_SEND | wings_abi::EFFECT_SEND_SELF => {
                        wings_abi::STATUS_SEND_REJECTED
                    }
                    _ => wings_abi::STATUS_INVALID_CONTROL,
                },
                0,
            ),
        }
    }
}
