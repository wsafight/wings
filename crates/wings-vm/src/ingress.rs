use std::sync::{Arc, Mutex};

use wings_abi::ValueType;

use crate::{HandlerId, Reservation, ResourceAccount, Resources, WorkerNotifier};

const MAX_ARGUMENTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngressWake {
    Notified,
    WakeFailed,
}

#[derive(Clone)]
pub struct IngressSender {
    shared: Arc<Shared>,
}

pub(crate) struct IngressReceiver {
    shared: Arc<Shared>,
    pub(crate) handler: HandlerId,
    pub(crate) max_steps: u64,
}

struct Shared {
    ring: Mutex<Ring>,
    parameters: [ValueType; MAX_ARGUMENTS],
    count: usize,
    message_bytes: u64,
    account: ResourceAccount,
    notifier: WorkerNotifier,
    _wait_charge: Arc<Reservation>,
    _charge: Reservation,
}

struct Ring {
    slots: Box<[Option<Envelope>]>,
    head: usize,
    count: usize,
    closed: bool,
}

pub(crate) struct Envelope {
    pub(crate) arguments: [i64; MAX_ARGUMENTS],
    pub(crate) count: usize,
    pub(crate) charge: Reservation,
}

impl IngressSender {
    pub fn send(&self, arguments: &[i64]) -> Result<IngressWake, String> {
        self.send_with_notification(arguments, || self.shared.notifier.notify())
    }

    fn send_with_notification(
        &self,
        arguments: &[i64],
        notify: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<IngressWake, String> {
        if arguments.len() != self.shared.count
            || arguments
                .iter()
                .zip(&self.shared.parameters)
                .any(|(value, ty)| !ty.accepts(*value))
        {
            return Err("ingress argument count or scalar type mismatch".into());
        }
        let charge = self.shared.account.reserve(Resources {
            messages: 1,
            bytes: self.shared.message_bytes,
            ..Resources::default()
        })?;
        let mut owned = [0; MAX_ARGUMENTS];
        owned[..arguments.len()].copy_from_slice(arguments);
        let message = Envelope {
            arguments: owned,
            count: arguments.len(),
            charge,
        };
        let mut ring = self
            .shared
            .ring
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let rejected = if ring.closed {
            Some("ingress_closed")
        } else if ring.count == ring.slots.len() {
            Some("ingress_full")
        } else {
            None
        };
        if let Some(error) = rejected {
            drop(ring);
            return Err(error.into());
        }
        let tail = (ring.head + ring.count) % ring.slots.len();
        ring.slots[tail] = Some(message);
        ring.count += 1;
        drop(ring);
        Ok(notification_result(notify()))
    }
}

fn notification_result(result: std::io::Result<()>) -> IngressWake {
    match result {
        Ok(()) => IngressWake::Notified,
        Err(_) => IngressWake::WakeFailed,
    }
}

impl IngressReceiver {
    pub(crate) fn new(
        account: &ResourceAccount,
        handler: HandlerId,
        capacity: usize,
        max_steps: u64,
        message_bytes: u64,
        notifier: WorkerNotifier,
        wait_charge: Arc<Reservation>,
    ) -> Result<(Self, IngressSender), String> {
        if !(1..=64).contains(&capacity) || max_steps == 0 {
            return Err("ingress requires 1..=64 slots and a nonzero execution budget".into());
        }
        let schema = handler.schema();
        if schema.reply.is_some()
            || schema.is_completion()
            || schema.parameters.len() > MAX_ARGUMENTS
            || schema
                .parameters
                .iter()
                .any(|(_, ty)| !matches!(ty, ValueType::Int | ValueType::Bool | ValueType::Unit))
        {
            return Err("ingress requires a scalar one-way handler".into());
        }
        let bytes = std::mem::size_of::<Shared>()
            + capacity * std::mem::size_of::<Option<Envelope>>()
            + 256;
        let charge = account.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| "ingress storage allocation failed")?;
        slots.resize_with(capacity, || None);
        let mut parameters = [ValueType::Unit; MAX_ARGUMENTS];
        for (target, (_, ty)) in parameters.iter_mut().zip(&schema.parameters) {
            *target = *ty;
        }
        let shared = Arc::new(Shared {
            ring: Mutex::new(Ring {
                slots: slots.into_boxed_slice(),
                head: 0,
                count: 0,
                closed: false,
            }),
            parameters,
            count: schema.parameters.len(),
            message_bytes,
            account: account.clone(),
            notifier,
            _wait_charge: wait_charge,
            _charge: charge,
        });
        Ok((
            Self {
                shared: shared.clone(),
                handler,
                max_steps,
            },
            IngressSender { shared },
        ))
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.shared
            .ring
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .count
            != 0
    }

    pub(crate) fn take(&self) -> Option<Envelope> {
        let mut ring = self
            .shared
            .ring
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if ring.count == 0 {
            return None;
        }
        let head = ring.head;
        let message = ring.slots[head].take();
        ring.head = (head + 1) % ring.slots.len();
        ring.count -= 1;
        message
    }
}

impl Drop for IngressReceiver {
    fn drop(&mut self) {
        let slots = {
            let mut ring = self
                .shared
                .ring
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            ring.closed = true;
            ring.count = 0;
            std::mem::take(&mut ring.slots)
        };
        drop(slots);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_error_is_accepted_not_a_retryable_rejection() {
        assert_eq!(notification_result(Ok(())), IngressWake::Notified);
        assert_eq!(
            notification_result(Err(std::io::Error::other("fixture"))),
            IngressWake::WakeFailed
        );
        let account = ResourceAccount::vm(Resources {
            bytes: 4096,
            messages: 1,
            ..Resources::default()
        });
        let wait_charge = Arc::new(account.reserve(Resources::default()).unwrap());
        let parker = crate::WorkerParker::new().unwrap();
        let sender = IngressSender {
            shared: Arc::new(Shared {
                ring: Mutex::new(Ring {
                    slots: vec![None].into_boxed_slice(),
                    head: 0,
                    count: 0,
                    closed: false,
                }),
                parameters: [ValueType::Unit; MAX_ARGUMENTS],
                count: 0,
                message_bytes: 16,
                account: account.clone(),
                notifier: parker.notifier(),
                _wait_charge: wait_charge,
                _charge: account.reserve(Resources::default()).unwrap(),
            }),
        };
        assert_eq!(
            sender
                .send_with_notification(&[], || Err(std::io::Error::other("fixture")))
                .unwrap(),
            IngressWake::WakeFailed
        );
        assert_eq!(sender.shared.ring.lock().unwrap().count, 1);
        assert_eq!(account.used().messages, 1);
        assert!(sender.send(&[]).is_err());
        drop(sender);
        assert_eq!(account.used(), Resources::default());
    }

    #[test]
    fn sender_is_safe_to_share_without_moving_the_worker() {
        fn assert_send_sync<Value: Send + Sync>() {}
        assert_send_sync::<IngressSender>();
    }
}
