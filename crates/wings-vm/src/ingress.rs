use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wings_abi::ValueType;

use crate::host_values::HostMessage;
use crate::ingress_requests::{ReplyPublisher, ReplyTable};
use crate::message_values::{BytePayload, ListPayload};
use crate::{
    HandlerId, IngressRequest, MessageValue, Reservation, ResourceAccount, Resources,
    WorkerNotifier,
};

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
    replies: Option<(ValueType, Arc<ReplyTable>)>,
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
    pub(crate) record_mask: u64,
    pub(crate) lists: ListPayload,
    pub(crate) bytes: BytePayload,
    pub(crate) charge: Reservation,
    pub(crate) reply: Option<ReplyPublisher>,
}

impl IngressSender {
    pub fn send_values(&self, values: &[MessageValue<'_>]) -> Result<IngressWake, String> {
        if self.shared.replies.is_some() {
            return Err("send requires a one-way ingress".into());
        }
        let message = self.copy_values(values)?;
        self.publish_message(message, None, || self.shared.notifier.notify())
    }

    pub fn request_values(
        &self,
        values: &[MessageValue<'_>],
        timeout: Duration,
    ) -> Result<(IngressRequest, IngressWake), String> {
        let (reply_type, replies) = self
            .shared
            .replies
            .as_ref()
            .ok_or("request requires a reply ingress")?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("ingress request deadline overflow")?;
        let message = self.copy_values(values)?;
        let (request, publisher) = replies.start(deadline, &self.shared.account, *reply_type)?;
        let wake =
            self.publish_message(message, Some(publisher), || self.shared.notifier.notify())?;
        Ok((request, wake))
    }

    fn copy_values(&self, values: &[MessageValue<'_>]) -> Result<HostMessage, String> {
        HostMessage::copy_types(
            self.shared.parameters[..self.shared.count].iter().copied(),
            values,
            &self.shared.account,
        )
    }

    pub fn send(&self, arguments: &[i64]) -> Result<IngressWake, String> {
        if self.shared.replies.is_some() {
            return Err("send requires a one-way ingress".into());
        }
        self.send_with_notification(arguments, || self.shared.notifier.notify())
    }

    pub fn request(
        &self,
        arguments: &[i64],
        timeout: Duration,
    ) -> Result<(IngressRequest, IngressWake), String> {
        let (reply_type, replies) = self
            .shared
            .replies
            .as_ref()
            .ok_or("request requires a reply ingress")?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("ingress request deadline overflow")?;
        self.validate_arguments(arguments)?;
        let (request, publisher) = replies.start(deadline, &self.shared.account, *reply_type)?;
        let wake = self.publish(arguments, Some(publisher), || self.shared.notifier.notify())?;
        Ok((request, wake))
    }

    fn send_with_notification(
        &self,
        arguments: &[i64],
        notify: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<IngressWake, String> {
        self.publish(arguments, None, notify)
    }

    fn validate_arguments(&self, arguments: &[i64]) -> Result<(), String> {
        if arguments.len() != self.shared.count
            || arguments
                .iter()
                .zip(&self.shared.parameters)
                .any(|(value, ty)| !ty.accepts(*value))
        {
            return Err("ingress argument count or scalar type mismatch".into());
        }
        Ok(())
    }

    fn publish(
        &self,
        arguments: &[i64],
        reply: Option<ReplyPublisher>,
        notify: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<IngressWake, String> {
        self.validate_arguments(arguments)?;
        let mut owned = [0; MAX_ARGUMENTS];
        owned[..arguments.len()].copy_from_slice(arguments);
        self.publish_message(
            HostMessage {
                arguments: owned,
                count: arguments.len(),
                record_mask: 0,
                lists: ListPayload::default(),
                bytes: BytePayload::default(),
            },
            reply,
            notify,
        )
    }

    fn publish_message(
        &self,
        payload: HostMessage,
        reply: Option<ReplyPublisher>,
        notify: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<IngressWake, String> {
        let charge = self.shared.account.reserve(Resources {
            messages: 1,
            bytes: self.shared.message_bytes,
            ..Resources::default()
        })?;
        let message = Envelope {
            arguments: payload.arguments,
            count: payload.count,
            record_mask: payload.record_mask,
            lists: payload.lists,
            bytes: payload.bytes,
            charge,
            reply,
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
    ) -> Result<(Self, IngressSender), String> {
        if !(1..=64).contains(&capacity) || max_steps == 0 {
            return Err("ingress requires 1..=64 slots and a nonzero execution budget".into());
        }
        let schema = handler.schema();
        if schema.is_completion()
            || schema.reply.is_some_and(|ty| {
                !matches!(
                    ty,
                    ValueType::Int
                        | ValueType::Bool
                        | ValueType::Unit
                        | ValueType::Ref
                        | ValueType::ListRef
                        | ValueType::BytesRef
                        | ValueType::StringRef
                )
            })
            || schema.parameters.len() > MAX_ARGUMENTS
            || schema.parameters.iter().any(|(_, ty)| {
                !matches!(
                    ty,
                    ValueType::Int
                        | ValueType::Bool
                        | ValueType::Unit
                        | ValueType::Ref
                        | ValueType::ListRef
                        | ValueType::BytesRef
                        | ValueType::StringRef
                )
            })
        {
            return Err("ingress requires supported message values and reply types".into());
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
        let replies = schema
            .reply
            .map(|ty| ReplyTable::new(account, capacity).map(|table| (ty, table)))
            .transpose()?;
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
            replies,
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

    fn typed_fixture(
        types: &[ValueType],
        reply: Option<ValueType>,
        capacity: usize,
    ) -> (
        ResourceAccount,
        ResourceAccount,
        ResourceAccount,
        crate::WorkerParker,
        IngressReceiver,
        IngressSender,
    ) {
        let limit = Resources {
            bytes: 2 << 20,
            messages: 4,
            requests: 4,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limit);
        let tenant = vm.tenant(limit).unwrap();
        let process = tenant.process(limit).unwrap();
        let mut module = crate::tests::module();
        let data = Arc::get_mut(&mut module.inner).unwrap();
        data.manifest.handlers[0].parameters = types
            .iter()
            .enumerate()
            .map(|(index, ty)| (format!("value_{index}"), *ty))
            .collect();
        data.manifest.handlers[0].reply = reply;
        let parker = crate::WorkerParker::new_accounted(&process).unwrap();
        let (receiver, sender) = IngressReceiver::new(
            &process,
            module.handler("Add").unwrap(),
            capacity,
            128,
            2048,
            parker.notifier(),
        )
        .unwrap();
        (vm, tenant, process, parker, receiver, sender)
    }

    #[test]
    fn typed_ingress_copies_mixed_values_and_preserves_scalar_rejection() {
        let types = [
            ValueType::Ref,
            ValueType::ListRef,
            ValueType::StringRef,
            ValueType::BytesRef,
            ValueType::Bool,
            ValueType::Unit,
        ];
        let (vm, tenant, account, parker, receiver, sender) = typed_fixture(&types, None, 1);
        let baseline = vm.used();
        assert!(sender.send(&[0; 6]).is_err());
        let mut text = String::from("中😀\0");
        let mut bytes = vec![17; 65536 - text.len()];
        let mut integers = [i64::MIN; 64];
        sender
            .send_values(&[
                MessageValue::RecordInt(7),
                MessageValue::IntList(&integers),
                MessageValue::String(&text),
                MessageValue::Bytes(&bytes),
                MessageValue::Bool(true),
                MessageValue::Unit,
            ])
            .unwrap();
        let full = vm.used();
        assert_eq!(full.bytes, baseline.bytes + 65536 + 2048);
        assert_eq!(full.messages, 1);
        assert!(
            sender
                .send_values(&[
                    MessageValue::RecordInt(7),
                    MessageValue::IntList(&integers),
                    MessageValue::String(&text),
                    MessageValue::Bytes(&bytes),
                    MessageValue::Bool(true),
                    MessageValue::Unit
                ])
                .is_err()
        );
        assert_eq!(vm.used(), full);
        text.clear();
        bytes.fill(99);
        integers.fill(99);
        let envelope = receiver.take().unwrap();
        assert_eq!(envelope.record_mask, 1);
        assert_eq!(envelope.lists.mask, 2);
        assert_eq!(envelope.lists.values, [i64::MIN; 64]);
        assert_eq!(envelope.bytes.mask, 12);
        assert_eq!(envelope.bytes.string_mask, 4);
        assert_eq!(&envelope.bytes.data[..8], "中😀\0".as_bytes());
        assert!(envelope.bytes.data[8..].iter().all(|byte| *byte == 17));
        drop(envelope);
        assert_eq!(vm.used(), baseline);
        drop((receiver, sender, parker, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn typed_ingress_validates_limits_and_each_account_before_publication() {
        let (vm, tenant, account, parker, receiver, sender) =
            typed_fixture(&[ValueType::BytesRef, ValueType::ListRef], None, 1);
        let baseline = vm.used();
        for values in [
            vec![MessageValue::Int(0), MessageValue::IntList(&[])],
            vec![MessageValue::Bytes(&[0; 65537]), MessageValue::IntList(&[])],
            vec![MessageValue::Bytes(&[]), MessageValue::IntList(&[0; 65])],
            vec![],
        ] {
            assert!(sender.send_values(&values).is_err());
            assert_eq!(vm.used(), baseline);
        }
        for owner in [&account, &tenant, &vm] {
            let fill = owner
                .reserve(Resources {
                    bytes: owner.limit().bytes - owner.used().bytes,
                    ..Resources::default()
                })
                .unwrap();
            let full = vm.used();
            assert!(
                sender
                    .send_values(&[MessageValue::Bytes(&[1]), MessageValue::IntList(&[])])
                    .is_err()
            );
            assert_eq!(vm.used(), full);
            assert!(receiver.take().is_none());
            drop(fill);
        }
        drop(receiver);
        assert!(
            sender
                .send_values(&[MessageValue::Bytes(&[1]), MessageValue::IntList(&[])])
                .unwrap_err()
                .contains("closed")
        );
        assert_eq!(vm.used(), baseline);
        drop((sender, parker, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn typed_ingress_timeout_cancel_and_drop_retain_the_old_publisher() {
        for terminal in 0..3 {
            let (vm, tenant, account, parker, receiver, sender) =
                typed_fixture(&[ValueType::StringRef], Some(ValueType::Int), 1);
            let baseline = vm.used();
            let timeout = if terminal == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs(1)
            };
            let (mut request, _) = sender
                .request_values(&[MessageValue::String("hello")], timeout)
                .unwrap();
            if terminal == 1 {
                request.cancel().unwrap();
            }
            if terminal != 2 {
                assert_eq!(
                    request.try_take().unwrap(),
                    Some(if terminal == 0 {
                        crate::RequestOutcome::TimedOut
                    } else {
                        crate::RequestOutcome::Cancelled
                    })
                );
            }
            drop(request);
            let full = vm.used();
            assert_eq!(full.requests, 1);
            assert!(
                sender
                    .request_values(&[MessageValue::String("other")], Duration::ZERO)
                    .is_err()
            );
            assert_eq!(vm.used(), full);
            let envelope = receiver.take().unwrap();
            assert_eq!(envelope.bytes.data, b"hello");
            assert_eq!(vm.used(), full);
            drop(envelope);
            assert_eq!(vm.used(), baseline);
            drop((sender, receiver, parker, account, tenant));
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn typed_ingress_notification_failure_keeps_the_published_copy() {
        let (vm, tenant, account, parker, receiver, sender) =
            typed_fixture(&[ValueType::StringRef], None, 1);
        let baseline = vm.used();
        let payload = sender.copy_values(&[MessageValue::String("中😀")]).unwrap();
        assert_eq!(
            sender
                .publish_message(payload, None, || Err(std::io::Error::other("fixture")))
                .unwrap(),
            IngressWake::WakeFailed
        );
        assert_eq!(vm.used().messages, baseline.messages + 1);
        let envelope = receiver.take().unwrap();
        assert_eq!(envelope.bytes.data, "中😀".as_bytes());
        drop(envelope);
        assert_eq!(vm.used(), baseline);
        drop((receiver, sender, parker, account, tenant));
        assert_eq!(vm.used(), Resources::default());
    }

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
                replies: None,
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
