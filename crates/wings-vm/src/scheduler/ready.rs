use super::*;

#[derive(Default)]
pub(super) struct ReadyQueue {
    head: Option<usize>,
    tail: Option<usize>,
}

impl State {
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

    pub(super) fn enqueue(&mut self, pid: SchedulerPid) {
        let index = pid.slot as usize;
        let slot = &self.slots[index];
        if self.closed || slot.generation != pid.generation || slot.ready_worker.is_some() {
            return;
        }
        let Some(actor) = slot.actor.as_ref() else {
            return;
        };
        if actor.owner.is_some() || actor.stopping || actor.mailbox.count == 0 {
            return;
        }
        let worker = actor
            .last_worker
            .map_or(index % self.ready.len(), |previous| {
                (previous + 1) % self.ready.len()
            });
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
    }

    pub(super) fn remove_ready(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        let Some(worker) = slot.ready_worker.take() else {
            return;
        };
        let previous = slot.ready_previous.take();
        let next = slot.ready_next.take();
        if let Some(previous) = previous {
            self.slots[previous].ready_next = next;
        } else {
            self.ready[worker].head = next;
        }
        if let Some(next) = next {
            self.slots[next].ready_previous = previous;
        } else {
            self.ready[worker].tail = previous;
        }
    }

    pub(super) fn pop_ready(&mut self, worker: usize) -> Option<usize> {
        let index = self.ready[worker].head?;
        self.remove_ready(index);
        Some(index)
    }
}
