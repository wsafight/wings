use super::*;
use std::sync::OnceLock;
use std::thread::Thread;

pub(super) const WAKE_GLOBAL_BIT: u32 = 1 << 31;

#[derive(Default)]
pub(super) struct ReadyQueue {
    pub(super) head: Option<usize>,
    tail: Option<usize>,
}

pub(super) struct WorkerWakeHub {
    parked_mask: AtomicU32,
    threads: [OnceLock<Thread>; 16],
}

impl WorkerWakeHub {
    pub(super) fn new() -> Self {
        Self {
            parked_mask: AtomicU32::new(0),
            threads: std::array::from_fn(|_| OnceLock::new()),
        }
    }

    pub(super) fn register(&self, worker: usize) {
        if let Some(slot) = self.threads.get(worker) {
            let _ = slot.set(std::thread::current());
        }
    }

    pub(super) fn mark_parked(&self, bit: u32) {
        self.parked_mask.fetch_or(bit, Ordering::SeqCst);
    }

    pub(super) fn clear_parked(&self, bit: u32) {
        self.parked_mask.fetch_and(!bit, Ordering::SeqCst);
    }

    pub(super) fn wake_bits(&self, bits: u32) {
        let mut mask = bits & self.parked_mask.load(Ordering::SeqCst);
        while mask != 0 {
            let worker = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if let Some(thread) = self.threads.get(worker).and_then(OnceLock::get) {
                thread.unpark();
            }
        }
    }

    #[cfg(test)]
    pub(super) fn notify_all(&self) {
        self.wake_bits(u32::MAX);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct WakeSignal(u32);

impl WakeSignal {
    pub(super) const NONE: Self = Self(0);
    pub(super) const ALL: Self = Self(u32::MAX);

    pub(super) fn worker(index: usize) -> Self {
        Self(1u32 << index)
    }

    pub(super) fn combine(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(super) fn fire(self, hub: &WorkerWakeHub) {
        if self.0 != 0 {
            hub.wake_bits(self.0);
        }
    }
}

impl State {
    pub(super) fn signal_bits(&self) -> u32 {
        let global = if self.closed || !self.requests.is_empty_fast() {
            WAKE_GLOBAL_BIT
        } else {
            0
        };
        self.ready_bits | global
    }

    pub(super) fn wake_all_parked(&self) -> WakeSignal {
        WakeSignal::ALL
    }

    pub(super) fn should_park(&self, worker: usize, progress: bool, attempted: bool) -> bool {
        !progress && (attempted || self.ready[worker].head.is_none())
    }

    pub(super) fn reserve_slot(&mut self) -> Result<usize, String> {
        let index = self.free.ok_or("scheduler process table full")?;
        let slot = &mut self.slots[index];
        self.free = slot.free_next.take();
        assert!(!slot.creating && slot.actor.is_none() && slot.generation != 0);
        slot.creating = true;
        Ok(index)
    }

    pub(super) fn release_slot(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        assert!(slot.actor.is_none() && slot.ready_worker.is_none());
        slot.creating = false;
        if slot.generation != 0 {
            slot.free_next = self.free;
            self.free = Some(index);
        }
    }

    fn link_ready(&mut self, index: usize, worker: usize) -> WakeSignal {
        let previous = self.ready[worker].tail;
        if let Some(previous) = previous {
            self.slots[previous].ready_next = Some(index);
        } else {
            self.ready[worker].head = Some(index);
        }
        self.ready[worker].tail = Some(index);
        let slot = &mut self.slots[index];
        slot.ready_previous = previous;
        slot.ready_next = None;
        slot.ready_worker = Some(worker);
        let bit = 1u32 << worker;
        self.ready_bits |= bit;
        WakeSignal::worker(worker)
    }

    pub(super) fn enqueue(&mut self, pid: SchedulerPid) -> WakeSignal {
        let index = pid.slot as usize;
        let slot = &self.slots[index];
        if self.closed || slot.generation != pid.generation || slot.ready_worker.is_some() {
            return WakeSignal::NONE;
        }
        let Some(actor) = slot.actor.as_ref() else {
            return WakeSignal::NONE;
        };
        if actor.owner.is_some() || actor.stopping {
            return WakeSignal::NONE;
        }
        if !actor.mailbox.mark_enqueued_if_nonempty() {
            return WakeSignal::NONE;
        }
        let worker = actor
            .last_worker
            .map_or(index % self.ready.len(), |previous| {
                (previous + 1) % self.ready.len()
            });
        self.link_ready(index, worker)
    }

    pub(super) fn finish_enqueue(&mut self, pid: SchedulerPid, failed: bool) -> WakeSignal {
        let index = pid.slot as usize;
        let slot = &self.slots[index];
        if slot.generation != pid.generation {
            return WakeSignal::NONE;
        }
        let Some(actor) = slot.actor.as_ref() else {
            return WakeSignal::NONE;
        };
        assert!(actor.owner.is_none());
        if failed || self.closed || actor.stopping {
            actor.mailbox.set_active_or_queued(false);
            return WakeSignal::NONE;
        }
        if slot.ready_worker.is_some() {
            return WakeSignal::NONE;
        }
        if !actor.mailbox.keep_active_if_nonempty() {
            return WakeSignal::NONE;
        }
        let worker = actor
            .last_worker
            .map_or(index % self.ready.len(), |previous| {
                (previous + 1) % self.ready.len()
            });
        self.link_ready(index, worker)
    }

    fn unlink_ready(&mut self, index: usize) -> Option<usize> {
        let slot = &mut self.slots[index];
        let worker = slot.ready_worker.take()?;
        let previous = slot.ready_previous.take();
        let next = slot.ready_next.take();
        if let Some(previous) = previous {
            self.slots[previous].ready_next = next;
        } else {
            self.ready[worker].head = next;
            if next.is_none() {
                self.ready_bits &= !(1u32 << worker);
            }
        }
        if let Some(next) = next {
            self.slots[next].ready_previous = previous;
        } else {
            self.ready[worker].tail = previous;
        }
        Some(worker)
    }

    pub(super) fn remove_ready(&mut self, index: usize) {
        if self.unlink_ready(index).is_some() {
            if let Some(actor) = self.slots[index].actor.as_ref() {
                if actor.owner.is_none() {
                    actor.mailbox.set_active_or_queued(false);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn pop_ready(&mut self, worker: usize) -> Option<usize> {
        let index = self.ready[worker].head?;
        self.remove_ready(index);
        Some(index)
    }

    pub(super) fn pop_ready_for_claim(&mut self, worker: usize) -> Option<usize> {
        let index = self.ready[worker].head?;
        self.unlink_ready(index);
        Some(index)
    }
}
